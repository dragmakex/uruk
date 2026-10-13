//! The run loop: admission, dispatch, recovery, and stopping (SPEC §6, §9).

use super::executor::execute_task;
use super::limits::ConcurrencyLimits;
use crate::agents::{AgentContext, supervisor};
use crate::prompts::PromptRegistry;
use crate::provider::{Provider, UpstreamGate};
use crate::records::*;
use crate::store::{RecordBatch, Store};
use crate::{Error, Result};
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Model calls the final deliverable needs.
pub const FINAL_OUTPUT_CALLS: u32 = 1;

/// Scheduler configuration.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub limits: ConcurrencyLimits,
    pub weights: supervisor::Weights,
    /// Tasks admitted per iteration.
    pub batch_size: usize,
    /// Attempts per task before it fails permanently. Only a store fault
    /// mid-task spends an attempt: a transient provider failure is waited
    /// out inside the adapter and never reaches the task, and a permanent
    /// provider refusal is final for it.
    pub max_attempts: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            limits: ConcurrencyLimits::default(),
            weights: supervisor::Weights::default(),
            batch_size: 4,
            max_attempts: 3,
        }
    }
}

/// How a run finished.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunSummary {
    pub run_id: RunId,
    pub state: RunState,
    pub stop_condition: Option<String>,
    pub iterations: u32,
    pub tasks_completed: u32,
    pub tasks_failed: u32,
    pub items: u32,
    pub reviews: u32,
    pub matches: u32,
    /// Tasks whose external effects are of unknown outcome (SPEC §9.2).
    pub uncertain_tasks: Vec<String>,
    /// Why the final deliverable was not produced, when it was not.
    pub final_output_skipped: Option<String>,
}

/// Drives a run to completion.
pub struct Scheduler {
    store: Store,
    provider: Arc<dyn Provider>,
    prompts: Arc<PromptRegistry>,
    config: SchedulerConfig,
    cancel: CancellationToken,
}

/// How often the watchers re-read the store.
const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// Watches the run while it executes: a durable stop request (the web
/// API's stop route) reaches in-flight work through the cancellation token
/// (SPEC §9.2), and the provider's availability gate is mirrored into the
/// run's persisted research clock, so a transient outage is recorded as a
/// pause — visible to the UI, survivable across a restart, and excluded
/// from the wall-clock budget.
struct RunWatch(tokio::task::JoinHandle<()>);

