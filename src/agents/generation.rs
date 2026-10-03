//! Generation: produce candidate items (SPEC §5).
//!
//! Strategies from the paper: literature exploration, simulated scientific
//! debate. Suggestions are grounded in supplied sources or approved retrieval;
//! when sources are unavailable the result is explicitly provisional.

use super::context::AgentContext;
use super::outputs::{self, DebateStatus, DebateTurn, GeneratedHypothesis};
use crate::prompts::ids;
use crate::records::*;
use crate::{Error, Result};
use time::OffsetDateTime;

/// A proposed item plus what producing it cost.
#[derive(Debug, Clone)]
pub struct Generated {
    pub item: ResearchItem,
    pub cost: CostActual,
    /// Transcript of a debate, when one was held. Persisted as an artifact:
    /// debate contributions are artifacts, not Evidence (SPEC §15.1).
    pub transcript: Option<String>,
    /// True when the model reported insufficient grounding.
    pub provisional: bool,
}

/// Generate one hypothesis from the supplied literature (2025 Fig. A.1).
pub async fn from_literature(
    ctx: &AgentContext,
    source_hypothesis: Option<&ResearchItem>,
    task_id: Option<TaskId>,
) -> Result<Generated> {
    let bindings = ctx
        .base_bindings_for(ids::GENERATION_LITERATURE)
        .set("source_coverage", ctx.source_coverage().await?)
        // Passage-grounded when the run has an index; whole sources otherwise.
        .set(
            "articles_with_reasoning",
            ctx.render_grounding(&ctx.goal.question, 12).await?,
        )
        .set_optional(
            "source_hypothesis",
            source_hypothesis.map(|i| format!("{}\n\n{}", i.title, i.content)),
        );

    let completion = ctx.call(ids::GENERATION_LITERATURE, bindings).await?;
    let generated: GeneratedHypothesis = outputs::parse(&completion.text)?;
    generated.validate()?;

    let item = build_item(
        ctx,
        &generated,
        source_hypothesis
            .map(|i| i.id.clone())
            .into_iter()
            .collect(),
        None,
        "literature",
        task_id,
    );

    Ok(Generated {
        provisional: generated.provisional || no_sources(ctx).await?,
        item,
        cost: completion.cost,
        transcript: None,
    })
}

/// Generate through simulated scientific debate (2025 Fig. A.2).
///
/// The turn cap is enforced here in Rust, not by the prompt: a debate that has
/// not converged returns a partial result rather than starting an unbudgeted
/// extra call (SPEC §6).
pub async fn through_debate(
    ctx: &AgentContext,
    max_turns: u32,
    initial_count: u32,
    task_id: Option<TaskId>,
) -> Result<DebateResult> {
    // The published prompts target 3-5 turns with a hard cap of 10.
    let cap = max_turns.clamp(1, HARD_TURN_CAP);
    let reviews_overview = render_reviews_overview(ctx).await?;

    let mut transcript = String::new();
    let mut cost = CostActual::default();
    let mut turn = 1;

    while turn <= cap {
        let bindings = ctx
            .base_bindings_for(ids::GENERATION_DEBATE)
            .set("reviews_overview", reviews_overview.clone())
            .set("initial_count", initial_count.to_string())
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

        let completion = ctx.call(ids::GENERATION_DEBATE, bindings).await?;
        cost.add(&completion.cost);

        let parsed: DebateTurn = outputs::parse(&completion.text)?;
        parsed.validate()?;

        transcript.push_str(&format!("\n--- Turn {turn} ---\n{}\n", parsed.contribution));

        match parsed.status {
            DebateStatus::Final => {
                let generated = parsed
                    .hypothesis
                    .expect("validate() guarantees a hypothesis on final");
                let item = build_item(ctx, &generated, vec![], None, "debate", task_id.clone());
                return Ok(DebateResult::Converged(Box::new(Generated {
                    provisional: generated.provisional || no_sources(ctx).await?,
                    item,
                    cost,
                    transcript: Some(transcript),
                })));
            }
            DebateStatus::Partial => {
                return Ok(DebateResult::Incomplete {
                    transcript,
                    unresolved: parsed.unresolved,
                    turns_used: turn,
                    cost,
                });
            }
            DebateStatus::Continue => turn += 1,
        }
    }

    // The cap is reached without convergence. SPEC §6: "Do not start an
    // unbudgeted eleventh call just to force a conclusion."
    Ok(DebateResult::Incomplete {
        transcript,
        unresolved: vec![format!(
            "the discussion did not converge within its {cap}-turn budget"
        )],
        turns_used: cap,
        cost,
    })
}

