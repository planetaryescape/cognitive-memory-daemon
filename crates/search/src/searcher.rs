//! `Searcher`: top-K vector retrieval over `MemoryRepo`.
//!
//! Composite score (paper Eq. 3):  score(m, q) = relevance(m, q) * R(m)^α.
//! `relevance` is cosine similarity for dense, RRF-fused score for hybrid.
//! `R(m)` is the lifecycle retention factor (paper Eq. 1, power-law).
//! `α` is `retrieval_score_exponent` from the lifecycle config (default 0.3).

use crate::{cosine_similarity, SearchError};
use cognitive_memory_lifecycle::{
    compute_retention, decay_association_weight, parse_category, DecayModel, LifecycleConfig,
    MemoryState,
};
use cognitive_memory_store::{AssociationRepo, MemoryRepo, SearchCandidate, Store};

/// Build a `MemoryState` view from a `SearchCandidate` row, ready for
/// `compute_retention`. Pulled out so the Searcher stays focused.
/// β_c is looked up via `cfg.beta_for(&category)` so daemon-level
/// `[lifecycle]` overrides reach the search path (Phase 0a-daemon).
fn candidate_to_state(c: &SearchCandidate, cfg: &LifecycleConfig) -> MemoryState {
    MemoryState {
        last_accessed_at: c.last_accessed_at,
        created_at: c.created_at,
        stability: c.stability,
        importance: c.importance,
        base_decay_rate: cfg.beta_for(&c.category),
        floor: c.retention_floor,
        is_stub: c.is_stub,
        access_count: c.retrieval_count as u64,
        session_count: 0,
        category: parse_category(&c.category),
    }
}

/// The lifecycle config the daemon's search uses by default. Power-law
/// decay, α=0.3 — paper-faithful. Used when a `Searcher` is constructed
/// via `new` without an explicit config.
fn default_search_lifecycle_config() -> LifecycleConfig {
    LifecycleConfig {
        decay_model: DecayModel::Power,
        ..LifecycleConfig::default()
    }
}

/// How a memory entered the result set. Drives differential
/// post-retrieval boosts: direct hits get the +0.1 stability bump
/// (paper Eq 6), graph-expanded hits get the smaller +0.03 (Eq 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultSource {
    /// The memory was a top-K cosine hit on the query embedding.
    Direct,
    /// The memory was added to the result set via association-graph
    /// expansion from a direct hit.
    GraphExpanded,
}

/// One result row returned by `Searcher::search`.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub memory_id: String,
    pub content: String,
    pub category: String,
    pub memory_type: String,
    /// Composite score: `relevance(m, q) * R(m)^α [* edge_weight]`.
    /// Equation 4 for direct hits; Equation 4 multiplied by edge
    /// weight for graph-expanded hits.
    pub score: f32,
    /// Whether this hit came from dense/hybrid search or graph
    /// expansion. Read by the handler to decide which stability
    /// boost amount to apply per the paper's Eq 6 vs Eq 8.
    pub source: ResultSource,
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
    /// Walk the association graph from the top dense hits this many
    /// hops, adding linked memories to the result set with score
    /// `relevance * R^α * edge_weight`. 0 disables; SDK default is 1.
    pub graph_expansion_hops: usize,
    /// Minimum association weight for graph expansion / bridge
    /// discovery edges. Edges weaker than this are not traversed.
    pub min_bridge_edge_weight: f64,
    /// Run BFS bridge discovery between the top-3 anchor results,
    /// attaching evidence chains to the response. Off by default.
    pub bridge_discovery: bool,
    /// Cap on bridge paths returned. Default 3.
    pub max_bridge_paths: usize,
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
            graph_expansion_hops: 0,
            min_bridge_edge_weight: 0.3,
            bridge_discovery: false,
            max_bridge_paths: 3,
        }
    }
}

/// Vector searcher over a `Store`.
pub struct Searcher<'a> {
    store: &'a Store,
    life_cfg: LifecycleConfig,
}

