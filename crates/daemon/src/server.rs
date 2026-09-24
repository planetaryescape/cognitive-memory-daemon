//! IPC server: accept loop, per-connection task, dispatcher.

use crate::handlers::{handle_request, paper_faithful_lifecycle_config, AppState};
use bytes::BytesMut;
use cognitive_memory_core::{secure_private_file_if_exists, secure_private_socket, RuntimePaths};
use cognitive_memory_embeddings::EmbeddingProvider;
use cognitive_memory_lifecycle::LifecycleConfig;
use cognitive_memory_protocol::{
    Event, IpcCodec, IpcMessage, IpcPayload, MemoryRequest, Response, ResponseData, ResponseError,
    ResponseErrorKind, SubscribedData, IPC_PROTOCOL_VERSION,
};
use cognitive_memory_store::Store;
use futures::{FutureExt, SinkExt, StreamExt};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Semaphore};
use tokio::task::JoinSet;
use tokio_util::codec::{Decoder, Encoder, Framed, LengthDelimitedCodec};
use tracing::{debug, error, info, warn};

const REQUEST_CONCURRENCY_LIMIT: usize = 64;
const BULK_CONCURRENCY_LIMIT: usize = 8;
const EVENT_CHANNEL_CAPACITY: usize = 1024;
const CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Errors surfaced by the daemon's setup and runtime.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("daemon already running at {0}")]
    AlreadyRunning(PathBuf),
    #[error("socket bind: {0}")]
    Bind(std::io::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage: {0}")]
    Storage(#[from] sqlx::Error),
}

#[derive(Debug, Clone)]
pub struct DaemonRuntime {
    pub instance: String,
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    pub db_path: PathBuf,
    pub log_path: PathBuf,
    pub build_id: String,
}

impl DaemonRuntime {
    pub fn from_paths(paths: &RuntimePaths) -> Self {
        Self {
            instance: paths.instance.clone(),
            socket_path: paths.socket_path.clone(),
            pid_path: paths.pid_path.clone(),
            db_path: paths.db_path.clone(),
            log_path: paths.daemon_log_path.clone(),
            build_id: build_id(),
        }
    }

    pub fn from_socket(socket_path: PathBuf) -> Self {
        let paths = RuntimePaths::resolve();
        let parent = socket_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| paths.runtime_dir.clone());
        Self {
            instance: paths.instance,
            pid_path: parent.join("cm-daemon.pid"),
            db_path: parent.join("data.db"),
            log_path: paths.daemon_log_path,
            socket_path,
            build_id: build_id(),
        }
    }
}

pub fn build_id() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let exe = std::env::current_exe().ok();
    let meta = exe.as_ref().and_then(|p| std::fs::metadata(p).ok());
    let len = meta.as_ref().map(std::fs::Metadata::len).unwrap_or(0);
    let modified = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = exe
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    format!("{version}:{path}:{len}:{modified}")
}

