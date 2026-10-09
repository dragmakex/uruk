//! Durable task queue, approvals, and the budget ledger (SPEC §6, §9.2).
//!
//! The persisted task set is the queue's source of truth; the in-memory ready
//! list is rebuilt from it on resume.

use super::writes::RecordBatch;
use super::{Store, now, to_rfc3339};
use crate::records::*;
use crate::{Error, Result};
use sqlx::{Row, SqliteConnection};

/// Aggregate resource use for a run, derived from the ledger.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BudgetUsage {
    pub model_calls: u32,
    pub tokens: u64,
    pub tool_executions: u32,
    /// Reserved but not yet settled.
    pub reserved_calls: u32,
    pub reserved_tokens: u64,
    pub reserved_tool_executions: u32,
    /// `None` when no provider reported a cost. Reported as unknown, never
    /// recorded as zero (SPEC §6).
    pub cost_usd: Option<f64>,
    /// True when at least one settled call reported no cost.
    pub cost_partially_unknown: bool,
}

impl BudgetUsage {
    /// Calls committed: settled plus still-reserved.
    pub fn committed_calls(&self) -> u32 {
        self.model_calls + self.reserved_calls
    }

    pub fn committed_tokens(&self) -> u64 {
        self.tokens + self.reserved_tokens
    }

    pub fn committed_tool_executions(&self) -> u32 {
        self.tool_executions + self.reserved_tool_executions
    }
}

impl Store {
    /// Persist a task before dispatch (SPEC §9.2).
    ///
    /// `dedupe_key` is the idempotency key. If a task with the same key
    /// already exists, in any state, its ID is returned and nothing is
    /// queued: a restart within the same scheduling round cannot duplicate
    /// work, and a failed round is re-proposed under a new key rather than
    /// re-queued in place.
    pub async fn enqueue_task(&self, task: &Task, dedupe_key: Option<&str>) -> Result<TaskId> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();

        if let Some(key) = dedupe_key {
            let existing: Option<String> =
                sqlx::query_scalar("SELECT id FROM tasks WHERE dedupe_key = ?")
                    .bind(key)
                    .fetch_optional(&mut *conn)
                    .await?;
            if let Some(id) = existing {
                return Ok(TaskId::from_raw(id));
            }
        }

        let ts = to_rfc3339(task.created_at);
        sqlx::query(
            "INSERT INTO tasks (id, run_id, role, strategy, state, priority, attempts, seed,
                                input_hash, dedupe_key, body, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(task.id.as_str())
        .bind(task.run_id.as_str())
        .bind(task.role.as_str())
        .bind(&task.strategy)
        .bind(task.state.as_str())
        .bind(task.priority)
        .bind(task.attempts)
        .bind(task.seed as i64)
        .bind(task.input_hash.as_str())
        .bind(dedupe_key)
        .bind(serde_json::to_string(task)?)
        .bind(&ts)
        .bind(&ts)
        .execute(&mut *conn)
        .await?;

        for dep in &task.depends_on {
            sqlx::query("INSERT INTO task_deps (task_id, depends_on) VALUES (?, ?)")
                .bind(task.id.as_str())
                .bind(dep.as_str())
                .execute(&mut *conn)
                .await?;
        }

        guard.commit().await?;
        Ok(task.id.clone())
    }

    pub async fn get_task(&self, id: &TaskId) -> Result<Task> {
        let body: String = sqlx::query_scalar("SELECT body FROM tasks WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| Error::not_found(format!("task {id}")))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn list_tasks(&self, run_id: &RunId) -> Result<Vec<Task>> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT body FROM tasks WHERE run_id = ? ORDER BY created_at, id")
                .bind(run_id.as_str())
                .fetch_all(self.pool())
                .await?;
        rows.iter().map(|b| Ok(serde_json::from_str(b)?)).collect()
    }

