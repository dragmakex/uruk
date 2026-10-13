//! Durable run control requests, issued by the web API's stop route.

use crate::records::*;
use crate::store::Store;
use crate::{Error, Result};

/// What a stop request changed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StopOutcome {
    pub run_id: RunId,
    /// Tasks cancelled before dispatch.
    pub cancelled_tasks: u32,
    pub decision_id: DecisionId,
}

/// Request that a run stop (SPEC §9.2).
///
/// A durable control request, written as one transaction ([`Store::cancel_run`]):
/// the run is cancelled and leaves the queue, its undispatched tasks are
/// cancelled with their reservations, and the decision is recorded — or
/// nothing happens at all. The owning scheduler observes the cancelled
/// state and settles in-flight work; a stopped run stays stopped across
/// restarts. Stopping an already-terminal run is refused so a finished
/// state is never silently overwritten, and a retried stop that finds the
/// run cancelled is told exactly that.
pub async fn request_stop(store: &Store, run_id: &RunId) -> Result<StopOutcome> {
    // 404 for unknown runs; the atomic cancel below handles the race
    // against a run that finishes between this read and the update.
    store.get_run(run_id).await?;

    let Some(cancelled) = store
        .cancel_run(run_id, "researcher requested stop")
        .await?
    else {
        let run = store.get_run(run_id).await?;
        return Err(Error::validation(format!(
            "run {run_id} already finished ({}); nothing to stop",
            run.state.as_str()
        )));
    };

    Ok(StopOutcome {
        run_id: run_id.clone(),
        cancelled_tasks: cancelled.cancelled_tasks,
        decision_id: cancelled.decision_id,
    })
}
