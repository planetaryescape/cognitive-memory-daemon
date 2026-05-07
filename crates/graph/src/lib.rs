//! Memory associations and n-hop graph expansion.
//!
//! Algorithms ported from
//! `cognitive-memory-sdk/sdks/python/src/cognitive_memory/engine.py`
//! (`strengthen_association`, `decay_association`, `_expand_graph`).
//!
//! Phase 9 ships the math (strengthen, decay, BFS); the integration with
//! `Memory::Search { graph_expansion: { enabled: true } }` lands when the
//! daemon's search request grows that field. For now the lifecycle and
//! search crates compose these helpers when their callers ask for it.

use std::collections::{HashMap, HashSet, VecDeque};

/// Per-edge weight bounded to `[0.0, 1.0]`.
pub type Weight = f64;

/// One association edge in the memory graph. Edges are *directed*; a
/// bidirectional association (the common case) is two edges.
#[derive(Debug, Clone, Copy)]
pub struct Association {
    pub target_id_index: usize,
    pub weight: Weight,
    /// Unix seconds of last co-retrieval. `decay_association` uses this
    /// against `now` to compute the decayed weight.
    pub last_co_retrieval: i64,
}

/// Configuration for graph operations. Mirrors the Python SDK.
#[derive(Debug, Clone)]
pub struct GraphConfig {
    /// Amount added to weight on `strengthen` (paper: 0.1).
    pub strengthen_amount: Weight,
    /// Decay constant in days (`tau` in `w *= exp(-dt/tau)`). Paper: 90.
    pub decay_constant_days: f64,
    /// Minimum decayed weight to surface in retrieval.
    pub retrieval_threshold: Weight,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            strengthen_amount: 0.1,
            decay_constant_days: 90.0,
            retrieval_threshold: 0.05,
        }
    }
}

/// Strengthen an association: `w += strengthen_amount`, capped at 1.0.
pub fn strengthen(assoc: &mut Association, now: i64, config: &GraphConfig) {
    assoc.weight = (assoc.weight + config.strengthen_amount).min(1.0);
    assoc.last_co_retrieval = now;
}

/// Decay an association weight in place: `w *= exp(-dt_days / tau)`.
/// Returns the decayed weight.
pub fn decay(assoc: &mut Association, now: i64, config: &GraphConfig) -> Weight {
    let dt_days = ((now - assoc.last_co_retrieval).max(0) as f64) / 86400.0;
    let decayed = assoc.weight * (-dt_days / config.decay_constant_days).exp();
    assoc.weight = decayed;
    decayed
}

/// Memory id type used by the graph. The daemon uses string IDs (ULIDs);
/// for graph traversal we work with usize indices into a parallel array
/// because BFS over arbitrary string keys is wasteful in tight loops.
pub type MemoryIndex = usize;

/// Per-memory adjacency list.
pub type AdjacencyList = HashMap<MemoryIndex, Vec<Association>>;

/// Result of one expansion hop.
#[derive(Debug, Clone, Copy)]
pub struct ExpandedMemory {
    pub index: MemoryIndex,
    pub combined_weight: Weight,
    pub hops_from_anchor: u32,
}

/// BFS expansion through the association graph.
///
/// Starting from `anchors`, walks `max_hops` levels of associations,
/// returning all memories reached and their accumulated weight (multiplied
/// along the path) and hop distance. Visited tracking avoids revisits;
/// the anchor set is excluded from results.
pub fn expand_graph(
    anchors: &[MemoryIndex],
    adjacency: &AdjacencyList,
    max_hops: u32,
    config: &GraphConfig,
) -> Vec<ExpandedMemory> {
    if max_hops == 0 || anchors.is_empty() {
        return Vec::new();
    }

    let mut results: Vec<ExpandedMemory> = Vec::new();
    let mut seen: HashSet<MemoryIndex> = anchors.iter().copied().collect();
    let mut frontier: VecDeque<(MemoryIndex, Weight, u32)> =
        anchors.iter().map(|&id| (id, 1.0, 0)).collect();

    while let Some((current, weight_so_far, hop)) = frontier.pop_front() {
        if hop >= max_hops {
            continue;
        }
        let Some(edges) = adjacency.get(&current) else {
            continue;
        };
        for edge in edges {
            if edge.weight < config.retrieval_threshold {
                continue;
            }
            if !seen.insert(edge.target_id_index) {
                continue;
            }
            let combined = weight_so_far * edge.weight;
            results.push(ExpandedMemory {
                index: edge.target_id_index,
                combined_weight: combined,
                hops_from_anchor: hop + 1,
            });
            frontier.push_back((edge.target_id_index, combined, hop + 1));
        }
    }

    results
}
