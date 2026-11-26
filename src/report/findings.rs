//! A readable explanation of the same active candidates shown on the live run.

use crate::Result;
use crate::records::*;
use crate::store::{ItemSummary, Store};

pub(super) struct Finding {
    summary: ItemSummary,
    item: ResearchItem,
    assessment_record: Option<AssessmentRecord>,
    evidence: Vec<(Evidence, EvidenceLink)>,
    reviews: Vec<Review>,
}

impl Finding {
    pub(super) fn id(&self) -> &ItemId {
        &self.item.id
    }
}

/// Match the live ranking: rated first, descending Elo, ties by immutable ID.
/// The count is before truncation; fewer than five is a recorded outcome.
pub(super) async fn collect(
    store: &Store,
    run_id: &RunId,
    plan: Option<&Plan>,
) -> Result<(usize, Vec<Finding>)> {
    let mut summaries: Vec<_> = store
        .item_summaries(run_id, plan.map(|p| &p.id))
        .await?
        .into_iter()
        .filter(|s| s.kind.is_rankable() && s.disposition == "active")
        .collect();
    summaries.sort_by(|a, b| match (a.rating, b.rating) {
        (Some(x), Some(y)) => y.total_cmp(&x).then_with(|| a.id.cmp(&b.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.id.cmp(&b.id),
    });
    let count = summaries.len();
    let mut findings = Vec::new();
    for summary in summaries.into_iter().take(5) {
        let assessment_record = store.current_assessment_record(&summary.id).await?;
        // An assessment may interpret an original context link without
        // mutating that link. Explain its actual recorded basis first.
        let mut evidence = Vec::new();
        if let Some(record) = &assessment_record {
            for link in &record.evidence {
                evidence.push((store.get_evidence(&link.evidence_id).await?, link.clone()));
            }
        }
        for (record, link) in store.evidence_for_item(&summary.id).await? {
            if !evidence
                .iter()
                .any(|(_, current)| same_link(current, &link))
            {
                evidence.push((record, link));
            }
        }
        findings.push(Finding {
            item: store.get_item(&summary.id).await?,
            assessment_record,
            evidence,
            reviews: store.list_reviews_for_item(&summary.id).await?,
            summary,
        });
    }
    Ok((count, findings))
}

pub(super) fn render(count: usize, findings: &[Finding]) -> String {
    let mut out = String::from("### Top 5 findings\n\n");
    out.push_str("These are the leading active candidates, not five confirmed discoveries. Order follows current-rubric Elo, with unrated candidates last and ties broken by item ID. Elo is relative prioritisation, not a probability or scientific confidence. An unrated position implies no measured advantage.\n\n");
    if count < 5 {
        out.push_str(&format!("Only {count} active candidate(s) were recorded; no additional findings have been invented.\n\n"));
    }
    for (index, finding) in findings.iter().enumerate() {
        let item = &finding.item;
        let summary = &finding.summary;
        out.push_str(&format!("#### {}. {}\n\n", index + 1, item.title));
        out.push_str(&format!(
            "Record `{}`. **Current status: {}.**\n\n",
            item.id,
            summary.assessment.as_str()
        ));
        match summary.rating {
            Some(rating) => out.push_str(&format!("**Why this position:** Elo {rating:.0} across {} recorded comparison(s){}. This is not an evidence tier.\n\n", summary.matches_played, if summary.stale_rating { " (stale: newer evidence has not been reflected)" } else { "" })),
            None => out.push_str("**Why this position:** no rating was recorded; this candidate follows rated items in stable ID order.\n\n"),
        }
        out.push_str(&format!("**Recorded explanation**\n\n{}\n\n", item.content));
        if let Some(h) = &item.hypothesis {
            for (label, value) in [
                ("Claim", &h.claim),
                ("Proposed mechanism", &h.mechanism),
                ("Scope", &h.scope),
            ] {
                if !value.trim().is_empty() {
                    out.push_str(&format!("- **{label}:** {value}\n"));
                }
            }
            for (label, values) in [
                ("Assumptions", &h.assumptions),
                ("Predictions", &h.predictions),
                ("Falsifiers", &h.falsifiers),
            ] {
                if !values.is_empty() {
                    out.push_str(&format!("- **{label}:** {}\n", values.join("; ")));
                }
            }
            out.push('\n');
        }
        render_evidence(&mut out, finding);
        render_reviews(&mut out, finding);
        out.push_str("**Next test**\n\n");
        if let Some(h) = &item.hypothesis
            && !h.validation_plan.trim().is_empty()
        {
            out.push_str(&format!(
                "Recorded validation plan (not an executed result): {}\n\n",
                h.validation_plan
            ));
        } else {
            out.push_str("No validation plan was recorded for this candidate.\n\n");
        }
        let actions: Vec<_> = finding
            .reviews
            .iter()
            .flat_map(|r| &r.next_actions)
            .collect();
        if actions.is_empty() {
            out.push_str("No review follow-up was recorded.\n\n");
        } else {
            for action in actions {
                out.push_str(&format!("- {action}\n"));
            }
            out.push('\n');
        }
    }
    out
}

fn render_evidence(out: &mut String, finding: &Finding) {
    out.push_str("**Evidence and limits**\n\n");
    if let Some(record) = &finding.assessment_record {
        out.push_str(&format!(
            "Current assessment rationale: {}\n\n",
            record.rationale
        ));
        if let Some(id) = &record.basis_review {
            out.push_str(&format!("Basis review: `{id}`.\n\n"));
        }
        if let Some(id) = &record.basis_decision {
            out.push_str(&format!("Basis decision: `{id}`.\n\n"));
        }
        if record.evidence.is_empty() {
            out.push_str("The current assessment cites no evidence links. Agent reasoning alone does not establish support.\n\n");
        }
    } else {
        out.push_str("No assessment has been recorded; the candidate remains untested.\n\n");
    }
    if finding.evidence.is_empty() {
        out.push_str("No evidence is linked to this candidate.\n\n");
    }
    for (evidence, link) in &finding.evidence {
        let current = finding
            .assessment_record
            .as_ref()
            .is_some_and(|r| r.evidence.iter().any(|l| same_link(l, link)));
        out.push_str(&format!(
            "- `{}` ({}, {}): **{}** the claim \"{}\". {}\n  - Observation: {}\n  - Method: {}\n",
            evidence.id,
            evidence.kind.as_str(),
            if current {
                "cited by the current assessment"
            } else {
                "recorded context, not a basis of the current assessment"
            },
            link.relation.as_str(),
            link.claim,
            link.justification,
            evidence.content,
            evidence.method
        ));
        if let Some(limits) = &link.limits {
            out.push_str(&format!("  - Claim limits: {limits}\n"));
        }
        if let Some(limits) = &evidence.limitations {
            out.push_str(&format!("  - Evidence limits: {limits}\n"));
        }
        for id in &evidence.source_ids {
            out.push_str(&format!(
                "  - Source: `{id}` (see Sources for access and citation)\n"
            ));
        }
    }
    if !finding.item.source_ids.is_empty() {
        out.push_str(&format!(
            "\nCandidate source references (not evidence by themselves): {}.\n",
            finding
                .item
                .source_ids
                .iter()
                .map(|id| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push('\n');
}

fn same_link(a: &EvidenceLink, b: &EvidenceLink) -> bool {
    a.evidence_id == b.evidence_id
        && a.item_id == b.item_id
        && a.claim == b.claim
        && a.relation == b.relation
        && a.justification == b.justification
        && a.limits == b.limits
}

fn render_reviews(out: &mut String, finding: &Finding) {
    out.push_str("**Review findings and objections**\n\n");
    if finding.reviews.is_empty() {
        out.push_str("No review was recorded for this candidate.\n\n");
        return;
    }
    out.push_str("Reviews are attributed reasoning, not independent empirical validation. Their proposed status does not replace the current recorded assessment.\n\n");
    for review in &finding.reviews {
        out.push_str(&format!(
            "- `{}` ({}): {}\n",
            review.id,
            review.strategy.as_str(),
            review.assessment_text
        ));
        for objection in &review.objections {
            out.push_str(&format!(
                "  - {} objection: {}\n",
                if objection.fatal {
                    "Fatal-premise"
                } else {
                    "Repairable"
                },
                objection.description
            ));
            if let Some(check) = &objection.suggested_check {
                out.push_str(&format!("    - Suggested check: {check}\n"));
            }
        }
        for unknown in &review.unknowns {
            out.push_str(&format!("  - Unresolved: {unknown}\n"));
        }
    }
    out.push('\n');
}