impl RunWatch {
    fn spawn(
        store: Store,
        run_id: RunId,
        cancel: CancellationToken,
        gate: Option<Arc<UpstreamGate>>,
    ) -> Self {
        Self(tokio::spawn(async move {
            loop {
                tokio::time::sleep(WATCH_INTERVAL).await;
                match store.get_run(&run_id).await {
                    Ok(run) if run.state == RunState::Cancelled => {
                        tracing::warn!("stop request observed; cancelling in-flight work");
                        cancel.cancel();
                        return;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::debug!(error = %e, "run watch could not read run"),
                }
                if let Some(gate) = &gate
                    && let Err(e) = sync_provider_wait(&store, &run_id, gate).await
                {
                    tracing::debug!(error = %e, "run watch could not record the provider wait");
                }
            }
        }))
    }
}

impl Drop for RunWatch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Mirror the gate into the run's persisted pause: open it when the
/// upstream goes down, keep its note current, settle it when the upstream
/// serves again.
async fn sync_provider_wait(store: &Store, run_id: &RunId, gate: &UpstreamGate) -> Result<()> {
    let status = gate.status();
    let timing = store.run_timing(run_id).await?;
    match (status.unavailable_since(), timing.paused_since) {
        (Some(since), None) => {
            let note = status.last_fault().unwrap_or("provider unavailable");
            tracing::warn!(%note, "provider unavailable; the run's research clock is paused");
            store.begin_provider_wait(run_id, since, note).await
        }
        (Some(since), Some(_)) => {
            let note = status.last_fault().unwrap_or("provider unavailable");
            if timing.pause_note.as_deref() != Some(note) {
                store.begin_provider_wait(run_id, since, note).await?;
            }
            Ok(())
        }
        (None, Some(_)) => {
            tracing::info!("provider serving again; the run's research clock resumes");
            store
                .end_provider_wait(run_id, OffsetDateTime::now_utc())
                .await
                .map(|_| ())
        }
        (None, None) => Ok(()),
    }
}

/// Cancels the run's work token when the wall-clock deadline passes, so an
/// in-flight model call ends at the deadline instead of running the run
/// past it (SPEC §6: "Enforce a finite deadline"). The deadline is the
/// persisted research clock: it runs from the run's first start, not from
/// this process start, so a restart cannot extend it; and it stands still
/// while the provider is unavailable, so an outage cannot spend it. Only
/// the work token is cancelled: the run's own token still means "the
/// researcher stopped this run".
struct DeadlineWatch(tokio::task::JoinHandle<()>);

impl DeadlineWatch {
    fn spawn(
        store: Store,
        run_id: RunId,
        wall_clock_secs: u64,
        gate: Option<Arc<UpstreamGate>>,
        work: CancellationToken,
    ) -> Self {
        Self(tokio::spawn(async move {
            loop {
                let timing = match store.run_timing(&run_id).await {
                    Ok(timing) => timing,
                    Err(e) => {
                        tracing::debug!(error = %e, "deadline watch could not read the clock");
                        tokio::time::sleep(WATCH_INTERVAL).await;
                        continue;
                    }
                };
                let upstream_down = gate.as_ref().is_some_and(|g| !g.status().is_available());
                let now = OffsetDateTime::now_utc();
                if timing.is_paused() || upstream_down {
                    tokio::time::sleep(WATCH_INTERVAL).await;
                    continue;
                }
                let Some(deadline) = timing.deadline(wall_clock_secs) else {
                    tokio::time::sleep(WATCH_INTERVAL).await;
                    continue;
                };
                if now >= deadline {
                    tracing::warn!("wall-clock deadline reached; cancelling in-flight work");
                    work.cancel();
                    return;
                }
                let remaining = Duration::try_from(deadline - now).unwrap_or(WATCH_INTERVAL);
                tokio::time::sleep(remaining.min(Duration::from_secs(1))).await;
            }
        }))
    }
}

impl Drop for DeadlineWatch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Scheduler {
    pub fn new(store: Store, provider: Arc<dyn Provider>, config: SchedulerConfig) -> Self {
        Self {
            store,
            provider,
            prompts: Arc::new(PromptRegistry::new()),
            config,
            cancel: CancellationToken::new(),
        }
    }

