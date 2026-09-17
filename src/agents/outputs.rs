//! Typed model outputs and their validation (SPEC §5.1).
//!
//! "Replace free-text ending markers with validated typed outputs."
//! A model proposes content; the runtime assigns IDs and commits records.
//! Anything that fails to parse here never becomes a scientific record.

use crate::records::*;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// Extract the JSON object from a model reply.
///
/// Models commonly wrap JSON in prose or a fenced code block, so this locates
/// the outermost balanced object rather than demanding a bare document.
pub fn extract_json(text: &str) -> Result<serde_json::Value> {
    let trimmed = text.trim();

    // Fast path: the whole reply is JSON.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed)
        && v.is_object()
    {
        return Ok(v);
    }

    // Strip a fenced block if present.
    let body = match trimmed.find("```") {
        Some(start) => {
            let after = &trimmed[start + 3..];
            let after = after.strip_prefix("json").unwrap_or(after);
            match after.find("```") {
                Some(end) => after[..end].trim(),
                None => after.trim(),
            }
        }
        None => trimmed,
    };

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body)
        && v.is_object()
    {
        return Ok(v);
    }

    // Last resort: scan for the outermost balanced braces, respecting strings.
    let bytes = body.as_bytes();
    let start = body.find('{').ok_or_else(|| {
        Error::validation(format!(
            "model output contains no JSON object: {}",
            truncate(body, 200)
        ))
    })?;

    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for (offset, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let candidate = &body[start..=offset];
                    return serde_json::from_str(candidate).map_err(|e| {
                        Error::validation(format!(
                            "model output is not valid JSON: {e}; got {}",
                            truncate(candidate, 200)
                        ))
                    });
                }
            }
            _ => {}
        }
    }

    Err(Error::validation(format!(
        "model output has unbalanced JSON: {}",
        truncate(body, 200)
    )))
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        // Respect char boundaries so this never panics on multi-byte input.
        let end = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= n)
            .last()
            .unwrap_or(0);
        format!("{}…", &s[..end])
    }
}

/// Parse a typed value from a model reply.
pub fn parse<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T> {
    let value = extract_json(text)?;
    serde_json::from_value(value).map_err(|e| {
        Error::validation(format!(
            "model output did not match the required shape: {e}"
        ))
    })
}

/// A grounding citation supplied by a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grounding {
    pub source_id: String,
    pub locator: String,
    #[serde(default)]
    pub supports: String,
}

/// Output of `generation.literature`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedHypothesis {
    pub title: String,
    pub claim: String,
    pub mechanism: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub predictions: Vec<String>,
    #[serde(default)]
    pub falsifiers: Vec<String>,
    #[serde(default)]
    pub validation_plan: String,
    #[serde(default)]
    pub grounding: Vec<Grounding>,
    #[serde(default)]
    pub limitations: String,
    /// True when the model reports insufficient source grounding.
    #[serde(default)]
    pub provisional: bool,
}

impl GeneratedHypothesis {
    /// Convert to the record's hypothesis fields.
    pub fn to_fields(&self) -> HypothesisFields {
        HypothesisFields {
            claim: self.claim.clone(),
            mechanism: self.mechanism.clone(),
            assumptions: self.assumptions.clone(),
            scope: self.scope.clone(),
            predictions: self.predictions.clone(),
            falsifiers: self.falsifiers.clone(),
            validation_plan: self.validation_plan.clone(),
        }
    }

    /// Prose body for the item record.
    pub fn to_content(&self) -> String {
        let mut s = format!(
            "{}\n\n## Claim\n{}\n\n## Mechanism\n{}\n",
            self.title, self.claim, self.mechanism
        );
        if !self.scope.is_empty() {
            s.push_str(&format!("\n## Scope\n{}\n", self.scope));
        }
        for (heading, items) in [
            ("Assumptions", &self.assumptions),
            ("Predictions", &self.predictions),
            ("Falsifiers", &self.falsifiers),
        ] {
            if !items.is_empty() {
                s.push_str(&format!("\n## {heading}\n"));
                for item in items {
                    s.push_str(&format!("- {item}\n"));
                }
            }
        }
        if !self.validation_plan.is_empty() {
            s.push_str(&format!("\n## Validation plan\n{}\n", self.validation_plan));
        }
        if !self.limitations.is_empty() {
            s.push_str(&format!("\n## Limitations\n{}\n", self.limitations));
        }
        s
    }

