//! The durable FIFO run queue and the research clock (docs/WEB.md "One run
//! at a time").
//!
//! Runs execute one at a time in enqueue order. The order is the
//! `run_queue.seq` sequence, assigned once per enqueue and never reused;
//! timestamps play no part. A run has at most one *live* entry
//! (`preparing`, `queued`, or `active`); the head is the live entry with
//! the smallest sequence, and only a `queued` head can be activated. A
//! web run's entry is born `preparing` and becomes `queued` in the same
//! transaction that records its plan, so a run is executable only once
//! its start request has fully prepared it; a `preparing` head whose
//! request died is abandoned (the run fails with `start_incomplete`) and
//! the queue moves on. Every transition is a store fact:
//! enqueue rides in the transaction that creates the run, and leaving the
//! queue rides in the transaction that cancels, blocks, or finishes it, so
//! a restarted server rebuilds exactly the queue the previous one had.
//!
//! The research clock is persisted beside the run: `started_at` is stamped
//! on first activation and the wall-clock budget runs from it, so time
//! spent queued is not research time. A transient provider outage opens a
//! pause (`paused_since`); settling it adds to `paused_secs`, and the
//! effective deadline moves by exactly that much. An open pause survives
//! a restart and is settled when the provider serves again.

use super::{Store, from_rfc3339, now, to_rfc3339};
use crate::records::*;
use crate::store::OwnerDigest;
use crate::{Error, Result};
use sqlx::{Row, SqliteConnection};
use time::OffsetDateTime;

/// Lifecycle of one queue entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueState {
    /// Created, holding its place, but its start request has not yet
    /// finished preparing it (safety, sources, plan); not executable.
    Preparing,
    /// Waiting for every earlier live entry to finish.
    Queued,
    /// The dispatcher is driving this run (it may be waiting for the
    /// provider; it is still the head).
    Active,
    /// Left the queue: finished, cancelled, blocked, or parked.
    Done,
}

impl QueueState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Queued => "queued",
            Self::Active => "active",
            Self::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "preparing" => Self::Preparing,
            "queued" => Self::Queued,
            "active" => Self::Active,
            "done" => Self::Done,
            _ => return None,
        })
    }
}

/// One row of the run queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    /// FIFO position: smaller runs first. Never reused.
    pub seq: i64,
    pub run_id: RunId,
    pub state: QueueState,
    pub enqueued_at: OffsetDateTime,
    pub activated_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

/// Where a run stands in the queue: its entry state and how many live
/// entries precede it (`0` means it is the head).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueStatus {
    pub state: QueueState,
    pub ahead: u32,
}

/// The persisted research clock of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTiming {
    pub created_at: OffsetDateTime,
    /// When the run first executed; `None` while it is still queued.
    pub started_at: Option<OffsetDateTime>,
    /// Settled provider waits, in whole seconds.
    pub paused_secs: u64,
    /// The provider wait currently open, if any.
    pub paused_since: Option<OffsetDateTime>,
    /// The last transient failure reported for the open wait.
    pub pause_note: Option<String>,
}

impl RunTiming {
    /// Whether a provider wait is open.
    pub fn is_paused(&self) -> bool {
        self.paused_since.is_some()
    }

    /// The wall-clock deadline as of now: start plus the budget plus every
    /// settled wait. `None` while the run has not started. An *open* wait
    /// is not folded in here; see [`Self::deadline_passed`].
    pub fn deadline(&self, wall_clock_secs: u64) -> Option<OffsetDateTime> {
        let budget = time::Duration::seconds(i64::try_from(wall_clock_secs).unwrap_or(i64::MAX));
        let paused = time::Duration::seconds(i64::try_from(self.paused_secs).unwrap_or(i64::MAX));
        self.started_at
            .map(|started| started.saturating_add(budget).saturating_add(paused))
    }

    /// Whether the budget is spent at `now`. Never while a wait is open:
    /// a paused clock cannot run out.
    pub fn deadline_passed(&self, wall_clock_secs: u64, now: OffsetDateTime) -> bool {
        if self.is_paused() {
            return false;
        }
        self.deadline(wall_clock_secs)
            .is_some_and(|deadline| now >= deadline)
    }
}