    /// Token that cancels this run (SPEC §9.3).
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Run until a stopping condition holds.
    ///
    /// Reconciles interrupted work first, so a resumed run does not duplicate
    /// records or blindly retry an uncertain external effect (SPEC §9.2).
    pub async fn run(&self, run_id: &RunId) -> Result<RunSummary> {
        // Recovery: reconcile anything left running by a previous process.
        let uncertain = self.store.reconcile_interrupted(run_id).await?;
        if !uncertain.is_empty() {
            tracing::warn!(
                count = uncertain.len(),
                "tasks with unknown external outcomes require review before retry"
            );
        }

        let run = self.store.get_run(run_id).await?;
        if run.state.is_terminal() {
            return Err(Error::validation(format!(
                "run {run_id} already finished ({}); nothing to resume",
                run.state.as_str()
            )));
        }

        let goal = self.store.current_goal(run_id).await?;
        let plan = match self.store.current_plan(run_id).await? {
            Some(p) => p,
            None => {
                let revision = self.store.next_plan_revision(run_id).await?;
                let plan = supervisor::build_plan(&goal, revision);
                self.store.insert_plan(&plan).await?;
                plan
            }
        };

        // Guarded transition: a stop request that lands between the
        // terminal check above and this point must win. When it does, the
        // loop below observes the cancelled state and winds down.
        if !self.store.mark_run_running(run_id).await? {
            tracing::warn!("run was cancelled while the scheduler was starting");
        }

        // The wall clock is the persisted research clock: it runs from the
        // run's first start (stamped above, once), so a restart cannot
        // extend the deadline, and it pauses while the provider is down.
        let wall_clock_secs = goal.budget.wall_clock_secs;
        let gate = self.provider.upstream_gate();
        let watch = RunWatch::spawn(
            self.store.clone(),
            run_id.clone(),
            self.cancel.clone(),
            gate.clone(),
        );
        // Work runs under a child token: a stop request cancels it through
        // the parent, the deadline cancels it alone.
        let work = self.cancel.child_token();
        let _deadline_watch = DeadlineWatch::spawn(
            self.store.clone(),
            run_id.clone(),
            wall_clock_secs,
            gate.clone(),
            work.clone(),
        );
        let stop_condition: StopCondition;

        loop {
            if self.cancel.is_cancelled() {
                stop_condition = StopCondition::Cancelled;
                break;
            }

            let run = self.store.get_run(run_id).await?;
            if run.state == RunState::Cancelled {
                // A durable control request from another process (SPEC §9.2).
                self.cancel.cancel();
                stop_condition = StopCondition::Cancelled;
                break;
            }

            let usage = self.store.budget_usage(run_id).await?;
            let deadline_passed = self.deadline_passed(run_id, wall_clock_secs).await?;

            if let Some(condition) = supervisor::should_stop(
                &self.store,
                run_id,
                &goal,
                &usage,
                run.iterations,
                deadline_passed,
            )
            .await?
            {
                stop_condition = condition;
                break;
            }

            // A pending approval parks dependent work; the scheduler does not
            // hold a worker open waiting for a person (SPEC §6).
            let pending = self
                .store
                .list_approvals(run_id, Some(ApprovalState::Pending))
                .await?;

            let admitted = self.admit(run_id, &goal, &plan, run.iterations).await?;
            let ready = self
                .store
                .ready_tasks(run_id, self.config.batch_size as u32)
                .await?;

            if ready.is_empty() {
                if !pending.is_empty() {
                    self.store
                        .set_run_state(run_id, RunState::WaitingForHuman, None)
                        .await?;
                    return self
                        .summarize(run_id, RunState::WaitingForHuman, None, uncertain, None)
                        .await;
                }
                if admitted == 0 {
                    stop_condition = StopCondition::WorkExhausted;
                    break;
                }
                continue;
            }

            // Ordinary work is capped below the output reserve (SPEC §6).
            let ceiling = goal
                .budget
                .max_model_calls
                .saturating_sub(goal.budget.reserve_calls_for_output);
            self.dispatch(run_id, &goal, &plan, ready, &work, ceiling)
                .await?;
            let iteration = self.store.bump_iteration(run_id).await?;
            self.snapshot(run_id, iteration).await?;

            if goal.mode == Mode::Task && self.deliverable_done(run_id).await? {
                // Task mode runs the work its plan requires, then stops; it
                // does not iterate looking for more (SPEC §6).
                let remaining = self.store.ready_tasks(run_id, 1).await?;
                if remaining.is_empty() {
                    stop_condition = StopCondition::DeliverableSatisfied;
                    break;
                }
            }
        }

        // The final deliverable draws on the output reserve and runs only if
        // permitted and budgeted; when it cannot, that is recorded (SPEC §6).
        let skipped = self
            .final_output(run_id, &goal, &plan, &stop_condition, &work)
            .await?;

        // A stop request that landed during the final meta-review wins over
        // the condition the loop stopped on.
        let mut stop_condition = if self.cancel.is_cancelled() {
            StopCondition::Cancelled
        } else {
            stop_condition
        };

        // A finished run holds no open work: whatever was admitted but never
        // dispatched (parked behind a failed prerequisite, or simply left
        // over) is closed with the run rather than shown as queued forever.
        self.store.cancel_pending_tasks(run_id).await?;

        // The watch must not reopen a pause after the run is over; settle
        // any wait that was still open (a stop during an outage).
        drop(watch);
        self.store
            .end_provider_wait(run_id, OffsetDateTime::now_utc())
            .await?;

        let mut state = match stop_condition {
            StopCondition::Cancelled => RunState::Cancelled,
            StopCondition::BudgetExhausted => RunState::BudgetExhausted,
            StopCondition::SafetyBoundary => RunState::Blocked,
            // A scientifically negative result is still a completed run.
            _ => RunState::Completed,
        };

        // Guarded write: a durable stop that the watcher has not observed
        // yet is already recorded as `cancelled`, and must not be overwritten.
        if !self
            .store
            .finish_run(run_id, state, stop_condition.clone())
            .await?
        {
            tracing::warn!("stop request observed at completion; the run is cancelled");
            self.cancel.cancel();
            state = RunState::Cancelled;
            stop_condition = StopCondition::Cancelled;
        }
        self.summarize(run_id, state, Some(stop_condition), uncertain, skipped)
            .await
    }

