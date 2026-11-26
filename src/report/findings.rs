//! Explain the leading ideas without exposing the agent's record schema.

use super::reading;
use crate::Result;
use crate::records::*;
use crate::store::{ItemSummary, Store};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Finding {
    summary: ItemSummary,
    item: ResearchItem,
    assessment_record: Option<AssessmentRecord>,
    evidence: Vec<(Evidence, EvidenceLink)>,
    reviews: Vec<Review>,
    sources: Vec<Source>,
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
    let sources: BTreeMap<_, _> = store
        .list_sources(run_id)
        .await?
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();
    let count = summaries.len();
    let mut findings = Vec::new();
    for summary in summaries.into_iter().take(5) {
        let assessment_record = store.current_assessment_record(&summary.id).await?;
        // The assessment's interpretation can differ from the historical link.
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
        let mut finding = Finding {
            item: store.get_item(&summary.id).await?,
            assessment_record,
            evidence,
            reviews: store.list_reviews_for_item(&summary.id).await?,
            summary,
            sources: Vec::new(),
        };
        finding.sources = linked_sources(&finding, &sources);
        findings.push(finding);
    }
    Ok((count, findings))
}

fn linked_sources(finding: &Finding, sources: &BTreeMap<SourceId, Source>) -> Vec<Source> {
    let evidence_sources = finding.evidence.iter().flat_map(|(e, _)| &e.source_ids);
    let reading_sources = finding
        .item
        .hypothesis
        .iter()
        .flat_map(|h| &h.reading)
        .map(|n| &n.source_id);
    let observation_sources = finding
        .reviews
        .iter()
        .flat_map(|r| &r.observations)
        .map(|o| &o.source_id);
    let mut seen = BTreeSet::new();
    let mut linked: Vec<_> = evidence_sources
        .chain(reading_sources)
        .chain(&finding.item.source_ids)
        .chain(observation_sources)
        .filter(|id| seen.insert((*id).clone()))
        .filter_map(|id| sources.get(id).cloned())
        .collect();
    // Give the reader something readable first, preserving relevance order
    // within each access level. Never attach unrelated run-wide sources.
    linked.sort_by_key(reading::access_priority);
    linked
}

pub(super) fn render(count: usize, findings: &[Finding]) -> String {
    let mut out = String::from("### Top 5 ideas by Elo\n\n");
    out.push_str("The highest-ranked active ideas come first. Elo is their comparison score in this run, not a probability that an idea is correct. These are ideas to explore, not five confirmed discoveries.\n\n");
    if count < 5 {
        out.push_str(&format!("Only {count} active candidate(s) were recorded; no additional findings have been invented.\n\n"));
    }
    for (index, finding) in findings.iter().enumerate() {
        out.push_str(&format!("#### {}. {}\n\n", index + 1, finding.item.title));
        match finding.summary.rating {
            Some(rating) => {
                out.push_str(&format!("**Elo {rating:.0}** · {} recorded comparison(s).\n\n", finding.summary.matches_played));
                if finding.summary.stale_rating {
                    out.push_str("**STALE ranking:** newer evidence has not yet been reflected in this score.\n\n");
                }
            }
            None => out.push_str("**Not rated yet.** Shown after rated ideas; its position is not a measured advantage.\n\n"),
        }
        render_idea(&mut out, finding);
        render_practical_steps(&mut out, finding);
        render_reading(&mut out, finding);
        render_known_and_unknown(&mut out, finding);
    }
    out
}

fn render_idea(out: &mut String, finding: &Finding) {
    out.push_str("**What the idea is**\n\n");
    match &finding.item.hypothesis {
        Some(h) => {
            out.push_str(&format!("{}\n\n**How it works**\n\n{}\n\n", h.claim, h.mechanism));
            if !h.scope.trim().is_empty() {
                out.push_str(&format!("**Where it would apply:** {}\n\n", h.scope));
            }
            if !h.predictions.is_empty() {
                out.push_str("**What you would expect to see** (proposed, not observed here)\n\n");
                for prediction in &h.predictions {
                    out.push_str(&format!("- {prediction}\n"));
                }
                out.push('\n');
            }
            // Generated content repeats these fields with technical headings.
            // Keep independently authored prose, but never dump that duplicate.
            if !finding.item.content.contains("\n## Claim\n") {
                out.push_str(&format!("**More about the idea**\n\n{}\n\n", finding.item.content));
            }
        }
        None => out.push_str(&format!("{}\n\n**How it works**\n\nNo separate explanation of how this idea works was recorded.\n\n", finding.item.content)),
    }
}

fn render_practical_steps(out: &mut String, finding: &Finding) {
    out.push_str("**What trying it would involve**\n\n");
    if let Some(h) = &finding.item.hypothesis {
        if h.validation_plan.trim().is_empty() {
            out.push_str("No practical plan was recorded for this idea.\n\n");
        } else {
            out.push_str(&format!(
                "Proposed approach, not a completed experiment:\n\n{}\n\n",
                h.validation_plan
            ));
        }
        if !h.assumptions.is_empty() {
            out.push_str("For this to work, the following would need to hold:\n\n");
            for assumption in &h.assumptions {
                out.push_str(&format!("- {assumption}\n"));
            }
            out.push('\n');
        }
    } else {
        out.push_str("No practical plan was recorded for this idea.\n\n");
    }
    let mut seen = BTreeSet::new();
    let actions = finding.reviews.iter().flat_map(|r| {
        r.next_actions.iter().chain(
            r.objections
                .iter()
                .filter_map(|o| o.suggested_check.as_ref()),
        )
    });
    for action in actions.filter(|a| !a.trim().is_empty() && seen.insert(a.as_str())) {
        out.push_str(&format!("- Suggested follow-up: {action}\n"));
    }
    if !seen.is_empty() {
        out.push('\n');
    }
}

