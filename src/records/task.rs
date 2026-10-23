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
    /// Literature search: federated discovery and open-access acquisition.
    /// A separate role gives search its own budget line and report row.
    Search,
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
            Self::Search => "search",
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
            "search" => Self::Search,
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
    /// Why a `Failed` task failed, as a typed fact for scheduling policy;
    /// `error` is the display text. Absent on tasks that did not fail.
    #[serde(default)]
    pub failure: Option<FailureKind>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

impl Task {
    /// The class of this task's failure, for scheduling policy.
    ///
    /// The persisted [`Task::failure`] is authoritative. Tasks persisted
    /// before it existed carry only the message; for those, and only when
    /// the task is `Failed`, the message shapes the old provider adapter
    /// produced are recognised as [`FailureKind::Provider`], so an outage
    /// recorded before the upgrade stays terminal for its work. Anything
    /// else stays unclassified, and is treated as re-proposable, so no
    /// ordinary output failure is reclassified by guesswork.
    pub fn failure_kind(&self) -> Option<FailureKind> {
        if let Some(kind) = self.failure {
            return Some(kind);
        }
        if self.state != TaskState::Failed {
            return None;
        }
        self.error
            .as_deref()
            .filter(|e| legacy_provider_failure(e))
            .map(|_| FailureKind::Provider)
    }
}

/// Whether `error` is a message the pre-upgrade provider adapter wrote for
/// a task that failed on the provider. The adapter stored its messages
/// verbatim (no `validation:`-style prefix), in exactly these shapes:
/// `HTTP <status>: …`, `request failed: …`, `reading response: …`,
/// `unparseable chat completion (…)`, with transient ones suffixed
/// `(after N attempt(s))` and possibly `(attempts exhausted: N)`.
fn legacy_provider_failure(error: &str) -> bool {
    error.contains(" attempt(s))")
        || error.starts_with("HTTP ")
        || error.starts_with("request failed: ")
        || error.starts_with("reading response: ")
        || error.starts_with("unparseable chat completion")
}

/// The class of a task failure, recorded so the supervisor can decide what a
/// failure means for the work without reading its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The model provider refused the request permanently (a 4xx other
    /// than 408/425/429, a malformed reply, a bad request). Terminal for
    /// the work; proposing it again would only repeat the refusal. A
    /// transient outage is never recorded here: the adapter waits it out.
    Provider,
    /// The model answered, but the role could not accept the result
    /// (unparseable, truncated, rejected by validation). A stochastic miss;
    /// the same work may be proposed again within the usual bound.
    Output,
    /// The run's budget could not cover another model call for this work.
    /// Terminal for the work: the budget does not come back.
    Budget,
}

/// What a decision recorded (SPEC §4.2 `Decision`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    /// The Supervisor changed the plan.
    PlanChange,
    /// The researcher supplied feedback.
    HumanFeedback,

    /// A scheduling choice and its rationale.
    Scheduling,
    /// An item's disposition changed.
    Disposition,

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

            Self::Scheduling => "scheduling",
            Self::Disposition => "disposition",

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

#[cfg(test)]
mod legacy_failure_tests {
    use super::*;

    fn failed_task(error: &str) -> Task {
        Task {
            id: TaskId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: RunId::new(),
            role: Role::MetaReview,
            strategy: "deliverable".into(),
            goal_id: GoalId::new(),
            plan_id: PlanId::new(),
            input_refs: vec![],
            input_hash: ContentHash::of_str("in"),
            prompt_id: None,
            prompt_hash: None,
            model: None,
            provider: None,
            payload: None,
            feedback_id: None,
            depends_on: vec![],
            priority: 1.0,
            priority_rationale: "test".into(),
            permissions: Permissions::default(),
            reservation: CostReservation { model_calls: 1 },
            state: TaskState::Failed,
            attempts: 3,
            max_attempts: 3,
            seed: 1,
            output_refs: vec![],
            actual_cost: CostActual::default(),
            error: Some(error.to_string()),
            failure: None,
            created_at: OffsetDateTime::now_utc(),
            started_at: None,
            finished_at: None,
        }
    }

    /// A task persisted before `failure` existed: the field is absent.
    fn from_legacy_body(error: &str) -> Task {
        let mut body = serde_json::to_value(failed_task(error)).unwrap();
        body.as_object_mut().unwrap().remove("failure");
        serde_json::from_value(body).unwrap()
    }

    #[test]
    fn the_pre_upgrade_screenshot_failure_stays_terminal_for_the_work() {
        let task = from_legacy_body(
            "request failed: error sending request for url \
             (http://localhost:8080/v1/chat/completions) (after 4 attempt(s)) \
             (attempts exhausted: 3)",
        );
        assert_eq!(task.failure, None, "nothing was persisted");
        assert_eq!(task.failure_kind(), Some(FailureKind::Provider));
    }

    #[test]
    fn other_pre_upgrade_adapter_messages_are_provider_failures() {
        for error in [
            "HTTP 503 Service Unavailable: overloaded (after 4 attempt(s))",
            "HTTP 401 Unauthorized: bad key",
            "unparseable chat completion (expected value at line 1): <html>",
            "reading response: connection reset",
        ] {
            assert_eq!(
                from_legacy_body(error).failure_kind(),
                Some(FailureKind::Provider),
                "{error:?}"
            );
        }
    }

    #[test]
    fn pre_upgrade_output_failures_are_not_reclassified() {
        for error in [
            "validation: model response was cut off by the token limit; a truncated \
             reply cannot be parsed into a complete record",
            "assessment rejected: unsupported claim",
            "validation: expected a JSON object with a `hypotheses` array",
            "interrupted by shutdown; outcome of any external effect is unknown",
        ] {
            assert_eq!(from_legacy_body(error).failure_kind(), None, "{error:?}");
        }
    }

    #[test]
    fn a_persisted_kind_is_authoritative_over_inference() {
        let mut task = failed_task("HTTP 500 Internal Server Error: boom (after 4 attempt(s))");
        task.failure = Some(FailureKind::Output);
        assert_eq!(task.failure_kind(), Some(FailureKind::Output));
    }

    #[test]
    fn inference_applies_only_to_failed_tasks() {
        let mut task = failed_task("HTTP 500 Internal Server Error: boom (after 4 attempt(s))");
        task.state = TaskState::Cancelled;
        assert_eq!(task.failure_kind(), None);
    }
}
