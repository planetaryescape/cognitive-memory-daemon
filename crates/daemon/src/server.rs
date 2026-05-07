//! IPC server: accept loop, per-connection task, dispatcher.

use crate::handlers::{handle_request, AppState};
use bytes::BytesMut;
use cognitive_memory_embeddings::EmbeddingProvider;
use cognitive_memory_protocol::{
    IpcCodec, IpcMessage, IpcPayload, Response, ResponseError, ResponseErrorKind,
    IPC_PROTOCOL_VERSION,
};
use cognitive_memory_store::Store;
use futures::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Semaphore};
use tokio_util::codec::{Decoder, Encoder, Framed, LengthDelimitedCodec};
use tracing::{debug, error, info, warn};

const REQUEST_CONCURRENCY_LIMIT: usize = 64;

/// Errors surfaced by the daemon's setup and runtime.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("socket bind: {0}")]
    Bind(std::io::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage: {0}")]
    Storage(#[from] sqlx::Error),
}

/// The daemon. Holds a store, an embedding provider, and (when running) a
/// Unix-socket accept loop. Constructed with explicit dependencies so tests
/// can swap in a `FakeEmbeddingProvider`.
pub struct Daemon {
    state: Arc<AppState>,
    socket_path: PathBuf,
    shutdown_tx: broadcast::Sender<()>,
}

impl Daemon {
    /// Construct a daemon with explicit dependencies. The caller is
    /// responsible for opening the `Store` and choosing the embedding
    /// provider; the daemon's job is to accept connections and dispatch.
    pub fn new(store: Store, embeddings: Arc<dyn EmbeddingProvider>, socket_path: PathBuf) -> Self {
        Self::new_with_llm(store, embeddings, socket_path, None)
    }

    /// Variant that wires an optional LLM provider into `AppState`.
    /// `None` keeps the daemon in heuristic-fallback mode for conflict
    /// resolution and disables consolidation summarisation. `Some`
    /// enables LLM-judged conflict resolution and consolidation per
    /// the SDK's `engine.tick()` pipeline (Stage 4 of the plan).
    pub fn new_with_llm(
        store: Store,
        embeddings: Arc<dyn EmbeddingProvider>,
        socket_path: PathBuf,
        llm: Option<Arc<dyn cognitive_memory_llm::LlmProvider>>,
    ) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        let state = Arc::new(AppState {
            store,
            embeddings,
            request_semaphore: Arc::new(Semaphore::new(REQUEST_CONCURRENCY_LIMIT)),
            started_at: Instant::now(),
            llm,
        });
        Self {
            state,
            socket_path,
            shutdown_tx,
        }
    }

    /// Get a handle that signals shutdown when fired.
    pub fn shutdown_handle(&self) -> broadcast::Sender<()> {
        self.shutdown_tx.clone()
    }

    /// Bind the socket and run until shutdown is signalled.
    pub async fn serve(&self) -> Result<(), DaemonError> {
        // Clean up a stale socket file if it exists. Per
        // ARCHITECTURE.md §3.1, a single-instance check on the PID file
        // would normally precede this — Phase 4 ships the basic version;
        // signal-probe single-instance lands when auto-spawn does.
        if self.socket_path.exists() {
            warn!(
                socket = %self.socket_path.display(),
                "removing stale socket file"
            );
            std::fs::remove_file(&self.socket_path)?;
        }

        let listener = UnixListener::bind(&self.socket_path).map_err(DaemonError::Bind)?;
        info!(
            socket = %self.socket_path.display(),
            "daemon listening"
        );

        let mut shutdown_rx = self.shutdown_tx.subscribe();
        loop {
            tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _addr)) => {
                            let state = Arc::clone(&self.state);
                            let mut conn_shutdown = self.shutdown_tx.subscribe();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, state, &mut conn_shutdown).await {
                                    debug!(error = %e, "connection ended with error");
                                }
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "accept failed");
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("daemon shutdown signal received");
                    break;
                }
            }
        }

        // Best-effort cleanup. Ignore errors — the socket may already be
        // gone if another daemon raced us to remove it.
        let _ = std::fs::remove_file(&self.socket_path);
        Ok(())
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    state: Arc<AppState>,
    shutdown_rx: &mut broadcast::Receiver<()>,
) -> Result<(), DaemonError> {
    // Pre-Framed handshake: Hello → Welcome.
    let hello = read_length_prefixed_json(&mut stream).await?;
    let client_proto = hello
        .get("protocol_version")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(0);

    if client_proto != IPC_PROTOCOL_VERSION {
        let welcome = serde_json::json!({
            "kind": "Error",
            "error": {
                "kind": "ProtocolMismatch",
                "client_version": client_proto,
                "daemon_version": IPC_PROTOCOL_VERSION,
            }
        });
        let _ = write_length_prefixed_json(&mut stream, &welcome).await;
        return Ok(());
    }

    let user_id = hello
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();

    let session_id = ulid::Ulid::new().to_string();
    let welcome = serde_json::json!({
        "kind": "Welcome",
        "daemon_version": env!("CARGO_PKG_VERSION"),
        "protocol_version": IPC_PROTOCOL_VERSION,
        "session_id": session_id,
    });
    write_length_prefixed_json(&mut stream, &welcome).await?;

    debug!(user_id, session_id, "client handshake complete");

    let mut framed = Framed::new(stream, IpcCodec::new());

    loop {
        tokio::select! {
            msg = framed.next() => {
                match msg {
                    Some(Ok(message)) => {
                        let permit = match state.request_semaphore.clone().acquire_owned().await {
                            Ok(p) => p,
                            Err(_) => return Ok(()),
                        };
                        let response = dispatch(message, &state, &user_id).await;
                        drop(permit);
                        if framed.send(response).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        debug!(error = %e, "frame decode error");
                        break;
                    }
                    None => break,
                }
            }
            _ = shutdown_rx.recv() => break,
        }
    }
    Ok(())
}

