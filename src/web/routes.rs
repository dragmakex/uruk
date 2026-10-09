//! HTTP handlers. Thin: validate, call the store or a shared service,
//! serialize. No business logic lives here.

use super::error::{ApiError, ApiResult, rejection_to_error};
use super::owner::{OwnedRun, Owner};
use super::{AppState, start, view};
use crate::records::{Actor, Decision, DecisionId, DecisionKind, RunId, RunState, SCHEMA_VERSION};
use crate::{Error, Result};
use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "service": "uruk",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

pub async fn list_runs(
    State(state): State<AppState>,
    Owner(owner): Owner,
) -> ApiResult<Json<serde_json::Value>> {
    let runs = state.store().list_run_overviews_owned(&owner).await?;
    Ok(Json(serde_json::json!({"ok": true, "runs": runs})))
}

pub async fn run_snapshot(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Json<serde_json::Value>> {
    let snapshot = view::run_snapshot(state.store(), &run_id).await?;
    Ok(Json(serde_json::json!({"ok": true, "snapshot": snapshot})))
}

pub async fn start_run(
    State(state): State<AppState>,
    Owner(owner): Owner,
    payload: std::result::Result<Json<start::StartRunRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(json) => json,
        Err(rejection) => return rejection_to_error(rejection),
    };
    match create_or_preview(&state, request, &owner).await {
        Ok(StartOutcome::Created(run_id)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"ok": true, "run_id": run_id.as_str()})),
        )
            .into_response(),
        Ok(StartOutcome::Preview(preview)) => (StatusCode::OK, Json(preview)).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

enum StartOutcome {
    Created(RunId),
    Preview(serde_json::Value),
}

async fn create_or_preview(
    state: &AppState,
    request: start::StartRunRequest,
    owner: &crate::store::OwnerDigest,
) -> Result<StartOutcome> {
    let stored = serde_json::to_string(&request)?;
    let validated = start::validate(request)?;
    if validated.dry_run {
        // A dry run persists nothing: the serve process auto-resumes every
        // persisted `running` run, so a parked-but-created run (the CLI's
        // dry-run shape) would dispatch itself seconds later.
        return Ok(StartOutcome::Preview(
            start::preview(state, validated, owner).await?,
        ));
    }
    let run_id = start::start_run(state, validated, owner).await?;
    state.store().save_web_run_config(&run_id, &stored).await?;
    Ok(StartOutcome::Created(run_id))
}

pub async fn stop_run(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Json<serde_json::Value>> {
    let paused = serde_json::json!({
        "ok": true, "run_id": run_id.as_str(), "state": "paused",
    });
    if state.store().try_pause_run(&run_id).await? {
        record_lifecycle(
            state.store(),
            &run_id,
            DecisionKind::Pause,
            "research paused",
            vec![],
            serde_json::json!({}),
        )
        .await?;
        return Ok(Json(paused));
    }
    // The guarded transition refused: answer from the state that won, so a
    // stop racing another stop (or the run finishing) stays idempotent.
    let current = state.store().get_run(&run_id).await?;
    if current.state == RunState::Paused {
        return Ok(Json(paused));
    }
    if current.state.is_terminal() {
        return Err(ApiError(Error::validation(
            "a finished run cannot be paused",
        )));
    }
    Err(ApiError(Error::conflict(
        "only a running or waiting run can be paused",
    )))
}

pub async fn resume_run(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Json<serde_json::Value>> {
    if state.store().try_resume_run(&run_id).await? {
        record_lifecycle(
            state.store(),
            &run_id,
            DecisionKind::Resume,
            "research resumed",
            vec![],
            serde_json::json!({}),
        )
        .await?;
        start::dispatch_when_available(state.clone(), run_id.clone());
        return Ok(Json(serde_json::json!({
            "ok": true, "run_id": run_id.as_str(), "state": "running", "resumed": true,
        })));
    }
    // The guarded transition refused: answer from the state that won.
    let current = state.store().get_run(&run_id).await?;
    if current.state == RunState::Running {
        return Ok(Json(serde_json::json!({
            "ok": true, "run_id": run_id.as_str(), "state": "running", "resumed": false,
        })));
    }
    if current.state.is_terminal() {
        return Err(ApiError(Error::validation(
            "a finished run cannot be resumed",
        )));
    }
    Err(ApiError(Error::conflict(
        "only a paused run can be resumed",
    )))
}

pub async fn restart_run(
    State(state): State<AppState>,
    Owner(owner): Owner,
    OwnedRun(old_id): OwnedRun,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let body = state
        .store()
        .web_run_config(&old_id)
        .await?
        .ok_or_else(|| {
            ApiError(Error::validation(
                "this run has no saved browser start configuration",
            ))
        })?;
    // Restarting a run that is still working would silently double its
    // spend; it must pause or finish first. (The missing-configuration
    // check comes first because that condition is permanent.)
    let original = state.store().get_run(&old_id).await?;
    if matches!(
        original.state,
        RunState::Running | RunState::WaitingForHuman
    ) {
        return Err(ApiError(Error::conflict(
            "the run is still active; pause it or let it finish before restarting",
        )));
    }
    let mut request: start::StartRunRequest = serde_json::from_str(&body).map_err(Error::from)?;
    request.dry_run = false;
    let new_id = start::start_run(&state, start::validate(request.clone())?, &owner).await?;
    let encoded = serde_json::to_string(&request).map_err(Error::from)?;
    state.store().save_web_run_config(&new_id, &encoded).await?;
    record_lifecycle(
        state.store(),
        &old_id,
        DecisionKind::Restart,
        "fresh run created from saved intent",
        vec![new_id.to_string()],
        serde_json::json!({ "restarted_as": new_id.as_str() }),
    )
    .await?;
    record_lifecycle(
        state.store(),
        &new_id,
        DecisionKind::Restart,
        "fresh run created from saved intent",
        vec![old_id.to_string()],
        serde_json::json!({ "restarted_from": old_id.as_str() }),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "ok": true,
            "run_id": new_id.as_str(),
            "restarted_from": old_id.as_str(),
        })),
    ))
}

