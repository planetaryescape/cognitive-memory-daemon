//! SQLite storage layer for cognitive-memory-daemon.
//!
//! Two-pool wrapper: 1 writer + N readers, WAL mode, idempotent migrations.
//! Adapted from `mxr/crates/store/src/pool.rs` (vendored mechanical, see
//! `docs/developer/code-reuse.md` Phase 1). Schema and repositories are
//! cognitive-memory-specific.

mod pool;
mod repos;

pub use pool::Store;
pub use repos::{
    AssociationRepo, AssociationRow, EmbeddingCacheRepo, EventLogRepo, LinkedMemory, MemoryCounts,
    MemoryFilters, MemoryRepo, MemoryRow, MemoryUpdate, SearchCandidate,
};

/// Errors surfaced by the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),
}