    /// Validate before a record is accepted (SPEC §5.1).
    pub fn validate(&self) -> Result<()> {
        if self.claim.trim().is_empty() {
            return Err(Error::validation("generated hypothesis has an empty claim"));
        }
        if self.mechanism.trim().is_empty() {
            return Err(Error::validation(
                "generated hypothesis has no mechanism; SPEC §4.2 requires one",
            ));
        }
        if self.title.trim().is_empty() {
            return Err(Error::validation("generated hypothesis has no title"));
        }
        Ok(())
    }
}

/// Status of one debate contribution (SPEC §6 turn budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebateStatus {
    Continue,
    Final,
    Partial,
}

/// Output of `generation.debate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebateTurn {
    pub status: DebateStatus,
    #[serde(default)]
    pub contribution: String,
    /// Present only when `status` is `final`.
    #[serde(default)]
    pub hypothesis: Option<GeneratedHypothesis>,
    #[serde(default)]
    pub unresolved: Vec<String>,
}

impl DebateTurn {
    pub fn validate(&self) -> Result<()> {
        match self.status {
            DebateStatus::Final => match &self.hypothesis {
                Some(h) => h.validate(),
                None => Err(Error::validation(
                    "debate reported `final` without a hypothesis; \
                     an incomplete discussion must not fabricate a conclusion",
                )),
            },
            DebateStatus::Continue | DebateStatus::Partial => {
                if self.contribution.trim().is_empty() {
                    return Err(Error::validation("debate turn has no contribution"));
                }
                Ok(())
            }
        }
    }
}

/// An objection as returned by a review template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawObjection {
    pub description: String,
    #[serde(default)]
    pub fatal: bool,
    #[serde(default)]
    pub targets: Option<String>,
    #[serde(default)]
    pub suggested_check: Option<String>,
}

impl From<RawObjection> for Objection {
    fn from(r: RawObjection) -> Self {
        Objection {
            description: r.description,
            fatal: r.fatal,
            targets: r.targets,
            suggested_check: r.suggested_check,
        }
    }
}

/// An evidence link as returned by a review template.
///
/// Cites either a source (a literature citation at a locator) or an existing
/// evidence record (for instance an execution result the review consumed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvidence {
    #[serde(default)]
    pub source_id: String,
    /// An existing evidence record this review interprets.
    #[serde(default)]
    pub evidence_id: Option<String>,
    #[serde(default)]
    pub locator: String,
    /// What the source reports at the locator, as distinct from the
    /// reviewer's inference (SPEC §4.3).
    #[serde(default)]
    pub reported: String,
    pub claim: String,
    pub relation: EvidenceRelation,
    #[serde(default)]
    pub justification: String,
    #[serde(default)]
    pub limits: Option<String>,
}

/// A computational check a reviewer asks for (SPEC §5: "Reflection may
/// request computational checks"). The Supervisor turns it into a `Tool`
/// task gated by the allowlist and, where required, a human approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawExecRequest {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Input files, which must already be supplied sources.
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub purpose: String,
    /// The claim in the reviewed item this check bears on.
    #[serde(default)]
    pub target_claim: String,
}

impl RawExecRequest {
    pub fn validate(&self) -> Result<()> {
        if self.program.trim().is_empty() {
            return Err(Error::validation("execution request names no program"));
        }
        if self.program.contains('/') || self.program.contains('\\') {
            return Err(Error::validation(
                "execution request must name an approved tool, not a path",
            ));
        }
        Ok(())
    }
}

/// Output of the non-observation review templates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewOutput {
    pub assessment_text: String,
    pub proposed_assessment: Assessment,
    #[serde(default)]
    pub evidence: Vec<RawEvidence>,
    #[serde(default)]
    pub objections: Vec<RawObjection>,
    #[serde(default)]
    pub unknowns: Vec<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    #[serde(default)]
    pub safety_concern: Option<String>,
    #[serde(default)]
    pub coverage_gaps: Option<String>,
    #[serde(default)]
    pub requires_execution: Vec<String>,
    /// Structured computational checks the reviewer requests.
    #[serde(default)]
    pub execution_requests: Vec<RawExecRequest>,
    /// Deep-verification assumption breakdown.
    #[serde(default)]
    pub assumptions: Vec<serde_json::Value>,
    #[serde(default)]
    pub steps: Vec<serde_json::Value>,
}

