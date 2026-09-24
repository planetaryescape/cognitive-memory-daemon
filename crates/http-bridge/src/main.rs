// `cm-http` HTTP bridge binary.
//
// Loopback-only HTTP/JSON proxy to the daemon's Unix socket. Per ADR 0005,
// refuses to bind any non-loopback address. Per `SECURITY.md` §2 T5, every
// request requires `Authorization: Bearer <token>`.

use anyhow::Context;
use axum::http::HeaderValue;
use cognitive_memory_core::{secure_private_file_if_exists, RuntimePaths};
use cognitive_memory_http_bridge::{enforce_loopback, router, AppState, Scope, TokenStore};
use std::env;
use std::net::SocketAddr;
use std::str::FromStr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind_str =
        env::var("COGNITIVE_MEMORY_HTTP_BIND").unwrap_or_else(|_| "127.0.0.1:7472".to_string());
    let addr = SocketAddr::from_str(&bind_str)
        .with_context(|| format!("parse bind address {bind_str}"))?;
    let addr = enforce_loopback(addr)?;

    let paths = RuntimePaths::resolve();
    let socket_path = paths.socket_path.clone();

    let salt = env::var("COGNITIVE_MEMORY_HTTP_SALT")
        .unwrap_or_else(|_| "cm-http-default-salt-change-me".to_string());

    let tokens = TokenStore::new(salt.into_bytes());

    // Two startup paths to seed the in-memory token store:
    //
    // 1. Daemon-minted (preferred). When `COGNITIVE_MEMORY_HTTP_MINT_USER`
    //    is set, the bridge connects to the daemon, calls
    //    `Diagnostics::MintBridgeToken`, registers the returned token
    //    locally, and writes the raw token to a private token file once.
    //    The daemon stores only a salted hash.
    //
    // 2. Env bootstrap (fallback). When `COGNITIVE_MEMORY_HTTP_BOOTSTRAP_TOKEN`
    //    is set, the bridge accepts that pre-shared token. Useful for
    //    test/CI flows where no live daemon mint is desired.
    if let Ok(mint_user) = env::var("COGNITIVE_MEMORY_HTTP_MINT_USER") {
        let scope_str =
            env::var("COGNITIVE_MEMORY_HTTP_MINT_SCOPE").unwrap_or_else(|_| "write".to_string());
        let scope = parse_scope(&scope_str);
        let proto_scope = match scope {
            Scope::Read => cognitive_memory_protocol::BridgeScope::Read,
            Scope::Write => cognitive_memory_protocol::BridgeScope::Write,
            Scope::Admin => cognitive_memory_protocol::BridgeScope::Admin,
        };
        let mut client =
            cognitive_memory_client::Client::connect(&socket_path, "cm-http", &mint_user).await?;
        let response = client
            .request(cognitive_memory_protocol::Request::Diagnostics(
                cognitive_memory_protocol::DiagnosticsRequest::MintBridgeToken(
                    cognitive_memory_protocol::MintBridgeTokenArgs {
                        user_id: mint_user.clone(),
                        scope: proto_scope,
                        ttl_seconds: 30 * 24 * 3600,
                    },
                ),
            ))
            .await?;
        if !response.ok {
            anyhow::bail!("daemon refused to mint bridge token: {:?}", response.error);
        }
        match response.data {
            Some(cognitive_memory_protocol::ResponseData::BridgeToken(t)) => {
                tracing::info!(
                    user = %mint_user,
                    expires_at_unix = t.expires_at_unix,
                    "bridge token minted from daemon"
                );
                write_startup_token_file(&paths, &mint_user, scope, &t.token, t.expires_at_unix)?;
                tracing::info!(
                    path = %paths.bridge_token_path.display(),
                    "bridge token written to private token file"
                );
                tokens.mint(t.token.as_bytes(), mint_user, scope);
            }
            other => anyhow::bail!("unexpected response data: {other:?}"),
        }
    } else if let Ok(bootstrap) = env::var("COGNITIVE_MEMORY_HTTP_BOOTSTRAP_TOKEN") {
        let user = env::var("COGNITIVE_MEMORY_HTTP_BOOTSTRAP_USER")
            .unwrap_or_else(|_| "default".to_string());
        let scope_str = env::var("COGNITIVE_MEMORY_HTTP_BOOTSTRAP_SCOPE")
            .unwrap_or_else(|_| "write".to_string());
        tokens.mint(bootstrap.as_bytes(), user, parse_scope(&scope_str));
        tracing::info!("bootstrap token registered from env (no daemon mint)");
    }

    let state = AppState {
        socket_path,
        tokens,
        allowed_hosts: parse_csv_env("COGNITIVE_MEMORY_HTTP_ALLOWED_HOSTS"),
        allowed_origins: parse_origin_env()?,
    };

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(bind = %addr, "cm-http listening");
    axum::serve(listener, router(state)).await?;

    Ok(())
}

fn parse_csv_env(name: &str) -> Vec<String> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_origin_env() -> anyhow::Result<Vec<HeaderValue>> {
    let origins = parse_csv_env("COGNITIVE_MEMORY_HTTP_ALLOWED_ORIGINS");
    let origins = if origins.is_empty() {
        parse_csv_env("COGNITIVE_MEMORY_HTTP_CORS_ORIGINS")
    } else {
        origins
    };
    origins
        .into_iter()
        .map(|origin| {
            HeaderValue::from_str(&origin)
                .with_context(|| format!("invalid HTTP bridge origin: {origin}"))
        })
        .collect()
}

fn parse_scope(s: &str) -> Scope {
    match s {
        "read" => Scope::Read,
        "admin" => Scope::Admin,
        _ => Scope::Write,
    }
}

fn write_startup_token_file(
    paths: &RuntimePaths,
    user_id: &str,
    scope: Scope,
    token: &str,
    expires_at_unix: i64,
) -> anyhow::Result<()> {
    if let Some(parent) = paths.bridge_token_path.parent() {
        cognitive_memory_core::ensure_private_dir(parent)?;
    }
    let scope = match scope {
        Scope::Read => "read",
        Scope::Write => "write",
        Scope::Admin => "admin",
    };
    let payload = serde_json::json!({
        "user_id": user_id,
        "scope": scope,
        "token": token,
        "expires_at_unix": expires_at_unix
    });
    std::fs::write(
        &paths.bridge_token_path,
        serde_json::to_vec_pretty(&payload)?,
    )?;
    secure_private_file_if_exists(&paths.bridge_token_path)?;
    Ok(())
}
