//! Deterministic fake LLM provider for tests.
//!
//! Returns scripted outputs based on a content-derived hash, so two calls
//! with the same transcript return the same result. Tracks call counts so
//! tests can assert cache behaviour ("was the provider called twice?").

use crate::{ExtractedMemory, ExtractionRequest, LlmError, LlmProvider, Turn};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct FakeLlmProvider {
    name: String,
    model: String,
    pub calls: Arc<AtomicUsize>,
}

impl FakeLlmProvider {
    pub fn new(name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            model: model.into(),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmProvider for FakeLlmProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn extract(&self, req: ExtractionRequest<'_>) -> Result<Vec<ExtractedMemory>, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(req
            .transcript
            .iter()
            .filter(|t| t.role == "user")
            .map(turn_to_extracted)
            .collect())
    }
}

fn turn_to_extracted(turn: &Turn) -> ExtractedMemory {
    ExtractedMemory {
        content: format!("FAKE-EXTRACT[{}]: {}", turn.role, turn.content),
        category: "semantic".to_string(),
        memory_type: "fact".to_string(),
    }
}
