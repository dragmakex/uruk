//! HTTP handlers. Thin: validate, call the store or a shared service,
//! serialize. No business logic lives here.

use super::error::{ApiError, ApiResult, rejection_to_error};
use super::owner::{OwnedRun, Owner};
use super::{AppState, start, view};
use crate::records::RunId;
use crate::runtime;
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
    let validated = start::validate(request)?;
    if validated.dry_run {
        // A dry run persists nothing: the serve process auto-resumes every
        // persisted `running` run, so a parked-but-created run (the CLI's
        // dry-run shape) would dispatch itself seconds later.
        return Ok(StartOutcome::Preview(
            start::preview(state, validated, owner).await?,
        ));
    }
    Ok(StartOutcome::Created(
        start::start_run(state, validated, owner).await?,
    ))
}

pub async fn stop_run(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Json<serde_json::Value>> {
    let outcome = runtime::request_stop(state.store(), &run_id).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "run_id": outcome.run_id.as_str(),
        "cancelled_tasks": outcome.cancelled_tasks,
    })))
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
