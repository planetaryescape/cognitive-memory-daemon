//! HTTP bridge for cognitive-memory-daemon.
//!
//! Loopback-only HTTP/JSON proxy to the daemon's Unix socket. Per
//! ADR 0005, refuses to bind any non-loopback address. Per `SECURITY.md`
//! §2 T5, every request requires `Authorization: Bearer <token>`. Tokens
//! are minted by the daemon (via `Diagnostics::MintBridgeToken` once that
//! request kind ships) and stored hashed.
//!
//! Phase 12 v1: socket binding refusal logic, bearer-token validation
//! against an in-memory token map (with hashed-at-rest discipline), and
//! POST routes for `/memory/store` and `/memory/search`. Live token mint
//! via `Diagnostics::MintBridgeToken` lands when the daemon protocol
//! grows that request kind.

// `result_large_err`: BridgeError carries a SocketAddr; that's the right
// shape for the error and boxing it would obscure the API for callers.
// `match_like_matches_macro`: `Scope::allows` uses tuple pattern matching
// across multi-arm cases; `matches!` is less readable here.
#![allow(clippy::result_large_err, clippy::match_like_matches_macro)]

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use cognitive_memory_client::Client;
use cognitive_memory_protocol::{
    MemoryRequest, Request, Response as DaemonResponse, SearchMemoryArgs, StoreMemoryArgs,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use subtle::ConstantTimeEq;
use tracing::warn;

/// Errors surfaced by the bridge during setup.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("the bridge refuses to bind a non-loopback address: {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Refuse to bind anything but loopback. Called from `cm-http` main and
/// any test that constructs a router with a real bind address.
pub fn enforce_loopback(addr: SocketAddr) -> Result<SocketAddr, BridgeError> {
    if !addr.ip().is_loopback() {
        return Err(BridgeError::NonLoopbackBind(addr));
    }
    Ok(addr)
}

/// Token capability scope. Mirrors ADR 0005.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    Write,
    Admin,
}

impl Scope {
    fn allows(self, required: Scope) -> bool {
        match (self, required) {
            (Scope::Admin, _) => true,
            (Scope::Write, Scope::Read | Scope::Write) => true,
            (Scope::Read, Scope::Read) => true,
            _ => false,
        }
    }
}

/// In-memory token store. Production keeps tokens hashed in `kv`; this
/// type accepts pre-hashed lookups so the storage layer can swap without
/// the bridge reading raw tokens. Token is hashed with `Sha256(token)` —
/// the bridge never persists the raw value.
pub struct TokenStore {
    inner: Mutex<HashMap<Vec<u8>, TokenInfo>>,
    salt: Vec<u8>,
}

#[derive(Debug, Clone)]
struct TokenInfo {
    user_id: String,
    scope: Scope,
}

impl TokenStore {
    pub fn new(salt: impl Into<Vec<u8>>) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            salt: salt.into(),
        }
    }

    /// Insert a token (hashed with the configured salt). Returns the raw
    /// token bytes the caller should pass to the issued holder. **The raw
    /// token is shown to the user once at mint time and never recoverable.**
    pub fn mint(&self, raw_token: &[u8], user_id: impl Into<String>, scope: Scope) {
        let hash = self.hash(raw_token);
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(
                hash,
                TokenInfo {
                    user_id: user_id.into(),
                    scope,
                },
            );
        }
    }

    /// Validate an incoming token. Constant-time comparison against the
    /// stored hash.
    pub fn validate(&self, raw_token: &[u8]) -> Option<(String, Scope)> {
        let candidate_hash = self.hash(raw_token);
        let guard = self.inner.lock().ok()?;
        for (stored_hash, info) in guard.iter() {
            if stored_hash.ct_eq(&candidate_hash).into() {
                return Some((info.user_id.clone(), info.scope));
            }
        }
        None
    }

    fn hash(&self, raw: &[u8]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(&self.salt);
        hasher.update(raw);
        hasher.finalize().to_vec()
    }
}

/// Shared state for the axum app.
pub struct AppState {
    pub socket_path: PathBuf,
    pub tokens: TokenStore,
}

/// Build the axum router with the routes Phase 12 ships. Add new routes
/// here as the daemon protocol grows.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/memory/store", post(memory_store))
        .route("/memory/search", post(memory_search))
        .with_state(std::sync::Arc::new(state))
}

#[derive(Debug, Deserialize)]
struct StoreBody {
    content: String,
    #[serde(default = "default_category")]
    category: String,
    #[serde(default = "default_memory_type")]
    memory_type: String,
    #[serde(default = "default_metadata")]
    metadata: String,
    #[serde(default)]
    importance: Option<f64>,
}
fn default_category() -> String {
    "semantic".to_string()
}
fn default_memory_type() -> String {
    "fact".to_string()
}
fn default_metadata() -> String {
    "{}".to_string()
}

#[derive(Debug, Deserialize)]
struct SearchBody {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    deep_recall: bool,
    #[serde(default)]
    hybrid: bool,
}
fn default_limit() -> usize {
    10
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

async fn memory_store(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<StoreBody>,
) -> Response {
    let (user_id, _scope) = match check_auth(&state.tokens, &headers, Scope::Write) {
        Ok(c) => c,
        Err(r) => return r,
    };

    let mut client = match Client::connect(&state.socket_path, "cm-http", &user_id).await {
        Ok(c) => c,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("connect: {e}")),
    };

    let req = Request::Memory(MemoryRequest::Store(StoreMemoryArgs {
        user_id,
        content: body.content,
        category: body.category,
        memory_type: body.memory_type,
        metadata: body.metadata,
        importance: body.importance,
    }));
    daemon_call(&mut client, req).await
}

async fn memory_search(
    State(state): State<std::sync::Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SearchBody>,
) -> Response {
    let (user_id, _scope) = match check_auth(&state.tokens, &headers, Scope::Read) {
        Ok(c) => c,
        Err(r) => return r,
    };

    let mut client = match Client::connect(&state.socket_path, "cm-http", &user_id).await {
        Ok(c) => c,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("connect: {e}")),
    };

    let req = Request::Memory(MemoryRequest::Search(SearchMemoryArgs {
        user_id,
        query: body.query,
        limit: body.limit,
        deep_recall: body.deep_recall,
        hybrid: body.hybrid,
    }));
    daemon_call(&mut client, req).await
}

fn check_auth(
    tokens: &TokenStore,
    headers: &HeaderMap,
    required_scope: Scope,
) -> Result<(String, Scope), Response> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "missing Authorization header"))?;
    let token = auth
        .strip_prefix("Bearer ")
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "expected Bearer token"))?;

    match tokens.validate(token.as_bytes()) {
        Some((uid, scope)) if scope.allows(required_scope) => Ok((uid, scope)),
        Some(_) => Err(error(StatusCode::FORBIDDEN, "token scope insufficient")),
        None => {
            warn!("rejected unknown bearer token");
            Err(error(StatusCode::UNAUTHORIZED, "invalid token"))
        }
    }
}

async fn daemon_call(client: &mut Client, req: Request) -> Response {
    match client.request(req).await {
        Ok(resp) => daemon_response_to_http(resp),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("daemon: {e}")),
    }
}

fn daemon_response_to_http(resp: DaemonResponse) -> Response {
    let status = if resp.ok {
        StatusCode::OK
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, Json(serde_json::to_value(resp).unwrap_or_default())).into_response()
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorBody {
            error: message.into(),
        }),
    )
        .into_response()
}
