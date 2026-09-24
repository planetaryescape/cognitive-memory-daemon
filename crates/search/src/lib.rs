//! Vector retrieval for cognitive-memory-daemon.
//!
//! Dense vector search via cosine similarity over `memories.embedding`,
//! plus optional BM25 hybrid retrieval fused with Reciprocal Rank Fusion.
//!
//! Final score is `similarity * R^alpha`, where `R` is the lifecycle
//! retention value and `alpha` comes from the lifecycle config.

mod cosine;
mod hybrid;
mod searcher;

pub use cosine::cosine_similarity;
pub use hybrid::{reciprocal_rank_fusion, RankedHit};
pub use searcher::{ResultSource, SearchOptions, SearchResult, Searcher};

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
