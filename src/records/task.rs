//! Tasks, decisions, artifacts, and experiments (SPEC §4.2, §8, §9.2).

use super::goal::Permissions;
use super::ids::*;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Which role a task belongs to (SPEC §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Supervisor,
    Generation,
    Reflection,
    Ranking,
    Evolution,
    Proximity,
    MetaReview,
    /// An explicit tool execution scheduled by the Supervisor.
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supervisor => "supervisor",
            Self::Generation => "generation",
            Self::Reflection => "reflection",
            Self::Ranking => "ranking",
            Self::Evolution => "evolution",
            Self::Proximity => "proximity",
            Self::MetaReview => "meta_review",
            Self::Tool => "tool",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "supervisor" => Self::Supervisor,
            "generation" => Self::Generation,
            "reflection" => Self::Reflection,
            "ranking" => Self::Ranking,
            "evolution" => Self::Evolution,
            "proximity" => Self::Proximity,
            "meta_review" => Self::MetaReview,
            "tool" => Self::Tool,
            _ => return None,
        })
    }

    /// Whether this role may request approved tools (SPEC §5 Tool access).
    pub fn may_request_tools(self) -> bool {
        matches!(
            self,
            Self::Generation | Self::Reflection | Self::Evolution | Self::Supervisor | Self::Tool
        )
    }
}

/// Lifecycle state of a task (SPEC §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Persisted, not yet dispatched.
    Pending,
    /// Waiting on unmet dependencies.
    Blocked,
    /// Dispatched and executing.
    Running,
    /// Waiting on a human approval or external result.
    WaitingForHuman,
    Completed,
    Failed,
    Cancelled,
    /// Attempted, and the external effect's outcome is unknown after a crash.
    ///
    /// SPEC §9.2: "An action may have succeeded before the process lost its
    /// result... mark it uncertain and require review before retrying."
    Uncertain,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Running => "running",
            Self::WaitingForHuman => "waiting_for_human",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Uncertain => "uncertain",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "blocked" => Self::Blocked,
            "running" => Self::Running,
            "waiting_for_human" => Self::WaitingForHuman,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "uncertain" => Self::Uncertain,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Uncertain
        )
    }
}

/// Resources a task reserved before dispatch (SPEC §6, §9.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostReservation {
    pub model_calls: u32,
    pub tokens: u64,
    pub tool_executions: u32,
}

/// Resources a task actually consumed.
///
/// `cost_usd` is `None` when the provider does not report cost; SPEC §6
/// requires reporting unknown rather than recording zero.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostActual {
    pub model_calls: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub tool_executions: u32,
    pub cost_usd: Option<f64>,
}

impl CostActual {
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }

    /// Accumulate another attempt's consumption. Unknown cost stays unknown;
    /// a known cost accumulates (SPEC §6).
    pub fn add(&mut self, other: &CostActual) {
        self.model_calls += other.model_calls;
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.tool_executions += other.tool_executions;
        if let Some(c) = other.cost_usd {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + c);
        }
    }
}

