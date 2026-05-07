//! Reciprocal Rank Fusion for combining dense + sparse retrieval results.
//!
//! RRF (Cormack et al., 2009) is the standard fusion technique for hybrid
//! retrieval. For each ranking list, a candidate's contribution is
//! `1 / (k + rank_in_list)`. Final score = sum of contributions across all
//! lists. The constant `k` controls how heavily top-ranked items dominate;
//! `k = 60` is the literature default.
//!
//! Why RRF and not weighted score combination: RRF is robust to score-
//! distribution differences between dense (cosine, [-1, 1]) and sparse
//! (BM25, unbounded positive) systems. No score normalisation needed.

/// One ranked hit. `id` identifies the memory; `rank` is the position in
/// the source ranking list (zero-indexed).
#[derive(Debug, Clone)]
pub struct RankedHit {
    pub id: String,
    pub rank: usize,
}

/// Fuse multiple ranking lists via Reciprocal Rank Fusion.
///
/// Returns hits ordered by descending fused score. `k` is the RRF
/// smoothing constant; pass 60 for the literature default.
pub fn reciprocal_rank_fusion(lists: &[&[RankedHit]], k: usize) -> Vec<(String, f64)> {
    let mut scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for list in lists {
        for hit in *list {
            let contribution = 1.0 / (k as f64 + hit.rank as f64 + 1.0);
            *scores.entry(hit.id.clone()).or_insert(0.0) += contribution;
        }
    }
    let mut fused: Vec<(String, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked(ids: &[&str]) -> Vec<RankedHit> {
        ids.iter()
            .enumerate()
            .map(|(rank, id)| RankedHit {
                id: id.to_string(),
                rank,
            })
            .collect()
    }

    #[test]
    fn rrf_fuses_two_lists_with_overlap() {
        let dense = ranked(&["a", "b", "c"]);
        let sparse = ranked(&["b", "a", "d"]);
        let fused = reciprocal_rank_fusion(&[&dense, &sparse], 60);

        // `a`: 1/61 + 1/62; `b`: 1/61 + 1/62 — same! But ordering by id
        // is unspecified for ties; both `a` and `b` should outrank `c` and `d`.
        let top_two: Vec<_> = fused.iter().take(2).map(|(id, _)| id.as_str()).collect();
        assert!(top_two.contains(&"a"));
        assert!(top_two.contains(&"b"));
    }

    #[test]
    fn rrf_top_of_one_list_outranks_only_appearing_in_other() {
        let dense = ranked(&["a", "b"]);
        let sparse = ranked(&["c"]);
        let fused = reciprocal_rank_fusion(&[&dense, &sparse], 60);

        // `a` rank 0 in dense → 1/61 ≈ 0.01639
        // `c` rank 0 in sparse → 1/61 ≈ 0.01639
        // tied; `b` rank 1 in dense → 1/62 ≈ 0.01613 (lower)
        let last = fused.last().expect("must have entries");
        assert_eq!(last.0, "b", "lower-ranked single-list hit should be last");
    }

    #[test]
    fn rrf_higher_k_dampens_ranking_dominance() {
        let dense = ranked(&["a", "b", "c", "d", "e"]);
        let high_k = reciprocal_rank_fusion(&[&dense], 1000);
        let low_k = reciprocal_rank_fusion(&[&dense], 1);

        // With k=1, top score = 1/2; with k=1000, top score = 1/1001.
        let high_k_top = high_k[0].1;
        let low_k_top = low_k[0].1;
        assert!(high_k_top < low_k_top);
    }

    #[test]
    fn rrf_empty_input_returns_empty() {
        let result = reciprocal_rank_fusion(&[], 60);
        assert!(result.is_empty());
    }

    #[test]
    fn rrf_handles_single_list() {
        let dense = ranked(&["x", "y"]);
        let result = reciprocal_rank_fusion(&[&dense], 60);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "x");
        assert_eq!(result[1].0, "y");
    }
}