    pub async fn count_tasks_in_state(&self, run_id: &RunId, state: TaskState) -> Result<u32> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE run_id = ? AND state = ?")
                .bind(run_id.as_str())
                .bind(state.as_str())
                .fetch_one(self.pool())
                .await?;
        Ok(n as u32)
    }

    /// Tasks eligible to run: pending, with every dependency completed.
    ///
    /// Rebuilt from storage, so resume does not need an in-memory queue
    /// (SPEC §9.2). Ordered by the Supervisor's priority.
    pub async fn ready_tasks(&self, run_id: &RunId, limit: u32) -> Result<Vec<Task>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT t.body FROM tasks t
             WHERE t.run_id = ? AND t.state IN ('pending', 'blocked')
               AND NOT EXISTS (
                   SELECT 1 FROM task_deps d
                   JOIN tasks dep ON dep.id = d.depends_on
                   WHERE d.task_id = t.id AND dep.state != 'completed'
               )
             ORDER BY t.priority DESC, t.created_at
             LIMIT ?",
        )
        .bind(run_id.as_str())
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(|b| Ok(serde_json::from_str(b)?)).collect()
    }

    /// Atomically claim a task for execution.
    ///
    /// Returns `false` if another worker already claimed it, so a task cannot
    /// be dispatched twice.
    pub async fn claim_task(&self, id: &TaskId) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();

        let body: Option<String> = sqlx::query_scalar(
            "SELECT body FROM tasks WHERE id = ? AND state IN ('pending', 'blocked')",
        )
        .bind(id.as_str())
        .fetch_optional(&mut *conn)
        .await?;

        let Some(body) = body else {
            return Ok(false);
        };

        let mut task: Task = serde_json::from_str(&body)?;
        task.state = TaskState::Running;
        task.attempts += 1;
        task.started_at = Some(now());

        sqlx::query(
            "UPDATE tasks SET state = ?, attempts = ?, body = ?, updated_at = ? WHERE id = ?",
        )
        .bind(TaskState::Running.as_str())
        .bind(task.attempts)
        .bind(serde_json::to_string(&task)?)
        .bind(to_rfc3339(now()))
        .bind(id.as_str())
        .execute(&mut *conn)
        .await?;

        guard.commit().await?;
        Ok(true)
    }

    /// Commit a task outcome and its budget settlement atomically (SPEC §9.2).
    ///
    /// A convenience over [`Store::commit_task`] for outcomes that produced
    /// no new records.
    pub async fn complete_task(
        &self,
        id: &TaskId,
        state: TaskState,
        output_refs: Vec<String>,
        actual: CostActual,
        error: Option<String>,
    ) -> Result<()> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        if let Some(mut task) = load_open_task(conn, id).await? {
            task.output_refs = output_refs;
            finish_task_in_tx(conn, &mut task, state, actual, error).await?;
        }
        guard.commit().await
    }

    /// Commit a task's records, outcome, and budget settlement in one
    /// transaction (SPEC §9.2: "commit the task outcome, referenced research
    /// records, budget settlement, and any applicable rating update
    /// atomically").
    ///
    /// Duplicate delivery after a crash is ignored: a terminal task is left
    /// untouched and the batch is not applied again.
    pub async fn commit_task(
        &self,
        id: &TaskId,
        state: TaskState,
        batch: &RecordBatch,
        actual: CostActual,
        error: Option<String>,
    ) -> Result<Vec<String>> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let Some(mut task) = load_open_task(conn, id).await? else {
            return Ok(vec![]);
        };
        batch.apply(conn).await?;
        task.output_refs = batch.output_refs();
        let refs = task.output_refs.clone();
        finish_task_in_tx(conn, &mut task, state, actual, error).await?;
        guard.commit().await?;
        Ok(refs)
    }

    /// Put a task back to pending for a bounded retry (SPEC §6).
    ///
    /// The failed attempt's consumption is settled: "Retries are bounded and
    /// charged." Returns `false` when `max_attempts` is exhausted, in which
    /// case the task is failed and its reservation released.
    pub async fn requeue_task(
        &self,
        id: &TaskId,
        reason: &str,
        spent: &CostActual,
    ) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let Some(mut task) = load_open_task(conn, id).await? else {
            return Ok(false);
        };

        settle_attempt(conn, &task, spent).await?;

        if task.attempts >= task.max_attempts {
            task.error = Some(format!("{reason} (attempts exhausted: {})", task.attempts));
            release_reservation(conn, &task).await?;
            task.state = TaskState::Failed;
            task.finished_at = Some(now());
            store_task(conn, &task).await?;
            guard.commit().await?;
            return Ok(false);
        }

        task.state = TaskState::Pending;
        task.error = Some(reason.to_string());
        store_task(conn, &task).await?;
        guard.commit().await?;
        Ok(true)
    }

    /// Reconcile tasks left `running` by a crash (SPEC §9.2).
    ///
    /// A task whose side effects are unknown becomes `Uncertain` and requires
    /// review; a safely repeatable one returns to `pending`. Its first attempt
    /// committed nothing (records commit with the outcome), so re-running it
    /// cannot duplicate research records.
    pub async fn reconcile_interrupted(&self, run_id: &RunId) -> Result<Vec<TaskId>> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT body FROM tasks WHERE run_id = ? AND state = 'running'")
                .bind(run_id.as_str())
                .fetch_all(&mut *conn)
                .await?;

        let mut uncertain = Vec::new();
        for body in rows {
            let mut task: Task = serde_json::from_str(&body)?;
            // Tool executions may have had external effects we cannot observe;
            // pure model calls are safe to redo.
            let safe_to_retry = task.role != Role::Tool;

            task.state = if safe_to_retry {
                TaskState::Pending
            } else {
                uncertain.push(task.id.clone());
                TaskState::Uncertain
            };
            task.error =
                Some("interrupted by shutdown; outcome of any external effect is unknown".into());
            if !safe_to_retry {
                // Possible incurred cost is preserved, not assumed free.
                release_reservation(conn, &task).await?;
            }
            store_task(conn, &task).await?;
        }
        guard.commit().await?;
        Ok(uncertain)
    }

    /// Cancel every task not yet terminal (SPEC §6).
    pub async fn cancel_pending_tasks(&self, run_id: &RunId) -> Result<u32> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT body FROM tasks WHERE run_id = ?
               AND state IN ('pending', 'blocked', 'waiting_for_human')",
        )
        .bind(run_id.as_str())
        .fetch_all(&mut *conn)
        .await?;
        let n = rows.len() as u32;
        for body in rows {
            let mut task: Task = serde_json::from_str(&body)?;
            task.state = TaskState::Cancelled;
            task.error = Some("cancelled before dispatch".into());
            task.finished_at = Some(now());
            release_reservation(conn, &task).await?;
            store_task(conn, &task).await?;
        }
        guard.commit().await?;
        Ok(n)
    }

    /// Reserve budget before dispatch (SPEC §6: "Reserve budget for in-flight
    /// work and final output before admitting more tasks").
    ///
    /// Ordinary work is capped below `max_model_calls` by the output reserve;
    /// the final deliverable (`final_output = true`) may use that reserve.
    /// Returns `Err(Error::Budget)` when the reservation would exceed a limit.
    pub async fn reserve_budget(
        &self,
        run_id: &RunId,
        task_id: &TaskId,
        reservation: &CostReservation,
        budget: &Budget,
        final_output: bool,
    ) -> Result<()> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let usage = usage_from(conn, run_id).await?;

        let call_ceiling = if final_output {
            budget.max_model_calls
        } else {
            budget
                .max_model_calls
                .saturating_sub(budget.reserve_calls_for_output)
        };

        if usage.committed_calls() + reservation.model_calls > call_ceiling {
            return Err(Error::Budget(format!(
                "model calls: {} committed + {} requested exceeds {} \
                 ({} of {} reserved for final output)",
                usage.committed_calls(),
                reservation.model_calls,
                call_ceiling,
                budget.reserve_calls_for_output,
                budget.max_model_calls
            )));
        }
        if usage.committed_tokens() + reservation.tokens > budget.max_tokens {
            return Err(Error::Budget(format!(
                "tokens: {} committed + {} requested exceeds {}",
                usage.committed_tokens(),
                reservation.tokens,
                budget.max_tokens
            )));
        }
        if usage.committed_tool_executions() + reservation.tool_executions
            > budget.max_tool_executions
        {
            return Err(Error::Budget(format!(
                "tool executions: {} committed + {} requested exceeds {}",
                usage.committed_tool_executions(),
                reservation.tool_executions,
                budget.max_tool_executions
            )));
        }

        sqlx::query(
            "INSERT OR IGNORE INTO budget_ledger (run_id, task_id, settlement_key, kind, model_calls,
                                        tokens, tool_executions, cost_usd, created_at)
             VALUES (?, ?, ?, 'reserve', ?, ?, ?, NULL, ?)",
        )
        .bind(run_id.as_str())
        .bind(task_id.as_str())
        .bind(format!("reserve:{task_id}"))
        .bind(reservation.model_calls)
        .bind(reservation.tokens as i64)
        .bind(reservation.tool_executions)
        .bind(to_rfc3339(now()))
        .execute(&mut *conn)
        .await?;

        guard.commit().await
    }

    /// Whether the final-output allowance can still pay for `calls`.
    pub async fn can_afford_final_output(
        &self,
        run_id: &RunId,
        calls: u32,
        budget: &Budget,
    ) -> Result<bool> {
        let usage = self.budget_usage(run_id).await?;
        Ok(usage.committed_calls() + calls <= budget.max_model_calls)
    }

    pub async fn budget_usage(&self, run_id: &RunId) -> Result<BudgetUsage> {
        let mut conn = self.pool().acquire().await?;
        usage_from(&mut conn, run_id).await
    }

    /// Persist an approval request (SPEC §9.2: a persisted record, not a
    /// live worker thread).
    pub async fn insert_approval(&self, req: &ApprovalRequest) -> Result<()> {
        let mut guard = self.begin_write().await?;
        insert_approval_in_tx(guard.conn(), req).await?;
        guard.commit().await
    }

    /// Park a task on a human gate: persist the request and move the task to
    /// `waiting_for_human` in one transaction.
    pub async fn park_for_approval(&self, task_id: &TaskId, req: &ApprovalRequest) -> Result<()> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        insert_approval_in_tx(conn, req).await?;
        if let Some(mut task) = load_open_task(conn, task_id).await? {
            task.state = TaskState::WaitingForHuman;
            store_task(conn, &task).await?;
        }
        guard.commit().await
    }

    pub async fn get_approval(&self, id: &RequestId) -> Result<ApprovalRequest> {
        let body: String = sqlx::query_scalar("SELECT body FROM approvals WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| Error::not_found(format!("approval request {id}")))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn list_approvals(
        &self,
        run_id: &RunId,
        state: Option<ApprovalState>,
    ) -> Result<Vec<ApprovalRequest>> {
        let rows: Vec<String> =
            match state {
                Some(s) => sqlx::query_scalar(
                    "SELECT body FROM approvals WHERE run_id = ? AND state = ? ORDER BY created_at",
                )
                .bind(run_id.as_str())
                .bind(s.as_str())
                .fetch_all(self.pool())
                .await?,
                None => {
                    sqlx::query_scalar(
                        "SELECT body FROM approvals WHERE run_id = ? ORDER BY created_at",
                    )
                    .bind(run_id.as_str())
                    .fetch_all(self.pool())
                    .await?
                }
            };
        rows.iter().map(|b| Ok(serde_json::from_str(b)?)).collect()
    }

    /// Decide a pending approval, binding to the exact payload (SPEC §11).
    ///
    /// `expected_hash` must match the stored payload hash: "Changing that
    /// payload requires a new approval."
    pub async fn decide_approval(
        &self,
        id: &RequestId,
        approved: bool,
        expected_hash: Option<&ContentHash>,
        reason: Option<String>,
    ) -> Result<ApprovalRequest> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();

        let body: String = sqlx::query_scalar("SELECT body FROM approvals WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| Error::not_found(format!("approval request {id}")))?;

        let mut req: ApprovalRequest = serde_json::from_str(&body)?;

        if req.state != ApprovalState::Pending {
            return Err(Error::validation(format!(
                "approval {id} is already {}; a changed request needs a new approval",
                req.state.as_str()
            )));
        }
        if let Some(expected) = expected_hash
            && expected != &req.payload_hash
        {
            {
                return Err(Error::validation(format!(
                    "approval {id} targets payload {} but the pending request is {}; \
                     a changed request needs a new approval (SPEC §11)",
                    expected.short(),
                    req.payload_hash.short()
                )));
            }
        }

        req.state = if approved {
            ApprovalState::Approved
        } else {
            ApprovalState::Denied
        };
        req.decided_by = Some(Actor::Researcher);
        req.decided_reason = reason;
        req.decided_at = Some(now());

        sqlx::query("UPDATE approvals SET state = ?, body = ?, decided_at = ? WHERE id = ?")
            .bind(req.state.as_str())
            .bind(serde_json::to_string(&req)?)
            .bind(to_rfc3339(now()))
            .bind(id.as_str())
            .execute(&mut *conn)
            .await?;

        // Unblock or cancel the waiting task.
        if let Some(task_id) = &req.task_id
            && let Some(mut task) = load_open_task(conn, task_id).await?
        {
            {
                if approved {
                    task.state = TaskState::Pending;
                } else {
                    task.state = TaskState::Cancelled;
                    task.error = Some("denied by researcher".into());
                    task.finished_at = Some(now());
                    release_reservation(conn, &task).await?;
                }
                store_task(conn, &task).await?;
            }
        }

        guard.commit().await?;
        Ok(req)
    }
}