/// The daemon. Holds a store, an embedding provider, and (when running) a
/// Unix-socket accept loop. Constructed with explicit dependencies so tests
/// can swap in a `FakeEmbeddingProvider`.
pub struct Daemon {
    state: Arc<AppState>,
    runtime: DaemonRuntime,
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
        Self::new_full(
            store,
            embeddings,
            socket_path,
            llm,
            paper_faithful_lifecycle_config(),
        )
    }

    /// Full constructor with explicit lifecycle config — used by `main.rs`
    /// after merging `DaemonConfig.lifecycle` overrides onto the
    /// paper-faithful defaults. Existing callers (tests, simple wires)
    /// stay on `new` / `new_with_llm` and get the paper defaults.
    pub fn new_full(
        store: Store,
        embeddings: Arc<dyn EmbeddingProvider>,
        socket_path: PathBuf,
        llm: Option<Arc<dyn cognitive_memory_llm::LlmProvider>>,
        lifecycle: LifecycleConfig,
    ) -> Self {
        Self::new_full_runtime(
            store,
            embeddings,
            DaemonRuntime::from_socket(socket_path),
            llm,
            lifecycle,
        )
    }

    pub fn new_full_runtime(
        store: Store,
        embeddings: Arc<dyn EmbeddingProvider>,
        runtime: DaemonRuntime,
        llm: Option<Arc<dyn cognitive_memory_llm::LlmProvider>>,
        lifecycle: LifecycleConfig,
    ) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let state = Arc::new(AppState {
            store,
            embeddings,
            request_semaphore: Arc::new(Semaphore::new(REQUEST_CONCURRENCY_LIMIT)),
            bulk_semaphore: Arc::new(Semaphore::new(BULK_CONCURRENCY_LIMIT)),
            started_at: Instant::now(),
            llm,
            lifecycle,
            shutdown_tx: shutdown_tx.clone(),
            event_tx,
            trace_ring: Arc::new(crate::trace::TraceRing::new(1000)),
            runtime: runtime.clone(),
        });
        Self {
            state,
            runtime,
            shutdown_tx,
        }
    }

    /// Get a handle that signals shutdown when fired.
    pub fn shutdown_handle(&self) -> broadcast::Sender<()> {
        self.shutdown_tx.clone()
    }

    /// Bind the socket and run until shutdown is signalled.
    pub async fn serve(&self) -> Result<(), DaemonError> {
        if let Some(parent) = self.runtime.socket_path.parent() {
            cognitive_memory_core::ensure_private_dir(parent)?;
        }
        if let Some(parent) = self.runtime.pid_path.parent() {
            cognitive_memory_core::ensure_private_dir(parent)?;
        }
        secure_private_file_if_exists(&self.runtime.db_path)?;

        if self.runtime.socket_path.exists() {
            if UnixStream::connect(&self.runtime.socket_path).await.is_ok() {
                return Err(DaemonError::AlreadyRunning(
                    self.runtime.socket_path.clone(),
                ));
            }
            warn!(
                socket = %self.runtime.socket_path.display(),
                "removing stale socket file"
            );
            std::fs::remove_file(&self.runtime.socket_path)?;
        }

        let listener = UnixListener::bind(&self.runtime.socket_path).map_err(DaemonError::Bind)?;
        secure_private_socket(&self.runtime.socket_path)?;
        write_pid_file(&self.runtime.pid_path)?;
        info!(
            socket = %self.runtime.socket_path.display(),
            pid = std::process::id(),
            instance = %self.runtime.instance,
            "daemon listening"
        );

        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _addr)) => {
                            let state = Arc::clone(&self.state);
                            let mut conn_shutdown = self.shutdown_tx.subscribe();
                            connections.spawn(async move {
                                if let Err(e) = handle_connection(stream, state, &mut conn_shutdown).await {
                                    debug!(error = %e, "connection ended with error");
                                }
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "accept failed");
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("daemon shutdown signal received");
                    break;
                }
            }
        }

        drop(listener);
        drain_connection_tasks(&mut connections, CONNECTION_DRAIN_TIMEOUT).await;
        let _ = std::fs::remove_file(&self.runtime.socket_path);
        let _ = std::fs::remove_file(&self.runtime.pid_path);
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

    let (mut sink, mut stream) = Framed::new(stream, IpcCodec::new()).split();
    let mut request_tasks = JoinSet::new();
    let mut event_rx: Option<broadcast::Receiver<Event>> = None;
    let mut accept_requests = true;
    let mut can_send = true;
    let mut shutdown_requested = false;

    loop {
        tokio::select! {
            biased;

            joined = request_tasks.join_next(), if !request_tasks.is_empty() => {
                match joined {
                    Some(Ok(response)) if can_send => match sink.send(response).await {
                        Ok(()) => {}
                        Err(_) => {
                            can_send = false;
                            accept_requests = false;
                        }
                    },
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        debug!(error = %e, "request task failed");
                    }
                    None => {}
                }
            }
            msg = stream.next(), if accept_requests => {
                match msg {
                    Some(Ok(message)) => {
                        if let IpcPayload::Request(cognitive_memory_protocol::Request::Memory(
                            MemoryRequest::Subscribe(args),
                        )) = &message.payload
                        {
                            event_rx = Some(state.event_tx.subscribe());
                            let response = IpcMessage {
                                id: message.id,
                                payload: IpcPayload::Response(Response::ok(
                                    ResponseData::Subscribed(SubscribedData {
                                        replay_snapshot_sent: args.replay_snapshot,
                                    }),
                                )),
                            };
                            if sink.send(response).await.is_err() {
                                break;
                            }
                            if args.replay_snapshot {
                                if let Ok(event) = current_state_event(&state).await {
                                    let _ = sink
                                        .send(IpcMessage {
                                            id: 0,
                                            payload: IpcPayload::Event(event),
                                        })
                                        .await;
                                }
                            }
                            continue;
                        }
                        let semaphore = if is_bulk_payload(&message.payload) {
                            state.bulk_semaphore.clone()
                        } else {
                            state.request_semaphore.clone()
                        };
                        let permit = match semaphore.acquire_owned().await {
                            Ok(p) => p,
                            Err(_) => return Ok(()),
                        };
                        let state = Arc::clone(&state);
                        let user_id = user_id.clone();
                        request_tasks.spawn(async move {
                            let _permit = permit;
                            guard_dispatch(message, state, user_id).await
                        });
                    }
                    Some(Err(e)) => {
                        debug!(error = %e, "frame decode error");
                        break;
                    }
                    None => break,
                }
            }
            event = async {
                match event_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending::<Result<Event, broadcast::error::RecvError>>().await,
                }
            } => {
                match event {
                    Ok(event) => {
                        if sink.send(IpcMessage { id: 0, payload: IpcPayload::Event(event) }).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let event = Event::EventStreamLagged {
                            skipped,
                            occurred_at: chrono::Utc::now().to_rfc3339(),
                        };
                        if sink.send(IpcMessage { id: 0, payload: IpcPayload::Event(event) }).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = shutdown_rx.recv(), if !shutdown_requested => {
                shutdown_requested = true;
                accept_requests = false;
            }
        }

        if !accept_requests && request_tasks.is_empty() {
            break;
        }
    }
    Ok(())
}

