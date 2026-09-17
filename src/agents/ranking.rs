//! Ranking: compare competing candidates (SPEC §7).
//!
//! Judges see the same evidence snapshot, substantive reviews without their
//! numeric scores, and no ratings. Presentation order is randomized and mapped
//! back to stable item IDs.

use super::context::AgentContext;
use super::outputs::{self, DebateStatus, JudgedOutcome, RankingDebateTurn, RankingOutput};
use super::reflection::render_item;
use crate::prompts::ids;
use crate::records::*;
use crate::{Error, Result};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use time::OffsetDateTime;

/// A judged comparison, ready for the controlled write path.
#[derive(Debug, Clone)]
pub struct Judged {
    pub r#match: Match,
    pub cost: CostActual,
    /// A stable key making duplicate delivery harmless (SPEC §9.2).
    pub dedupe_key: String,
    /// What evidence would settle an insufficient-basis outcome.
    pub missing_to_decide: Vec<String>,
    pub transcript: Option<String>,
}

/// Compare two candidates under the plan's rubric.
///
/// `first_presented`/`second_presented` are chosen by a seeded shuffle, so the
/// same task on resume presents the same order (SPEC §6).
pub async fn compare(
    ctx: &AgentContext,
    a: &ResearchItem,
    b: &ResearchItem,
    method: MatchMethod,
    max_turns: u32,
    task_id: Option<TaskId>,
) -> Result<Judged> {
    if a.id == b.id {
        return Err(Error::validation("cannot compare an item with itself"));
    }

    // Randomize presentation order, seeded so it is reproducible on resume.
    let mut rng = ChaCha8Rng::seed_from_u64(ctx.seed);
    let swap = rng.gen_bool(0.5);
    let (first, second) = if swap { (b, a) } else { (a, b) };

    let review_first = judge_reviews(ctx, first).await?;
    let review_second = judge_reviews(ctx, second).await?;
    let snapshot = evidence_snapshot(ctx, first, second).await?;

    let (outcome, rationale, missing, cost, transcript) = match method {
        MatchMethod::Pairwise => {
            let bindings = ctx
                .base_bindings_for(ids::RANKING_PAIRWISE)
                .set("hypothesis 1", render_item(first))
                .set("hypothesis 2", render_item(second))
                .set("review 1", review_first)
                .set("review 2", review_second);

            let completion = ctx.call(ids::RANKING_PAIRWISE, bindings).await?;
            let out: RankingOutput = outputs::parse(&completion.text)?;
            out.validate()?;
            (
                out.outcome,
                out.rationale,
                out.missing_to_decide,
                completion.cost,
                None,
            )
        }
        MatchMethod::Debate => {
            let result =
                debate(ctx, first, second, &review_first, &review_second, max_turns).await?;
            (
                result.outcome,
                result.rationale,
                result.missing_to_decide,
                result.cost,
                Some(result.transcript),
            )
        }
    };

    // Map the judge's positional verdict back to stable item IDs.
    let resolved = resolve_outcome(outcome);

    let dedupe_key = format!(
        "match:{}:{}:{}:{}",
        ctx.plan.id,
        min_id(&a.id, &b.id),
        max_id(&a.id, &b.id),
        task_id.as_ref().map(|t| t.0.as_str()).unwrap_or("adhoc")
    );

    Ok(Judged {
        r#match: Match {
            id: MatchId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: ctx.run_id.clone(),
            // item_a / item_b record presentation order, as shown to the judge.
            item_a: first.id.clone(),
            item_b: second.id.clone(),
            rubric_id: ctx.plan.id.clone(),
            evidence_snapshot: snapshot,
            order_randomized: swap,
            method,
            judge_model: ctx.model(),
            outcome: resolved,
            rationale,
            rating_a_before: 0.0,
            rating_b_before: 0.0,
            rating_a_after: 0.0,
            rating_b_after: 0.0,
            produced_by: task_id,
            created_at: OffsetDateTime::now_utc(),
        },
        cost,
        dedupe_key,
        missing_to_decide: missing,
        transcript,
    })
}

/// Translate the judge's positional verdict into a presentation-order outcome.
///
/// The judge names "1" or "2" by position. `item_a` in the stored match is
/// whatever was presented first, so a positional win maps directly to a
/// stable item ID.
fn resolve_outcome(outcome: JudgedOutcome) -> MatchOutcome {
    match outcome {
        JudgedOutcome::Win1 => MatchOutcome::WinA,
        JudgedOutcome::Win2 => MatchOutcome::WinB,
        JudgedOutcome::Draw => MatchOutcome::Draw,
        JudgedOutcome::InsufficientBasis => MatchOutcome::InsufficientBasis,
    }
}