    /// Whether the research clock has run out: the persisted deadline has
    /// passed and the clock is not standing still on a provider wait (the
    /// gate is consulted directly, so the moments before the watch records
    /// a new outage count as paused too).
    async fn deadline_passed(&self, run_id: &RunId, wall_clock_secs: u64) -> Result<bool> {
        if self
            .provider
            .upstream_gate()
            .is_some_and(|gate| !gate.status().is_available())
        {
            return Ok(false);
        }
        let timing = self.store.run_timing(run_id).await?;
        Ok(timing.deadline_passed(wall_clock_secs, OffsetDateTime::now_utc()))
    }

    async fn deliverable_done(&self, run_id: &RunId) -> Result<bool> {
        Ok(self.store.list_tasks(run_id).await?.iter().any(|t| {
            t.role == Role::MetaReview
                && t.strategy == "deliverable"
                && t.state == TaskState::Completed
        }))
    }

    /// Produce the final deliverable after the loop, if not already done.
    /// Returns the reason it was skipped, if it was.
    ///
    /// The deliverable is one more model call under the same bounds as the
    /// rest of the run: it is skipped once the wall-clock deadline has
    /// passed, and it is not re-run when an attempt at it already failed
    /// during the loop (that failure is reported instead).
    async fn final_output(
        &self,
        run_id: &RunId,
        goal: &Goal,
        plan: &Plan,
        stop: &StopCondition,
        cancel: &CancellationToken,
    ) -> Result<Option<String>> {
        if !plan.roles.iter().any(|r| r == Role::MetaReview.as_str()) {
            return Ok(None);
        }
        if self.deliverable_done(run_id).await? {
            return Ok(None);
        }

        let failed_deliverable = self
            .store
            .list_tasks(run_id)
            .await?
            .into_iter()
            .filter(|t| {
                t.role == Role::MetaReview
                    && t.strategy == "deliverable"
                    && t.state == TaskState::Failed
            })
            .max_by_key(|t| t.finished_at);

        let skip_reason = if matches!(stop, StopCondition::Cancelled) {
            Some("run was cancelled before the final meta-review".to_string())
        } else if matches!(stop, StopCondition::SafetyBoundary) {
            Some("run stopped at a safety boundary; no final meta-review".to_string())
        } else if self
            .deadline_passed(run_id, goal.budget.wall_clock_secs)
            .await?
        {
            Some("wall-clock deadline passed before the final meta-review".to_string())
        } else if let Some(failed) = failed_deliverable {
            Some(format!(
                "final meta-review already attempted and failed: {}",
                failed
                    .error
                    .unwrap_or_else(|| "no error recorded".to_string())
            ))
        } else if !self
            .store
            .can_afford_final_output(run_id, FINAL_OUTPUT_CALLS, &goal.budget)
            .await?
        {
            Some(format!(
                "budget cannot cover the {FINAL_OUTPUT_CALLS} call(s) the final meta-review needs"
            ))
        } else {
            None
        };

        if let Some(reason) = skip_reason {
            self.record_skip(run_id, &reason).await?;
            return Ok(Some(reason));
        }

        let work = supervisor::deliverable_work(goal, &self.config.weights);
        let key = format!("{}:meta_review:deliverable:final", plan.id);
        let task = self.build_task(run_id, goal, plan, &work, &key)?;
        let task_id = self.store.enqueue_task(&task, Some(&key)).await?;
        if task_id == task.id {
            if let Err(Error::Budget(reason)) = self
                .store
                .reserve_budget(run_id, &task.id, &task.reservation, &goal.budget, true)
                .await
            {
                self.store
                    .complete_task(
                        &task.id,
                        TaskState::Cancelled,
                        vec![],
                        CostActual::default(),
                        Some(format!("not admitted: {reason}")),
                    )
                    .await?;
                self.record_skip(run_id, &reason).await?;
                return Ok(Some(reason));
            }
            self.store
                .insert_decision(&supervisor::scheduling_decision(run_id, &work, &task.id))
                .await?;
        }

        let ready: Vec<Task> = self
            .store
            .ready_tasks(run_id, 1)
            .await?
            .into_iter()
            .filter(|t| t.id == task_id)
            .collect();
        if !ready.is_empty() {
            // The deliverable may spend the output reserve.
            self.dispatch(
                run_id,
                goal,
                plan,
                ready,
                cancel,
                goal.budget.max_model_calls,
            )
            .await?;
        }

        // The deliverable ran under the same bounds as the rest of the run;
        // when it did not complete, say why rather than leaving a cancelled
        // task with no reason.
        let task = self.store.get_task(&task_id).await?;
        if task.state == TaskState::Completed {
            return Ok(None);
        }
        let reason = if task.state == TaskState::Cancelled && self.cancel.is_cancelled() {
            "run was cancelled during the final meta-review".to_string()
        } else if task.state == TaskState::Cancelled
            && self
                .deadline_passed(run_id, goal.budget.wall_clock_secs)
                .await?
        {
            "wall-clock deadline passed during the final meta-review".to_string()
        } else {
            format!(
                "final meta-review {}: {}",
                task.state.as_str(),
                task.error
                    .unwrap_or_else(|| "no error recorded".to_string())
            )
        };
        self.record_skip(run_id, &reason).await?;
        Ok(Some(reason))
    }

