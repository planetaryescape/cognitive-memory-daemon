// `cm-daemon` binary entrypoint.
//
// Constructs a `Daemon` with production defaults: file-backed `Store` at the
// resolved data path and the local embedding provider (bge-small-en-v1.5
// via fastembed-rs) when the `local-model` feature is enabled. Falls back
// to `FakeEmbeddingProvider` when the feature is off so CI builds the
// daemon without pulling fastembed.
//
// Auto-spawn, PID-file single-instance, structured logging, and signal
// handling beyond Ctrl-C land in subsequent phases (per docs/decisions/
// and ROADMAP.md).

use cognitive_memory_daemon::Daemon;
use cognitive_memory_embeddings::EmbeddingProvider;
use cognitive_memory_store::Store;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let socket_path = std::env::var("COGNITIVE_MEMORY_SOCKET_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::data_dir()
                .expect("data dir resolvable")
                .join("cognitive-memory")
                .join("cm.sock")
        });

    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let db_path = socket_path
        .parent()
        .expect("socket path has parent")
        .join("data.db");
    let store = Store::open(&db_path).await?;

    let embeddings = build_embeddings()?;
    tracing::info!(
        provider = embeddings.name(),
        model = embeddings.model(),
        dimension = embeddings.dimension(),
        "embedding provider configured"
    );

    let daemon = Daemon::new(store, embeddings, socket_path);
    let shutdown = daemon.shutdown_handle();

    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown.send(());
        }
    });

    daemon.serve().await?;
    Ok(())
}

#[cfg(feature = "local-model")]
fn build_embeddings() -> Result<Arc<dyn EmbeddingProvider>, Box<dyn std::error::Error>> {
    use cognitive_memory_embeddings::LocalProvider;
    Ok(Arc::new(LocalProvider::bge_small_en()?))
}

#[cfg(not(feature = "local-model"))]
fn build_embeddings() -> Result<Arc<dyn EmbeddingProvider>, Box<dyn std::error::Error>> {
    use cognitive_memory_embeddings::FakeEmbeddingProvider;
    Ok(Arc::new(FakeEmbeddingProvider::new("local", "fake-16", 16)))
}