impl ReviewOutput {
    /// A review's own reading cannot assert empirical support (SPEC §4.3).
    pub fn validate(&self) -> Result<()> {
        if self.assessment_text.trim().is_empty() {
            return Err(Error::validation("review has no assessment text"));
        }
        if self.proposed_assessment.requires_empirical_evidence() {
            return Err(Error::validation(format!(
                "a review may not propose `{}` directly; that requires claim-relevant \
                 empirical evidence recorded separately (SPEC §4.3)",
                self.proposed_assessment.as_str()
            )));
        }
        for e in &self.evidence {
            if e.source_id.trim().is_empty() && e.evidence_id.is_none() {
                return Err(Error::validation(
                    "an evidence citation must name a source or an evidence record",
                ));
            }
        }
        for r in &self.execution_requests {
            r.validate()?;
        }
        Ok(())
    }

    /// Whether this review reports a safety concern needing a §12 decision.
    pub fn has_safety_concern(&self) -> bool {
        self.safety_concern
            .as_deref()
            .map(|s| {
                let t = s.trim().to_ascii_lowercase();
                !t.is_empty() && t != "none" && t != "n/a" && t != "no concerns"
            })
            .unwrap_or(false)
    }
}

/// One extracted observation from `reflection.observation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawObservation {
    pub source_id: String,
    pub locator: serde_json::Value,
    pub observation: String,
    #[serde(default)]
    pub cause_established: bool,
    #[serde(default)]
    pub consistent_with_hypothesis: bool,
    #[serde(default)]
    pub expected_regardless: bool,
    #[serde(default)]
    pub alternative_explanations: Vec<String>,
    pub label: ExplanatoryLabel,
}

/// Output of `reflection.observation` (2025 Fig. A.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationReviewOutput {
    #[serde(default)]
    pub observations: Vec<RawObservation>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub disproof: String,
    pub label: ExplanatoryLabel,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub uncertainty: String,
}

impl ObservationReviewOutput {
    /// Validate against the §15.4 adaptation rules.
    ///
    /// An empty observation set is allowed, but a label that asserts an
    /// explanatory relation requires at least one located observation.
    pub fn validate(&self) -> Result<()> {
        for obs in &self.observations {
            if obs.source_id.trim().is_empty() {
                return Err(Error::validation(
                    "observation has no source; SPEC §15.4 requires a locator per observation",
                ));
            }
            if obs.locator.is_null() {
                return Err(Error::validation(format!(
                    "observation {:?} has no locator (SPEC §15.4)",
                    truncate(&obs.observation, 60)
                )));
            }
        }

        if self.observations.is_empty()
            && matches!(
                self.label,
                ExplanatoryLabel::MissingPiece | ExplanatoryLabel::Disproved
            )
        {
            return Err(Error::validation(format!(
                "label `{}` asserted with no observations; an empty observation set \
                 cannot be forced into an explanatory claim (SPEC §15.4)",
                self.label.as_str()
            )));
        }
        Ok(())
    }

    /// The assessment this review may propose.
    ///
    /// Capped by [`ExplanatoryLabel::implied_assessment`], so a `missing_piece`
    /// never becomes `Supported` and a `disproved` never becomes `Refuted`
    /// without separate claim-relevant evidence.
    pub fn proposed_assessment(&self) -> Assessment {
        // An observation expected whether or not the hypothesis holds is
        // neutral, whatever the model labelled it.
        if !self.observations.is_empty()
            && self.observations.iter().all(|o| o.expected_regardless)
            && self.label != ExplanatoryLabel::Disproved
        {
            return Assessment::Untested;
        }
        self.label.implied_assessment()
    }

    /// The effective label after the non-discriminating-observation rule.
    pub fn effective_label(&self) -> ExplanatoryLabel {
        if !self.observations.is_empty()
            && self.observations.iter().all(|o| o.expected_regardless)
            && self.label != ExplanatoryLabel::Disproved
        {
            return ExplanatoryLabel::Neutral;
        }
        self.label
    }
}

/// A judged comparison outcome, as the ranking templates return it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgedOutcome {
    #[serde(rename = "win_1")]
    Win1,
    #[serde(rename = "win_2")]
    Win2,
    Draw,
    InsufficientBasis,
}

/// Output of `ranking.pairwise`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingOutput {
    pub outcome: JudgedOutcome,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub tradeoffs: Vec<String>,
    #[serde(default)]
    pub evidence_cited: Vec<String>,
    #[serde(default)]
    pub missing_to_decide: Vec<String>,
}