/// A unit of scheduled work (SPEC §4.2 `Task`).
///
/// The persisted task set is the queue's source of truth (SPEC §9.2); the
/// in-memory ready list is rebuilt from it on resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub schema_version: u32,
    pub run_id: RunId,
    pub role: Role,
    /// Named strategy within the role, e.g. `literature`, `debate`.
    pub strategy: String,
    /// Goal revision this task was created under.
    pub goal_id: GoalId,
    pub plan_id: PlanId,
    /// Versioned input references (item IDs, source IDs, artifact hashes).
    pub input_refs: Vec<String>,
    /// Hash of the fully rendered input, pinning what was actually sent.
    pub input_hash: ContentHash,
    /// Prompt template identity and revision (SPEC §5.1).
    pub prompt_id: Option<String>,
    pub prompt_hash: Option<ContentHash>,
    /// Model and configuration.
    pub model: Option<String>,
    /// Provider adapter that served the model calls (SPEC §11: record the
    /// effective configuration).
    #[serde(default)]
    pub provider: Option<String>,
    /// Role-specific payload. A `Tool` task carries its `ExecRequest` and the
    /// item whose claim the execution bears on.
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    /// Meta-review feedback version supplied to this task.
    pub feedback_id: Option<FeedbackId>,
    /// Tasks that must complete first.
    pub depends_on: Vec<TaskId>,
    /// Scheduling weight and the reason it was selected (SPEC §6).
    pub priority: f64,
    pub priority_rationale: String,
    pub permissions: Permissions,
    pub reservation: CostReservation,
    pub state: TaskState,
    pub attempts: u32,
    pub max_attempts: u32,
    /// Deterministic seed, persisted so resume does not redraw it (SPEC §6).
    pub seed: u64,
    /// IDs of records this task produced.
    pub output_refs: Vec<String>,
    pub actual_cost: CostActual,
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

/// What a decision recorded (SPEC §4.2 `Decision`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    /// The Supervisor changed the plan.
    PlanChange,
    /// The researcher supplied feedback.
    HumanFeedback,
    /// A human approved or denied a request.
    Approval,
    /// A scheduling choice and its rationale.
    Scheduling,
    /// An item's disposition changed.
    Disposition,
    /// Work was refused or blocked on safety grounds (SPEC §12).
    SafetyBlock,
    /// Meta-review feedback was issued.
    MetaFeedback,
    /// A stopping condition fired.
    Stop,
}

impl DecisionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PlanChange => "plan_change",
            Self::HumanFeedback => "human_feedback",
            Self::Approval => "approval",
            Self::Scheduling => "scheduling",
            Self::Disposition => "disposition",
            Self::SafetyBlock => "safety_block",
            Self::MetaFeedback => "meta_feedback",
            Self::Stop => "stop",
        }
    }
}

/// Who took an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "actor", content = "detail")]
pub enum Actor {
    Researcher,
    Supervisor,
    Agent(String),
    System,
}

/// A recorded decision (SPEC §4.2 `Decision`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub id: DecisionId,
    pub schema_version: u32,
    pub run_id: RunId,
    pub kind: DecisionKind,
    pub actor: Actor,
    pub reason: String,
    /// Records this decision refers to.
    pub referenced: Vec<String>,
    /// Structured payload, decision-kind specific.
    pub payload: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// How sensitive an artifact's content is (SPEC §4.2 access classification).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessClass {
    /// Safe to include in exports and model requests.
    Open,
    /// Exportable, but never sent to a remote provider (SPEC §12).
    LocalOnly,
    /// Redacted from logs and reports.
    Sensitive,
}

impl AccessClass {
    /// Whether content may be included in a remote model request.
    pub fn may_disclose_to_provider(self) -> bool {
        matches!(self, Self::Open)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::LocalOnly => "local_only",
            Self::Sensitive => "sensitive",
        }
    }
}

