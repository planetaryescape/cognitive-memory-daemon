// `cm-daemon` binary entrypoint.
//
// Constructs a `Daemon` with production defaults: file-backed `Store` at the
// resolved data path and the local embedding provider (bge-small-en-v1.5
// via fastembed-rs) when the `local-model` feature is enabled. Falls back
// to `FakeEmbeddingProvider` when the feature is off so CI builds the
// daemon without pulling fastembed.
//
// The CLI owns auto-spawn: it starts this binary with
// `COGNITIVE_MEMORY_SOCKET_PATH` set, then polls the socket until ready.
// This binary stays foreground-style and exits on Ctrl-C/shutdown signal.

use clap::Parser;
use cognitive_memory_core::{secure_private_file_if_exists, RuntimePaths};
use cognitive_memory_daemon::{
    paper_faithful_lifecycle_config, Daemon, DaemonConfig, DaemonRuntime, LlmConfig,
};
use cognitive_memory_embeddings::EmbeddingProvider;
use cognitive_memory_lifecycle::LifecycleConfig;
use cognitive_memory_llm::LlmProvider;
use cognitive_memory_store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::prelude::*;

#[derive(Debug, Parser)]
#[command(
    name = "cm-daemon",
    about = "Cognitive Memory resident daemon",
    version
)]
struct Args {
    /// Keep the daemon in the foreground. Accepted for symmetry with `cm daemon start`.
    #[arg(long)]
    foreground: bool,

    /// Override runtime identity. Defaults to cognitive-memory in release and
    /// cognitive-memory-dev in debug builds.
    #[arg(long, env = "COGNITIVE_MEMORY_INSTANCE")]
    instance: Option<String>,

    /// Override Unix socket path.
    #[arg(long, env = "COGNITIVE_MEMORY_SOCKET_PATH")]
    socket: Option<PathBuf>,

    /// Override SQLite database path.
    #[arg(long, env = "COGNITIVE_MEMORY_DB_PATH")]
    db: Option<PathBuf>,

    /// Override PID file path.
    #[arg(long, env = "COGNITIVE_MEMORY_PID_PATH")]
    pid: Option<PathBuf>,

    /// Override daemon log file path.
    #[arg(long, env = "COGNITIVE_MEMORY_LOG_PATH")]
    log: Option<PathBuf>,

    /// Emit JSON logs.
    #[arg(long, env = "COGNITIVE_MEMORY_LOG_JSON")]
    json_logs: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    set_private_umask();

    if let Some(instance) = &args.instance {
        std::env::set_var("COGNITIVE_MEMORY_INSTANCE", instance);
    }

    let mut paths = RuntimePaths::resolve();
    if let Some(socket) = args.socket {
        paths.socket_path = socket;
    }
    if let Some(db) = args.db {
        paths.db_path = db;
    }
    if let Some(pid) = args.pid {
        paths.pid_path = pid;
    }
    if let Some(log) = args.log {
        paths.daemon_log_path = log;
    }
    paths.ensure_private_dirs()?;
    if let Some(parent) = paths.socket_path.parent() {
        cognitive_memory_core::ensure_private_dir(parent)?;
    }
    if let Some(parent) = paths.db_path.parent() {
        cognitive_memory_core::ensure_private_dir(parent)?;
    }
    if let Some(parent) = paths.daemon_log_path.parent() {
        cognitive_memory_core::ensure_private_dir(parent)?;
    }

    let _log_guard = init_logging(&paths, args.json_logs)?;

    let store = Store::open(&paths.db_path).await?;
    secure_private_file_if_exists(&paths.db_path)?;

    let embeddings = build_embeddings()?;
    tracing::info!(
        provider = embeddings.name(),
        model = embeddings.model(),
        dimension = embeddings.dimension(),
        "embedding provider configured"
    );

    let llm = build_llm()?;
    if let Some(p) = llm.as_ref() {
        tracing::info!(
            provider = p.name(),
            model = p.model(),
            "LLM provider configured (conflict judge + consolidation enabled)"
        );
    } else {
        tracing::info!(
            "no LLM provider configured (conflict resolution falls back to heuristic; \
             consolidation skipped). Run `cm download-model && cm config set-llm local` to enable."
        );
    }

    let lifecycle = build_lifecycle_config();
    if !is_paper_default(&lifecycle) {
        tracing::info!(
            base_decay_rates = ?lifecycle.base_decay_rates,
            "lifecycle overrides applied from config.toml [lifecycle]"
        );
    }

    let runtime = DaemonRuntime::from_paths(&paths);
    let daemon = Daemon::new_full_runtime(store, embeddings, runtime, llm, lifecycle);
    let shutdown = daemon.shutdown_handle();

    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown.send(());
        }
    });

    daemon.serve().await?;
    Ok(())
}