fn render_reading(out: &mut String, finding: &Finding) {
    out.push_str("**What to read**\n\n");
    if finding.sources.is_empty() {
        out.push_str("No readable source reference was recorded for this idea. No reading list has been invented.\n\n");
        return;
    }
    for (index, source) in finding.sources.iter().enumerate() {
        let start = if index == 0 && reading::access_priority(source) < 2 {
            "Start here: "
        } else {
            ""
        };
        out.push_str(&format!(
            "{}. **{start}{}**\n",
            index + 1,
            reading::title(source)
        ));
        let mut notes = BTreeSet::new();
        for note in finding
            .item
            .hypothesis
            .iter()
            .flat_map(|h| &h.reading)
            .filter(|n| n.source_id == source.id)
        {
            if !note.reason.trim().is_empty() {
                notes.insert(format!("Read for: {}", note.reason));
            }
            if !note.locator.trim().is_empty() {
                notes.insert(format!("Where to look: {}", note.locator));
            }
        }
        for (evidence, link) in &finding.evidence {
            if evidence.source_ids.contains(&source.id) {
                notes.insert(format!("Linked research note: {}", link.justification));
                notes.insert(format!("How the source was used: {}", evidence.method));
            }
        }
        for observation in finding
            .reviews
            .iter()
            .flat_map(|r| &r.observations)
            .filter(|o| o.source_id == source.id)
        {
            notes.insert(format!("Where to look: {}", observation.locator.render()));
            notes.insert(format!("Recorded observation: {}", observation.observation));
        }
        if notes.is_empty() {
            out.push_str(
                "   - Linked to this idea. No explanation of its relevance was recorded.\n",
            );
        } else {
            for note in notes {
                out.push_str(&format!("   - {note}\n"));
            }
        }
        reading::details(out, source);
        out.push('\n');
    }
}

fn render_known_and_unknown(out: &mut String, finding: &Finding) {
    out.push_str("**What we know so far**\n\n");
    let status = match finding.summary.assessment {
        Assessment::Untested => "Not tested yet.",
        Assessment::Plausible => "Promising after review, but not experimentally established.",
        Assessment::Supported => "Evidence supports this idea within its stated limits.",
        Assessment::Contested => "The evidence is mixed; the disagreement is unresolved.",
        Assessment::Refuted => "Evidence goes against this idea or something it depends on.",
        Assessment::Inconclusive => "The available checks did not settle whether this works.",
    };
    out.push_str(&format!("{status}\n\n"));
    if let Some(record) = &finding.assessment_record {
        out.push_str(&format!("{}\n\n", record.rationale));
        if record.evidence.is_empty() {
            out.push_str("This assessment does not cite evidence; reasoning alone does not show that the idea works.\n\n");
        }
    }
    for (evidence, link) in &finding.evidence {
        let current = finding
            .assessment_record
            .as_ref()
            .is_some_and(|r| r.evidence.iter().any(|l| same_link(l, link)));
        let relation = match link.relation {
            EvidenceRelation::Supports => "Supports this idea",
            EvidenceRelation::Contradicts => "Goes against this idea",
            EvidenceRelation::Inconclusive => "Does not settle the question",
            EvidenceRelation::Context => "Background information",
        };
        let role = if current {
            "used in the current assessment"
        } else {
            "Background only; not used in the current assessment"
        };
        out.push_str(&format!(
            "- **{relation}** ({role}): {}\n  - What was found: {}\n  - How it was obtained: {}\n",
            link.justification, evidence.content, evidence.method
        ));
        if evidence.kind == EvidenceKind::AgentCritique {
            out.push_str("  - This is an agent's reasoning, not an independent observation.\n");
        }
        for limit in [&link.limits, &evidence.limitations].into_iter().flatten() {
            out.push_str(&format!("  - What this does not establish: {limit}\n"));
        }
        for source in &finding.sources {
            if evidence.source_ids.contains(&source.id) {
                out.push_str(&format!("  - Read more: {}\n", reading::title(source)));
            }
        }
    }
    if !finding.evidence.is_empty() {
        out.push('\n');
    }
    let mut concerns = BTreeSet::new();
    if let Some(h) = &finding.item.hypothesis {
        let limits = if h.limitations.trim().is_empty() {
            // Older records stored this note only in their generated prose.
            finding
                .item
                .content
                .split("\n## Limitations\n")
                .nth(1)
                .map(|s| s.split("\n## ").next().unwrap_or(s))
                .unwrap_or("")
                .trim()
        } else {
            h.limitations.trim()
        };
        if !limits.is_empty() {
            concerns.insert(limits.to_owned());
        }
    }
    for review in &finding.reviews {
        for objection in &review.objections {
            concerns.insert(objection.description.clone());
        }
        concerns.extend(review.unknowns.iter().cloned());
    }
    if !concerns.is_empty() {
        out.push_str("**What still needs to be understood**\n\n");
        for concern in concerns {
            out.push_str(&format!("- {concern}\n"));
        }
        out.push('\n');
    }
}

fn same_link(a: &EvidenceLink, b: &EvidenceLink) -> bool {
    a.evidence_id == b.evidence_id
        && a.item_id == b.item_id
        && a.claim == b.claim
        && a.relation == b.relation
        && a.justification == b.justification
        && a.limits == b.limits
}
