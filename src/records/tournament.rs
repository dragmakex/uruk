//! Tournament matches and Elo ratings (SPEC §7).

use super::ids::*;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Initial rating from the paper (SPEC §7, 2026 Methods "Ranking agent").
pub const INITIAL_ELO: f64 = 1200.0;

/// K-factor. A Uruk starting choice, explicitly **not** attributed to the
/// paper (SPEC §7).
pub const DEFAULT_K: f64 = 32.0;

/// The outcome of one comparison (SPEC §7 step 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchOutcome {
    /// Candidate presented first won.
    WinA,
    /// Candidate presented second won.
    WinB,
    Draw,
    /// Evidence inadequate or candidates incomparable.
    ///
    /// "Insufficient basis triggers more work or remains unresolved; it is not
    /// a fabricated win." Never updates ratings.
    InsufficientBasis,
}

impl MatchOutcome {
    /// Whether this outcome updates Elo ratings (SPEC §7).
    pub fn updates_ratings(self) -> bool {
        !matches!(self, Self::InsufficientBasis)
    }

    /// Score for candidate A: win 1, draw 0.5, loss 0 (SPEC §7).
    pub fn score_a(self) -> Option<f64> {
        match self {
            Self::WinA => Some(1.0),
            Self::WinB => Some(0.0),
            Self::Draw => Some(0.5),
            Self::InsufficientBasis => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WinA => "win_a",
            Self::WinB => "win_b",
            Self::Draw => "draw",
            Self::InsufficientBasis => "insufficient_basis",
        }
    }
}

/// How the comparison was conducted (SPEC §7 step 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMethod {
    /// Cheaper single-turn comparison (2025 Fig. A.4).
    Pairwise,
    /// Multi-turn debate for consequential comparisons (2025 Fig. A.5).
    Debate,
}

impl MatchMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pairwise => "pairwise",
            Self::Debate => "debate",
        }
    }
}

/// A recorded comparison between two candidates (SPEC §4.2 `Match`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Match {
    pub id: MatchId,
    pub schema_version: u32,
    pub run_id: RunId,
    /// Item presented first to the judge.
    pub item_a: ItemId,
    /// Item presented second to the judge.
    pub item_b: ItemId,
    /// Rubric revision under which they were compared. Ratings never mix
    /// across rubric revisions (SPEC §7).
    pub rubric_id: PlanId,
    /// Hash of the evidence snapshot both sides were given.
    pub evidence_snapshot: ContentHash,
    /// True when the pair was shuffled before presentation. `item_a` is
    /// always the candidate the judge saw first, so a positional verdict maps
    /// back to a stable ID without further bookkeeping.
    pub order_randomized: bool,
    pub method: MatchMethod,
    /// Model and configuration that judged.
    pub judge_model: String,
    pub outcome: MatchOutcome,
    pub rationale: String,
    /// Ratings before and after, for audit.
    pub rating_a_before: f64,
    pub rating_b_before: f64,
    pub rating_a_after: f64,
    pub rating_b_after: f64,
    pub produced_by: Option<TaskId>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A candidate's rating within one rubric cohort (SPEC §7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rating {
    pub item_id: ItemId,
    /// Rubric revision. A new revision starts a new cohort at `INITIAL_ELO`.
    pub rubric_id: PlanId,
    pub rating: f64,
    pub matches_played: u32,
    /// Marked stale when new evidence arrives that bears on this item
    /// (SPEC §7: "visibly mark stale rankings until refreshed").
    pub stale: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Rating {
    pub fn new(item_id: ItemId, rubric_id: PlanId, now: OffsetDateTime) -> Self {
        Self {
            item_id,
            rubric_id,
            rating: INITIAL_ELO,
            matches_played: 0,
            stale: false,
            updated_at: now,
        }
    }
}

/// Expected score for A under classic Elo.
pub fn expected_score(rating_a: f64, rating_b: f64) -> f64 {
    1.0 / (1.0 + 10f64.powf((rating_b - rating_a) / 400.0))
}

/// Classic Elo update. Returns `(new_a, new_b)`.
///
/// `InsufficientBasis` leaves both ratings unchanged (SPEC §7).
pub fn apply_elo(rating_a: f64, rating_b: f64, outcome: MatchOutcome, k: f64) -> (f64, f64) {
    let Some(score_a) = outcome.score_a() else {
        return (rating_a, rating_b);
    };
    let expected_a = expected_score(rating_a, rating_b);
    let delta = k * (score_a - expected_a);
    (rating_a + delta, rating_b - delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_ratings_expect_half() {
        assert!((expected_score(1200.0, 1200.0) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn win_moves_ratings_symmetrically() {
        let (a, b) = apply_elo(1200.0, 1200.0, MatchOutcome::WinA, DEFAULT_K);
        assert!((a - 1216.0).abs() < 1e-9, "a={a}");
        assert!((b - 1184.0).abs() < 1e-9, "b={b}");
        // Zero-sum: total rating is conserved.
        assert!((a + b - 2400.0).abs() < 1e-9);
    }

    #[test]
    fn draw_between_equals_changes_nothing() {
        let (a, b) = apply_elo(1200.0, 1200.0, MatchOutcome::Draw, DEFAULT_K);
        assert!((a - 1200.0).abs() < 1e-9);
        assert!((b - 1200.0).abs() < 1e-9);
    }

    #[test]
    fn draw_favours_the_underdog() {
        let (a, b) = apply_elo(1400.0, 1200.0, MatchOutcome::Draw, DEFAULT_K);
        assert!(a < 1400.0, "favourite should lose rating on a draw: {a}");
        assert!(b > 1200.0, "underdog should gain: {b}");
    }

    #[test]
    fn insufficient_basis_never_updates() {
        let (a, b) = apply_elo(1400.0, 1100.0, MatchOutcome::InsufficientBasis, DEFAULT_K);
        assert_eq!((a, b), (1400.0, 1100.0));
        assert!(!MatchOutcome::InsufficientBasis.updates_ratings());
    }

    #[test]
    fn upset_moves_more_than_expected_win() {
        let (favourite_wins, _) = apply_elo(1400.0, 1200.0, MatchOutcome::WinA, DEFAULT_K);
        let (_, underdog_wins) = apply_elo(1400.0, 1200.0, MatchOutcome::WinB, DEFAULT_K);
        assert!(favourite_wins - 1400.0 < underdog_wins - 1200.0);
    }
}
