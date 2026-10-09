//! HTTP handlers. Thin: validate, call the store or a shared service,
//! serialize. No business logic lives here.

use super::error::{ApiError, ApiResult, rejection_to_error};
use super::owner::{OwnedRun, OwnedSource, Owner};
use super::{AppState, start, view};
use crate::records::{AccessLevel, Origin, RunId};
use crate::runtime;
use crate::store::SourceFilter;
use crate::{Error, Result};
use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
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

/// The run's canonical `REPORT.md`, or the stable not-found error a run
/// without an export answers with. Only reached through [`OwnedRun`], so
/// the id names an existing, owner-checked run before any path is formed.
async fn read_report_markdown(state: &AppState, run_id: &RunId) -> Result<String> {
    tokio::fs::read_to_string(state.store().run_dir(run_id).join("REPORT.md"))
        .await
        .map_err(|_| {
            Error::not_found(format!(
                "run {run_id} has no exported report yet; it is written when the run finishes"
            ))
        })
}

/// A file-download response: explicit content type, attachment
/// disposition, raw bytes.
fn file_download(bytes: Vec<u8>, content_type: &'static str, filename: String) -> Response {
    let mut response = (
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    )
        .into_response();
    // Downloads contain generated/untrusted prose. Prevent MIME sniffing and
    // stale intermediary caches from changing which canonical snapshot an
    // owner receives.
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

pub async fn report(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Json<serde_json::Value>> {
    let markdown = read_report_markdown(&state, &run_id).await?;
    let dir = state.store().run_dir(&run_id);
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

/// `REPORT.pdf` as a download, always derived from the canonical markdown.
/// The exported PDF is a convenience artifact, not a second source of truth:
/// deriving here prevents an interrupted old export or tampered PDF from
/// disagreeing with `REPORT.md`. Rendering is deterministic and model-free.
pub async fn report_pdf(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Response> {
    let markdown = read_report_markdown(&state, &run_id).await?;
    let bytes = crate::report::render_report_pdf(&markdown);
    Ok(file_download(
        bytes,
        "application/pdf",
        format!("uruk-report-{run_id}.pdf"),
    ))
}

/// `REPORT.md` as a download: the canonical bytes, verbatim.
pub async fn report_markdown(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Response> {
    let markdown = read_report_markdown(&state, &run_id).await?;
    Ok(file_download(
        markdown.into_bytes(),
        "text/markdown; charset=utf-8",
        format!("uruk-report-{run_id}.md"),
    ))
}

pub async fn sources(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Json<serde_json::Value>> {
    let sources = state.store().list_sources(&run_id).await?;
    Ok(Json(serde_json::json!({"ok": true, "sources": sources})))
}

/// Metadata filters for the library listing. Empty values mean "no
/// filter" (an untouched form field submits an empty string); unknown
/// parameter names and unknown filter values are validation errors.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryQuery {
    #[serde(default)]
    access: Option<String>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

/// Map the query-string filters onto a typed [`SourceFilter`], rejecting
/// values the store would silently never match.
fn library_filter(query: LibraryQuery) -> Result<SourceFilter> {
    let given = |value: Option<String>| {
        value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let mut filter = SourceFilter::default();
    if let Some(access) = given(query.access) {
        filter.access = Some(AccessLevel::parse(&access).ok_or_else(|| {
            Error::validation(format!(
                "access must be one of full_text, abstract_only, metadata_only, \
                 unavailable; got {access:?}"
            ))
        })?);
    }
    if let Some(origin) = given(query.origin) {
        if !Origin::KINDS.contains(&origin.as_str()) {
            return Err(Error::validation(format!(
                "origin must be one of {}; got {origin:?}",
                Origin::KINDS.join(", ")
            )));
        }
        filter.origin_kind = Some(origin);
    }
    filter.text = given(query.q);
    Ok(filter)
}

pub async fn library(
    State(state): State<AppState>,
    Owner(owner): Owner,
    query: std::result::Result<Query<LibraryQuery>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let Query(query) = query.map_err(|r| Error::validation(r.body_text()))?;
    let filter = library_filter(query)?;
    let sources = state.store().list_sources_owned(&owner, &filter).await?;
    Ok(Json(serde_json::json!({"ok": true, "sources": sources})))
}

/// Owner-wide passage search over every run this browser created.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryPassagesQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

pub async fn library_passages(
    State(state): State<AppState>,
    Owner(owner): Owner,
    query: std::result::Result<Query<LibraryPassagesQuery>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let Query(query) = query.map_err(|r| Error::validation(r.body_text()))?;
    let q = query.q.trim();
    if q.is_empty() {
        return Err(ApiError(Error::validation("q must not be empty")));
    }
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let passages = state
        .store()
        .search_passages_owned(&owner, q, limit)
        .await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "query": q,
        "passages": passages,
    })))
}

/// One source of one owned run: the full recorded source plus how many
/// passages its extracted text contributes to the index.
pub async fn source_detail(
    State(state): State<AppState>,
    owned: OwnedSource,
) -> ApiResult<Json<serde_json::Value>> {
    let passage_count = state
        .store()
        .count_source_passages(&owned.run_id, &owned.source.id)
        .await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "run_id": owned.run_id.as_str(),
        "source": owned.source,
        "passage_count": passage_count,
    })))
}

/// Browse (no `q`) or search (`q`) one source's indexed passages. Browsing
/// pages with `offset`/`limit` in sequence order; searching is BM25-ranked,
/// so `offset` does not apply to it.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePassagesQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

pub async fn source_passages(
    State(state): State<AppState>,
    owned: OwnedSource,
    query: std::result::Result<Query<SourcePassagesQuery>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let Query(query) = query.map_err(|r| Error::validation(r.body_text()))?;
    let store = state.store();

    if let Some(q) = &query.q {
        let q = q.trim();
        if q.is_empty() {
            return Err(ApiError(Error::validation("q must not be empty")));
        }
        if query.offset.is_some() {
            return Err(ApiError(Error::validation(
                "offset applies to browsing; search results are ranked",
            )));
        }
        let limit = query.limit.unwrap_or(10).clamp(1, 100);
        let passages = store
            .search_passages_in_source(&owned.run_id, &owned.source.id, q, limit)
            .await?;
        return Ok(Json(serde_json::json!({
            "ok": true,
            "run_id": owned.run_id.as_str(),
            "source_id": owned.source.id.as_str(),
            "query": q,
            "passages": passages,
        })));
    }

    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    let total = store
        .count_source_passages(&owned.run_id, &owned.source.id)
        .await?;
    let passages = store
        .list_source_passages(&owned.run_id, &owned.source.id, offset, limit)
        .await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "run_id": owned.run_id.as_str(),
        "source_id": owned.source.id.as_str(),
        "total": total,
        "offset": offset,
        "limit": limit,
        "passages": passages,
    })))
}

/// Structured citation inspection: every persisted citation of the run
/// with its honest resolution (see [`crate::report::collect_citations`]).
pub async fn citations(
    State(state): State<AppState>,
    OwnedRun(run_id): OwnedRun,
) -> ApiResult<Json<serde_json::Value>> {
    let citations = crate::report::collect_citations(state.store(), &run_id).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "run_id": run_id.as_str(),
        "citations": citations,
    })))
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