async fn dispatch(message: IpcMessage, state: &Arc<AppState>, user_id: &str) -> IpcMessage {
    let id = message.id;
    let response = match message.payload {
        IpcPayload::Request(req) => match handle_request(req, state, user_id).await {
            Ok(resp) => resp,
            Err(e) => Response::err(ResponseError {
                kind: error_kind_for(&e),
                message: e.to_string(),
                retriable: false,
            }),
        },
        other => Response::err(ResponseError {
            kind: ResponseErrorKind::InvalidPayload,
            message: format!("unexpected payload kind: {other:?}"),
            retriable: false,
        }),
    };
    IpcMessage {
        id,
        payload: IpcPayload::Response(response),
    }
}

fn error_kind_for(e: &crate::handlers::HandlerError) -> ResponseErrorKind {
    use crate::handlers::HandlerError::*;
    match e {
        Storage(_) => ResponseErrorKind::StorageError,
        Embedding(_) => ResponseErrorKind::ProviderError,
        Search(_) => ResponseErrorKind::Internal,
        InvalidPayload(_) => ResponseErrorKind::InvalidPayload,
        UnknownBucket => ResponseErrorKind::InvalidPayload,
        NotFound => ResponseErrorKind::NotFound,
    }
}

async fn write_length_prefixed_json(
    stream: &mut UnixStream,
    value: &serde_json::Value,
) -> Result<(), DaemonError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| DaemonError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
    let mut codec = LengthDelimitedCodec::builder()
        .length_field_length(4)
        .max_frame_length(16 * 1024 * 1024)
        .new_codec();
    let mut buf = BytesMut::new();
    codec.encode(bytes::Bytes::from(bytes), &mut buf)?;
    stream.write_all(&buf).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_length_prefixed_json(
    stream: &mut UnixStream,
) -> Result<serde_json::Value, DaemonError> {
    let mut buf = BytesMut::with_capacity(8 * 1024);
    let mut codec = LengthDelimitedCodec::builder()
        .length_field_length(4)
        .max_frame_length(16 * 1024 * 1024)
        .new_codec();

    loop {
        if let Some(frame) = codec.decode(&mut buf)? {
            return serde_json::from_slice(&frame).map_err(|e| {
                DaemonError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            });
        }
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            return Err(DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed during handshake",
            )));
        }
    }
}
