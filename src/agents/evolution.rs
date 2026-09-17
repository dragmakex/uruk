//! Evolution: create children, never edit a ranked parent (SPEC §5).
//!
//! "Children state what changed, why, and which assumptions/evidence were
//! inherited or rejected. They require their own review; they do not inherit a
//! parent's support or rating."

use super::context::AgentContext;
use super::generation::build_item;
use super::outputs::{self, EvolvedHypothesis};
use super::reflection::render_item;
use crate::prompts::ids;
use crate::records::*;
use crate::{Error, Result};

/// A child item plus the cost of producing it.
#[derive(Debug, Clone)]
pub struct Evolved {
    pub child: ResearchItem,
    pub cost: CostActual,
    /// A test whose outcome differs between parent and child.
    pub discriminating_test: String,
    pub remaining_limitations: String,
}

/// Which evolution strategy to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvolutionStrategy {
    /// Improve practical implementability (2025 Fig. A.6).
    Feasibility,
    /// Transfer a principle by analogy into a new mechanism (2025 Fig. A.7).
    Analogy,
}

impl EvolutionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Feasibility => "feasibility",
            Self::Analogy => "analogy",
        }
    }

    fn template(self) -> &'static str {
        match self {
            Self::Feasibility => ids::EVOLUTION_FEASIBILITY,
            Self::Analogy => ids::EVOLUTION_ANALOGY,
        }
    }
}

/// Improve an item's feasibility, producing a linked child (2025 Fig. A.6).
pub async fn improve_feasibility(
    ctx: &AgentContext,
    parent: &ResearchItem,
    task_id: Option<TaskId>,
) -> Result<Evolved> {
    let reviews = render_reviews(ctx, parent).await?;

    let bindings = ctx
        .base_bindings_for(ids::EVOLUTION_FEASIBILITY)
        .set("hypothesis", render_item(parent))
        .set("reviews", reviews)
        .set("constraints", render_constraints(ctx))
        .set("articles_with_reasoning", ctx.render_sources(8).await?);

    let completion = ctx
        .call(EvolutionStrategy::Feasibility.template(), bindings)
        .await?;
    let evolved: EvolvedHypothesis = outputs::parse(&completion.text)?;
    evolved.validate()?;

    finish(
        ctx,
        evolved,
        vec![parent.id.clone()],
        EvolutionStrategy::Feasibility,
        completion.cost,
        task_id,
    )
}

/// Produce one new candidate by analogy from several inspirations (Fig. A.7).
pub async fn through_analogy(
    ctx: &AgentContext,
    inspirations: &[ResearchItem],
    task_id: Option<TaskId>,
) -> Result<Evolved> {
    if inspirations.is_empty() {
        return Err(Error::validation(
            "analogical evolution needs at least one inspiring concept",
        ));
    }

    let rendered = inspirations
        .iter()
        .map(render_item)
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    let bindings = ctx
        .base_bindings_for(ids::EVOLUTION_ANALOGY)
        .set("hypotheses", rendered)
        .set("articles_with_reasoning", ctx.render_sources(8).await?);

    let completion = ctx
        .call(EvolutionStrategy::Analogy.template(), bindings)
        .await?;
    let evolved: EvolvedHypothesis = outputs::parse(&completion.text)?;
    evolved.validate()?;

    // All inspirations are parents: SPEC §15.8 requires preserving every
    // parent ID so borrowed evidence stays traceable.
    let parents = inspirations.iter().map(|i| i.id.clone()).collect();
    finish(
        ctx,
        evolved,
        parents,
        EvolutionStrategy::Analogy,
        completion.cost,
        task_id,
    )
}

fn finish(
    ctx: &AgentContext,
    evolved: EvolvedHypothesis,
    parents: Vec<ItemId>,
    strategy: EvolutionStrategy,
    cost: CostActual,
    task_id: Option<TaskId>,
) -> Result<Evolved> {
    let derivation = evolved.derivation();
    let child = build_item(
        ctx,
        &evolved.hypothesis,
        parents,
        Some(derivation),
        strategy.as_str(),
        task_id,
    );

    // A child starts unrated and unreviewed. Nothing here copies the parent's
    // assessment, rating, or evidence links (SPEC §5).
    let discriminating_test = if evolved.discriminating_test.is_empty() {
        evolved.discriminating_prediction.clone()
    } else {
        evolved.discriminating_test.clone()
    };

    Ok(Evolved {
        child,
        cost,
        discriminating_test,
        remaining_limitations: evolved.remaining_limitations,
    })
}

async fn render_reviews(ctx: &AgentContext, item: &ResearchItem) -> Result<String> {
    let reviews = ctx.store.list_reviews_for_item(&item.id).await?;
    if reviews.is_empty() {
        return Ok("(no reviews yet for this item)".to_string());
    }
    let mut out = String::new();
    for r in &reviews {
        out.push_str(&format!(
            "[{}] {}\n",
            r.strategy.as_str(),
            r.assessment_text
        ));
        for o in &r.objections {
            out.push_str(&format!(
                "- objection{}: {}\n",
                if o.fatal { " (fatal)" } else { "" },
                o.description
            ));
        }
        out.push('\n');
    }
    Ok(out)
}

fn render_constraints(ctx: &AgentContext) -> String {
    let mut parts = Vec::new();

    if !ctx.plan.rubric.constraints.is_empty() {
        parts.push(ctx.plan.rubric.constraints.join("; "));
    }

    // State the actual execution envelope, so "contemporary technology" is not
    // read as a licence to assume capabilities this run does not have.
    let p = &ctx.permissions;
    parts.push(format!(
        "Execution available: {}. Network retrieval: {}. Approved tools: {}.",
        if p.execute { "yes" } else { "no" },
        if p.network { "yes" } else { "no" },
        if p.allowed_tools.is_empty() {
            "none".to_string()
        } else {
            p.allowed_tools.join(", ")
        }
    ));

    let b = &ctx.goal.budget;
    parts.push(format!(
        "Remaining budget envelope: at most {} model calls and {} tool executions for \
         the whole run.",
        b.max_model_calls, b.max_tool_executions
    ));

    parts.join("\n")
}