async fn insert_approval_in_tx(conn: &mut SqliteConnection, req: &ApprovalRequest) -> Result<()> {
    sqlx::query(
        "INSERT INTO approvals (id, run_id, task_id, state, payload_hash, body, created_at, decided_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(req.id.as_str())
    .bind(req.run_id.as_str())
    .bind(req.task_id.as_ref().map(|t| t.0.clone()))
    .bind(req.state.as_str())
    .bind(req.payload_hash.as_str())
    .bind(serde_json::to_string(req)?)
    .bind(to_rfc3339(req.created_at))
    .execute(conn)
    .await?;
    Ok(())
}

/// Load a task unless it is already terminal (duplicate delivery after a
/// crash is ignored).
async fn load_open_task(conn: &mut SqliteConnection, id: &TaskId) -> Result<Option<Task>> {
    let body: String = sqlx::query_scalar("SELECT body FROM tasks WHERE id = ?")
        .bind(id.as_str())
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| Error::not_found(format!("task {id}")))?;
    let task: Task = serde_json::from_str(&body)?;
    Ok(if task.state.is_terminal() {
        None
    } else {
        Some(task)
    })
}

async fn store_task(conn: &mut SqliteConnection, task: &Task) -> Result<()> {
    sqlx::query("UPDATE tasks SET state = ?, attempts = ?, body = ?, updated_at = ? WHERE id = ?")
        .bind(task.state.as_str())
        .bind(task.attempts)
        .bind(serde_json::to_string(task)?)
        .bind(to_rfc3339(now()))
        .bind(task.id.as_str())
        .execute(conn)
        .await?;
    Ok(())
}

