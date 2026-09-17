//! Safety and research integrity checks (SPEC §12).
//!
//! Following 2025 §6, the initial goal, each generated or evolved candidate,
//! and emerging research directions are checked *separately*. A safe goal is
//! not blanket approval for unsafe intermediate work.

use crate::records::*;
use time::OffsetDateTime;

/// What a safety check concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyVerdict {
    /// No concern identified by this check.
    Clear,
    /// Proceed, but the concern is recorded and surfaced in the report.
    Flagged { concern: String, category: Category },
    /// Blocked: the work does not proceed, and the candidate does not enter
    /// the tournament (SPEC §12).
    Blocked { reason: String, category: Category },
}

impl SafetyVerdict {
    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }

    pub fn concern(&self) -> Option<&str> {
        match self {
            Self::Clear => None,
            Self::Flagged { concern, .. } => Some(concern),
            Self::Blocked { reason, .. } => Some(reason),
        }
    }
}

/// Domains requiring qualified oversight and institutional approval (§12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Human subjects research.
    HumanSubjects,
    /// Animal research.
    Animal,
    /// Hazardous materials or procedures.
    Hazardous,
    /// Dual-use: research whose primary application is harm.
    DualUse,
    /// Clinical intervention on identifiable patients.
    Clinical,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HumanSubjects => "human_subjects",
            Self::Animal => "animal",
            Self::Hazardous => "hazardous",
            Self::DualUse => "dual_use",
            Self::Clinical => "clinical",
        }
    }

    /// What oversight this category requires before work may proceed.
    pub fn required_oversight(self) -> &'static str {
        match self {
            Self::HumanSubjects => "institutional review board (IRB) approval and informed consent",
            Self::Animal => "institutional animal care and use committee (IACUC) approval",
            Self::Hazardous => "institutional biosafety or chemical safety approval",
            Self::DualUse => "institutional dual-use research of concern (DURC) review",
            Self::Clinical => "clinical trial authorisation and ethics approval",
        }
    }
}

/// Phrases whose primary purpose is harm. These block rather than flag.
///
/// Deliberately narrow: this is a backstop for unambiguous cases, not a
/// topic filter. Ordinary research that merely mentions a sensitive area is
/// flagged for oversight, not refused (SPEC: refusals are for genuinely
/// harmful requests, not sensitive-sounding ones).
const BLOCKING_PATTERNS: &[(&str, Category)] = &[
    (
        "enhance transmissibility of a human pathogen",
        Category::DualUse,
    ),
    ("increase lethality of a pathogen", Category::DualUse),
    ("evade existing vaccines", Category::DualUse),
    ("synthesize a nerve agent", Category::Hazardous),
    ("weaponize", Category::DualUse),
    ("bioweapon", Category::DualUse),
    ("chemical weapon", Category::DualUse),
    ("without informed consent", Category::HumanSubjects),
    ("bypass ethics review", Category::HumanSubjects),
    ("circumvent irb", Category::HumanSubjects),
    ("undisclosed human experiment", Category::HumanSubjects),
];

/// Topics requiring oversight. These flag, and gate execution, rather than
/// refusing the research question itself.
const OVERSIGHT_PATTERNS: &[(&str, Category)] = &[
    ("human participants", Category::HumanSubjects),
    ("human subjects", Category::HumanSubjects),
    ("patient", Category::Clinical),
    ("clinical trial", Category::Clinical),
    ("in vivo", Category::Animal),
    ("animal model", Category::Animal),
    ("mouse model", Category::Animal),
    ("murine", Category::Animal),
    ("bsl-3", Category::Hazardous),
    ("bsl-4", Category::Hazardous),
    ("select agent", Category::Hazardous),
    ("radioactive", Category::Hazardous),
    ("gain-of-function", Category::DualUse),
];

/// Check a goal, candidate, or research direction for safety concerns.
///
/// `text` is the full content being checked: a goal's question, an item's
/// claim and mechanism, or a described direction.
pub fn check(text: &str) -> SafetyVerdict {
    let lower = text.to_ascii_lowercase();

    for (pattern, category) in BLOCKING_PATTERNS {
        if lower.contains(pattern) {
            return SafetyVerdict::Blocked {
                reason: format!(
                    "the work describes {pattern:?}, which Uruk does not assist with. \
                     This is blocked regardless of goal-level approval (SPEC §12)."
                ),
                category: *category,
            };
        }
    }

    for (pattern, category) in OVERSIGHT_PATTERNS {
        if lower.contains(pattern) {
            return SafetyVerdict::Flagged {
                concern: format!(
                    "involves {pattern:?}, which requires {}. Uruk can assist with \
                     design and analysis; actual execution requires that approval and \
                     qualified oversight.",
                    category.required_oversight()
                ),
                category: *category,
            };
        }
    }

    SafetyVerdict::Clear
}