    async fn record_skip(&self, run_id: &RunId, reason: &str) -> Result<()> {
        self.store
            .insert_decision(&Decision {
                id: DecisionId::new(),
                schema_version: SCHEMA_VERSION,
                run_id: run_id.clone(),
                kind: DecisionKind::Stop,
                actor: Actor::System,
                reason: format!("final meta-review skipped: {reason}"),
                referenced: vec![],
                payload: serde_json::json!({ "final_output_skipped": reason }),
                created_at: OffsetDateTime::now_utc(),
            })
            .await
    }

    /// Ask the Supervisor for work and persist it before dispatch (SPEC §6).
    async fn admit(
        &self,
        run_id: &RunId,
        goal: &Goal,
        plan: &Plan,
        iteration: u32,
    ) -> Result<usize> {
        let existing = self
            .store
            .count_tasks_in_state(run_id, TaskState::Pending)
            .await?;
        if existing as usize >= self.config.batch_size {
            return Ok(0);
        }

        let work = supervisor::select_work(
            &self.store,
            run_id,
            goal,
            plan,
            &self.config.weights,
            self.config.batch_size - existing as usize,
            iteration,
        )
        .await?;

        let mut admitted = 0;
        for item in work {
            // The idempotency key names the work and the scheduling round.
            // Restart within a round reuses the task; a later round proposing
            // the same input-less work (another generation, another
            // synthesis) is a distinct stochastic trial with its own identity
            // and seed (SPEC §9.2).
            let dedupe = format!(
                "{}:{}:{}:{}:iter{iteration}",
                plan.id,
                item.role.as_str(),
                item.strategy,
                item.input_refs.join(",")
            );
            let task = self.build_task(run_id, goal, plan, &item, &dedupe)?;

            let task_id = self.store.enqueue_task(&task, Some(&dedupe)).await?;
            if task_id != task.id {
                // Identical work already exists in this round; nothing new.
                continue;
            }

            // A human gate for work the permissions say needs one. The wait
            // is a persisted record, not a live thread (SPEC §9.2).
            if task.role == Role::Tool
                && goal
                    .permissions
                    .require_approval_for
                    .iter()
                    .any(|r| r == "execute")
            {
                let request = task.payload.clone().unwrap_or_default();
                let payload = request["request"].clone();
                let req = ApprovalRequest {
                    id: RequestId::new(),
                    schema_version: SCHEMA_VERSION,
                    run_id: run_id.clone(),
                    task_id: Some(task.id.clone()),
                    action: format!(
                        "execute `{} {}` ({})",
                        payload["program"].as_str().unwrap_or("?"),
                        payload["args"]
                            .as_array()
                            .map(|a| a
                                .iter()
                                .filter_map(|v| v.as_str())
                                .collect::<Vec<_>>()
                                .join(" "))
                            .unwrap_or_default(),
                        request["purpose"].as_str().unwrap_or("no purpose stated")
                    ),
                    payload_hash: ContentHash::of_json(&payload)?,
                    payload,
                    state: ApprovalState::Pending,
                    decided_by: None,
                    decided_reason: None,
                    created_at: OffsetDateTime::now_utc(),
                    decided_at: None,
                };
                self.store.park_for_approval(&task.id, &req).await?;
            }

            // Reserve budget before dispatch; a refusal simply means this work
            // does not get admitted (SPEC §6).
            match self
                .store
                .reserve_budget(run_id, &task.id, &task.reservation, &goal.budget, false)
                .await
            {
                Ok(()) => {}
                Err(Error::Budget(reason)) => {
                    tracing::info!(task = %task.id, %reason, "work not admitted: budget");
                    self.store
                        .complete_task(
                            &task.id,
                            TaskState::Cancelled,
                            vec![],
                            CostActual::default(),
                            Some(format!("not admitted: {reason}")),
                        )
                        .await?;
                    continue;
                }
                Err(e) => return Err(e),
            }

            let decision = supervisor::scheduling_decision(run_id, &item, &task.id);
            self.store.insert_decision(&decision).await?;
            admitted += 1;
        }
        Ok(admitted)
    }