async fn finish_task_in_tx(
    conn: &mut SqliteConnection,
    task: &mut Task,
    state: TaskState,
    actual: CostActual,
    error: Option<String>,
) -> Result<()> {
    task.state = state;
    task.actual_cost = actual.clone();
    task.error = error;
    task.finished_at = Some(now());
    store_task(conn, task).await?;
    settle_attempt(conn, task, &actual).await?;
    release_reservation(conn, task).await
}

/// Record what an attempt actually consumed. Keyed by attempt so a retry is
/// charged again and a replay is not.
async fn settle_attempt(
    conn: &mut SqliteConnection,
    task: &Task,
    actual: &CostActual,
) -> Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO budget_ledger (run_id, task_id, settlement_key, kind,
                                              model_calls, tokens, tool_executions,
                                              cost_usd, created_at)
         VALUES (?, ?, ?, 'settle', ?, ?, ?, ?, ?)",
    )
    .bind(task.run_id.as_str())
    .bind(task.id.as_str())
    .bind(format!("settle:{}:{}", task.id, task.attempts))
    .bind(actual.model_calls)
    .bind(actual.total_tokens() as i64)
    .bind(actual.tool_executions)
    .bind(actual.cost_usd)
    .bind(to_rfc3339(now()))
    .execute(conn)
    .await?;
    Ok(())
}