const LIVE: &str = "('preparing', 'queued', 'active')";

fn entry_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<QueueEntry> {
    let state = QueueState::parse(&row.get::<String, _>("state"))
        .ok_or_else(|| Error::validation("unknown queue state"))?;
    Ok(QueueEntry {
        seq: row.get::<i64, _>("seq"),
        run_id: RunId::from_raw(row.get::<String, _>("run_id")),
        state,
        enqueued_at: from_rfc3339(&row.get::<String, _>("enqueued_at"))?,
        activated_at: row
            .get::<Option<String>, _>("activated_at")
            .as_deref()
            .map(from_rfc3339)
            .transpose()?,
        finished_at: row
            .get::<Option<String>, _>("finished_at")
            .as_deref()
            .map(from_rfc3339)
            .transpose()?,
    })
}

/// Insert a live entry for `run_id` at the tail in `initial` state,
/// unless a live one exists. Returns whether a row was inserted.
pub(crate) async fn enqueue_in_tx(
    conn: &mut SqliteConnection,
    run_id: &RunId,
    initial: QueueState,
) -> Result<bool> {
    let live: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT seq FROM run_queue WHERE run_id = ? AND state IN {LIVE}"
    ))
    .bind(run_id.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    if live.is_some() {
        return Ok(false);
    }
    sqlx::query("INSERT INTO run_queue (run_id, state, enqueued_at) VALUES (?, ?, ?)")
        .bind(run_id.as_str())
        .bind(initial.as_str())
        .bind(to_rfc3339(now()))
        .execute(conn)
        .await?;
    Ok(true)
}

/// Mark a `preparing` entry `queued`: the run is fully prepared and may
/// execute when it reaches the head. Returns whether the transition
/// happened.
pub(crate) async fn ready_queue_entry_in_tx(
    conn: &mut SqliteConnection,
    run_id: &RunId,
) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE run_queue SET state = 'queued' WHERE run_id = ? AND state = 'preparing'",
    )
    .bind(run_id.as_str())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Close the live entry of `run_id`, if any. Idempotent.
pub(crate) async fn close_queue_entry_in_tx(
    conn: &mut SqliteConnection,
    run_id: &RunId,
) -> Result<()> {
    sqlx::query(&format!(
        "UPDATE run_queue SET state = 'done', finished_at = ?
         WHERE run_id = ? AND state IN {LIVE}"
    ))
    .bind(to_rfc3339(now()))
    .bind(run_id.as_str())
    .execute(conn)
    .await?;
    Ok(())
}

/// Stamp the research clock's start, once.
pub(crate) async fn stamp_started_in_tx(conn: &mut SqliteConnection, run_id: &RunId) -> Result<()> {
    sqlx::query("UPDATE runs SET started_at = COALESCE(started_at, ?) WHERE id = ?")
        .bind(to_rfc3339(now()))
        .bind(run_id.as_str())
        .execute(conn)
        .await?;
    Ok(())
}

