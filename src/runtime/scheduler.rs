//! The run loop: admission, dispatch, recovery, and stopping (SPEC §6, §9).

use super::executor::execute_task;
use super::limits::ConcurrencyLimits;
use crate::agents::{AgentContext, supervisor};
use crate::prompts::PromptRegistry;
use crate::provider::Provider;
use crate::records::*;
use crate::store::{RecordBatch, Store};
use crate::{Error, Result};
use std::sync::Arc;
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
    /// Attempts per task before it fails permanently.
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

/// Watches the persisted run state so a `uruk stop` from another process
/// reaches in-flight work through the cancellation token (SPEC §9.2).
struct StopWatcher(tokio::task::JoinHandle<()>);

impl StopWatcher {
    fn spawn(store: Store, run_id: RunId, cancel: CancellationToken) -> Self {
        Self(tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                match store.get_run(&run_id).await {
                    Ok(run) if run.state == RunState::Cancelled => {
                        tracing::warn!("stop request observed; cancelling in-flight work");
                        cancel.cancel();
                        return;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::debug!(error = %e, "stop watcher could not read run"),
                }
            }
        }))
    }
}

impl Drop for StopWatcher {
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

        // The wall clock runs from the run's creation, not from this process
        // start, so a restart cannot extend the deadline (SPEC §6).
        let deadline = run.created_at + time::Duration::seconds(goal.budget.wall_clock_secs as i64);
        let _watcher = StopWatcher::spawn(self.store.clone(), run_id.clone(), self.cancel.clone());
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
            let deadline_passed = OffsetDateTime::now_utc() >= deadline;

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

            self.dispatch(run_id, &goal, &plan, ready).await?;
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
            .final_output(run_id, &goal, &plan, &stop_condition)
            .await?;

        let state = match stop_condition {
            StopCondition::Cancelled => {
                self.store.cancel_pending_tasks(run_id).await?;
                RunState::Cancelled
            }
            StopCondition::BudgetExhausted => RunState::BudgetExhausted,
            StopCondition::SafetyBoundary => RunState::Blocked,
            // A scientifically negative result is still a completed run.
            _ => RunState::Completed,
        };

        self.store
            .set_run_state(run_id, state, Some(stop_condition.clone()))
            .await?;
        self.summarize(run_id, state, Some(stop_condition), uncertain, skipped)
            .await
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
    async fn final_output(
        &self,
        run_id: &RunId,
        goal: &Goal,
        plan: &Plan,
        stop: &StopCondition,
    ) -> Result<Option<String>> {
        if !plan.roles.iter().any(|r| r == Role::MetaReview.as_str()) {
            return Ok(None);
        }
        if self.deliverable_done(run_id).await? {
            return Ok(None);
        }

        let skip_reason = if matches!(stop, StopCondition::Cancelled) {
            Some("run was cancelled before the final meta-review".to_string())
        } else if matches!(stop, StopCondition::SafetyBoundary) {
            Some("run stopped at a safety boundary; no final meta-review".to_string())
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
            self.dispatch(run_id, goal, plan, ready).await?;
        }
        Ok(None)
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
            created_at: OffsetDateTime::now_utc(),
            started_at: None,
            finished_at: None,
        })
    }

    /// Dispatch ready tasks concurrently, bounded by the configured limits.
    async fn dispatch(
        &self,
        run_id: &RunId,
        goal: &Goal,
        plan: &Plan,
        ready: Vec<Task>,
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

            let mut ctx = AgentContext::new(
                self.store.clone(),
                self.provider.clone(),
                self.prompts.clone(),
                run_id.clone(),
                goal.clone(),
                plan.clone(),
                task.permissions.clone(),
                task.seed,
                self.cancel.clone(),
            );
            ctx.feedback = feedback.clone();
            ctx.human_feedback = human_feedback.clone();
            ctx.overview_hints = overview_hints.clone();
            ctx.model = task.model.clone();

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
                Ok(outcome) if outcome.retryable => {
                    // Infrastructure failures are retried within bounds, and
                    // each attempt is charged (SPEC §6).
                    let reason = outcome.error.clone().unwrap_or_default();
                    let requeued = self
                        .store
                        .requeue_task(&task_id, &reason, &outcome.cost)
                        .await?;
                    if !requeued {
                        tracing::warn!(task = %task_id, %reason, "task failed permanently");
                    }
                }
                Ok(outcome) => {
                    self.store
                        .commit_task(
                            &task_id,
                            outcome.state,
                            &outcome.batch,
                            outcome.cost,
                            outcome.error,
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