/// Check a generated or evolved candidate (SPEC §12).
///
/// A candidate is checked on its own content: a permitted goal cannot admit a
/// blocked candidate.
pub fn check_item(item: &ResearchItem) -> SafetyVerdict {
    let mut text = format!("{} {}", item.title, item.content);
    if let Some(h) = &item.hypothesis {
        text.push(' ');
        text.push_str(&h.claim);
        text.push(' ');
        text.push_str(&h.mechanism);
        text.push(' ');
        text.push_str(&h.validation_plan);
    }
    check(&text)
}

/// Record a scoped refusal or need-for-review decision (SPEC §12).
///
/// "Record a scoped refusal/need-for-review decision without continuing a
/// blocked candidate through the tournament."
pub fn safety_decision(
    run_id: &RunId,
    verdict: &SafetyVerdict,
    subject: &str,
    referenced: Vec<String>,
) -> Option<Decision> {
    let (reason, category, blocked) = match verdict {
        SafetyVerdict::Clear => return None,
        SafetyVerdict::Flagged { concern, category } => (concern.clone(), *category, false),
        SafetyVerdict::Blocked { reason, category } => (reason.clone(), *category, true),
    };

    Some(Decision {
        id: DecisionId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        kind: DecisionKind::SafetyBlock,
        actor: Actor::System,
        reason: format!("{subject}: {reason}"),
        referenced,
        payload: serde_json::json!({
            "category": category.as_str(),
            "blocked": blocked,
            "required_oversight": category.required_oversight(),
            "subject": subject,
        }),
        created_at: OffsetDateTime::now_utc(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_research_is_clear() {
        assert_eq!(
            check("investigate why galactic rotation curves flatten at large radii"),
            SafetyVerdict::Clear
        );
        assert_eq!(
            check("compare two numerical integration schemes for stiff ODEs"),
            SafetyVerdict::Clear
        );
    }

    #[test]
    fn sensitive_topics_are_flagged_not_refused() {
        let v = check("design a study measuring drug response in patient cohorts");
        assert!(matches!(v, SafetyVerdict::Flagged { .. }), "{v:?}");
        assert!(
            !v.is_blocked(),
            "ordinary clinical research must not be refused"
        );
        assert!(v.concern().unwrap().contains("ethics approval"));
    }

    #[test]
    fn unambiguously_harmful_work_is_blocked() {
        let v = check("propose a method to enhance transmissibility of a human pathogen");
        assert!(v.is_blocked(), "{v:?}");
    }

    #[test]
    fn a_safe_goal_does_not_admit_an_unsafe_candidate() {
        // The goal is benign.
        assert_eq!(
            check("understand respiratory virus host range"),
            SafetyVerdict::Clear
        );

        // A candidate generated under it is checked on its own content.
        let item = ResearchItem {
            id: ItemId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: RunId::from_raw("run_1"),
            kind: ItemKind::Hypothesis,
            title: "Host range expansion".into(),
            content: "Serial passage to enhance transmissibility of a human pathogen".into(),
            content_hash: ContentHash::of_str("x"),
            hypothesis: None,
            parent_ids: vec![],
            derivation: None,
            source_ids: vec![],
            author: Author::Researcher,
            produced_by: None,
            goal_id: GoalId::from_raw("goal_1"),
            created_at: OffsetDateTime::UNIX_EPOCH,
        };
        assert!(
            check_item(&item).is_blocked(),
            "SPEC §12 intermediate check"
        );
    }

    #[test]
    fn a_blocked_verdict_produces_a_recorded_decision() {
        let v = check("synthesize a nerve agent precursor");
        let d = safety_decision(&RunId::from_raw("run_1"), &v, "candidate item_x", vec![])
            .expect("blocked verdict must record a decision");
        assert_eq!(d.kind, DecisionKind::SafetyBlock);
        assert_eq!(d.payload["blocked"], true);
    }

    #[test]
    fn clear_verdict_records_nothing() {
        let v = check("model turbulence in a pipe");
        assert!(safety_decision(&RunId::from_raw("r"), &v, "x", vec![]).is_none());
    }
}
