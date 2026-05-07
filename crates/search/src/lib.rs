//! Vector retrieval for cognitive-memory-daemon.
//!
//! Phase 3 (this crate): pure vector search via cosine similarity over the
//! `memories.embedding` column. Score = cosine; the retention factor `R^alpha`
//! that the v6 spec calls for lands when Phase 8 (lifecycle) wires in.
//!
//! Hybrid retrieval (BM25 fused with dense via RRF) and per-query traces are
//! follow-on work; the Searcher's interface is shaped to accept those without
//! breaking callers.

mod cosine;
mod hybrid;
mod searcher;

pub use cosine::cosine_similarity;
pub use hybrid::{reciprocal_rank_fusion, RankedHit};
pub use searcher::{SearchOptions, SearchResult, Searcher};

/// Errors surfaced by the search layer.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("storage: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("embedding dimension mismatch: query={query}, memory={memory}")]
    DimensionMismatch { query: usize, memory: usize },
    #[error("invalid query: {0}")]
    InvalidQuery(String),
}