impl RankingOutput {
    pub fn validate(&self) -> Result<()> {
        if self.outcome == JudgedOutcome::InsufficientBasis && self.missing_to_decide.is_empty() {
            return Err(Error::validation(
                "insufficient_basis must say what evidence would settle the comparison",
            ));
        }
        if self.rationale.trim().is_empty() {
            return Err(Error::validation("comparison has no rationale"));
        }
        Ok(())
    }
}

/// Output of `ranking.debate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingDebateTurn {
    pub status: DebateStatus,
    #[serde(default)]
    pub contribution: String,
    #[serde(default)]
    pub outcome: Option<JudgedOutcome>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub tradeoffs: Vec<String>,
    #[serde(default)]
    pub missing_to_decide: Vec<String>,
}

impl RankingDebateTurn {
    pub fn validate(&self) -> Result<()> {
        if self.status == DebateStatus::Final && self.outcome.is_none() {
            return Err(Error::validation(
                "debate reported `final` without an outcome; \
                 an incomplete discussion must not fabricate a winner",
            ));
        }
        Ok(())
    }
}

/// Output of the evolution templates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolvedHypothesis {
    #[serde(flatten)]
    pub hypothesis: GeneratedHypothesis,
    #[serde(default)]
    pub changes: String,
    #[serde(default)]
    pub assumptions_retained: Vec<String>,
    #[serde(default)]
    pub assumptions_rejected: Vec<String>,
    #[serde(default)]
    pub feasibility_basis: String,
    #[serde(default)]
    pub discriminating_test: String,
    #[serde(default)]
    pub remaining_limitations: String,
    // Analogy-specific fields.
    #[serde(default)]
    pub transferable_principle: String,
    #[serde(default)]
    pub analogy_source: String,
    #[serde(default)]
    pub analogy_failure_modes: Vec<String>,
    #[serde(default)]
    pub discriminating_prediction: String,
    #[serde(default)]
    pub borrowed_vs_new_evidence: String,
}

impl EvolvedHypothesis {
    pub fn validate(&self) -> Result<()> {
        self.hypothesis.validate()?;
        // A child must say what changed, or it is not a derivation.
        if self.changes.trim().is_empty() && self.transferable_principle.trim().is_empty() {
            return Err(Error::validation(
                "evolved item states neither what changed nor the transferred principle",
            ));
        }
        Ok(())
    }

    /// The derivation note recorded on the child item.
    pub fn derivation(&self) -> String {
        let mut parts = Vec::new();
        if !self.changes.is_empty() {
            parts.push(format!("Changes: {}", self.changes));
        }
        if !self.feasibility_basis.is_empty() {
            parts.push(format!("Feasibility basis: {}", self.feasibility_basis));
        }
        if !self.transferable_principle.is_empty() {
            parts.push(format!(
                "Transferred principle: {}",
                self.transferable_principle
            ));
        }
        if !self.analogy_source.is_empty() {
            parts.push(format!("Analogy: {}", self.analogy_source));
        }
        if !self.assumptions_retained.is_empty() {
            parts.push(format!(
                "Retained: {}",
                self.assumptions_retained.join("; ")
            ));
        }
        if !self.assumptions_rejected.is_empty() {
            parts.push(format!(
                "Rejected: {}",
                self.assumptions_rejected.join("; ")
            ));
        }
        parts.join("\n")
    }
}

/// One critique from `meta.review`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawCritique {
    pub issue: String,
    #[serde(default)]
    pub occurrences: u32,
    #[serde(default)]
    pub supporting_records: Vec<String>,
    #[serde(default)]
    pub exceptions: Vec<String>,
    #[serde(default)]
    pub recommended_check: Option<String>,
    #[serde(default)]
    pub target_roles: Vec<String>,
}

/// Output of `meta.review`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaReviewOutput {
    #[serde(default)]
    pub critiques: Vec<RawCritique>,
    #[serde(default)]
    pub methodological_gaps: Vec<String>,
    #[serde(default)]
    pub coverage_gaps: Vec<String>,
    #[serde(default)]
    pub caution: String,
}

