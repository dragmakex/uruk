//! Reviews and the Reflection strategies (SPEC §5, §15.4).

use super::evidence::{Assessment, EvidenceLink};
use super::ids::*;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Reflection strategies from the paper (SPEC §5 Reflection table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStrategy {
    /// Cheap goal-alignment, correctness, novelty, feasibility, safety screen.
    Initial,
    /// Source-grounded review including prior work and conflicting evidence.
    Full,
    /// Decompose and independently check assumptions.
    DeepVerification,
    /// Does the proposal explain existing observations better than alternatives?
    Observation,
    /// Walk the mechanism; identify failure modes without claiming execution.
    Simulation,
    /// Revisit using accumulated evidence and comparison patterns.
    Recurrent,
    /// A review supplied by the researcher, accepted with attribution
    /// (SPEC §5). Not a paper strategy.
    Researcher,
}

impl ReviewStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Full => "full",
            Self::DeepVerification => "deep_verification",
            Self::Observation => "observation",
            Self::Simulation => "simulation",
            Self::Recurrent => "recurrent",
            Self::Researcher => "researcher",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "initial" => Self::Initial,
            "full" => Self::Full,
            "deep_verification" => Self::DeepVerification,
            "observation" => Self::Observation,
            "simulation" => Self::Simulation,
            "recurrent" => Self::Recurrent,
            "researcher" => Self::Researcher,
            _ => return None,
        })
    }

    /// Whether this strategy requires retrieved or supplied sources.
    pub fn requires_sources(self) -> bool {
        matches!(self, Self::Full | Self::Observation)
    }
}

/// The explanatory label from the published observation prompt (2025 Fig. A.3).
///
/// SPEC §5: these are **not** replacements for §4.3 assessments.
/// `MissingPiece` means a plausible explanatory contribution, not `Supported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplanatoryLabel {
    /// Hypothesis is consistent, but the causes are already known.
    AlreadyExplained,
    /// It *could* explain, but better explanations exist.
    OtherExplanationsMoreLikely,
    /// Offers a novel, plausible explanation.
    MissingPiece,
    /// Neither explains nor is contradicted.
    Neutral,
    /// Observations contradict the hypothesis. A *proposed* contradiction.
    Disproved,
}

impl ExplanatoryLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyExplained => "already_explained",
            Self::OtherExplanationsMoreLikely => "other_explanations_more_likely",
            Self::MissingPiece => "missing_piece",
            Self::Neutral => "neutral",
            Self::Disproved => "disproved",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        // Accept the prompt's prose spellings as well as the snake_case form.
        let norm = s.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        Some(match norm.as_str() {
            "already_explained" => Self::AlreadyExplained,
            "other_explanations_more_likely" => Self::OtherExplanationsMoreLikely,
            "missing_piece" => Self::MissingPiece,
            "neutral" => Self::Neutral,
            "disproved" => Self::Disproved,
            _ => return None,
        })
    }

    /// The assessment this label may justify on its own.
    ///
    /// SPEC §5 is explicit: a `missing_piece` label alone cannot become
    /// `Supported`, and a `disproved` label is a proposed contradiction that
    /// requires evidence and applicability checks before `Refuted` is recorded.
    /// So every label maps to at most `Plausible`, and `Disproved` maps to
    /// `Contested` pending that check — never straight to `Refuted`.
    pub fn implied_assessment(self) -> Assessment {
        match self {
            Self::MissingPiece => Assessment::Plausible,
            Self::AlreadyExplained | Self::OtherExplanationsMoreLikely | Self::Neutral => {
                Assessment::Untested
            }
            Self::Disproved => Assessment::Contested,
        }
    }
}

