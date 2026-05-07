//! LLM providers and memory extraction.
//!
//! Provider trait + fake for tests. Real OpenAI/Anthropic implementations
//! land when the daemon's `Memory::ExtractAndStore` request is wired up.
//! Per-request key override flows through `LlmRequest::api_key_override`.
//!
//! Mirrors the trait shape of the embeddings crate; consistency across
//! provider boundaries makes both easier to reason about.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

mod anthropic;
mod fake;
mod openai;
mod rate_limit;

pub use anthropic::AnthropicProvider;
pub use fake::FakeLlmProvider;
pub use openai::OpenAiProvider;
pub use rate_limit::{RateLimiter, TokenBucket};

/// Errors surfaced by the LLM layer.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("provider: {0}")]
    Provider(String),
    #[error("rate-limited: try again in {retry_after_seconds}s")]
    RateLimited { retry_after_seconds: u64 },
    #[error("missing api key for provider {provider}")]
    MissingApiKey { provider: String },
}

/// One turn in a transcript that the extractor processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

/// Inputs to a single extraction call.
#[derive(Debug, Clone)]
pub struct ExtractionRequest<'a> {
    pub transcript: &'a [Turn],
    pub api_key_override: Option<String>,
}

/// One extracted memory candidate. The daemon's handler turns each of
/// these into a `MemoryRow` insert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedMemory {
    pub content: String,
    pub category: String,
    pub memory_type: String,
}

/// LLM provider abstraction. Implementations must be deterministic given
/// the same `transcript` (with no api-key-override semantic difference);
/// the cache layer over this trait depends on that contract.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    /// Run an extraction over `transcript`. Returns zero or more
    /// extracted memory candidates.
    async fn extract(&self, req: ExtractionRequest<'_>) -> Result<Vec<ExtractedMemory>, LlmError>;
}

/// Cache key for an extraction call.
pub fn extraction_cache_key(provider: &str, model: &str, transcript: &[Turn]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(provider.as_bytes());
    hasher.update(b"\x00");
    hasher.update(model.as_bytes());
    hasher.update(b"\x00");
    for turn in transcript {
        hasher.update(turn.role.as_bytes());
        hasher.update(b"\x01");
        hasher.update(turn.content.as_bytes());
        hasher.update(b"\x02");
    }
    hasher.finalize().to_vec()
}

/// In-memory extraction cache, useful for tests and as a stand-in until
/// the persistent `ExtractionCacheRepo` (Phase 1 schema) is wired in.
#[derive(Debug, Default)]
pub struct InMemoryExtractionCache {
    inner: Arc<Mutex<std::collections::HashMap<Vec<u8>, Vec<ExtractedMemory>>>>,
}

impl InMemoryExtractionCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<ExtractedMemory>> {
        self.inner.lock().ok()?.get(key).cloned()
    }

    pub fn insert(&self, key: Vec<u8>, value: Vec<ExtractedMemory>) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.insert(key, value);
        }
    }
}

/// Wrapper that caches extraction results.
pub struct CachedExtractor<P: LlmProvider> {
    provider: P,
    cache: InMemoryExtractionCache,
}

impl<P: LlmProvider> CachedExtractor<P> {
    pub fn new(provider: P, cache: InMemoryExtractionCache) -> Self {
        Self { provider, cache }
    }

    pub async fn extract(
        &self,
        req: ExtractionRequest<'_>,
    ) -> Result<Vec<ExtractedMemory>, LlmError> {
        let key = extraction_cache_key(self.provider.name(), self.provider.model(), req.transcript);
        if let Some(cached) = self.cache.get(&key) {
            tracing::debug!(provider = self.provider.name(), "extraction cache hit");
            return Ok(cached);
        }
        let result = self.provider.extract(req).await?;
        self.cache.insert(key, result.clone());
        Ok(result)
    }
}