async fn record_lifecycle(
    store: &crate::store::Store,
    run_id: &RunId,
    kind: DecisionKind,
    reason: &str,
    referenced: Vec<String>,
    payload: serde_json::Value,
) -> Result<()> {
    store
        .insert_decision(&Decision {
            id: DecisionId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: run_id.clone(),
            kind,
            actor: Actor::Researcher,
            reason: reason.into(),
            referenced,
            payload,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
}

pub async fn report(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Json<serde_json::Value>> {
    let dir = state.store().run_dir(&run_id);
    let markdown = tokio::fs::read_to_string(dir.join("REPORT.md"))
        .await
        .map_err(|_| {
            Error::not_found(format!(
                "run {run_id} has no exported report yet; it is written when the run finishes"
            ))
        })?;
    // The manifest is supplementary; a missing or unreadable one is not an
    // error, it is just absent.
    let manifest = tokio::fs::read_to_string(dir.join("manifest.json"))
        .await
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());

    Ok(Json(serde_json::json!({
        "ok": true,
        "run_id": run_id.as_str(),
        "markdown": markdown,
        "manifest": manifest,
    })))
}

pub async fn sources(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Json<serde_json::Value>> {
    let sources = state.store().list_sources(&run_id).await?;
    Ok(Json(serde_json::json!({"ok": true, "sources": sources})))
}

pub async fn library(
    State(state): State<AppState>,
    Owner(owner): Owner,
) -> ApiResult<Json<serde_json::Value>> {
    let sources = state.store().list_sources_owned(&owner).await?;
    Ok(Json(serde_json::json!({"ok": true, "sources": sources})))
}

#[derive(Debug, serde::Deserialize)]
pub struct PassagesQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

pub async fn passages(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
    query: std::result::Result<Query<PassagesQuery>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    // An unparsable query string is a caller mistake and must carry the
    // stable JSON error body, not axum's plain-text rejection.
    let Query(query) = query.map_err(|r| Error::validation(r.body_text()))?;

    let q = query.q.trim();
    if q.is_empty() {
        return Err(ApiError(Error::validation("q must not be empty")));
    }
    let limit = query.limit.unwrap_or(10).clamp(1, 100);
    let passages = state.store().search_passages(&run_id, q, limit).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "run_id": run_id.as_str(),
        "query": q,
        "passages": passages,
    })))
}

/// JSON 404 for unknown routes, same body shape as every other error.
pub async fn unknown_route() -> ApiError {
    ApiError(Error::not_found("no such API route"))
}
