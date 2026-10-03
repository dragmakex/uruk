//! Reciprocal Rank Fusion across (connector × query) ranked lists (§8).
//!
//! `score(w) = Σ over lists containing w of 1 / (k + rank(w))` with the
//! standard constant k = 60. Ties break by work_key lexicographic ascending,
//! so the fused order is fully deterministic given the persisted hit lists.

use super::types::WorkKey;
use std::collections::BTreeMap;

/// The standard RRF constant.
pub const RRF_K: f64 = 60.0;

/// One ranked list: the work keys in connector order (rank 1 first).
pub type RankedList = Vec<WorkKey>;

/// Fuse ranked lists into `(work_key, rrf_score)` sorted best-first.
pub fn rrf_fuse(lists: &[RankedList]) -> Vec<(WorkKey, f64)> {
    let mut scores: BTreeMap<&WorkKey, f64> = BTreeMap::new();
    for list in lists {
        for (i, key) in list.iter().enumerate() {
            let rank = (i + 1) as f64;
            *scores.entry(key).or_insert(0.0) += 1.0 / (RRF_K + rank);
        }
    }
    let mut out: Vec<(WorkKey, f64)> = scores.into_iter().map(|(k, s)| (k.clone(), s)).collect();
    // Descending score; ascending work_key on ties.
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> WorkKey {
        WorkKey(s.to_string())
    }

    #[test]
    fn rrf_matches_hand_computed_values() {
        // List 1: a, b; List 2: b, c.
        let lists = vec![vec![key("a"), key("b")], vec![key("b"), key("c")]];
        let fused = rrf_fuse(&lists);

        let score_of = |k: &str| fused.iter().find(|(w, _)| w.as_str() == k).unwrap().1;
        let a = 1.0 / 61.0;
        let b = 1.0 / 62.0 + 1.0 / 61.0;
        let c = 1.0 / 62.0;
        assert!((score_of("a") - a).abs() < 1e-12);
        assert!((score_of("b") - b).abs() < 1e-12);
        assert!((score_of("c") - c).abs() < 1e-12);
        // b appears in both lists, so it wins.
        assert_eq!(fused[0].0.as_str(), "b");
        assert_eq!(fused[1].0.as_str(), "a");
        assert_eq!(fused[2].0.as_str(), "c");
    }

    #[test]
    fn ties_break_by_work_key_ascending() {
        // x and y each appear once at rank 1 of different lists: equal score.
        let lists = vec![vec![key("y")], vec![key("x")]];
        let fused = rrf_fuse(&lists);
        assert_eq!(fused[0].0.as_str(), "x", "lexicographic tie-break");
        assert_eq!(fused[1].0.as_str(), "y");
        assert_eq!(fused[0].1, fused[1].1);
    }

    #[test]
    fn empty_input_fuses_to_nothing() {
        assert!(rrf_fuse(&[]).is_empty());
        assert!(rrf_fuse(&[vec![]]).is_empty());
    }
}
