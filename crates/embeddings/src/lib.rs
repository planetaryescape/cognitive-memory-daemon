//! Embedding providers for cognitive-memory-daemon.
//!
//! Default: bge-small-en-v1.5 via fastembed-rs (ADR 0003, 384-dim, ~130MB).
//! Hosted providers (OpenAI etc.) added per demand. The cache wrapper sits
//! over `EmbeddingCacheRepo` and is keyed by `(provider, model, text_hash)`.
//!
//! Tests use `FakeEmbeddingProvider` for unit speed; the real `LocalProvider`
//! is gated behind the `local-model` feature so model download isn't on the
//! CI critical path.

use async_trait::async_trait;
use cognitive_memory_store::{EmbeddingCacheRepo, Store};
use sha2::{Digest, Sha256};

mod canonical;
mod fake;
#[cfg(feature = "local-model")]
mod local;

pub use canonical::canonicalise;
pub use fake::FakeEmbeddingProvider;
#[cfg(feature = "local-model")]
pub use local::LocalProvider;

/// Errors surfaced by the embeddings layer.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("provider error: {0}")]
    Provider(String),
    #[error("storage error: {0}")]
    Storage(#[from] sqlx::Error),
}

/// An embedding provider produces a fixed-dimension vector for a text input.
///
/// `name` and `model` together identify the provider+model pair for cache
/// keying. Implementations must be deterministic: two calls to `embed` with
/// the same input must return the same vector (within numerical noise the
/// caller deems acceptable).
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Provider identifier — e.g. "local", "openai", "voyage".
    fn name(&self) -> &str;

    /// Specific model — e.g. "bge-small-en-v1.5", "text-embedding-3-small".
    fn model(&self) -> &str;

    /// Output vector dimension.
    fn dimension(&self) -> usize;

    /// Embed a single text. Implementations should canonicalise the input
    /// internally if their cache assumes canonicalised input.
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
}

/// Wrapper that caches embedding results in `EmbeddingCacheRepo`.
///
/// On `embed`, hashes the canonicalised text, checks the cache, returns
/// cached vector on hit, otherwise calls the wrapped provider and inserts.
///
/// Two agents asking for the same text under the same `(provider, model)`
/// pair pay one provider call across the whole daemon lifetime — the
/// central efficiency win that motivates the daemon (ARCHITECTURE.md §1).
pub struct CachedEmbeddings<'a, P: EmbeddingProvider> {
    provider: P,
    store: &'a Store,
}

impl<'a, P: EmbeddingProvider> CachedEmbeddings<'a, P> {
    pub fn new(provider: P, store: &'a Store) -> Self {
        Self { provider, store }
    }

    /// Embed text, populating the cache on miss.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let canonical = canonical::canonicalise(text);
        let hash = sha256(canonical.as_bytes());
        let repo = EmbeddingCacheRepo::new(self.store);

        if let Some(cached) = repo
            .get(self.provider.name(), self.provider.model(), &hash)
            .await?
        {
            tracing::debug!(
                provider = self.provider.name(),
                model = self.provider.model(),
                "embedding cache hit"
            );
            return Ok(cached);
        }

        let vector = self.provider.embed(&canonical).await?;
        repo.insert(self.provider.name(), self.provider.model(), &hash, &vector)
            .await?;
        Ok(vector)
    }
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}