/// Output of `proximity.similarity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProximityOutput {
    pub relation: ProximityRelation,
    #[serde(default)]
    pub shared_mechanism: String,
    #[serde(default)]
    pub differences: Vec<ProximityDifference>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub cluster_label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProximityRelation {
    Duplicate,
    Related,
    Distinct,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProximityDifference {
    pub dimension: String,
    pub difference: String,
    #[serde(default)]
    pub material: bool,
}

impl ProximityOutput {
    /// Similarity alone must not invalidate a candidate (SPEC §5 Proximity).
    ///
    /// A `duplicate` verdict is downgraded to `related` when any difference is
    /// material, so a merely similar idea is preserved.
    pub fn effective_relation(&self) -> ProximityRelation {
        if self.relation == ProximityRelation::Duplicate
            && self.differences.iter().any(|d| d.material)
        {
            return ProximityRelation::Related;
        }
        self.relation
    }
}

/// A claim made in a synthesis deliverable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisClaim {
    pub claim: String,
    pub basis: ClaimBasis,
    #[serde(default)]
    pub citations: Vec<Grounding>,
}

/// SPEC §11: reports must distinguish source-reported results, Uruk's
/// interpretations, and untested proposals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimBasis {
    SourceReported,
    UrukInference,
    UntestedProposal,
}

impl ClaimBasis {
    pub fn label(self) -> &'static str {
        match self {
            Self::SourceReported => "source-reported",
            Self::UrukInference => "Uruk inference",
            Self::UntestedProposal => "untested proposal",
        }
    }
}

/// Output of `task.synthesis`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisOutput {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub claims: Vec<SynthesisClaim>,
    #[serde(default)]
    pub disagreements: Vec<String>,
    #[serde(default)]
    pub limitations: String,
    #[serde(default)]
    pub next_actions: Vec<String>,
}

impl SynthesisOutput {
    pub fn validate(&self) -> Result<()> {
        if self.body.trim().is_empty() {
            return Err(Error::validation("synthesis produced an empty body"));
        }
        // A source-reported claim without a citation is an unsupported
        // assertion dressed as a finding (SPEC §4.3).
        for claim in &self.claims {
            if claim.basis == ClaimBasis::SourceReported && claim.citations.is_empty() {
                return Err(Error::validation(format!(
                    "claim {:?} is marked source-reported but cites nothing (SPEC §4.3)",
                    truncate(&claim.claim, 80)
                )));
            }
        }
        Ok(())
    }
}