async fn drain_connection_tasks(connections: &mut JoinSet<()>, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !connections.is_empty() {
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            warn!("client connection drain timed out");
            connections.abort_all();
            while let Some(joined) = connections.join_next().await {
                if let Err(e) = joined {
                    debug!(error = %e, "aborted connection task");
                }
            }
            return;
        };

        match tokio::time::timeout(remaining, connections.join_next()).await {
            Ok(Some(Ok(()))) => {}
            Ok(Some(Err(e))) => debug!(error = %e, "connection task failed"),
            Ok(None) => break,
            Err(_) => {
                warn!("client connection drain timed out");
                connections.abort_all();
                while let Some(joined) = connections.join_next().await {
                    if let Err(e) = joined {
                        debug!(error = %e, "aborted connection task");
                    }
                }
                return;
            }
        }
    }
}

async fn guard_dispatch(message: IpcMessage, state: Arc<AppState>, user_id: String) -> IpcMessage {
    let id = message.id;
    match AssertUnwindSafe(dispatch(message, &state, &user_id))
        .catch_unwind()
        .await
    {
        Ok(response) => response,
        Err(_) => IpcMessage {
            id,
            payload: IpcPayload::Response(Response::err(ResponseError {
                kind: ResponseErrorKind::Internal,
                message: "request handler panicked".to_string(),
                retriable: false,
            })),
        },
    }
}

async fn dispatch(message: IpcMessage, state: &Arc<AppState>, user_id: &str) -> IpcMessage {
    let id = message.id;
    let started = Instant::now();
    let (bucket, op) = describe_payload(&message.payload);
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
    state.trace_ring.push(crate::trace::Trace {
        trace_id: format!("tr_{}", ulid::Ulid::new()),
        request_id: id,
        bucket,
        op,
        embed_ms: None,
        vector_ms: None,
        fusion_ms: None,
        format_ms: None,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    });
    IpcMessage {
        id,
        payload: IpcPayload::Response(response),
    }
}

async fn current_state_event(state: &Arc<AppState>) -> Result<Event, DaemonError> {
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memories")
        .fetch_one(state.store.reader())
        .await?;
    Ok(Event::CurrentState {
        memory_count: count.0 as u64,
        occurred_at: chrono::Utc::now().to_rfc3339(),
    })
}

fn describe_payload(payload: &IpcPayload) -> (&'static str, &'static str) {
    match payload {
        IpcPayload::Request(req) => match req {
            cognitive_memory_protocol::Request::Diagnostics(d) => {
                ("Diagnostics", diagnostics_op_name(d))
            }
            cognitive_memory_protocol::Request::Memory(m) => ("Memory", memory_op_name(m)),
            cognitive_memory_protocol::Request::Lifecycle(l) => ("Lifecycle", lifecycle_op_name(l)),
            cognitive_memory_protocol::Request::UnknownBucket => ("Unknown", "UnknownBucket"),
        },
        IpcPayload::Response(_) => ("Response", "Unexpected"),
        IpcPayload::Event(_) => ("Event", "Unexpected"),
    }
}