impl Store {
    /// Create a run, its first goal revision, its owner binding when there
    /// is one, and its queue place in one transaction: a web run is either
    /// fully created *and* in the queue, or not created at all. The entry
    /// is `preparing`: it holds the run's place in the order but cannot
    /// execute until [`Store::insert_plan_and_ready`] completes the
    /// preparation.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Storage`] when any insert cannot be committed;
    /// nothing is persisted in that case.
    pub async fn create_run_queued(
        &self,
        run: &Run,
        goal: &Goal,
        owner: Option<&OwnerDigest>,
    ) -> Result<QueueEntry> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        super::writes::insert_run_and_goal_in_tx(conn, run, goal).await?;
        if let Some(owner) = owner {
            super::owners::insert_owner_in_tx(conn, &run.id, owner).await?;
        }
        enqueue_in_tx(conn, &run.id, QueueState::Preparing).await?;
        let entry = live_entry(conn, &run.id)
            .await?
            .ok_or_else(|| Error::not_found(format!("queue entry for run {}", run.id)))?;
        guard.commit().await?;
        Ok(entry)
    }

    /// Create a run that is refused before it starts: the run (in the
    /// `blocked` state the caller set), its goal, its owner binding when
    /// there is one, and the decision explaining the refusal, in one
    /// transaction and with no queue place, so it can never execute.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Storage`] when any insert cannot be committed;
    /// nothing is persisted in that case.
    pub async fn create_run_blocked(
        &self,
        run: &Run,
        goal: &Goal,
        owner: Option<&OwnerDigest>,
        decision: &Decision,
    ) -> Result<()> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        super::writes::insert_run_and_goal_in_tx(conn, run, goal).await?;
        if let Some(owner) = owner {
            super::owners::insert_owner_in_tx(conn, &run.id, owner).await?;
        }
        super::writes::insert_decision_in_tx(conn, decision).await?;
        guard.commit().await
    }

    /// Give an existing, already prepared run a place at the tail of the
    /// queue, unless it already holds a live one. Returns whether it was
    /// enqueued.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn enqueue_run(&self, run_id: &RunId) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let inserted = enqueue_in_tx(guard.conn(), run_id, QueueState::Queued).await?;
        guard.commit().await?;
        Ok(inserted)
    }

    /// Complete a web run's preparation: record its plan and make its
    /// `preparing` entry `queued` in one transaction. Returns `false`
    /// (recording the plan all the same) when the entry was not
    /// `preparing` — it was abandoned or stopped meanwhile — so the caller
    /// knows the run will not execute.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn insert_plan_and_ready(&self, plan: &Plan) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        super::writes::insert_plan_in_tx(conn, plan).await?;
        let ready = ready_queue_entry_in_tx(conn, &plan.run_id).await?;
        guard.commit().await?;
        Ok(ready)
    }

    /// Make a `preparing` entry `queued` without a plan (the scheduler
    /// plans a run that has none). Returns whether the transition
    /// happened.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn ready_queue_entry(&self, run_id: &RunId) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let ready = ready_queue_entry_in_tx(guard.conn(), run_id).await?;
        guard.commit().await?;
        Ok(ready)
    }

    /// Give up on a run whose start request never finished preparing it:
    /// the run fails with [`StopCondition::StartIncomplete`], the reason
    /// is recorded as a decision, and its queue place is closed, all in
    /// one transaction. Only a `queued` run with a `preparing` entry is
    /// affected; returns whether anything changed.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn abandon_preparation(&self, run_id: &RunId, reason: &str) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let preparing: Option<i64> = sqlx::query_scalar(
            "SELECT seq FROM run_queue WHERE run_id = ? AND state = 'preparing'",
        )
        .bind(run_id.as_str())
        .fetch_optional(&mut *conn)
        .await?;
        if preparing.is_none() {
            return Ok(false);
        }
        let result = sqlx::query(
            "UPDATE runs SET state = ?, stop_condition = ?, updated_at = ?
             WHERE id = ? AND state = ?",
        )
        .bind(RunState::Failed.as_str())
        .bind(StopCondition::StartIncomplete.as_str())
        .bind(to_rfc3339(now()))
        .bind(run_id.as_str())
        .bind(RunState::Queued.as_str())
        .execute(&mut *conn)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(false);
        }
        close_queue_entry_in_tx(conn, run_id).await?;
        let decision = Decision {
            id: DecisionId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: run_id.clone(),
            kind: DecisionKind::Stop,
            actor: Actor::System,
            reason: format!("start abandoned: {reason}"),
            referenced: vec![],
            payload: serde_json::json!({ "start_incomplete": reason }),
            created_at: now(),
        };
        super::writes::insert_decision_in_tx(conn, &decision).await?;
        guard.commit().await?;
        Ok(true)
    }

    /// Every live entry, head first.
    pub async fn list_queue(&self) -> Result<Vec<QueueEntry>> {
        let rows = sqlx::query(&format!(
            "SELECT seq, run_id, state, enqueued_at, activated_at, finished_at
             FROM run_queue WHERE state IN {LIVE} ORDER BY seq"
        ))
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(entry_from_row).collect()
    }

    /// The live entry with the smallest sequence, if any.
    pub async fn queue_head(&self) -> Result<Option<QueueEntry>> {
        let row = sqlx::query(&format!(
            "SELECT seq, run_id, state, enqueued_at, activated_at, finished_at
             FROM run_queue WHERE state IN {LIVE} ORDER BY seq LIMIT 1"
        ))
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(entry_from_row).transpose()
    }

    /// Where `run_id` stands, or `None` when it holds no live entry.
    pub async fn queue_status(&self, run_id: &RunId) -> Result<Option<QueueStatus>> {
        let row = sqlx::query(&format!(
            "SELECT q.state,
                    (SELECT COUNT(*) FROM run_queue q2
                      WHERE q2.state IN {LIVE} AND q2.seq < q.seq) AS ahead
             FROM run_queue q WHERE q.run_id = ? AND q.state IN {LIVE}"
        ))
        .bind(run_id.as_str())
        .fetch_optional(self.pool())
        .await?;
        row.map(|r| {
            let state = QueueState::parse(&r.get::<String, _>("state"))
                .ok_or_else(|| Error::validation("unknown queue state"))?;
            Ok(QueueStatus {
                state,
                ahead: u32::try_from(r.get::<i64, _>("ahead")).unwrap_or(u32::MAX),
            })
        })
        .transpose()
    }

    /// Make the head run active: its entry becomes `active`, the run
    /// `running` (a durable stop request wins), and its research clock
    /// starts if it never has. Returns `false`, changing nothing, when
    /// `run_id` is not the queued head — another run precedes it, it is
    /// already active, or it holds no live entry — so concurrent
    /// claimants resolve to exactly one activation.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn activate_queue_entry(&self, run_id: &RunId) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let head = sqlx::query(&format!(
            "SELECT run_id, state FROM run_queue WHERE state IN {LIVE} ORDER BY seq LIMIT 1"
        ))
        .fetch_optional(&mut *conn)
        .await?;
        let is_queued_head = head.is_some_and(|row| {
            row.get::<String, _>("run_id") == run_id.as_str()
                && row.get::<String, _>("state") == QueueState::Queued.as_str()
        });
        if !is_queued_head {
            return Ok(false);
        }
        let ts = to_rfc3339(now());
        sqlx::query(
            "UPDATE run_queue SET state = 'active', activated_at = ?
             WHERE run_id = ? AND state = 'queued'",
        )
        .bind(&ts)
        .bind(run_id.as_str())
        .execute(&mut *conn)
        .await?;
        let started = sqlx::query(
            "UPDATE runs SET state = ?, started_at = COALESCE(started_at, ?), updated_at = ?
             WHERE id = ? AND state != ?",
        )
        .bind(RunState::Running.as_str())
        .bind(&ts)
        .bind(&ts)
        .bind(run_id.as_str())
        .bind(RunState::Cancelled.as_str())
        .execute(&mut *conn)
        .await?;
        if started.rows_affected() == 0 {
            // A stop landed first; the entry is already closed by the
            // cancel, but never leave an active row for a cancelled run.
            close_queue_entry_in_tx(conn, run_id).await?;
            guard.commit().await?;
            return Ok(false);
        }
        guard.commit().await?;
        Ok(true)
    }

    /// Take `run_id` off the queue. Idempotent.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn finish_queue_entry(&self, run_id: &RunId) -> Result<()> {
        let mut guard = self.begin_write().await?;
        close_queue_entry_in_tx(guard.conn(), run_id).await?;
        guard.commit().await
    }

    /// The persisted research clock of `run_id`.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] for an unknown run.
    pub async fn run_timing(&self, run_id: &RunId) -> Result<RunTiming> {
        let row = sqlx::query(
            "SELECT created_at, started_at, paused_secs, paused_since, pause_note
             FROM runs WHERE id = ?",
        )
        .bind(run_id.as_str())
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| Error::not_found(format!("run {run_id}")))?;
        Ok(RunTiming {
            created_at: from_rfc3339(&row.get::<String, _>("created_at"))?,
            started_at: row
                .get::<Option<String>, _>("started_at")
                .as_deref()
                .map(from_rfc3339)
                .transpose()?,
            paused_secs: u64::try_from(row.get::<i64, _>("paused_secs")).unwrap_or(0),
            paused_since: row
                .get::<Option<String>, _>("paused_since")
                .as_deref()
                .map(from_rfc3339)
                .transpose()?,
            pause_note: row.get::<Option<String>, _>("pause_note"),
        })
    }

    /// Open a provider wait at `since` (kept if one is already open) and
    /// record `note`, the transient failure last observed. The wait never
    /// starts before the run itself did: an outage that began during an
    /// earlier run is credited only from this run's start.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn begin_provider_wait(
        &self,
        run_id: &RunId,
        since: OffsetDateTime,
        note: &str,
    ) -> Result<()> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let started: Option<String> =
            sqlx::query_scalar("SELECT started_at FROM runs WHERE id = ?")
                .bind(run_id.as_str())
                .fetch_optional(&mut *conn)
                .await?
                .flatten();
        let floor = started.as_deref().map(from_rfc3339).transpose()?;
        let since = floor.map_or(since, |started| since.max(started));
        sqlx::query(
            "UPDATE runs SET paused_since = COALESCE(paused_since, ?), pause_note = ?,
                             updated_at = ? WHERE id = ?",
        )
        .bind(to_rfc3339(since))
        .bind(note)
        .bind(to_rfc3339(now()))
        .bind(run_id.as_str())
        .execute(&mut *conn)
        .await?;
        guard.commit().await
    }

    /// Settle the open provider wait as ending at `until`: its whole
    /// seconds (rounded up) join `paused_secs`. Returns `false`, changing
    /// nothing, when no wait is open.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn end_provider_wait(&self, run_id: &RunId, until: OffsetDateTime) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let since: Option<String> =
            sqlx::query_scalar("SELECT paused_since FROM runs WHERE id = ?")
                .bind(run_id.as_str())
                .fetch_optional(&mut *conn)
                .await?
                .flatten();
        let Some(since) = since else {
            return Ok(false);
        };
        let waited = (until - from_rfc3339(&since)?).max(time::Duration::ZERO);
        // Round up: a wait of any fraction of a second credits the whole
        // second, never shortchanging the run.
        let millis = waited.whole_milliseconds();
        let secs = i64::try_from((millis + 999) / 1000).unwrap_or(i64::MAX);
        sqlx::query(
            "UPDATE runs SET paused_secs = paused_secs + ?, paused_since = NULL,
                             pause_note = NULL, updated_at = ? WHERE id = ?",
        )
        .bind(secs)
        .bind(to_rfc3339(now()))
        .bind(run_id.as_str())
        .execute(&mut *conn)
        .await?;
        guard.commit().await?;
        Ok(true)
    }
}