/// An immutable stored artifact (SPEC §4.2 `Artifact`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub schema_version: u32,
    pub run_id: RunId,
    pub media_type: String,
    pub content_hash: ContentHash,
    pub size_bytes: u64,
    /// Path relative to the run's artifact directory.
    pub storage_path: String,
    pub produced_by: Option<TaskId>,
    pub access: AccessClass,
    /// Human-facing label.
    pub label: Option<String>,
    /// Set when this artifact supersedes an earlier one; the original is kept.
    pub supersedes: Option<ArtifactId>,
    pub superseded_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A computational or external experiment (SPEC §4.2 `Experiment`, §8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experiment {
    pub id: ExperimentId,
    pub schema_version: u32,
    pub run_id: RunId,
    /// Whether Uruk executed it or it was supplied from outside.
    pub external: bool,
    pub protocol: String,
    /// Claims this experiment bears on.
    pub target_claims: Vec<ItemId>,
    /// Input data/source versions and hashes.
    pub inputs: Vec<String>,
    pub controls: Vec<String>,
    pub expected_outcomes: Vec<String>,
    pub analysis_method: String,
    /// Permissions required to run it.
    pub required_permissions: Vec<String>,
    /// Execution record, once run.
    pub execution: Option<ExecutionRecord>,
    /// Evidence produced from the result.
    pub evidence_ids: Vec<EvidenceId>,
    /// For external results: contributor and collection context (SPEC §8).
    pub contributor: Option<String>,
    pub collection_context: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A reproducibility record for one execution (SPEC §8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    /// Command or equivalent API request, with secrets excluded.
    pub command: String,
    /// Tool and environment versions.
    pub environment: Vec<String>,
    pub parameters: Vec<String>,
    pub seed: Option<u64>,
    /// Hash of the code artifact that ran.
    pub code_hash: Option<ContentHash>,
    /// Hashes of the input data.
    pub input_hashes: Vec<ContentHash>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub finished_at: OffsetDateTime,
    pub exit_status: i32,
    pub stdout_artifact: Option<ArtifactId>,
    pub stderr_artifact: Option<ArtifactId>,
    pub output_artifacts: Vec<ArtifactId>,
    /// A command that reproduces this run, or why reproduction is unavailable.
    pub rerun: Result<String, String>,
    /// Whether outputs passed validation. Malformed or partial output fails
    /// rather than being promoted to an observation (SPEC §8).
    pub outputs_valid: bool,
    pub validation_note: Option<String>,
}

impl ExecutionRecord {
    /// Whether this execution may become supporting evidence.
    ///
    /// SPEC §8: "failed execution cannot become support."
    pub fn may_support_claims(&self) -> bool {
        self.exit_status == 0 && self.outputs_valid
    }
}

/// A persisted request for human approval (SPEC §9.2, §11).
///
/// Approval targets the exact payload version; a changed request needs a new
/// approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: RequestId,
    pub schema_version: u32,
    pub run_id: RunId,
    /// The task blocked on this request.
    pub task_id: Option<TaskId>,
    /// What is being requested, in human-readable form.
    pub action: String,
    /// Exact action, inputs, scope, and limits.
    pub payload: serde_json::Value,
    /// Hash of the payload. Approval binds to this exact value (SPEC §11).
    pub payload_hash: ContentHash,
    pub state: ApprovalState,
    pub decided_by: Option<Actor>,
    pub decided_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    /// The underlying request changed, so the approval no longer applies.
    Invalidated,
}

impl ApprovalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Invalidated => "invalidated",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "approved" => Self::Approved,
            "denied" => Self::Denied,
            "invalidated" => Self::Invalidated,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_execution_cannot_support() {
        let now = OffsetDateTime::UNIX_EPOCH;
        let mut exec = ExecutionRecord {
            command: "python analyze.py".into(),
            environment: vec![],
            parameters: vec![],
            seed: None,
            code_hash: None,
            input_hashes: vec![],
            started_at: now,
            finished_at: now,
            exit_status: 1,
            stdout_artifact: None,
            stderr_artifact: None,
            output_artifacts: vec![],
            rerun: Ok("python analyze.py".into()),
            outputs_valid: true,
            validation_note: None,
        };
        assert!(!exec.may_support_claims(), "nonzero exit must not support");

        exec.exit_status = 0;
        assert!(exec.may_support_claims());

        exec.outputs_valid = false;
        assert!(
            !exec.may_support_claims(),
            "invalid output must not support"
        );
    }

    #[test]
    fn local_only_artifacts_never_reach_the_provider() {
        assert!(!AccessClass::LocalOnly.may_disclose_to_provider());
        assert!(!AccessClass::Sensitive.may_disclose_to_provider());
        assert!(AccessClass::Open.may_disclose_to_provider());
    }
}
