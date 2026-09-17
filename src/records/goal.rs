//! Goals, plans, permissions, and budgets (SPEC §4.1, §6, §12).

use super::ids::*;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Operating mode (SPEC §0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Complete a bounded request. No mandatory hypothesis generation,
    /// tournament, or iteration.
    Task,
    /// Explore an open research goal until a stopping condition holds.
    Campaign,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Campaign => "campaign",
        }
    }
}

/// Preferences, attributes, and constraints, kept separate (SPEC §4.1).
///
/// The 2025 preprint §3.2 / Fig. A.9 keeps these distinct, and the same plan
/// revision supplies them to generation, review, comparison, and evolution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rubric {
    /// Desired properties of a good answer.
    pub preferences: Vec<String>,
    /// Comparison axes used by Ranking.
    pub attributes: Vec<String>,
    /// Requirements and limits.
    pub constraints: Vec<String>,
    /// Acceptance criteria for the deliverable.
    pub acceptance: Vec<String>,
}

impl Rubric {
    /// Render preferences for `{preferences}` in the §15 templates.
    pub fn render_preferences(&self) -> String {
        if self.preferences.is_empty() {
            "(none specified)".to_string()
        } else {
            self.preferences
                .iter()
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// Render attributes for `{idea_attributes}`.
    ///
    /// SPEC §15.1: names the requested qualities, *not always novelty*.
    pub fn render_attributes(&self) -> String {
        if self.attributes.is_empty() {
            "well-reasoned".to_string()
        } else {
            self.attributes.join(", ")
        }
    }

    /// Whether novelty is actually being asked for (SPEC §7).
    ///
    /// "Do not apply novelty as a requirement to replication."
    pub fn requires_novelty(&self) -> bool {
        self.attributes
            .iter()
            .chain(&self.preferences)
            .any(|s| s.to_ascii_lowercase().contains("novel"))
    }
}

/// What Uruk is permitted to do (SPEC §12).
///
/// Enforced at execution boundaries, never by prompt text alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    /// Paths readable by tools. Raw inputs stay read-only (SPEC §12).
    pub read_paths: Vec<String>,
    /// Paths writable by tools. Normally only task workspaces.
    pub write_paths: Vec<String>,
    /// Whether any network retrieval is allowed.
    pub network: bool,
    /// Whether local subprocess execution is allowed.
    pub execute: bool,
    /// Tool names explicitly allowed. Empty means none.
    pub allowed_tools: Vec<String>,
    /// Whether local data may be sent to a remote model provider.
    ///
    /// SPEC §12 keeps this separate from `network`: "No web search" does not
    /// mean "no data leaves the machine."
    pub disclose_to_provider: bool,
    /// Work requiring a human gate before dispatch.
    pub require_approval_for: Vec<String>,
}

impl Permissions {
    /// Permissions for a literature-only run: read supplied files, nothing else.
    pub fn read_only(paths: Vec<String>) -> Self {
        Self {
            read_paths: paths,
            disclose_to_provider: true,
            ..Default::default()
        }
    }

    pub fn allows_tool(&self, tool: &str) -> bool {
        self.allowed_tools.iter().any(|t| t == tool)
    }
}

/// Hard resource limits (SPEC §6 Budgets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    /// Finite deadline, enforced even when provider cost is unknown.
    pub wall_clock_secs: u64,
    pub max_model_calls: u32,
    pub max_tokens: u64,
    pub max_tool_executions: u32,
    /// Campaign iterations.
    pub max_iterations: u32,
    /// Monetary cap where measurable. `None` means cost is unknown, which is
    /// reported as unknown rather than recorded as zero (SPEC §6).
    pub max_cost_usd: Option<f64>,
    /// Calls reserved for producing the final report.
    pub reserve_calls_for_output: u32,
    /// Turn cap for simulated debates. The published prompts target 3-5
    /// turns with a hard cap of 10; stricter run budgets take precedence
    /// (SPEC §6).
    #[serde(default = "default_debate_turns")]
    pub max_debate_turns: u32,
}

