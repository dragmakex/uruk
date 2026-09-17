//! Evidence, epistemic assessment, and workflow disposition (SPEC §4.3).

use super::ids::*;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Where an evidence item came from.
///
/// SPEC §4.3 requires these be distinguishable: an LLM simulation review is
/// reasoning, *not* an executed simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// Retrieved from literature (a paper, page, database record).
    RetrievedLiterature,
    /// Supplied by the researcher as an observation.
    SuppliedObservation,
    /// Produced by a computation Uruk actually executed.
    ComputationalResult,
    /// An experimental result performed outside Uruk and imported.
    ExternalExperiment,
    /// Output of a formal checker or proof tool.
    FormalVerification,
    /// A human's assessment, attributed.
    HumanAssessment,
    /// An agent's critique. Reasoning, not an empirical observation.
    AgentCritique,
}

impl EvidenceKind {
    /// Whether this kind can, on its own, justify `Supported` or `Refuted`.
    ///
    /// SPEC §4.3: "Supported and Refuted require claim-relevant evidence
    /// beyond agent opinion."
    pub fn is_empirical(self) -> bool {
        !matches!(self, Self::AgentCritique)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RetrievedLiterature => "retrieved_literature",
            Self::SuppliedObservation => "supplied_observation",
            Self::ComputationalResult => "computational_result",
            Self::ExternalExperiment => "external_experiment",
            Self::FormalVerification => "formal_verification",
            Self::HumanAssessment => "human_assessment",
            Self::AgentCritique => "agent_critique",
        }
    }
}

/// How one evidence item bears on one specific claim (SPEC §4.3).
///
/// An evidence item may support one claim while contradicting another, so this
/// lives on the link, never on the evidence itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRelation {
    Supports,
    Contradicts,
    Inconclusive,
    Context,
}

impl EvidenceRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Inconclusive => "inconclusive",
            Self::Context => "context",
        }
    }
}

/// A link from an evidence item to the specific claim it bears on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceLink {
    pub evidence_id: EvidenceId,
    /// The item version this evidence bears on. Immutable target (SPEC §4.2).
    pub item_id: ItemId,
    /// The specific claim within that item, quoted or located.
    pub claim: String,
    pub relation: EvidenceRelation,
    /// Why this relation holds.
    pub justification: String,
    /// Scope limits: what this evidence does *not* establish.
    pub limits: Option<String>,
}

/// An observation or result, with provenance (SPEC §4.2 `Evidence`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: EvidenceId,
    pub schema_version: u32,
    pub run_id: RunId,
    pub kind: EvidenceKind,
    /// What was observed or produced.
    pub content: String,
    /// How it was obtained (method, protocol, query).
    pub method: String,
    /// Input identities: source IDs, dataset hashes, code revisions.
    pub inputs: Vec<String>,
    /// Sources this evidence derives from, when applicable.
    pub source_ids: Vec<SourceId>,
    /// Artifacts holding raw output (logs, data, figures).
    pub artifact_ids: Vec<ArtifactId>,
    /// Known limitations of this evidence.
    pub limitations: Option<String>,
    /// The task that produced it.
    pub produced_by: Option<TaskId>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Revisable epistemic assessment of a claim (SPEC §4.3).
///
/// Explicitly *not* a monotonic confidence ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assessment {
    /// Proposed, with no relevant validation yet.
    Untested,
    /// Survived review; not experimentally established.
    Plausible,
    /// Relevant evidence supports the scoped claim, with limitations stated.
    Supported,
    /// Material supporting and contradicting evidence remain unresolved.
    Contested,
    /// Evidence contradicts the scoped claim or a necessary assumption.
    Refuted,
    /// Available checks cannot resolve the claim.
    Inconclusive,
}

impl Assessment {
    /// Whether this assessment requires claim-relevant empirical evidence
    /// beyond agent opinion (SPEC §4.3).
    pub fn requires_empirical_evidence(self) -> bool {
        matches!(self, Self::Supported | Self::Refuted)
    }

