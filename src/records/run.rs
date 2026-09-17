//! Run state, meta-review feedback, and proximity clusters (SPEC §6, §5).

use super::goal::StopCondition;
use super::ids::*;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Run lifecycle state (SPEC §6).
///
/// "A scientifically negative result can be a successfully completed task."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunState {
    Running,
    WaitingForHuman,
    Blocked,
    Completed,
    BudgetExhausted,
    Cancelled,
    Failed,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::WaitingForHuman => "waiting-for-human",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::BudgetExhausted => "budget-exhausted",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => Self::Running,
            "waiting-for-human" => Self::WaitingForHuman,
            "blocked" => Self::Blocked,
            "completed" => Self::Completed,
            "budget-exhausted" => Self::BudgetExhausted,
            "cancelled" => Self::Cancelled,
            "failed" => Self::Failed,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::BudgetExhausted | Self::Cancelled | Self::Failed
        )
    }
}

/// One invocation of a goal (SPEC §11).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub schema_version: u32,
    pub project_id: ProjectId,
    /// Current goal revision.
    pub goal_id: GoalId,
    /// Current plan revision.
    pub plan_id: Option<PlanId>,
    pub state: RunState,
    pub stop_condition: Option<StopCondition>,
    /// Campaign iterations completed.
    pub iterations: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Versioned meta-review feedback supplied to later tasks (SPEC §5).
///
/// "This feedback changes subsequent context/prompts, not model weights."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feedback {
    pub id: FeedbackId,
    pub schema_version: u32,
    pub run_id: RunId,
    pub revision: u32,
    /// Recurring critiques, each with supporting record IDs.
    pub critiques: Vec<Critique>,
    /// Methodological and coverage gaps.
    pub gaps: Vec<String>,
    /// Roles this feedback is directed at.
    pub target_roles: Vec<String>,
    pub produced_by: Option<TaskId>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// One recurring critique with provenance (SPEC §15.9 adaptation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Critique {
    pub issue: String,
    /// Review or match IDs where this issue appeared.
    pub supporting_records: Vec<String>,
    /// Counterexamples, or the scope where the issue does not apply.
    pub exceptions: Vec<String>,
    /// A concrete check that would address it.
    pub recommended_check: Option<String>,
    /// How many times it recurred.
    pub occurrences: u32,
}

/// A proximity cluster of related items (SPEC §5 Proximity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cluster {
    pub id: ClusterId,
    pub schema_version: u32,
    pub run_id: RunId,
    /// Human-facing label for the research direction.
    pub label: String,
    pub item_ids: Vec<ItemId>,
    /// Why these items were grouped.
    pub basis: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A periodic campaign snapshot (SPEC §6, 2025 §§3.2-3.3).
///
/// Used to adjust work weights and spot diminishing returns. Tracked
/// separately from debate wins so the scheduler is not optimized for Elo alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CampaignSnapshot {
    pub iteration: u32,
    pub items_generated: u32,
    pub items_awaiting_review: u32,
    pub reviews_completed: u32,
    pub matches_completed: u32,
    pub clusters: u32,
    pub task_failures: u32,
    pub model_calls: u32,
    pub tokens: u64,
    /// Outcomes keyed by `role.strategy`, so strategy contribution is visible.
    pub outcomes_by_strategy: Vec<StrategyOutcome>,
    /// Objections raised, counted by type, separate from debate wins.
    pub novelty_objections: u32,
    pub feasibility_objections: u32,
    /// Evidence obtained from outside the model, e.g. executions.
    pub external_evidence_items: u32,
}

/// Per-strategy accounting (2025 §§3.2-3.3: "track which strategies work").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyOutcome {
    pub role: String,
    pub strategy: String,
    pub attempts: u32,
    pub successes: u32,
    /// Items produced that later won at least one match.
    pub produced_winners: u32,
    pub model_calls: u32,
    pub tokens: u64,
}