fn default_debate_turns() -> u32 {
    5
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            wall_clock_secs: 3600,
            max_model_calls: 200,
            max_tokens: 2_000_000,
            max_tool_executions: 50,
            max_iterations: 10,
            max_cost_usd: None,
            reserve_calls_for_output: 4,
            max_debate_turns: 5,
        }
    }
}

/// A source supplied with the goal, before retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputRef {
    /// Path or URL as given by the researcher.
    pub locator: String,
    /// Optional researcher-supplied description.
    pub note: Option<String>,
}

/// A research goal revision (SPEC §4.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,
    pub schema_version: u32,
    pub run_id: RunId,
    /// Monotonic revision number within the run.
    pub revision: u32,
    /// The research question or requested task.
    pub question: String,
    pub mode: Mode,
    /// Requested deliverables.
    pub deliverables: Vec<String>,
    pub inputs: Vec<InputRef>,
    pub scope: Option<String>,
    pub exclusions: Vec<String>,
    /// Facts the researcher asserts as known.
    pub known_facts: Vec<String>,
    /// Assumptions recorded rather than asked about (SPEC §4.1).
    pub assumptions: Vec<String>,
    pub rubric: Rubric,
    pub permissions: Permissions,
    pub budget: Budget,
    /// Domain profile name, if any (SPEC §10).
    pub profile: Option<String>,
    /// Why this revision was created.
    pub revision_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A stopping condition (SPEC §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCondition {
    /// The requested deliverable is satisfied.
    DeliverableSatisfied,
    /// A budget limit was reached.
    BudgetExhausted,
    /// No permitted work remains.
    WorkExhausted,
    /// Progress stalled under the plan's criterion.
    ProgressStalled,
    /// The researcher cancelled.
    Cancelled,
    /// A safety boundary was violated.
    SafetyBoundary,
}

impl StopCondition {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "deliverable_satisfied" => Self::DeliverableSatisfied,
            "budget_exhausted" => Self::BudgetExhausted,
            "work_exhausted" => Self::WorkExhausted,
            "progress_stalled" => Self::ProgressStalled,
            "cancelled" => Self::Cancelled,
            "safety_boundary" => Self::SafetyBoundary,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeliverableSatisfied => "deliverable_satisfied",
            Self::BudgetExhausted => "budget_exhausted",
            Self::WorkExhausted => "work_exhausted",
            Self::ProgressStalled => "progress_stalled",
            Self::Cancelled => "cancelled",
            Self::SafetyBoundary => "safety_boundary",
        }
    }
}

/// The Supervisor's versioned plan (SPEC §4.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: PlanId,
    pub schema_version: u32,
    pub run_id: RunId,
    /// Goal revision this plan serves.
    pub goal_id: GoalId,
    pub revision: u32,
    /// Roles selected for this plan.
    pub roles: Vec<String>,
    /// Proposed methods, in order of intended use.
    pub methods: Vec<String>,
    /// Research priorities guiding work selection.
    pub priorities: Vec<String>,
    /// Outputs this plan is expected to produce.
    pub outputs: Vec<String>,
    pub stopping_conditions: Vec<StopCondition>,
    /// The rubric revision used for tournament cohorts (SPEC §7).
    pub rubric: Rubric,
    /// Rationale for this plan, recorded before dispatch.
    pub rationale: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn novelty_is_detected_only_when_requested() {
        let replication = Rubric {
            attributes: vec!["fidelity to the original".into()],
            ..Default::default()
        };
        assert!(!replication.requires_novelty());

        let discovery = Rubric {
            attributes: vec!["novel".into(), "testable".into()],
            ..Default::default()
        };
        assert!(discovery.requires_novelty());
    }

    #[test]
    fn read_only_permissions_grant_nothing_else() {
        let p = Permissions::read_only(vec!["/papers".into()]);
        assert!(!p.network);
        assert!(!p.execute);
        assert!(!p.allows_tool("shell"));
    }
}