    /// Whether reaching this assessment must cite a Review or Decision.
    ///
    /// SPEC §4.3: "Every assessment beyond Untested references a Review or
    /// Decision explaining its basis."
    pub fn requires_basis(self) -> bool {
        !matches!(self, Self::Untested)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Untested => "untested",
            Self::Plausible => "plausible",
            Self::Supported => "supported",
            Self::Contested => "contested",
            Self::Refuted => "refuted",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// Workflow disposition, kept separate from scientific assessment (SPEC §4.3).
///
/// "A blocked tool says nothing about whether a claim is true."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "of")]
pub enum Disposition {
    Active,
    Blocked,
    Archived,
    /// Marked a duplicate of a representative item; provenance is preserved.
    Duplicate(ItemId),
}

impl Disposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Archived => "archived",
            Self::Duplicate(_) => "duplicate",
        }
    }
}

/// A recorded epistemic assessment of an item, with its basis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssessmentRecord {
    pub item_id: ItemId,
    pub assessment: Assessment,
    /// The Review or Decision justifying this assessment (SPEC §4.3).
    pub basis_review: Option<ReviewId>,
    pub basis_decision: Option<DecisionId>,
    /// Evidence links considered when reaching it.
    pub evidence: Vec<EvidenceLink>,
    pub rationale: String,
    #[serde(with = "time::serde::rfc3339")]
    pub assessed_at: OffsetDateTime,
}

/// Why a proposed assessment was rejected by the §4.3 gate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssessmentError {
    #[error("{0} requires a Review or Decision explaining its basis (SPEC §4.3)")]
    MissingBasis(&'static str),
    #[error(
        "{assessment} requires claim-relevant evidence beyond agent opinion; \
         got {n_agent} agent critique(s) and no empirical evidence (SPEC §4.3)"
    )]
    AgentOpinionOnly {
        assessment: &'static str,
        n_agent: usize,
    },
    #[error(
        "{assessment} requires evidence that {expected} the claim; \
         none of the {n} linked evidence item(s) do (SPEC §4.3)"
    )]
    NoRelevantRelation {
        assessment: &'static str,
        expected: &'static str,
        n: usize,
    },
    #[error(
        "assessment of {item} cites evidence linked to a different item {link_item}; \
         evidence must bear on the claim being assessed (SPEC §4.3)"
    )]
    WrongItem { item: String, link_item: String },
}