fn init_logging(
    paths: &RuntimePaths,
    json_logs: bool,
) -> Result<tracing_appender::non_blocking::WorkerGuard, Box<dyn std::error::Error>> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.daemon_log_path)?;
    secure_private_file_if_exists(&paths.daemon_log_path)?;
    let appender = tracing_appender::rolling::never(&paths.log_dir, "daemon.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = tracing_subscriber::EnvFilter::try_from_env("COGNITIVE_MEMORY_LOG")
        .or_else(|_| tracing_subscriber::EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    if json_logs {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().json().with_writer(writer))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(writer))
            .init();
    }
    Ok(guard)
}

fn set_private_umask() {
    #[cfg(unix)]
    {
        use nix::sys::stat::{umask, Mode};
        let _ = umask(Mode::from_bits_truncate(0o077));
    }
}

#[cfg(feature = "local-model")]
fn build_embeddings() -> Result<Arc<dyn EmbeddingProvider>, Box<dyn std::error::Error>> {
    if std::env::var("COGNITIVE_MEMORY_EMBEDDINGS")
        .map(|v| v == "fake")
        .unwrap_or(false)
    {
        use cognitive_memory_embeddings::FakeEmbeddingProvider;
        return Ok(Arc::new(FakeEmbeddingProvider::new("local", "fake-16", 16)));
    }
    use cognitive_memory_embeddings::LocalProvider;
    Ok(Arc::new(LocalProvider::bge_small_en()?))
}

#[cfg(not(feature = "local-model"))]
fn build_embeddings() -> Result<Arc<dyn EmbeddingProvider>, Box<dyn std::error::Error>> {
    use cognitive_memory_embeddings::FakeEmbeddingProvider;
    Ok(Arc::new(FakeEmbeddingProvider::new("local", "fake-16", 16)))
}

/// Build the daemon's `LifecycleConfig` from paper-faithful defaults
/// merged with `[lifecycle]` overrides from config.toml. Missing or
/// malformed config falls back to defaults so a config typo doesn't
/// prevent the daemon from starting (logged warning at load time
/// covers the diagnostic path).
fn build_lifecycle_config() -> LifecycleConfig {
    let mut cfg = paper_faithful_lifecycle_config();
    let daemon_cfg = match DaemonConfig::load() {
        Ok(c) => c,
        Err(_) => return cfg,
    };
    let Some(overrides) = daemon_cfg.lifecycle else {
        return cfg;
    };
    if let Some(rates) = overrides.base_decay_rates {
        for (k, v) in rates {
            // Replace one category's β; siblings retain paper default.
            cfg.base_decay_rates.insert(k, v);
        }
    }
    cfg
}

/// Cheap check used for the "overrides applied" log line. Avoids
/// printing noise on every startup when the file has only `[llm]`.
fn is_paper_default(cfg: &LifecycleConfig) -> bool {
    let paper = paper_faithful_lifecycle_config();
    cfg.base_decay_rates == paper.base_decay_rates
}

/// Read `~/.config/cognitive-memory/config.toml` and instantiate the
/// configured LLM provider (or None). Missing config file ⇒ None.
/// Malformed config ⇒ logged warning + None (don't fail-stop the
/// daemon over a config typo).
fn build_llm() -> Result<Option<Arc<dyn LlmProvider>>, Box<dyn std::error::Error>> {
    let config = match DaemonConfig::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(%e, "could not load config.toml; running without LLM");
            return Ok(None);
        }
    };
    let Some(llm_cfg) = config.llm else {
        return Ok(None);
    };
    match llm_cfg {
        LlmConfig::None => Ok(None),
        LlmConfig::Local { model_path } => instantiate_local_llm(model_path),
        LlmConfig::Openai { api_key_env, model } => {
            let key = std::env::var(&api_key_env)
                .map_err(|_| format!("env var {api_key_env} not set for OpenAI provider"))?;
            Ok(Some(Arc::new(cognitive_memory_llm::OpenAiProvider::new(
                key, model,
            ))))
        }
        LlmConfig::Anthropic { api_key_env, model } => {
            let key = std::env::var(&api_key_env)
                .map_err(|_| format!("env var {api_key_env} not set for Anthropic provider"))?;
            Ok(Some(Arc::new(
                cognitive_memory_llm::AnthropicProvider::new(key, model),
            )))
        }
    }
}

#[cfg(feature = "local-llm")]
fn instantiate_local_llm(
    model_path: PathBuf,
) -> Result<Option<Arc<dyn LlmProvider>>, Box<dyn std::error::Error>> {
    if !model_path.exists() {
        return Err(format!(
            "local model file not found at {}; run `cm download-model` first",
            model_path.display()
        )
        .into());
    }
    Ok(Some(Arc::new(cognitive_memory_llm::LocalLlmProvider::new(
        model_path,
    ))))
}

#[cfg(not(feature = "local-llm"))]
fn instantiate_local_llm(
    _model_path: PathBuf,
) -> Result<Option<Arc<dyn LlmProvider>>, Box<dyn std::error::Error>> {
    Err(
        "this daemon was built without the `local-llm` cargo feature; \
         either rebuild with --features local-llm, or switch the config to \
         provider = \"openai\" / \"anthropic\" / \"none\""
            .into(),
    )
}