/// Output of `meta.overview`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverviewOutput {
    #[serde(default)]
    pub directions: Vec<serde_json::Value>,
    #[serde(default)]
    pub discriminating_experiments: Vec<serde_json::Value>,
    #[serde(default)]
    pub underexplored: Vec<String>,
    #[serde(default)]
    pub suggested_reviewers: Vec<String>,
    #[serde(default)]
    pub limitations: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_extracted_from_prose_and_fences() {
        let v = extract_json("Here you go:\n```json\n{\"a\": 1}\n```\nHope that helps").unwrap();
        assert_eq!(v["a"], 1);

        let v = extract_json("{\"b\": 2}").unwrap();
        assert_eq!(v["b"], 2);

        let v = extract_json("Thinking... {\"c\": {\"d\": 3}} done").unwrap();
        assert_eq!(v["c"]["d"], 3);
    }

    #[test]
    fn braces_inside_strings_do_not_confuse_extraction() {
        let v = extract_json(r#"{"text": "a } brace", "n": 1}"#).unwrap();
        assert_eq!(v["n"], 1);
        assert_eq!(v["text"], "a } brace");
    }

    #[test]
    fn non_json_output_is_a_validation_error() {
        let err = extract_json("better idea: 1").unwrap_err();
        assert!(err.to_string().contains("no JSON object"), "{err}");
    }

    #[test]
    fn review_cannot_propose_supported() {
        let out = ReviewOutput {
            assessment_text: "great".into(),
            proposed_assessment: Assessment::Supported,
            evidence: vec![],
            objections: vec![],
            unknowns: vec![],
            next_actions: vec![],
            safety_concern: None,
            coverage_gaps: None,
            requires_execution: vec![],
            execution_requests: vec![],
            assumptions: vec![],
            steps: vec![],
        };
        let err = out.validate().unwrap_err();
        assert!(err.to_string().contains("may not propose"), "{err}");
    }

    #[test]
    fn final_debate_without_hypothesis_is_refused() {
        let turn = DebateTurn {
            status: DebateStatus::Final,
            contribution: "we agree".into(),
            hypothesis: None,
            unresolved: vec![],
        };
        let err = turn.validate().unwrap_err();
        assert!(err.to_string().contains("fabricate"), "{err}");
    }

    #[test]
    fn insufficient_basis_must_say_what_is_missing() {
        let out = RankingOutput {
            outcome: JudgedOutcome::InsufficientBasis,
            rationale: "cannot tell".into(),
            tradeoffs: vec![],
            evidence_cited: vec![],
            missing_to_decide: vec![],
        };
        assert!(out.validate().is_err());
    }

    fn obs(expected_regardless: bool, label: ExplanatoryLabel) -> RawObservation {
        RawObservation {
            source_id: "src_1".into(),
            locator: serde_json::json!({"kind": "page", "at": "3"}),
            observation: "something".into(),
            cause_established: false,
            consistent_with_hypothesis: true,
            expected_regardless,
            alternative_explanations: vec![],
            label,
        }
    }

    #[test]
    fn non_discriminating_observations_are_neutral() {
        // The model claimed missing_piece, but every observation would occur
        // regardless of the hypothesis (SPEC §15.4).
        let out = ObservationReviewOutput {
            observations: vec![obs(true, ExplanatoryLabel::MissingPiece)],
            summary: String::new(),
            disproof: "none".into(),
            label: ExplanatoryLabel::MissingPiece,
            rationale: String::new(),
            uncertainty: String::new(),
        };
        out.validate().unwrap();
        assert_eq!(out.effective_label(), ExplanatoryLabel::Neutral);
        assert_eq!(out.proposed_assessment(), Assessment::Untested);
    }

    #[test]
    fn missing_piece_tops_out_at_plausible() {
        let out = ObservationReviewOutput {
            observations: vec![obs(false, ExplanatoryLabel::MissingPiece)],
            summary: String::new(),
            disproof: "none".into(),
            label: ExplanatoryLabel::MissingPiece,
            rationale: String::new(),
            uncertainty: String::new(),
        };
        assert_eq!(out.proposed_assessment(), Assessment::Plausible);
    }

    #[test]
    fn disproved_tops_out_at_contested() {
        let out = ObservationReviewOutput {
            observations: vec![obs(false, ExplanatoryLabel::Disproved)],
            summary: String::new(),
            disproof: "contradicts figure 2".into(),
            label: ExplanatoryLabel::Disproved,
            rationale: String::new(),
            uncertainty: String::new(),
        };
        assert_eq!(
            out.proposed_assessment(),
            Assessment::Contested,
            "a proposed disproof is not Refuted until evidence is checked"
        );
    }

    #[test]
    fn empty_observations_cannot_assert_an_explanatory_claim() {
        let out = ObservationReviewOutput {
            observations: vec![],
            summary: String::new(),
            disproof: String::new(),
            label: ExplanatoryLabel::MissingPiece,
            rationale: String::new(),
            uncertainty: String::new(),
        };
        assert!(out.validate().is_err());

        // But an empty set with a neutral label is fine.
        let neutral = ObservationReviewOutput {
            label: ExplanatoryLabel::Neutral,
            ..out
        };
        neutral.validate().unwrap();
    }

    #[test]
    fn observation_without_locator_is_refused() {
        let mut o = obs(false, ExplanatoryLabel::MissingPiece);
        o.locator = serde_json::Value::Null;
        let out = ObservationReviewOutput {
            observations: vec![o],
            summary: String::new(),
            disproof: String::new(),
            label: ExplanatoryLabel::MissingPiece,
            rationale: String::new(),
            uncertainty: String::new(),
        };
        assert!(out.validate().unwrap_err().to_string().contains("locator"));
    }

    #[test]
    fn material_difference_downgrades_a_duplicate_verdict() {
        let out = ProximityOutput {
            relation: ProximityRelation::Duplicate,
            shared_mechanism: "both use X".into(),
            differences: vec![ProximityDifference {
                dimension: "predictions".into(),
                difference: "different predicted magnitude".into(),
                material: true,
            }],
            rationale: String::new(),
            cluster_label: None,
        };
        assert_eq!(out.effective_relation(), ProximityRelation::Related);
    }

    #[test]
    fn uncited_source_reported_claim_is_refused() {
        let out = SynthesisOutput {
            title: "t".into(),
            body: "body".into(),
            claims: vec![SynthesisClaim {
                claim: "the study found a 40% reduction".into(),
                basis: ClaimBasis::SourceReported,
                citations: vec![],
            }],
            disagreements: vec![],
            limitations: String::new(),
            next_actions: vec![],
        };
        assert!(
            out.validate()
                .unwrap_err()
                .to_string()
                .contains("cites nothing")
        );
    }
}