struct DebateOutcome {
    outcome: JudgedOutcome,
    rationale: String,
    missing_to_decide: Vec<String>,
    cost: CostActual,
    transcript: String,
}

/// Multi-turn comparison debate (2025 Fig. A.5).
async fn debate(
    ctx: &AgentContext,
    first: &ResearchItem,
    second: &ResearchItem,
    review_first: &str,
    review_second: &str,
    max_turns: u32,
) -> Result<DebateOutcome> {
    let cap = max_turns.clamp(1, super::generation::HARD_TURN_CAP);
    let mut transcript = String::new();
    let mut cost = CostActual::default();
    let mut turn = 1;

    while turn <= cap {
        let bindings = ctx
            .base_bindings_for(ids::RANKING_DEBATE)
            .set("hypothesis 1", render_item(first))
            .set("hypothesis 2", render_item(second))
            .set("review 1", review_first)
            .set("review 2", review_second)
            .set("turn", turn.to_string())
            .set("max_turns", cap.to_string())
            .set("turns_remaining", (cap - turn).to_string())
            .set(
                "transcript",
                if transcript.is_empty() {
                    "(the discussion has not started)".to_string()
                } else {
                    transcript.clone()
                },
            );

        let completion = ctx.call(ids::RANKING_DEBATE, bindings).await?;
        cost.add(&completion.cost);

        let parsed: RankingDebateTurn = outputs::parse(&completion.text)?;
        parsed.validate()?;

        transcript.push_str(&format!("\n--- Turn {turn} ---\n{}\n", parsed.contribution));

        if parsed.status == DebateStatus::Final {
            return Ok(DebateOutcome {
                outcome: parsed.outcome.expect("validate() guarantees an outcome"),
                rationale: parsed.rationale,
                missing_to_decide: parsed.missing_to_decide,
                cost,
                transcript,
            });
        }
        if parsed.status == DebateStatus::Partial {
            break;
        }
        turn += 1;
    }

    // Budget exhausted with no defensible conclusion (SPEC §6, §7 step 5).
    Ok(DebateOutcome {
        outcome: JudgedOutcome::InsufficientBasis,
        rationale: format!(
            "the debate did not reach a defensible conclusion within its {cap}-turn budget"
        ),
        missing_to_decide: vec![
            "the comparison needs more turns or more evidence to be settled".to_string(),
        ],
        cost,
        transcript,
    })
}

/// Render an item's reviews for a judge: substance without numeric scores.
///
/// SPEC §7 step 2: "omit independent reviewers' numeric scores from the
/// judge's input because they may not be comparable; retain their evidence and
/// substantive criticisms." Elo and credits are likewise absent.
async fn judge_reviews(ctx: &AgentContext, item: &ResearchItem) -> Result<String> {
    let reviews = ctx.store.list_reviews_for_item(&item.id).await?;
    if reviews.is_empty() {
        return Ok("(no review available for this candidate)".to_string());
    }

    let mut out = String::new();
    for review in &reviews {
        let view = review.judge_view();
        out.push_str(&format!(
            "[{} review]\n{}\n",
            view.strategy.as_str(),
            view.assessment_text
        ));
        if !view.objections.is_empty() {
            out.push_str("Objections:\n");
            for o in &view.objections {
                out.push_str(&format!("- {o}\n"));
            }
        }
        if !view.unknowns.is_empty() {
            out.push_str("Unresolved:\n");
            for u in &view.unknowns {
                out.push_str(&format!("- {u}\n"));
            }
        }
        out.push_str(&format!(
            "Evidence items considered: {}\n\n",
            view.evidence_count
        ));
    }
    Ok(out)
}

/// A hash of the evidence both sides were shown (SPEC §7 step 2).
async fn evidence_snapshot(
    ctx: &AgentContext,
    a: &ResearchItem,
    b: &ResearchItem,
) -> Result<ContentHash> {
    let mut parts = Vec::new();
    for item in [a, b] {
        let links = ctx.store.list_evidence_links(&item.id).await?;
        for l in links {
            parts.push(format!(
                "{}:{}:{}",
                item.id,
                l.evidence_id,
                l.relation.as_str()
            ));
        }
    }
    parts.sort();
    Ok(ContentHash::of_str(&parts.join("|")))
}

fn min_id<'a>(a: &'a ItemId, b: &'a ItemId) -> &'a str {
    if a.0 <= b.0 { a.as_str() } else { b.as_str() }
}

fn max_id<'a>(a: &'a ItemId, b: &'a ItemId) -> &'a str {
    if a.0 > b.0 { a.as_str() } else { b.as_str() }
}