    fn build_task(
        &self,
        run_id: &RunId,
        goal: &Goal,
        plan: &Plan,
        work: &supervisor::WorkItem,
        identity: &str,
    ) -> Result<Task> {
        // Model calls a task may need: a debate can take several turns.
        let calls = match (work.role, work.strategy.as_str()) {
            (Role::Tool, _) => 0,
            (_, "debate") => goal.budget.max_debate_turns.clamp(1, 10),
            _ => 1,
        };
        let tool_executions = u32::from(work.role == Role::Tool);

        Ok(Task {
            id: TaskId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: run_id.clone(),
            role: work.role,
            strategy: work.strategy.clone(),
            goal_id: goal.id.clone(),
            plan_id: plan.id.clone(),
            input_hash: ContentHash::of_str(identity),
            input_refs: work.input_refs.clone(),
            prompt_id: None,
            prompt_hash: None,
            model: Some(self.provider.default_model().to_string()),
            provider: Some(self.provider.name().to_string()),
            payload: work.payload.clone(),
            feedback_id: None,
            depends_on: vec![],
            priority: work.priority,
            priority_rationale: work.rationale.clone(),
            permissions: goal.permissions.clone(),
            reservation: CostReservation {
                model_calls: calls,
                tokens: 20_000 * calls as u64,
                tool_executions,
            },
            state: TaskState::Pending,
            attempts: 0,
            max_attempts: self.config.max_attempts,
            // A persisted seed keeps sampled choices stable across resume,
            // and distinct across trials (SPEC §6, §9.2).
            seed: derive_seed(identity),
            output_refs: vec![],
            actual_cost: CostActual::default(),
            error: None,
            failure: None,
            created_at: OffsetDateTime::now_utc(),
            started_at: None,
            finished_at: None,
        })
    }

