//! Live run state over Server-Sent Events.
//!
//! The stream re-derives a complete [`view::RunSnapshot`] from SQLite on a
//! short interval and emits it only when it changed, so the browser always
//! holds one coherent snapshot instead of replaying a granular event
//! protocol. When the run reaches a terminal state the stream emits a final
//! snapshot, an `end` event, and closes.

use super::error::ApiResult;
use super::owner::OwnedRun;
use super::{AppState, view};
use crate::records::RunId;
use crate::store::Store;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use std::convert::Infallible;
use std::time::Duration;

/// Stream phases, advanced by [`step`].
enum Phase {
    /// Emit the first snapshot immediately.
    Initial,
    /// Poll for changes; `last` is the serialized snapshot already sent.
    Poll { last: String },
    /// The run is terminal and its final snapshot is sent: emit `end`.
    End,
    /// Close the stream.
    Closed,
}

fn snapshot_event(json: String) -> Event {
    Event::default().event("snapshot").data(json)
}

fn end_event(run_id: &RunId) -> Event {
    Event::default()
        .event("end")
        .data(serde_json::json!({"run_id": run_id.as_str()}).to_string())
}

fn error_event(e: &crate::Error) -> Event {
    // Same body and redaction rules as HTTP errors (see `error::error_body`).
    Event::default()
        .event("error")
        .data(super::error::error_body(e).to_string())
}

async fn load(store: &Store, run_id: &RunId) -> crate::Result<(String, bool)> {
    let snapshot = view::run_snapshot(store, run_id).await?;
    let terminal = view::is_terminal(&snapshot);
    Ok((serde_json::to_string(&snapshot)?, terminal))
}

/// Produce the next event. `None` from the caller's perspective means the
/// phase advanced without an event and the loop continues.
async fn step(
    store: &Store,
    run_id: &RunId,
    poll: Duration,
    phase: Phase,
) -> Option<(Event, Phase)> {
    match phase {
        Phase::Initial => match load(store, run_id).await {
            Ok((json, terminal)) => {
                let next = if terminal {
                    Phase::End
                } else {
                    Phase::Poll { last: json.clone() }
                };
                Some((snapshot_event(json), next))
            }
            Err(e) => Some((error_event(&e), Phase::Closed)),
        },
        Phase::Poll { mut last } => loop {
            tokio::time::sleep(poll).await;
            match load(store, run_id).await {
                Ok((json, terminal)) => {
                    if json != last {
                        let next = if terminal {
                            Phase::End
                        } else {
                            Phase::Poll { last: json.clone() }
                        };
                        return Some((snapshot_event(json), next));
                    }
                    if terminal {
                        // Terminal and already reported: settle the stream.
                        return Some((end_event(run_id), Phase::Closed));
                    }
                    last = json;
                }
                Err(e) => return Some((error_event(&e), Phase::Closed)),
            }
        },
        Phase::End => Some((end_event(run_id), Phase::Closed)),
        Phase::Closed => None,
    }
}

fn snapshot_stream(
    store: Store,
    run_id: RunId,
    poll: Duration,
) -> impl Stream<Item = Result<Event, Infallible>> {
    futures_util::stream::unfold(Phase::Initial, move |phase| {
        let store = store.clone();
        let run_id = run_id.clone();
        async move {
            step(&store, &run_id, poll, phase)
                .await
                .map(|(event, next)| (Ok(event), next))
        }
    })
}

/// `GET /api/runs/{id}/events`
///
/// [`OwnedRun`] 404s unknown and foreign runs up front instead of opening
/// a dead stream, with the same non-disclosing response for both.
pub async fn run_events(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let stream = snapshot_stream(state.store().clone(), run_id, state.config().sse_poll);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