/// Release a task's reservation exactly once.
async fn release_reservation(conn: &mut SqliteConnection, task: &Task) -> Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO budget_ledger (run_id, task_id, settlement_key, kind,
                                              model_calls, tokens, tool_executions,
                                              cost_usd, created_at)
         VALUES (?, ?, ?, 'release', ?, ?, ?, NULL, ?)",
    )
    .bind(task.run_id.as_str())
    .bind(task.id.as_str())
    .bind(format!("release:{}", task.id))
    .bind(task.reservation.model_calls)
    .bind(task.reservation.tokens as i64)
    .bind(task.reservation.tool_executions)
    .bind(to_rfc3339(now()))
    .execute(conn)
    .await?;
    Ok(())
}

async fn usage_from(conn: &mut SqliteConnection, run_id: &RunId) -> Result<BudgetUsage> {
    let rows = sqlx::query(
        "SELECT kind,
                SUM(model_calls)     AS calls,
                SUM(tokens)          AS tokens,
                SUM(tool_executions) AS tools,
                SUM(cost_usd)        AS cost,
                COUNT(*)             AS n,
                SUM(CASE WHEN cost_usd IS NULL THEN 1 ELSE 0 END) AS unknown_cost
         FROM budget_ledger WHERE run_id = ? GROUP BY kind",
    )
    .bind(run_id.as_str())
    .fetch_all(conn)
    .await?;

    let mut usage = BudgetUsage::default();
    let (mut reserved, mut released) = ((0u32, 0u64, 0u32), (0u32, 0u64, 0u32));
    let mut any_cost = false;

    for row in rows {
        let kind: String = row.get("kind");
        let calls = row.get::<Option<i64>, _>("calls").unwrap_or(0) as u32;
        let tokens = row.get::<Option<i64>, _>("tokens").unwrap_or(0) as u64;
        let tools = row.get::<Option<i64>, _>("tools").unwrap_or(0) as u32;

        match kind.as_str() {
            "reserve" => reserved = (calls, tokens, tools),
            "release" => released = (calls, tokens, tools),
            "settle" => {
                usage.model_calls = calls;
                usage.tokens = tokens;
                usage.tool_executions = tools;
                if let Some(cost) = row.get::<Option<f64>, _>("cost") {
                    usage.cost_usd = Some(cost);
                    any_cost = true;
                }
                // A settled call that reported no cost leaves total cost partial.
                usage.cost_partially_unknown =
                    row.get::<Option<i64>, _>("unknown_cost").unwrap_or(0) > 0;
            }
            _ => {}
        }
    }

    // Outstanding reservations are what was reserved minus what was released.
    usage.reserved_calls = reserved.0.saturating_sub(released.0);
    usage.reserved_tokens = reserved.1.saturating_sub(released.1);
    usage.reserved_tool_executions = reserved.2.saturating_sub(released.2);

    if !any_cost {
        usage.cost_usd = None;
    }
    Ok(usage)
}
