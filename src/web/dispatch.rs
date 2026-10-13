//! The run dispatcher: one task per server, executing the durable run
//! queue head to head (docs/WEB.md "One run at a time").
//!
//! SQLite is the authority: the dispatcher reads the head of `run_queue`,
//! activates it, runs the scheduler to completion, closes the entry, and
//! looks again. The in-process [`tokio::sync::Notify`] on [`AppState`] is
//! only a wakeup — enqueue, stop, and the sweep ring it so the loop does
//! not sit through its idle tick — and a periodic tick covers any wakeup
//! that was missed. Nothing in memory decides the order or holds a claim:
//! a restarted server reads the same head and resumes it.

use super::AppState;
use crate::records::{ApprovalState, RunId, RunState};
use crate::runtime::{Scheduler, SchedulerConfig};
use crate::store::QueueState;
use crate::{Result, report};
use std::time::Duration;

/// What the dispatcher found at the head of the queue.
enum Head {
    /// Nothing live, or the head is still being prepared by its start
    /// request: wait for a wakeup or the next tick.
    Wait,
    /// A run to execute now.
    Execute(RunId),
}

/// How one execution of the head ended.
enum Execution {
    /// The run is over (or parked); its place was released.
    Released,
    /// The scheduler failed while the run is still open: the head keeps
    /// its place and is resumed after a pause.
    Retry,
}

/// The longest pause between retries of a failing head.
const RETRY_CAP: Duration = Duration::from_secs(30);

/// Run the dispatcher loop until the task is aborted.
pub(super) async fn run(state: AppState) {
    let tick = state
        .config()
        .reconcile_interval
        .max(Duration::from_millis(10));
    // Consecutive scheduler failures of the current head; resets when a
    // head is released.
    let mut failures = 0u32;
    loop {
        match next_head(&state).await {
            Ok(Head::Execute(run_id)) => match execute(&state, &run_id).await {
                Execution::Released => failures = 0,
                Execution::Retry => {
                    failures = failures.saturating_add(1);
                    let pause = tick.saturating_mul(1u32 << failures.min(16)).min(RETRY_CAP);
                    tracing::warn!(
                        run = %run_id,
                        failures,
                        pause_ms = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX),
                        "the head's scheduler failed; it keeps its place and is retried"
                    );
                    tokio::select! {
                        _ = state.queue_wake().notified() => {}
                        _ = tokio::time::sleep(pause) => {}
                    }
                }
            },
            Ok(Head::Wait) => {
                tokio::select! {
                    _ = state.queue_wake().notified() => {}
                    _ = tokio::time::sleep(tick) => {}
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "dispatcher could not read the run queue");
                tokio::time::sleep(tick).await;
            }
        }
    }
}

/// Resolve the head of the queue into something to do. Entries whose run
/// is no longer eligible — finished, blocked, parked for a person — are
/// closed here, and a `preparing` entry no live start request owns is
/// abandoned, so a stale entry can never wedge the queue.
async fn next_head(state: &AppState) -> Result<Head> {
    let store = state.store();
    let Some(entry) = store.queue_head().await? else {
        return Ok(Head::Wait);
    };
    if entry.state == QueueState::Preparing {
        if state.is_preparing(&entry.run_id) {
            // Its start request is still ingesting sources and planning;
            // the place is held, the execution waits.
            return Ok(Head::Wait);
        }
        // No request in this process is preparing it: the one that created
        // it died (a crash, a client timeout) before the run was safe and
        // complete to execute. It is never run as it is.
        tracing::warn!(run = %entry.run_id, "abandoning a run whose start request did not complete");
        store
            .abandon_preparation(
                &entry.run_id,
                "the start request did not complete the run's preparation",
            )
            .await?;
        return Ok(Head::Wait);
    }
    let run = store.get_run(&entry.run_id).await?;
    if !eligible(store, &run).await? {
        tracing::warn!(run = %entry.run_id, state = run.state.as_str(), "closing a stale queue entry");
        store.finish_queue_entry(&entry.run_id).await?;
        return Ok(Head::Wait);
    }
    match entry.state {
        // An interrupted head keeps its place: resume it, do not re-queue.
        QueueState::Active => Ok(Head::Execute(entry.run_id)),
        QueueState::Queued => {
            if store.activate_queue_entry(&entry.run_id).await? {
                Ok(Head::Execute(entry.run_id))
            } else {
                // A stop landed between the read and the activation.
                Ok(Head::Wait)
            }
        }
        QueueState::Preparing | QueueState::Done => Ok(Head::Wait),
    }
}

/// Whether a run may (still) execute.
async fn eligible(store: &crate::store::Store, run: &crate::records::Run) -> Result<bool> {
    Ok(match run.state {
        RunState::Queued | RunState::Running => true,
        RunState::WaitingForHuman => store
            .list_approvals(&run.id, Some(ApprovalState::Pending))
            .await?
            .is_empty(),
        RunState::Blocked
        | RunState::Completed
        | RunState::BudgetExhausted
        | RunState::Cancelled
        | RunState::Failed => false,
    })
}

/// Drive one run, then either release its place (it is over, blocked, or
/// parked) or keep it for a retry (the scheduler failed while the run is
/// still open — a store fault, a corrupt record — and giving the place up
/// would let the next run overtake it). Stop requests reach the scheduler
/// through the persisted run state (SPEC §9.2), so no handle is kept.
async fn execute(state: &AppState, run_id: &RunId) -> Execution {
    let scheduler = Scheduler::new(
        state.store().clone(),
        state.provider().clone(),
        SchedulerConfig::default(),
    );
    state.set_executing(Some(run_id.clone()));
    let result = scheduler.run(run_id).await;
    state.set_executing(None);
    match result {
        Ok(summary) => tracing::info!(
            run = %run_id,
            state = summary.state.as_str(),
            "run finished"
        ),
        Err(e) => {
            tracing::error!(run = %run_id, error = %e, "run scheduler failed");
            let still_open = match state.store().get_run(run_id).await {
                Ok(run) => matches!(run.state, RunState::Queued | RunState::Running),
                Err(e) => {
                    tracing::error!(run = %run_id, error = %e, "could not read the failed run");
                    true
                }
            };
            if still_open {
                return Execution::Retry;
            }
        }
    }
    // Export whatever state the run reached; a partial export is marked
    // as partial rather than hidden (SPEC §6).
    if let Err(e) = report::export_run(state.store(), run_id).await {
        tracing::error!(run = %run_id, error = %e, "report export failed");
    }
    if let Err(e) = state.store().finish_queue_entry(run_id).await {
        tracing::error!(run = %run_id, error = %e, "could not release the run's queue place");
        return Execution::Retry;
    }
    Execution::Released
}