/// Hard ceiling on debate turns from the published prompts (SPEC §6).
pub const HARD_TURN_CAP: u32 = 10;

/// How a debate ended.
#[derive(Debug, Clone)]
pub enum DebateResult {
    /// The panel converged on a hypothesis. Boxed: a converged result is much
    /// larger than an incomplete one, and this enum is returned by value.
    Converged(Box<Generated>),
    /// The budget ran out, or the panel reported partial progress.
    Incomplete {
        transcript: String,
        unresolved: Vec<String>,
        turns_used: u32,
        cost: CostActual,
    },
}

impl DebateResult {
    pub fn cost(&self) -> &CostActual {
        match self {
            Self::Converged(g) => &g.cost,
            Self::Incomplete { cost, .. } => cost,
        }
    }

    pub fn transcript(&self) -> &str {
        match self {
            Self::Converged(g) => g.transcript.as_deref().unwrap_or(""),
            Self::Incomplete { transcript, .. } => transcript,
        }
    }
}

/// Build an immutable item from a model's proposal.
///
/// The runtime assigns the ID; the model proposes content only (SPEC §15.1).
pub(crate) fn build_item(
    ctx: &AgentContext,
    generated: &GeneratedHypothesis,
    parent_ids: Vec<ItemId>,
    derivation: Option<String>,
    strategy: &str,
    task_id: Option<TaskId>,
) -> ResearchItem {
    let content = generated.to_content();
    let source_ids = generated
        .grounding
        .iter()
        .map(|g| SourceId::from_raw(g.source_id.clone()))
        .collect();

    ResearchItem {
        id: ItemId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: ctx.run_id.clone(),
        kind: ItemKind::Hypothesis,
        title: generated.title.clone(),
        content_hash: ContentHash::of_str(&content),
        content,
        hypothesis: Some(generated.to_fields()),
        parent_ids,
        derivation,
        source_ids,
        author: Author::Agent {
            role: Role::Generation.as_str().to_string(),
            strategy: strategy.to_string(),
        },
        produced_by: task_id,
        goal_id: ctx.goal.id.clone(),
        created_at: OffsetDateTime::now_utc(),
    }
}

async fn no_sources(ctx: &AgentContext) -> Result<bool> {
    Ok(ctx.store.list_sources(&ctx.run_id).await?.is_empty())
}

/// Summarise existing reviews for `{reviews_overview}`.
async fn render_reviews_overview(ctx: &AgentContext) -> Result<String> {
    let reviews = ctx.store.list_reviews(&ctx.run_id).await?;
    if reviews.is_empty() {
        return Ok("(no reviews yet)".to_string());
    }

    let mut out = String::new();
    for r in reviews.iter().rev().take(10) {
        out.push_str(&format!(
            "- [{}] {}\n",
            r.strategy.as_str(),
            super::context::clip(&r.assessment_text, 400)
        ));
        for o in r.objections.iter().take(3) {
            out.push_str(&format!(
                "    - objection{}: {}\n",
                if o.fatal { " (fatal)" } else { "" },
                o.description
            ));
        }
    }
    Ok(out)
}

/// Turn a researcher-supplied idea into an item (SPEC §2).
///
/// "A campaign can begin from researcher-supplied ideas rather than Generation."
/// A free-text idea has no structured hypothesis fields, so it is recorded as
/// a `Proposal` unless the caller says otherwise (SPEC §4.2: do not force
/// hypothesis fields onto material that lacks them).
pub fn from_researcher(
    ctx: &AgentContext,
    title: &str,
    content: &str,
    kind: ItemKind,
) -> Result<ResearchItem> {
    if content.trim().is_empty() {
        return Err(Error::validation("researcher-supplied item has no content"));
    }
    Ok(ResearchItem {
        id: ItemId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: ctx.run_id.clone(),
        kind,
        title: title.to_string(),
        content: content.to_string(),
        content_hash: ContentHash::of_str(content),
        hypothesis: None,
        parent_ids: vec![],
        derivation: None,
        source_ids: vec![],
        author: Author::Researcher,
        produced_by: None,
        goal_id: ctx.goal.id.clone(),
        created_at: OffsetDateTime::now_utc(),
    })
}