/// Validate a proposed assessment against SPEC §4.3 before it is recorded.
///
/// `evidence_kinds` gives the kind of each linked evidence item, in the same
/// order as `links`. This is the single gate every write path routes through,
/// so an agent cannot promote its own opinion to `Supported`.
pub fn validate_assessment(
    item_id: &ItemId,
    assessment: Assessment,
    links: &[EvidenceLink],
    evidence_kinds: &[EvidenceKind],
    basis_review: Option<&ReviewId>,
    basis_decision: Option<&DecisionId>,
) -> Result<(), AssessmentError> {
    let name = assessment.as_str();

    // Evidence for a different claim is not evidence for this one.
    if let Some(link) = links.iter().find(|l| &l.item_id != item_id) {
        return Err(AssessmentError::WrongItem {
            item: item_id.0.clone(),
            link_item: link.item_id.0.clone(),
        });
    }

    if assessment.requires_basis() && basis_review.is_none() && basis_decision.is_none() {
        return Err(AssessmentError::MissingBasis(name));
    }

    if assessment.requires_empirical_evidence() {
        let expected = match assessment {
            Assessment::Supported => EvidenceRelation::Supports,
            Assessment::Refuted => EvidenceRelation::Contradicts,
            _ => unreachable!("only Supported/Refuted require empirical evidence"),
        };

        // Only evidence that actually bears on the claim in the right
        // direction, and is not merely an agent critique, counts.
        let qualifying = links
            .iter()
            .zip(evidence_kinds)
            .filter(|(link, kind)| link.relation == expected && kind.is_empirical())
            .count();

        if qualifying == 0 {
            let n_agent = evidence_kinds.iter().filter(|k| !k.is_empirical()).count();
            let directional = links.iter().filter(|l| l.relation == expected).count();

            // Distinguish "argued but never measured" from "measured the wrong thing".
            return Err(if directional > 0 && n_agent > 0 {
                AssessmentError::AgentOpinionOnly {
                    assessment: name,
                    n_agent,
                }
            } else {
                AssessmentError::NoRelevantRelation {
                    assessment: name,
                    expected: expected.as_str(),
                    n: links.len(),
                }
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> ItemId {
        ItemId::from_raw("item_x")
    }

    fn link(relation: EvidenceRelation) -> EvidenceLink {
        EvidenceLink {
            evidence_id: EvidenceId::new(),
            item_id: item(),
            claim: "the claim".into(),
            relation,
            justification: "because".into(),
            limits: None,
        }
    }

    #[test]
    fn untested_needs_nothing() {
        assert!(validate_assessment(&item(), Assessment::Untested, &[], &[], None, None).is_ok());
    }

    #[test]
    fn plausible_needs_a_basis_but_not_evidence() {
        let rev = ReviewId::new();
        assert_eq!(
            validate_assessment(&item(), Assessment::Plausible, &[], &[], None, None),
            Err(AssessmentError::MissingBasis("plausible"))
        );
        assert!(
            validate_assessment(&item(), Assessment::Plausible, &[], &[], Some(&rev), None).is_ok()
        );
    }

    #[test]
    fn agent_critique_alone_cannot_support() {
        let rev = ReviewId::new();
        let err = validate_assessment(
            &item(),
            Assessment::Supported,
            &[link(EvidenceRelation::Supports)],
            &[EvidenceKind::AgentCritique],
            Some(&rev),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, AssessmentError::AgentOpinionOnly { .. }),
            "{err}"
        );
    }

    #[test]
    fn empirical_evidence_supports() {
        let rev = ReviewId::new();
        assert!(
            validate_assessment(
                &item(),
                Assessment::Supported,
                &[link(EvidenceRelation::Supports)],
                &[EvidenceKind::ComputationalResult],
                Some(&rev),
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn wrong_direction_evidence_cannot_support() {
        let rev = ReviewId::new();
        let err = validate_assessment(
            &item(),
            Assessment::Supported,
            &[link(EvidenceRelation::Contradicts)],
            &[EvidenceKind::ComputationalResult],
            Some(&rev),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, AssessmentError::NoRelevantRelation { .. }),
            "{err}"
        );
    }

    #[test]
    fn evidence_for_another_item_cannot_support_this_one() {
        let rev = ReviewId::new();
        let mut other = link(EvidenceRelation::Supports);
        other.item_id = ItemId::from_raw("item_y");
        let err = validate_assessment(
            &item(),
            Assessment::Supported,
            &[other],
            &[EvidenceKind::ComputationalResult],
            Some(&rev),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, AssessmentError::WrongItem { .. }), "{err}");
    }

    #[test]
    fn refuted_needs_contradicting_empirical_evidence() {
        let rev = ReviewId::new();
        // Agent critique claiming contradiction is not enough.
        assert!(
            validate_assessment(
                &item(),
                Assessment::Refuted,
                &[link(EvidenceRelation::Contradicts)],
                &[EvidenceKind::AgentCritique],
                Some(&rev),
                None,
            )
            .is_err()
        );
        assert!(
            validate_assessment(
                &item(),
                Assessment::Refuted,
                &[link(EvidenceRelation::Contradicts)],
                &[EvidenceKind::ExternalExperiment],
                Some(&rev),
                None,
            )
            .is_ok()
        );
    }
}
