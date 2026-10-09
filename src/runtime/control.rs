//! Durable run control requests, issued by the web API's stop route.

use crate::records::*;
use crate::store::Store;
use crate::{Error, Result};
use time::OffsetDateTime;

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
/// A durable control request: the owning scheduler observes the cancelled
/// state, cancels in-flight work, and a stopped run stays stopped across
/// restarts. Stopping an already-terminal run is refused so a finished
/// state is never silently overwritten.
pub async fn request_stop(store: &Store, run_id: &RunId) -> Result<StopOutcome> {
    // 404 for unknown runs; the atomic cancel below handles the race
    // against a run that finishes between this read and the update.
    store.get_run(run_id).await?;

    if !store.try_cancel_run(run_id).await? {
        let run = store.get_run(run_id).await?;
        return Err(Error::validation(format!(
            "run {run_id} already finished ({}); nothing to stop",
            run.state.as_str()
        )));
    }
    let cancelled = store.cancel_pending_tasks(run_id).await?;

    let decision = Decision {
        id: DecisionId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        kind: DecisionKind::Stop,
        actor: Actor::Researcher,
        reason: "researcher requested stop".into(),
        referenced: vec![],
        payload: serde_json::json!({"cancelled_tasks": cancelled}),
        created_at: OffsetDateTime::now_utc(),
    };
    store.insert_decision(&decision).await?;

    Ok(StopOutcome {
        run_id: run_id.clone(),
        cancelled_tasks: cancelled,
        decision_id: decision.id,
    })
}
