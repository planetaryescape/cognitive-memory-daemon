//! Deterministic fake LLM provider for tests.
//!
//! Returns scripted outputs based on a content-derived hash, so two calls
//! with the same transcript return the same result. Tracks call counts so
//! tests can assert cache behaviour ("was the provider called twice?").

use crate::{ExtractedMemory, ExtractionRequest, LlmError, LlmProvider, Turn};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct FakeLlmProvider {
    name: String,
    model: String,
    pub calls: Arc<AtomicUsize>,
    /// Scripted responses for `complete()`. Tests `with_responses(...)`
    /// then assert behaviour as the daemon pops one per call. When the
    /// queue is empty, `complete()` returns "" (so callers handling
    /// missing labels gracefully are exercised).
    completions: Arc<Mutex<VecDeque<String>>>,
}

impl FakeLlmProvider {
    pub fn new(name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            model: model.into(),
            calls: Arc::new(AtomicUsize::new(0)),
            completions: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Pre-load a queue of scripted `complete()` responses. Each call
    /// to `complete()` pops the head; when empty, returns "".
    pub fn with_responses<I, S>(self, responses: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        {
            let mut q = self.completions.lock().expect("mutex");
            q.extend(responses.into_iter().map(Into::into));
        }
        self
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

    async fn complete(&self, _prompt: &str, _max_tokens: usize) -> Result<String, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let next = self
            .completions
            .lock()
            .expect("mutex")
            .pop_front()
            .unwrap_or_default();
        Ok(next)
    }
}

fn turn_to_extracted(turn: &Turn) -> ExtractedMemory {
    ExtractedMemory {
        content: format!("FAKE-EXTRACT[{}]: {}", turn.role, turn.content),
        category: "semantic".to_string(),
        memory_type: "fact".to_string(),
    }
}
