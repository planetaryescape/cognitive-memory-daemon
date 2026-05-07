//! Deterministic fake embedding provider for tests.
//!
//! `FakeEmbeddingProvider` produces a vector derived from `Sha256(text)`
//! split into `dimension` floats in `[-1.0, 1.0]`. Same text → same vector
//! (the determinism contract). Different text → different vector with
//! overwhelming probability.
//!
//! Use in unit tests; do not ship in the daemon binary. The real
//! `LocalProvider` (gated by `local-model` feature) does the heavy lifting
//! in production.

use crate::{sha256, EmbeddingError, EmbeddingProvider};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct FakeEmbeddingProvider {
    name: String,
    model: String,
    dimension: usize,
    /// Optional per-text overrides. When `embed(text)` is called and
    /// `overrides` contains `text`, return that vector verbatim instead
    /// of the SHA-256 derivation. Lets tests pin similarity between
    /// inputs (e.g. v1 = [1, 0, ...], v2 = [0.8, 0.6, 0, ...] → cosine 0.8).
    overrides: Mutex<HashMap<String, Vec<f32>>>,
}

impl FakeEmbeddingProvider {
    pub fn new(name: impl Into<String>, model: impl Into<String>, dimension: usize) -> Self {
        Self {
            name: name.into(),
            model: model.into(),
            dimension,
            overrides: Mutex::new(HashMap::new()),
        }
    }

    /// Pin the embedding for a specific input text. Subsequent
    /// `embed(text)` calls will return `vector` instead of the SHA-256
    /// derivation. Used by tests that need controlled cosine similarity
    /// between specific inputs.
    pub fn set_override(&self, text: impl Into<String>, vector: Vec<f32>) {
        let mut map = self.overrides.lock().expect("override map not poisoned");
        map.insert(text.into(), vector);
    }
}

#[async_trait]
impl EmbeddingProvider for FakeEmbeddingProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        // Test-pinned vectors take priority. Allows controlled-cosine
        // setups for similarity-band tests (synaptic tag / reinforce /
        // conflict bands).
        if let Some(v) = self
            .overrides
            .lock()
            .expect("override map not poisoned")
            .get(text)
        {
            return Ok(v.clone());
        }
        // Stretch the 32-byte SHA-256 across `dimension` floats by hashing
        // an integer-suffixed input until enough bytes are produced. Each
        // 4-byte window becomes one f32 in [-1.0, 1.0].
        let mut bytes = Vec::with_capacity(self.dimension * 4);
        let mut counter: u32 = 0;
        while bytes.len() < self.dimension * 4 {
            let mut input = text.as_bytes().to_vec();
            input.extend_from_slice(&counter.to_le_bytes());
            bytes.extend(sha256(&input));
            counter += 1;
        }

        let vector = bytes
            .chunks_exact(4)
            .take(self.dimension)
            .map(|chunk| {
                let raw = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                // Map u32 → [-1.0, 1.0].
                (raw as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
            })
            .collect();

        Ok(vector)
    }
}