async fn live_entry(conn: &mut SqliteConnection, run_id: &RunId) -> Result<Option<QueueEntry>> {
    let row = sqlx::query(&format!(
        "SELECT seq, run_id, state, enqueued_at, activated_at, finished_at
         FROM run_queue WHERE run_id = ? AND state IN {LIVE}"
    ))
    .bind(run_id.as_str())
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(entry_from_row).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn timing(
        started: Option<OffsetDateTime>,
        paused_secs: u64,
        open: Option<OffsetDateTime>,
    ) -> RunTiming {
        RunTiming {
            created_at: datetime!(2026-10-09 12:00:00 UTC),
            started_at: started,
            paused_secs,
            paused_since: open,
            pause_note: None,
        }
    }

    #[test]
    fn deadline_is_start_plus_budget_plus_settled_waits() {
        let started = datetime!(2026-10-09 12:05:00 UTC);
        let t = timing(Some(started), 30, None);
        assert_eq!(
            t.deadline(60),
            Some(datetime!(2026-10-09 12:06:30 UTC)),
            "queued time before the start is not counted"
        );
    }

    #[test]
    fn a_run_that_has_not_started_has_no_deadline() {
        assert_eq!(timing(None, 0, None).deadline(60), None);
        assert!(!timing(None, 0, None).deadline_passed(60, datetime!(2030-01-01 0:00 UTC)));
    }

    #[test]
    fn an_open_wait_never_lets_the_deadline_pass() {
        let started = datetime!(2026-10-09 12:00:00 UTC);
        let t = timing(
            Some(started),
            0,
            Some(started + time::Duration::seconds(10)),
        );
        assert!(!t.deadline_passed(60, started + time::Duration::hours(5)));
        let settled = timing(Some(started), 0, None);
        assert!(settled.deadline_passed(60, started + time::Duration::seconds(60)));
        assert!(!settled.deadline_passed(60, started + time::Duration::seconds(59)));
    }
}
