//! `Searcher`: top-K vector retrieval over `MemoryRepo`.
//!
//! Phase 3 implementation: cosine similarity, no decay (R=1.0), no hybrid.
//! The Phase 8 lifecycle layer composes its retention factor on top.

use crate::{cosine_similarity, SearchError};
use cognitive_memory_store::{MemoryRepo, Store};

/// One result row returned by `Searcher::search`.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub memory_id: String,
    pub content: String,
    pub category: String,
    pub memory_type: String,
    /// Composite score: cosine similarity in v1; multiplied by `R^alpha`
    /// once the lifecycle layer is wired in (Phase 8).
    pub score: f32,
}

/// Knobs the caller can pass to `Searcher::search`.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub limit: usize,
    /// If true, expired (`valid_until` past) memories are returned.
    pub deep_recall: bool,
    /// Embedding provider to filter against. Memories embedded under a
    /// different `(provider, model)` are not searched — re-embedding under
    /// a new provider is an explicit operation, not a silent fallback.
    pub provider: String,
    pub model: String,
    /// Current time, in unix seconds. Caller injects to keep the searcher
    /// deterministic in tests.
    pub now: i64,
    /// Enable hybrid retrieval (dense + BM25 fused via RRF). When true,
    /// `query_text` is used for the BM25 side; pass the same text the
    /// caller embedded into the query vector.
    pub hybrid: bool,
    /// Original query text, required when `hybrid = true`. Ignored
    /// otherwise.
    pub query_text: Option<String>,
}

impl SearchOptions {
    pub fn new(provider: impl Into<String>, model: impl Into<String>, now: i64) -> Self {
        Self {
            limit: 10,
            deep_recall: false,
            provider: provider.into(),
            model: model.into(),
            now,
            hybrid: false,
            query_text: None,
        }
    }
}

/// Vector searcher over a `Store`.
pub struct Searcher<'a> {
    store: &'a Store,
}

impl<'a> Searcher<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Search for the top-`limit` memories under `user_id` whose embeddings
    /// are most similar to `query_vec`. Validity-filtered by default.
    pub async fn search(
        &self,
        user_id: &str,
        query_vec: &[f32],
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>, SearchError> {
        if query_vec.is_empty() {
            return Err(SearchError::InvalidQuery(
                "query vector is empty".to_string(),
            ));
        }
        if options.limit == 0 {
            return Ok(Vec::new());
        }

        let repo = MemoryRepo::new(self.store);
        let candidates = repo
            .candidates_for_search(
                user_id,
                &options.provider,
                &options.model,
                options.now,
                options.deep_recall,
            )
            .await?;

        let mut scored: Vec<SearchResult> = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let memory_vec = candidate.embedding_vec();
            if memory_vec.len() != query_vec.len() {
                return Err(SearchError::DimensionMismatch {
                    query: query_vec.len(),
                    memory: memory_vec.len(),
                });
            }
            let sim = cosine_similarity(query_vec, &memory_vec);
            scored.push(SearchResult {
                memory_id: candidate.id,
                content: candidate.content,
                category: candidate.category,
                memory_type: candidate.memory_type,
                score: sim,
            });
        }

        // Sort descending by score. f32 NaN cannot occur because cosine
        // returns 0.0 for zero vectors and finite values otherwise.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if options.hybrid {
            let query_text = options.query_text.as_deref().ok_or_else(|| {
                SearchError::InvalidQuery(
                    "hybrid mode requires query_text in SearchOptions".to_string(),
                )
            })?;
            // Fetch a wider candidate set from BM25 to give RRF something
            // to fuse against. Pull 4× the limit; final limit applied after.
            let bm25_limit = (options.limit * 4).max(20);
            let bm25_ids = repo.bm25_search(user_id, query_text, bm25_limit).await?;

            // Build ranked-hit lists from the dense scoring above plus BM25.
            let dense_ranked: Vec<crate::RankedHit> = scored
                .iter()
                .enumerate()
                .map(|(rank, r)| crate::RankedHit {
                    id: r.memory_id.clone(),
                    rank,
                })
                .collect();
            let sparse_ranked: Vec<crate::RankedHit> = bm25_ids
                .iter()
                .enumerate()
                .map(|(rank, id)| crate::RankedHit {
                    id: id.clone(),
                    rank,
                })
                .collect();

            let fused = crate::reciprocal_rank_fusion(&[&dense_ranked, &sparse_ranked], 60);

            // Re-order `scored` by the fused ordering. Items only present
            // in BM25 (no dense score) are dropped — the daemon only
            // surfaces memories whose embedding is present and matches the
            // current (provider, model) pair.
            let mut by_id: std::collections::HashMap<String, SearchResult> = scored
                .into_iter()
                .map(|r| (r.memory_id.clone(), r))
                .collect();
            let mut hybrid_scored: Vec<SearchResult> = Vec::with_capacity(fused.len());
            for (id, fused_score) in fused {
                if let Some(mut r) = by_id.remove(&id) {
                    r.score = fused_score as f32;
                    hybrid_scored.push(r);
                }
            }
            hybrid_scored.truncate(options.limit);
            return Ok(hybrid_scored);
        }

        scored.truncate(options.limit);
        Ok(scored)
    }
}