fn is_bulk_payload(payload: &IpcPayload) -> bool {
    matches!(
        payload,
        IpcPayload::Request(cognitive_memory_protocol::Request::Lifecycle(_))
            | IpcPayload::Request(cognitive_memory_protocol::Request::Diagnostics(
                cognitive_memory_protocol::DiagnosticsRequest::Doctor
                    | cognitive_memory_protocol::DiagnosticsRequest::RecentTraces(_)
            ))
            | IpcPayload::Request(cognitive_memory_protocol::Request::Memory(
                MemoryRequest::StoreBatch(_)
                    | MemoryRequest::DeleteMany(_)
                    | MemoryRequest::BatchUpdate(_)
            ))
    )
}

fn diagnostics_op_name(req: &cognitive_memory_protocol::DiagnosticsRequest) -> &'static str {
    match req {
        cognitive_memory_protocol::DiagnosticsRequest::Status => "Status",
        cognitive_memory_protocol::DiagnosticsRequest::Doctor => "Doctor",
        cognitive_memory_protocol::DiagnosticsRequest::RecentTraces(_) => "RecentTraces",
        cognitive_memory_protocol::DiagnosticsRequest::Shutdown => "Shutdown",
        cognitive_memory_protocol::DiagnosticsRequest::MintBridgeToken(_) => "MintBridgeToken",
        cognitive_memory_protocol::DiagnosticsRequest::ValidateBridgeToken(_) => {
            "ValidateBridgeToken"
        }
        cognitive_memory_protocol::DiagnosticsRequest::Counts(_) => "Counts",
    }
}

fn memory_op_name(req: &MemoryRequest) -> &'static str {
    match req {
        MemoryRequest::Subscribe(_) => "Subscribe",
        MemoryRequest::Store(_) => "Store",
        MemoryRequest::StoreBatch(_) => "StoreBatch",
        MemoryRequest::Search(_) => "Search",
        MemoryRequest::Get(_) => "Get",
        MemoryRequest::GetMany(_) => "GetMany",
        MemoryRequest::List(_) => "List",
        MemoryRequest::Update(_) => "Update",
        MemoryRequest::Delete(_) => "Delete",
        MemoryRequest::DeleteMany(_) => "DeleteMany",
        MemoryRequest::Link(_) => "Link",
        MemoryRequest::Unlink(_) => "Unlink",
        MemoryRequest::GetLinked(_) => "GetLinked",
        MemoryRequest::GetLinkedMany(_) => "GetLinkedMany",
        MemoryRequest::VectorSearch(_) => "VectorSearch",
        MemoryRequest::SearchLexical(_) => "SearchLexical",
        MemoryRequest::BatchUpdate(_) => "BatchUpdate",
    }
}

fn lifecycle_op_name(req: &cognitive_memory_protocol::LifecycleRequest) -> &'static str {
    match req {
        cognitive_memory_protocol::LifecycleRequest::Tick(_) => "Tick",
        cognitive_memory_protocol::LifecycleRequest::FindFading(_) => "FindFading",
        cognitive_memory_protocol::LifecycleRequest::FindStable(_) => "FindStable",
        cognitive_memory_protocol::LifecycleRequest::MarkSuperseded(_) => "MarkSuperseded",
        cognitive_memory_protocol::LifecycleRequest::MigrateToCold(_) => "MigrateToCold",
        cognitive_memory_protocol::LifecycleRequest::MigrateToHot(_) => "MigrateToHot",
        cognitive_memory_protocol::LifecycleRequest::ConvertToStub(_) => "ConvertToStub",
        cognitive_memory_protocol::LifecycleRequest::UpdateRetention(_) => "UpdateRetention",
        cognitive_memory_protocol::LifecycleRequest::Clear(_) => "Clear",
    }
}

fn write_pid_file(path: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        cognitive_memory_core::ensure_private_dir(parent)?;
    }
    std::fs::write(path, format!("{}\n", std::process::id()))?;
    secure_private_file_if_exists(path)
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
