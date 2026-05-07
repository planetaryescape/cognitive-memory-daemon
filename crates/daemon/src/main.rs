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

use cognitive_memory_daemon::{
    paper_faithful_lifecycle_config, Daemon, DaemonConfig, LlmConfig,
};
use cognitive_memory_embeddings::EmbeddingProvider;
use cognitive_memory_lifecycle::LifecycleConfig;
use cognitive_memory_llm::LlmProvider;
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

    let daemon = Daemon::new_full(store, embeddings, socket_path, llm, lifecycle);
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