    /// Dispatch ready tasks concurrently, bounded by the configured limits.
    /// `work` cancels every task's model calls: it fires on a stop request
    /// and at the wall-clock deadline. `ceiling` is the model-call limit
    /// this work may spend up to; each task starts attempts against its own
    /// reservation, then leases headroom under the ceiling from the durable
    /// ledger, so retries can never overrun the budget.
    async fn dispatch(
        &self,
        run_id: &RunId,
        goal: &Goal,
        plan: &Plan,
        ready: Vec<Task>,
        work: &CancellationToken,
        ceiling: u32,
    ) -> Result<()> {
        let feedback = self.store.latest_feedback(run_id).await?;
        let human_feedback: Vec<String> = self
            .store
            .list_decisions_of_kind(run_id, DecisionKind::HumanFeedback)
            .await?
            .iter()
            .rev()
            .take(5)
            .rev()
            .filter_map(|d| {
                d.payload["content"]
                    .as_str()
                    .map(|c| format!("[{}] {}", d.id, c.trim()))
            })
            .collect();
        let overview_hints: Vec<String> = self
            .store
            .list_decisions_of_kind(run_id, DecisionKind::MetaFeedback)
            .await?
            .iter()
            .rev()
            .find_map(|d| d.payload["underexplored"].as_array().cloned())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let mut set: JoinSet<(TaskId, Result<super::TaskOutcome>)> = JoinSet::new();

        for task in ready {
            if !self.store.claim_task(&task.id).await? {
                continue; // Another worker took it.
            }
            // The claim advanced the attempt counter that keys this
            // attempt's budget records.
            let task = self.store.get_task(&task.id).await?;

            let mut ctx = AgentContext::new(
                self.store.clone(),
                self.provider.clone(),
                self.prompts.clone(),
                run_id.clone(),
                goal.clone(),
                plan.clone(),
                task.permissions.clone(),
                task.seed,
                work.clone(),
            );
            ctx.feedback = feedback.clone();
            ctx.human_feedback = human_feedback.clone();
            ctx.overview_hints = overview_hints.clone();
            ctx.model = task.model.clone();
            ctx.set_call_allowance(&task, ceiling);

            let limits = self.config.limits.clone();
            let task_id = task.id.clone();
            let tool = task
                .payload
                .as_ref()
                .and_then(|p| p["request"]["program"].as_str().map(str::to_string));

            set.spawn(async move {
                // The permit is held for the task's lifetime and released on
                // drop, including on cancellation.
                let _permit = limits.acquire(task.role, tool.as_deref()).await;
                let outcome = execute_task(&ctx, &task).await;
                (task_id, outcome)
            });
        }

        while let Some(joined) = set.join_next().await {
            let (task_id, result) = match joined {
                Ok(pair) => pair,
                Err(e) if e.is_cancelled() => continue,
                Err(e) => {
                    tracing::error!(error = %e, "task panicked");
                    continue;
                }
            };

            match result {
                // A provider failure arriving here is a permanent refusal
                // and final for the task; a transient outage never reaches
                // here, the adapter waits it out (SPEC §6, §9.1).
                Ok(outcome) => {
                    self.store
                        .commit_task(
                            &task_id,
                            outcome.state,
                            &outcome.batch,
                            outcome.cost,
                            outcome.error,
                            outcome.failure,
                        )
                        .await?;
                }
                Err(e @ Error::Storage(_)) | Err(e @ Error::Migration(_)) => {
                    // The store failed mid-task: nothing was recorded, so the
                    // attempt's consumption is unknown and cannot be settled.
                    // Retry within bounds.
                    let requeued = self
                        .store
                        .requeue_task(&task_id, &e.to_string(), &CostActual::default())
                        .await?;
                    if !requeued {
                        tracing::warn!(task = %task_id, error = %e, "task failed permanently");
                    }
                }
                Err(e) => {
                    self.store
                        .commit_task(
                            &task_id,
                            TaskState::Failed,
                            &RecordBatch::default(),
                            CostActual::default(),
                            Some(e.to_string()),
                            None,
                        )
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Persist a campaign snapshot for scheduler feedback (SPEC §6).
    async fn snapshot(&self, run_id: &RunId, iteration: u32) -> Result<()> {
        let items = self.store.list_items(run_id).await?;
        let reviews = self.store.list_reviews(run_id).await?;
        let matches = self.store.list_matches(run_id).await?;
        let tasks = self.store.list_tasks(run_id).await?;
        let usage = self.store.budget_usage(run_id).await?;
        let evidence = self.store.list_evidence(run_id).await?;
        let counts = self.store.review_counts(run_id).await?;

        let awaiting = items
            .iter()
            .filter(|i| i.kind.is_rankable() && !counts.contains_key(i.id.as_str()))
            .count() as u32;

        // Strategy accounting, so contribution is visible per strategy rather
        // than only as an aggregate (2025 §§3.2-3.3).
        let mut by_strategy: std::collections::BTreeMap<(String, String), StrategyOutcome> =
            Default::default();
        for task in &tasks {
            let key = (task.role.as_str().to_string(), task.strategy.clone());
            let entry = by_strategy.entry(key).or_insert_with(|| StrategyOutcome {
                role: task.role.as_str().to_string(),
                strategy: task.strategy.clone(),
                attempts: 0,
                successes: 0,
                produced_winners: 0,
                model_calls: 0,
                tokens: 0,
            });
            entry.attempts += 1;
            if task.state == TaskState::Completed {
                entry.successes += 1;
            }
            entry.model_calls += task.actual_cost.model_calls;
            entry.tokens += task.actual_cost.total_tokens();
        }

        // Count items produced by each strategy that later won a match.
        for m in &matches {
            let winner = match m.outcome {
                MatchOutcome::WinA => Some(&m.item_a),
                MatchOutcome::WinB => Some(&m.item_b),
                _ => None,
            };
            if let Some(winner_id) = winner
                && let Some(item) = items.iter().find(|i| &i.id == winner_id)
                && let Author::Agent { role, strategy } = &item.author
                && let Some(entry) = by_strategy.get_mut(&(role.clone(), strategy.clone()))
            {
                entry.produced_winners += 1;
            }
        }

        // Objection types are tracked separately from debate wins, so the
        // scheduler is not optimized for Elo alone (SPEC §6).
        let (mut novelty, mut feasibility) = (0, 0);
        for review in &reviews {
            for objection in &review.objections {
                let text = objection.description.to_ascii_lowercase();
                if text.contains("novel") {
                    novelty += 1;
                }
                if text.contains("feasib") || text.contains("practical") {
                    feasibility += 1;
                }
            }
        }

        let snapshot = CampaignSnapshot {
            iteration,
            items_generated: items.len() as u32,
            items_awaiting_review: awaiting,
            reviews_completed: reviews.len() as u32,
            matches_completed: matches.len() as u32,
            clusters: self.store.list_clusters(run_id).await?.len() as u32,
            task_failures: tasks
                .iter()
                .filter(|t| t.state == TaskState::Failed)
                .count() as u32,
            model_calls: usage.model_calls,
            tokens: usage.tokens,
            outcomes_by_strategy: by_strategy.into_values().collect(),
            novelty_objections: novelty,
            feasibility_objections: feasibility,
            external_evidence_items: evidence.iter().filter(|e| e.kind.is_empirical()).count()
                as u32,
        };
        self.store.insert_snapshot(run_id, &snapshot).await
    }

    async fn summarize(
        &self,
        run_id: &RunId,
        state: RunState,
        stop: Option<StopCondition>,
        uncertain: Vec<TaskId>,
        final_output_skipped: Option<String>,
    ) -> Result<RunSummary> {
        let tasks = self.store.list_tasks(run_id).await?;
        let run = self.store.get_run(run_id).await?;

        Ok(RunSummary {
            run_id: run_id.clone(),
            state,
            stop_condition: stop.map(|s| s.as_str().to_string()),
            iterations: run.iterations,
            tasks_completed: tasks
                .iter()
                .filter(|t| t.state == TaskState::Completed)
                .count() as u32,
            tasks_failed: tasks
                .iter()
                .filter(|t| t.state == TaskState::Failed)
                .count() as u32,
            items: self.store.list_items(run_id).await?.len() as u32,
            reviews: self.store.list_reviews(run_id).await?.len() as u32,
            matches: self.store.list_matches(run_id).await?.len() as u32,
            uncertain_tasks: uncertain.into_iter().map(|t| t.0).collect(),
            final_output_skipped,
        })
    }
}

/// Derive a stable seed from a task's identity.
///
/// The same work in the same round draws the same sample on resume, so a
/// replayed comparison presents candidates in the same order; a different
/// round is a different trial with a different seed (SPEC §6, §9.2).
fn derive_seed(identity: &str) -> u64 {
    let hash = ContentHash::of_str(identity);
    let bytes = hash.as_str().as_bytes();
    let mut seed = 0u64;
    for &b in bytes.iter().take(16) {
        seed = seed.wrapping_mul(31).wrapping_add(b as u64);
    }
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_is_stable_for_the_same_work_and_distinct_across_rounds() {
        let a = derive_seed("plan:generation:literature::iter1");
        let b = derive_seed("plan:generation:literature::iter1");
        assert_eq!(a, b, "resume must derive the same seed");

        let c = derive_seed("plan:generation:literature::iter2");
        assert_ne!(a, c, "a later trial must not reuse the seed");
    }
}