/// One observation extracted from an article, with its location (SPEC §15.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationNote {
    pub source_id: SourceId,
    /// Required locator: SPEC §15.4 adaptation demands one per observation.
    pub locator: super::source::Locator,
    pub observation: String,
    /// Whether the cause is already established in the literature.
    pub cause_established: bool,
    /// Would we see this observation if the hypothesis were true?
    pub consistent_with_hypothesis: bool,
    /// Would we see it anyway, hypothesis or not? If so, it is neutral.
    pub expected_regardless: bool,
    pub alternative_explanations: Vec<String>,
    pub label: ExplanatoryLabel,
}

/// A defect found by deep verification, classified by severity (SPEC §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Objection {
    pub description: String,
    /// A fatal premise invalidates the item; a repairable detail does not.
    pub fatal: bool,
    /// Which assumption or sub-assumption it targets.
    pub targets: Option<String>,
    /// A bounded action that would resolve it.
    pub suggested_check: Option<String>,
}

/// A review of one item version (SPEC §4.2 `Review`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: ReviewId,
    pub schema_version: u32,
    pub run_id: RunId,
    /// The exact immutable item version reviewed.
    pub item_id: ItemId,
    /// Hash of the reviewed content, so a stale review is detectable.
    pub item_hash: ContentHash,
    pub strategy: ReviewStrategy,
    /// Narrative assessment.
    pub assessment_text: String,
    /// Proposed epistemic assessment. Validated before it is recorded.
    pub proposed_assessment: Assessment,
    /// Evidence the reviewer considered, with per-claim relations.
    pub evidence: Vec<EvidenceLink>,
    pub objections: Vec<Objection>,
    /// Issues the review could not resolve.
    pub unknowns: Vec<String>,
    /// Bounded follow-up actions.
    pub next_actions: Vec<String>,
    /// Computational checks the reviewer asked for, as structured requests
    /// the Supervisor may turn into approved `Tool` tasks (SPEC §5).
    #[serde(default)]
    pub execution_requests: Vec<serde_json::Value>,
    /// Observation-strategy output (SPEC §15.4). Empty set is allowed.
    pub observations: Vec<ObservationNote>,
    /// The Fig. A.3 explanatory label, recorded separately from assessment.
    pub explanatory_label: Option<ExplanatoryLabel>,
    /// Numeric score, when the strategy produces one.
    ///
    /// SPEC §7 step 2: withheld from pairwise judges because scores are not
    /// comparable across reviews. Persisted, never forwarded to Ranking.
    pub score: Option<f32>,
    /// Who performed the review; researcher reviews are attributed.
    pub author: super::item::Author,
    pub produced_by: Option<TaskId>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Review {
    /// The score-free view supplied to a pairwise or debate judge (SPEC §7).
    ///
    /// Retains evidence and substantive criticism; drops the numeric score.
    pub fn judge_view(&self) -> JudgeReviewView {
        JudgeReviewView {
            strategy: self.strategy,
            assessment_text: self.assessment_text.clone(),
            objections: self
                .objections
                .iter()
                .map(|o| o.description.clone())
                .collect(),
            unknowns: self.unknowns.clone(),
            evidence_count: self.evidence.len(),
        }
    }
}

/// A review as presented to a tournament judge: substance without scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeReviewView {
    pub strategy: ReviewStrategy,
    pub assessment_text: String,
    pub objections: Vec<String>,
    pub unknowns: Vec<String>,
    pub evidence_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_piece_cannot_imply_supported() {
        assert_eq!(
            ExplanatoryLabel::MissingPiece.implied_assessment(),
            Assessment::Plausible
        );
    }

    #[test]
    fn disproved_is_contested_not_refuted() {
        assert_eq!(
            ExplanatoryLabel::Disproved.implied_assessment(),
            Assessment::Contested
        );
    }

    #[test]
    fn label_parses_prompt_prose_spelling() {
        assert_eq!(
            ExplanatoryLabel::parse("other explanations more likely"),
            Some(ExplanatoryLabel::OtherExplanationsMoreLikely)
        );
        assert_eq!(ExplanatoryLabel::parse("nonsense"), None);
    }
}
