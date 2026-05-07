//! Graph tests: strengthen/decay parity, BFS expansion correctness.

#![allow(clippy::panic, clippy::unwrap_used)]

use cognitive_memory_graph::{
    decay, expand_graph, strengthen, AdjacencyList, Association, GraphConfig,
};
use pretty_assertions::assert_eq;
use std::collections::HashMap;

const TOL: f64 = 1e-4;

#[test]
fn strengthen_increments_and_caps_at_one() {
    let cfg = GraphConfig::default();
    let mut a = Association {
        target_id_index: 1,
        weight: 0.0,
        last_co_retrieval: 0,
    };
    strengthen(&mut a, 100, &cfg);
    assert!((a.weight - 0.1).abs() < TOL);
    assert_eq!(a.last_co_retrieval, 100);

    a.weight = 0.95;
    strengthen(&mut a, 200, &cfg);
    assert!(a.weight <= 1.0);
    assert!((a.weight - 1.0).abs() < TOL); // 0.95 + 0.1 = 1.05 → cap to 1.0
}

#[test]
fn decay_applies_exponential_formula() {
    // From Python: w = 0.8, dt = 90 days, tau = 90 → w' = 0.8 * exp(-1) ≈ 0.2943
    let cfg = GraphConfig::default();
    let now = 90 * 86400; // 90 days
    let mut a = Association {
        target_id_index: 1,
        weight: 0.8,
        last_co_retrieval: 0,
    };
    let decayed = decay(&mut a, now, &cfg);
    let expected = 0.8 * (-1.0_f64).exp();
    assert!(
        (decayed - expected).abs() < TOL,
        "expected {expected}, got {decayed}"
    );
    assert_eq!(a.weight, decayed);
}

#[test]
fn expand_graph_returns_empty_with_zero_hops() {
    let cfg = GraphConfig::default();
    let mut adj: AdjacencyList = HashMap::new();
    adj.insert(
        0,
        vec![Association {
            target_id_index: 1,
            weight: 0.5,
            last_co_retrieval: 0,
        }],
    );
    let result = expand_graph(&[0], &adj, 0, &cfg);
    assert!(result.is_empty());
}

#[test]
fn expand_graph_one_hop_returns_direct_neighbours() {
    let cfg = GraphConfig::default();
    let mut adj: AdjacencyList = HashMap::new();
    adj.insert(
        0,
        vec![
            Association {
                target_id_index: 1,
                weight: 0.5,
                last_co_retrieval: 0,
            },
            Association {
                target_id_index: 2,
                weight: 0.7,
                last_co_retrieval: 0,
            },
        ],
    );
    let result = expand_graph(&[0], &adj, 1, &cfg);
    let ids: Vec<usize> = result.iter().map(|e| e.index).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    for e in &result {
        assert_eq!(e.hops_from_anchor, 1);
    }
}

#[test]
fn expand_graph_two_hops_traverses_further() {
    let cfg = GraphConfig::default();
    let mut adj: AdjacencyList = HashMap::new();
    adj.insert(
        0,
        vec![Association {
            target_id_index: 1,
            weight: 0.5,
            last_co_retrieval: 0,
        }],
    );
    adj.insert(
        1,
        vec![Association {
            target_id_index: 2,
            weight: 0.5,
            last_co_retrieval: 0,
        }],
    );

    let result = expand_graph(&[0], &adj, 2, &cfg);
    let by_id: HashMap<usize, _> = result.iter().map(|e| (e.index, *e)).collect();
    assert_eq!(by_id.get(&1).unwrap().hops_from_anchor, 1);
    assert_eq!(by_id.get(&2).unwrap().hops_from_anchor, 2);
    // combined weight at node 2 = 0.5 * 0.5 = 0.25
    assert!((by_id.get(&2).unwrap().combined_weight - 0.25).abs() < TOL);
}

#[test]
fn expand_graph_excludes_anchors_from_results() {
    let cfg = GraphConfig::default();
    let mut adj: AdjacencyList = HashMap::new();
    adj.insert(
        0,
        vec![Association {
            target_id_index: 1,
            weight: 0.5,
            last_co_retrieval: 0,
        }],
    );
    adj.insert(
        1,
        vec![Association {
            target_id_index: 0, // back-edge to anchor
            weight: 0.5,
            last_co_retrieval: 0,
        }],
    );
    let result = expand_graph(&[0], &adj, 5, &cfg);
    let ids: Vec<usize> = result.iter().map(|e| e.index).collect();
    assert!(!ids.contains(&0), "anchor must not be re-added");
    assert!(ids.contains(&1));
}

#[test]
fn expand_graph_skips_below_threshold_edges() {
    let cfg = GraphConfig {
        retrieval_threshold: 0.5,
        ..GraphConfig::default()
    };
    let mut adj: AdjacencyList = HashMap::new();
    adj.insert(
        0,
        vec![
            Association {
                target_id_index: 1,
                weight: 0.6, // above threshold
                last_co_retrieval: 0,
            },
            Association {
                target_id_index: 2,
                weight: 0.3, // below threshold
                last_co_retrieval: 0,
            },
        ],
    );
    let result = expand_graph(&[0], &adj, 1, &cfg);
    let ids: Vec<usize> = result.iter().map(|e| e.index).collect();
    assert_eq!(ids, vec![1]);
}
