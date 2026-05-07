//! Local embedding provider: bge-small-en-v1.5 via fastembed-rs.
//!
//! Lazy-loaded on first use. Holds the model in memory for the daemon's
//! lifetime — the principle 9 commitment ("operable without provider keys")
//! lives here.
//!
//! Compiled only when the `local-model` feature is enabled. The daemon
//! binary turns it on; unit tests using `FakeEmbeddingProvider` do not.

use crate::{EmbeddingError, EmbeddingProvider};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::sync::Mutex;

/// Local embedding provider backed by fastembed-rs.
///
/// fastembed handles model download (to the user's cache dir on first use)
/// and ONNX runtime initialisation. The model is wrapped in a `Mutex` because
/// fastembed's `embed` takes `&mut self`; the mutex serialises calls per
/// daemon process. Throughput is bounded by single-threaded inference, which
/// is fine at our query rates — the daemon is not running thousands of
/// embeddings per second.
pub struct LocalProvider {
    inner: Mutex<TextEmbedding>,
    name: String,
    model_label: String,
    dimension: usize,
}

impl LocalProvider {
    /// Initialise the provider with `bge-small-en-v1.5`. Triggers model
    /// download on first call (cached after).
    pub fn bge_small_en() -> Result<Self, EmbeddingError> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(false),
        )
        .map_err(|e| EmbeddingError::Provider(format!("init bge-small-en-v1.5: {e}")))?;

        Ok(Self {
            inner: Mutex::new(model),
            name: "local".to_string(),
            model_label: "bge-small-en-v1.5".to_string(),
            dimension: 384,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for LocalProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn model(&self) -> &str {
        &self.model_label
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let text_owned = text.to_string();
        // fastembed inference is CPU-bound; offload to spawn_blocking so we
        // don't stall the runtime. The mutex wrap means at most one
        // concurrent inference in the process at a time, which is what we
        // want — the bge-small CPU footprint dominates if you run many.
        let inner = &self.inner;
        let result = tokio::task::block_in_place(|| {
            let guard = inner
                .lock()
                .map_err(|_| EmbeddingError::Provider("model mutex poisoned".to_string()))?;
            guard
                .embed(vec![text_owned], None)
                .map_err(|e| EmbeddingError::Provider(format!("embed: {e}")))
        })?;

        result
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::Provider("empty result from fastembed".to_string()))
    }
}