impl<'a> Searcher<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            store,
            life_cfg: default_search_lifecycle_config(),
        }
    }

    /// Construct with an explicit lifecycle config. The daemon uses this
    /// to thread `AppState.lifecycle` (with config.toml overrides) into
    /// the search path so `[lifecycle].base_decay_rates` reaches β_c
    /// lookups in scoring + graph expansion.
    pub fn with_lifecycle(store: &'a Store, life_cfg: LifecycleConfig) -> Self {
        Self { store, life_cfg }
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

        let life_cfg = &self.life_cfg;
        let alpha = life_cfg.retrieval_score_exponent;

        let mut scored: Vec<SearchResult> = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let memory_vec = candidate.embedding_vec();
            if memory_vec.len() != query_vec.len() {
                return Err(SearchError::DimensionMismatch {
                    query: query_vec.len(),
                    memory: memory_vec.len(),
                });
            }
            let sim = cosine_similarity(query_vec, &memory_vec) as f64;
            let state = candidate_to_state(&candidate, life_cfg);
            let retention = compute_retention(&state, options.now, life_cfg);
            // Equation 3: score = relevance * R^α.
            let composite = sim * retention.powf(alpha);
            scored.push(SearchResult {
                memory_id: candidate.id,
                content: candidate.content,
                category: candidate.category,
                memory_type: candidate.memory_type,
                score: composite as f32,
                source: ResultSource::Direct,
            });
        }

        // Sort descending by score. f32 NaN cannot occur because cosine
        // returns 0.0 for zero vectors and finite values otherwise.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Hybrid: fuse dense ordering with BM25 via RRF.
        if options.hybrid {
            let query_text = options.query_text.as_deref().ok_or_else(|| {
                SearchError::InvalidQuery(
                    "hybrid mode requires query_text in SearchOptions".to_string(),
                )
            })?;
            let bm25_limit = (options.limit * 4).max(20);
            let bm25_ids = repo.bm25_search(user_id, query_text, bm25_limit).await?;

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

            // Items only present in BM25 (no dense score) are dropped —
            // the daemon only surfaces memories whose embedding matches
            // the current (provider, model) pair.
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
            scored = hybrid_scored;
        }

        // Graph expansion: walk associations from the top hits, scoring
        // newly-discovered memories by relevance * R^α * edge_weight.
        if options.graph_expansion_hops > 0 && !scored.is_empty() {
            self.expand_via_graph(user_id, query_vec, &mut scored, life_cfg, alpha, options)
                .await?;
            // Re-sort after expansion adds new entries.
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        scored.truncate(options.limit);
        Ok(scored)
    }

    /// Walk the association graph from the current top hits, adding
    /// linked memories to `scored` with `relevance * R^α * edge_weight`.
    /// Multi-hop: each hop's discovered memories become the next hop's
    /// frontier. Already-included IDs are skipped to avoid double-counting.
    async fn expand_via_graph(
        &self,
        user_id: &str,
        query_vec: &[f32],
        scored: &mut Vec<SearchResult>,
        life_cfg: &LifecycleConfig,
        alpha: f64,
        options: &SearchOptions,
    ) -> Result<(), SearchError> {
        let assoc_repo = AssociationRepo::new(self.store);

        // Initial frontier: top hits to expand from. Limited to the
        // result limit so we don't blow up on a 10k-result set.
        let mut already_have: std::collections::HashSet<String> =
            scored.iter().map(|r| r.memory_id.clone()).collect();
        let mut frontier_ids: Vec<String> = scored
            .iter()
            .take(options.limit.max(5))
            .map(|r| r.memory_id.clone())
            .collect();

        // We pull the linked set with the *stored* weight ≥ min, then
        // re-check after decay below. This means an edge that's stored
        // ≥0.3 but has decayed under 0.3 will be dropped at the
        // post-decay check. Pulling with min_weight=0 would be more
        // permissive but waste fetches; using the stored threshold as
        // an upper bound is conservative — see the decay-threshold test.
        for _hop in 0..options.graph_expansion_hops {
            if frontier_ids.is_empty() {
                break;
            }
            let linked = assoc_repo
                .linked_for_many(user_id, &frontier_ids, options.min_bridge_edge_weight)
                .await?;
            let mut next_frontier = Vec::new();
            for lm in linked {
                if already_have.contains(&lm.memory.id) {
                    continue;
                }
                // Apply Eq 10 read-side decay before the threshold
                // check. Edges with old `last_co_retrieval` whose
                // decayed weight falls below the threshold are skipped.
                let live_weight = match lm.last_co_retrieval {
                    Some(last) => decay_association_weight(
                        lm.link_strength,
                        last,
                        options.now,
                        life_cfg.association_decay_constant_days,
                    ),
                    None => lm.link_strength,
                };
                if live_weight < options.min_bridge_edge_weight {
                    continue;
                }
                let Some(emb_bytes) = &lm.memory.embedding else {
                    continue;
                };
                let memory_vec: Vec<f32> = emb_bytes
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                if memory_vec.len() != query_vec.len() {
                    continue;
                }
                let sim = cosine_similarity(query_vec, &memory_vec) as f64;

                let state = MemoryState {
                    last_accessed_at: lm.memory.last_accessed_at,
                    created_at: lm.memory.created_at,
                    stability: lm.memory.stability,
                    importance: lm.memory.importance,
                    base_decay_rate: life_cfg.beta_for(&lm.memory.category),
                    floor: lm.memory.retention_floor,
                    is_stub: lm.memory.is_stub,
                    access_count: lm.memory.retrieval_count as u64,
                    session_count: 0,
                    category: parse_category(&lm.memory.category),
                };
                let retention = compute_retention(&state, options.now, life_cfg);
                let composite = sim * retention.powf(alpha) * live_weight;

                scored.push(SearchResult {
                    memory_id: lm.memory.id.clone(),
                    content: lm.memory.content.clone(),
                    category: lm.memory.category.clone(),
                    memory_type: lm.memory.memory_type.clone(),
                    score: composite as f32,
                    source: ResultSource::GraphExpanded,
                });
                already_have.insert(lm.memory.id.clone());
                next_frontier.push(lm.memory.id);
            }
            frontier_ids = next_frontier;
        }
        Ok(())
    }

    /// BFS bridge discovery: find shortest paths through the
    /// association graph between the top-3 anchor results, max depth 3.
    /// Returns a list of paths, each `[anchor_a, ..., anchor_b]` where
    /// the intermediate nodes are bridge memories.
    ///
    /// Mirrors `_find_bridge_paths` + `_bfs_path` in the Python SDK
    /// (engine.py:301–357). Anchors are passed in by the caller — the
    /// search handler walks its top-K results.
    pub async fn find_bridges(
        &self,
        anchors: &[String],
        options: &SearchOptions,
    ) -> Result<Vec<Vec<String>>, SearchError> {
        if !options.bridge_discovery || anchors.len() < 2 {
            return Ok(Vec::new());
        }
        let assoc_repo = AssociationRepo::new(self.store);
        let pool: Vec<&String> = anchors.iter().take(3).collect();
        let mut chains: Vec<Vec<String>> = Vec::new();

        for i in 0..pool.len() {
            for j in (i + 1)..pool.len() {
                if chains.len() >= options.max_bridge_paths {
                    break;
                }
                if let Some(path) = bfs_path(
                    &assoc_repo,
                    pool[i],
                    pool[j],
                    3,
                    options.min_bridge_edge_weight,
                )
                .await?
                {
                    // Non-trivial: at least one intermediate node.
                    if path.len() > 2 {
                        chains.push(path);
                    }
                }
            }
        }
        Ok(chains)
    }
}

/// BFS shortest path through the association graph from `from` to `to`.
/// Edges below `min_weight` are not traversed. Returns `None` if no
/// path within `max_depth` hops.
async fn bfs_path(
    assoc_repo: &AssociationRepo<'_>,
    from: &str,
    to: &str,
    max_depth: usize,
    min_weight: f64,
) -> Result<Option<Vec<String>>, SearchError> {
    use std::collections::{HashSet, VecDeque};
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(from.to_string());
    let mut queue: VecDeque<(String, Vec<String>)> = VecDeque::new();
    queue.push_back((from.to_string(), vec![from.to_string()]));

    while let Some((current, path)) = queue.pop_front() {
        if path.len() > max_depth {
            continue;
        }
        let edges = assoc_repo.neighbor_edges(&current, min_weight).await?;
        for edge in edges {
            let neighbor = edge.target_id;
            if visited.contains(&neighbor) {
                continue;
            }
            let mut new_path = path.clone();
            new_path.push(neighbor.clone());
            if neighbor == to {
                return Ok(Some(new_path));
            }
            visited.insert(neighbor.clone());
            queue.push_back((neighbor, new_path));
        }
    }
    Ok(None)
}
