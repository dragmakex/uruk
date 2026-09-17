//! Meta-review: synthesize recurring critiques and produce deliverables (§5).
//!
//! Feedback is linked to its supporting records and supplied to later tasks.
//! It changes subsequent context, not model weights, and creates no findings.

use super::context::{AgentContext, clip};
use super::outputs::{self, MetaReviewOutput, OverviewOutput, SynthesisOutput};
use crate::Result;
use crate::prompts::ids;
use crate::records::*;
use time::OffsetDateTime;

/// Synthesized feedback plus its cost.
#[derive(Debug, Clone)]
pub struct Synthesized {
    pub feedback: Feedback,
    pub cost: CostActual,
    /// Where applying this feedback universally would narrow future work.
    pub caution: String,
}

/// Synthesize recurring critiques from the run's reviews (2025 Fig. A.8).
pub async fn synthesize(
    ctx: &AgentContext,
    revision: u32,
    task_id: Option<TaskId>,
) -> Result<Synthesized> {
    let reviews = ctx.store.list_reviews(&ctx.run_id).await?;
    let matches = ctx.store.list_matches(&ctx.run_id).await?;

    let bindings = ctx
        .base_bindings_for(ids::META_REVIEW)
        .set("reviews", render_reviews_and_matches(&reviews, &matches))
        .set("failed_checks", render_failed_checks(ctx).await?);

    let completion = ctx.call(ids::META_REVIEW, bindings).await?;
    let output: MetaReviewOutput = outputs::parse(&completion.text)?;

    let critiques = output
        .critiques
        .iter()
        .map(|c| Critique {
            issue: c.issue.clone(),
            supporting_records: c.supporting_records.clone(),
            exceptions: c.exceptions.clone(),
            recommended_check: c.recommended_check.clone(),
            occurrences: c.occurrences.max(1),
        })
        .collect();

    let mut gaps = output.methodological_gaps.clone();
    gaps.extend(output.coverage_gaps.clone());

    let target_roles = output
        .critiques
        .iter()
        .flat_map(|c| c.target_roles.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    Ok(Synthesized {
        feedback: Feedback {
            id: FeedbackId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: ctx.run_id.clone(),
            revision,
            critiques,
            gaps,
            target_roles,
            produced_by: task_id,
            created_at: OffsetDateTime::now_utc(),
        },
        cost: completion.cost,
        caution: output.caution,
    })
}

/// A produced deliverable plus its cost.
#[derive(Debug, Clone)]
pub struct Deliverable {
    pub output: SynthesisOutput,
    pub cost: CostActual,
}

/// Produce the requested deliverable from recorded material (SPEC §11).
pub async fn produce_deliverable(ctx: &AgentContext, deliverable: &str) -> Result<Deliverable> {
    let bindings = ctx
        .base_bindings_for(ids::TASK_SYNTHESIS)
        .set("deliverable", deliverable)
        .set("source_coverage", ctx.source_coverage().await?)
        .set("articles_with_reasoning", ctx.render_sources(16).await?)
        .set("records", render_records(ctx).await?);

    let completion = ctx.call(ids::TASK_SYNTHESIS, bindings).await?;
    let output: SynthesisOutput = outputs::parse(&completion.text)?;
    output.validate()?;

    Ok(Deliverable {
        output,
        cost: completion.cost,
    })
}

/// A research overview plus its cost.
#[derive(Debug, Clone)]
pub struct Overview {
    pub output: OverviewOutput,
    pub cost: CostActual,
}

/// Produce the periodic research overview (SPEC §5 Meta-review).
///
/// This feeds back into Generation, not only into the final report.
pub async fn research_overview(ctx: &AgentContext) -> Result<Overview> {
    let summaries = ctx
        .store
        .item_summaries(&ctx.run_id, Some(&ctx.plan.id))
        .await?;
    let reviews = ctx.store.list_reviews(&ctx.run_id).await?;
    let feedback = ctx.store.latest_feedback(&ctx.run_id).await?;

    let items = summaries
        .iter()
        .map(|s| {
            format!(
                "- [{}] {} ({}, {}{})",
                s.id,
                s.title,
                s.kind.as_str(),
                s.assessment.as_str(),
                match s.rating {
                    Some(r) =>
                        format!(", Elo {r:.0}{}", if s.stale_rating { " STALE" } else { "" }),
                    None => String::new(),
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let bindings = ctx
        .base_bindings_for(ids::META_OVERVIEW)
        .set(
            "items",
            if items.is_empty() {
                "(none)".into()
            } else {
                items
            },
        )
        .set(
            "reviews",
            render_reviews_and_matches(&reviews, &ctx.store.list_matches(&ctx.run_id).await?),
        )
        .set(
            "feedback",
            match &feedback {
                Some(f) => f
                    .critiques
                    .iter()
                    .map(|c| format!("- {} (seen {}×)", c.issue, c.occurrences))
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => "(no meta-review feedback yet)".to_string(),
            },
        );

    let completion = ctx.call(ids::META_OVERVIEW, bindings).await?;
    let output: OverviewOutput = outputs::parse(&completion.text)?;

    Ok(Overview {
        output,
        cost: completion.cost,
    })
}

/// Render reviews and match critiques with their record IDs, so the synthesis
/// can cite provenance (SPEC §15.9 adaptation).
fn render_reviews_and_matches(reviews: &[Review], matches: &[Match]) -> String {
    if reviews.is_empty() && matches.is_empty() {
        return "(no reviews or comparisons yet)".to_string();
    }

    let mut out = String::new();
    for r in reviews {
        out.push_str(&format!(
            "[review {} | item {} | strategy {} | author {}]\n{}\n",
            r.id,
            r.item_id,
            r.strategy.as_str(),
            match &r.author {
                Author::Agent { role, strategy } => format!("{role}/{strategy}"),
                Author::Researcher => "researcher".into(),
                Author::Imported(s) => format!("imported from {s}"),
            },
            clip(&r.assessment_text, 800)
        ));
        for o in &r.objections {
            out.push_str(&format!(
                "  - objection{}: {}\n",
                if o.fatal { " (fatal)" } else { "" },
                o.description
            ));
        }
        out.push('\n');
    }

    for m in matches {
        out.push_str(&format!(
            "[match {} | {} vs {} | outcome {}]\n{}\n\n",
            m.id,
            m.item_a,
            m.item_b,
            m.outcome.as_str(),
            clip(&m.rationale, 600)
        ));
    }
    out
}

/// Failed checks and unavailable evidence, so the synthesis sees what did not
/// work rather than only what did (SPEC §12: preserve negative results).
async fn render_failed_checks(ctx: &AgentContext) -> Result<String> {
    let tasks = ctx.store.list_tasks(&ctx.run_id).await?;
    let experiments = ctx.store.list_experiments(&ctx.run_id).await?;

    let mut out = String::new();
    for t in tasks.iter().filter(|t| {
        matches!(
            t.state,
            TaskState::Failed | TaskState::Uncertain | TaskState::Cancelled
        )
    }) {
        out.push_str(&format!(
            "- task {} ({}/{}) {}: {}\n",
            t.id,
            t.role.as_str(),
            t.strategy,
            t.state.as_str(),
            t.error.as_deref().unwrap_or("no detail recorded")
        ));
    }

    for e in &experiments {
        if let Some(exec) = &e.execution
            && !exec.may_support_claims()
        {
            {
                out.push_str(&format!(
                    "- experiment {} did not produce usable evidence (exit {}, outputs {})\n",
                    e.id,
                    exec.exit_status,
                    if exec.outputs_valid {
                        "valid"
                    } else {
                        "invalid"
                    }
                ));
            }
        }
    }

    Ok(if out.is_empty() {
        "(no failed checks recorded)".to_string()
    } else {
        out
    })
}

/// Render recorded items, evidence, and reviews for a deliverable.
async fn render_records(ctx: &AgentContext) -> Result<String> {
    let items = ctx.store.list_items(&ctx.run_id).await?;
    let evidence = ctx.store.list_evidence(&ctx.run_id).await?;
    let reviews = ctx.store.list_reviews(&ctx.run_id).await?;

    let mut out = String::new();

    if !items.is_empty() {
        out.push_str("## Items\n\n");
        for item in &items {
            let assessment = ctx.store.current_assessment(&item.id).await?;
            let disposition = ctx.store.get_disposition(&item.id).await?;
            out.push_str(&format!(
                "[{}] {} ({}, assessment: {}, disposition: {})\n{}\n\n",
                item.id,
                item.title,
                item.kind.as_str(),
                assessment.as_str(),
                disposition.as_str(),
                clip(&item.content, 2_000)
            ));
        }
    }

    if !evidence.is_empty() {
        out.push_str("## Evidence\n\n");
        for e in &evidence {
            out.push_str(&format!(
                "[{}] ({}) {}\n  method: {}\n  limitations: {}\n",
                e.id,
                e.kind.as_str(),
                clip(&e.content, 600),
                e.method,
                e.limitations.as_deref().unwrap_or("none recorded")
            ));
            for link in ctx
                .store
                .links_for_evidence(&e.id)
                .await
                .unwrap_or_default()
            {
                out.push_str(&format!(
                    "  {} claim {:?}: {}\n",
                    link.relation.as_str(),
                    clip(&link.claim, 120),
                    link.justification
                ));
            }
            out.push('\n');
        }
    }

    if !reviews.is_empty() {
        out.push_str("## Reviews\n\n");
        for r in &reviews {
            out.push_str(&format!(
                "[{}] item {} ({}): {}\n\n",
                r.id,
                r.item_id,
                r.strategy.as_str(),
                clip(&r.assessment_text, 600)
            ));
        }
    }

    Ok(if out.is_empty() {
        "(no records yet)".to_string()
    } else {
        out
    })
}
