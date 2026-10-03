//! The Search role: federated literature discovery and open-access
//! acquisition (plan §§3, 9, 11).
//!
//! `search.discover` plans queries (one model call, seeded, with a
//! deterministic offline fallback), executes them against the enabled
//! connectors, persists the audit trail (SearchRecords, raw responses,
//! per-connector ranks inside each work's body), fuses rankings with RRF,
//! and enqueues one `search.acquire` task per top-ranked work.
//! `search.acquire` resolves a legal open-access location locally from the
//! discovery data, routes it through the existing ingestion pipeline, and
//! falls back to an abstract-only source or a recorded unavailable entry.
//! Nothing here fetches a URL that was not supplied by the researcher or
//! designated by a connector as an OA location.

use super::context::AgentContext;
use super::outputs;
use crate::prompts::ids;
use crate::records::*;
use crate::search::{
    self, ConnectorCx, ConnectorSearchOutcome, FulltextLocation, FusedWork, SearchQuery, WorkRecord,
};
use crate::store::RecordBatch;
use crate::tools::ingest_input;
use crate::{Error, Result};
use serde::Deserialize;
use std::sync::atomic::{AtomicI64, Ordering};
use time::OffsetDateTime;

/// Dispatch a Search task by strategy.
pub async fn run_search(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    match task.strategy.as_str() {
        "discover" => discover(ctx, task, batch).await,
        "acquire" => acquire(ctx, task, batch).await,
        other => Err(Error::validation(format!(
            "unknown search strategy {other:?}"
        ))),
    }
}

/// The typed output of `search.plan_queries`.
#[derive(Debug, Deserialize)]
struct QueryPlan {
    queries: Vec<PlannedQuery>,
}

#[derive(Debug, Deserialize)]
struct PlannedQuery {
    text: String,
    /// Part of the prompt's output contract; validated but not consumed.
    #[serde(default)]
    #[expect(dead_code)]
    rationale: String,
    #[serde(default)]
    year_from: Option<i32>,
    #[serde(default)]
    year_to: Option<i32>,
}

/// At most this many queries per discovery (§3).
const MAX_QUERIES: usize = 4;

/// Longest query text sent to a connector, in characters. Model output is
/// untrusted input; anything longer is truncated before it reaches a URL.
const MAX_QUERY_CHARS: usize = 400;

/// Publication years a planned filter may name; anything outside is model
/// noise, not a meaningful scholarly filter.
const YEAR_RANGE: std::ops::RangeInclusive<i32> = 1000..=9999;

/// Normalize one planned query: whitespace collapsed, length capped, year
/// filters validated (out-of-range or inverted bounds are dropped, not
/// guessed at). Returns `None` when no query text survives.
fn sanitize_planned_query(q: PlannedQuery) -> Option<SearchQuery> {
    let mut text = q.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((idx, _)) = text.char_indices().nth(MAX_QUERY_CHARS) {
        text.truncate(idx);
        text.truncate(text.trim_end().len());
    }
    if text.is_empty() {
        return None;
    }
    let mut year_from = q.year_from.filter(|y| YEAR_RANGE.contains(y));
    let mut year_to = q.year_to.filter(|y| YEAR_RANGE.contains(y));
    if let (Some(from), Some(to)) = (year_from, year_to)
        && from > to
    {
        tracing::warn!(from, to, "inverted year filter dropped");
        (year_from, year_to) = (None, None);
    }
    Some(SearchQuery {
        text,
        year_from,
        year_to,
        max_results: crate::search::types::DEFAULT_RESULTS_PER_QUERY,
    })
}

/// Plan queries with one seeded model call; fall back to a single
/// deterministic stopword-stripped query so search works offline and with
/// the stand-in provider (§11).
async fn plan_queries(ctx: &AgentContext) -> Result<Vec<SearchQuery>> {
    let template = ids::SEARCH_PLAN_QUERIES;
    let bindings = ctx
        .base_bindings_for(template)
        .set(
            "scope",
            ctx.goal
                .scope
                .clone()
                .unwrap_or_else(|| "(none specified)".to_string()),
        )
        .set(
            "exclusions",
            if ctx.goal.exclusions.is_empty() {
                "(none)".to_string()
            } else {
                ctx.goal
                    .exclusions
                    .iter()
                    .map(|e| format!("- {e}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        )
        .set("source_coverage", ctx.source_coverage().await?);

    let planned = match ctx.call(template, bindings).await {
        Ok(completion) => match outputs::parse::<QueryPlan>(&completion.text) {
            Ok(plan) => plan
                .queries
                .into_iter()
                .filter_map(sanitize_planned_query)
                .take(MAX_QUERIES)
                .collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!(error = %e, "query plan did not parse; using the fallback query");
                vec![]
            }
        },
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(e) => {
            tracing::warn!(error = %e, "query planning call failed; using the fallback query");
            vec![]
        }
    };

    if planned.is_empty() {
        return Ok(vec![SearchQuery::new(fallback_query(&ctx.goal.question))]);
    }
    Ok(planned)
}

/// The deterministic fallback: the goal question stripped of stopwords.
pub(crate) fn fallback_query(question: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "a", "an", "and", "are", "about", "as", "at", "be", "but", "by", "can", "do", "does",
        "for", "from", "how", "in", "is", "it", "its", "known", "of", "on", "or", "that", "the",
        "their", "there", "these", "this", "to", "we", "what", "when", "where", "which", "who",
        "why", "will", "with",
    ];
    let stripped: Vec<&str> = question
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .filter(|w| !STOPWORDS.contains(&w.to_ascii_lowercase().as_str()))
        .collect();
    if stripped.is_empty() {
        question.trim().to_string()
    } else {
        stripped.join(" ")
    }
}

/// Remaining tool-execution allowance, charged per connector call.
async fn allowance(ctx: &AgentContext) -> Result<AtomicI64> {
    let usage = ctx.store.budget_usage(&ctx.run_id).await?;
    let remaining = ctx
        .goal
        .budget
        .max_tool_executions
        .saturating_sub(usage.committed_tool_executions());
    Ok(AtomicI64::new(remaining as i64))
}

async fn discover(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    let connectors = search::discovery_connectors(&ctx.permissions);
    if connectors.is_empty() {
        return Err(Error::permission(
            "no search connector is enabled for this task; discovery cannot run",
        ));
    }

    let queries = plan_queries(ctx).await?;
    let cx = ConnectorCx::new(search::contact_email())?;
    let remaining = allowance(ctx).await?;
    let charge = || {
        if ctx.cancel.is_cancelled() {
            return false;
        }
        if remaining.fetch_sub(1, Ordering::SeqCst) <= 0 {
            return false;
        }
        ctx.charge_tool_execution();
        true
    };

    let outcomes = search::federated_search(&connectors, &queries, &cx, &charge).await;

    // One SearchRecord per (connector × query), query text verbatim (§13).
    let mut search_ids = Vec::with_capacity(outcomes.len());
    for outcome in &outcomes {
        let record = SearchRecord {
            id: SearchId::new(),
            run_id: ctx.run_id.clone(),
            query: outcome.query.text.clone(),
            tool: outcome.connector.to_string(),
            executed_at: OffsetDateTime::now_utc(),
            filters: outcome.query.filters_json(),
            // The connector-reported total, not the page size: coverage
            // records how much the query matched, not how much was read.
            results_found: outcome.total_found.unwrap_or(outcome.hits.len()),
            results_retrieved: vec![],
            unavailable: vec![],
            connector_errors: outcome.error.clone().into_iter().collect(),
        };
        ctx.store.insert_search_record(&record).await?;
        search_ids.push(record.id);
    }

    persist_raw_responses(ctx, task, &outcomes).await?;

    // Dedup, merge, fuse; persist works. The pre-dedup per-connector ranks
    // and raw-item hashes travel inside each work's body (`body.hits`).
    let fused = search::fuse_outcomes(&outcomes, &search_ids);
    for work in &fused {
        ctx.store
            .insert_work(&ctx.run_id, &work.record, work.rrf_score)
            .await?;
    }

    let enqueued = enqueue_acquisitions(ctx, task, &fused).await?;

    batch.decisions.push(Decision {
        id: DecisionId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: ctx.run_id.clone(),
        kind: DecisionKind::Scheduling,
        actor: Actor::Agent("search".into()),
        reason: format!(
            "federated discovery: {} quer(ies) across {} connector(s); {} work(s) after \
             dedup; {} acquisition(s) enqueued",
            queries.len(),
            connectors.len(),
            fused.len(),
            enqueued
        ),
        referenced: search_ids.iter().map(|s| s.0.clone()).collect(),
        payload: serde_json::json!({
            "queries": queries.iter().map(|q| &q.text).collect::<Vec<_>>(),
            "connectors": connectors.iter().map(|c| c.name()).collect::<Vec<_>>(),
            // Executed < planned means the budget or a cancellation stopped
            // the federation early; the skipped searches are not hidden.
            "searches_planned": connectors.len() * queries.len(),
            "searches_executed": outcomes.len(),
            "works": fused.len(),
            "acquisitions_enqueued": enqueued,
            "connector_errors": outcomes
                .iter()
                .filter_map(|o| o.error.as_ref().map(|e| format!("{}: {e}", o.connector)))
                .collect::<Vec<_>>(),
        }),
        created_at: OffsetDateTime::now_utc(),
    });
    Ok(())
}

/// Persist every raw connector response as a local-only artifact, so the
/// discovery result is reconstructable offline, byte for byte (§13).
async fn persist_raw_responses(
    ctx: &AgentContext,
    task: &Task,
    outcomes: &[ConnectorSearchOutcome],
) -> Result<()> {
    for (i, outcome) in outcomes.iter().enumerate() {
        for (j, raw) in outcome.raw.iter().enumerate() {
            let extension = if raw.media_type.contains("xml") {
                "xml"
            } else {
                "json"
            };
            let path = ctx
                .store
                .artifact_dir(&ctx.run_id)
                .join(task.id.as_str())
                .join(format!("{}-q{i}-{j}.{extension}", raw.connector));
            crate::store::write_atomic(&path, &raw.body).await?;
            let artifact = Artifact {
                id: ArtifactId::new(),
                schema_version: SCHEMA_VERSION,
                run_id: ctx.run_id.clone(),
                media_type: raw.media_type.to_string(),
                content_hash: ContentHash::of_bytes(&raw.body),
                size_bytes: raw.body.len() as u64,
                storage_path: ctx.store.storable_path(&path),
                produced_by: Some(task.id.clone()),
                // Raw connector payloads are audit material, never model input.
                access: AccessClass::LocalOnly,
                label: Some(format!(
                    "{} response: {} (page 1)",
                    raw.connector, outcome.query.text
                )),
                supersedes: None,
                superseded_reason: None,
                created_at: OffsetDateTime::now_utc(),
            };
            ctx.store.insert_artifact(&artifact).await?;
        }
    }
    Ok(())
}

/// Enqueue one `search.acquire` task per top-ranked work, dependent on the
/// discover task, priority ordered by fused score (§11).
async fn enqueue_acquisitions(
    ctx: &AgentContext,
    task: &Task,
    fused: &[FusedWork],
) -> Result<usize> {
    let cap = ctx.goal.budget.max_acquisitions as usize;
    let mut enqueued = 0;
    for (index, work) in fused.iter().take(cap).enumerate() {
        let key = work.record.work_key.as_str();
        let dedupe = format!("search:acquire:{}:{key}", ctx.goal.revision);
        let acquire = Task {
            id: TaskId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: ctx.run_id.clone(),
            role: Role::Search,
            strategy: "acquire".to_string(),
            goal_id: ctx.goal.id.clone(),
            plan_id: ctx.plan.id.clone(),
            input_refs: vec![key.to_string()],
            input_hash: ContentHash::of_str(&dedupe),
            prompt_id: None,
            prompt_hash: None,
            model: None,
            provider: None,
            payload: Some(serde_json::json!({ "work_key": key })),
            feedback_id: None,
            depends_on: vec![task.id.clone()],
            priority: 4.0 + work.rrf_score,
            priority_rationale: format!(
                "acquire fused rank {} (rrf {:.5})",
                index + 1,
                work.rrf_score
            ),
            permissions: ctx.permissions.clone(),
            reservation: CostReservation {
                model_calls: 0,
                tokens: 0,
                // One tool execution per acquired work (§11 revised).
                tool_executions: 1,
            },
            state: TaskState::Pending,
            attempts: 0,
            max_attempts: 1,
            seed: derive_seed(&dedupe),
            output_refs: vec![],
            actual_cost: CostActual::default(),
            error: None,
            created_at: OffsetDateTime::now_utc(),
            started_at: None,
            finished_at: None,
        };
        let task_id = ctx.store.enqueue_task(&acquire, Some(&dedupe)).await?;
        if task_id != acquire.id {
            continue; // Replay: this work's acquisition already exists.
        }
        match ctx
            .store
            .reserve_budget(
                &ctx.run_id,
                &acquire.id,
                &acquire.reservation,
                &ctx.goal.budget,
                false,
            )
            .await
        {
            Ok(()) => enqueued += 1,
            Err(Error::Budget(reason)) => {
                tracing::info!(work = key, %reason, "acquisition not admitted: budget");
                ctx.store
                    .complete_task(
                        &acquire.id,
                        TaskState::Cancelled,
                        vec![],
                        CostActual::default(),
                        Some(format!("not admitted: {reason}")),
                    )
                    .await?;
            }
            Err(e) => return Err(e),
        }
    }
    if fused.len() > cap {
        tracing::info!(
            works = fused.len(),
            cap,
            "acquisition cap truncated the fused list; remaining works stay metadata-only"
        );
    }
    Ok(enqueued)
}

/// Same identity-derived seed scheme as the scheduler, so a replayed
/// acquisition draws the same choices.
fn derive_seed(identity: &str) -> u64 {
    let hash = ContentHash::of_str(identity);
    let mut seed = 0u64;
    for &b in hash.as_str().as_bytes().iter().take(16) {
        seed = seed.wrapping_mul(31).wrapping_add(b as u64);
    }
    seed
}

async fn acquire(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    let work_key = task
        .input_refs
        .first()
        .ok_or_else(|| Error::validation("acquire task names no work"))?;
    let stored = ctx.store.get_work(&ctx.run_id, work_key).await?;
    if stored.source_id.is_some() {
        return Ok(()); // Replayed delivery: the work is already acquired.
    }
    let work = &stored.work;

    // One tool execution per acquired work (§11 revised); resolution itself
    // is local over the discovery data and makes no network call.
    ctx.charge_tool_execution();
    let location = search::resolve_fulltext(work);

    // A fetch that yielded no readable text (metadata-only) is kept so the
    // work can still be linked to it when no abstract exists either.
    let mut unreadable: Option<Source> = None;
    if let Some(location) = &location {
        let ingest = ingest_input(
            &ctx.store,
            &ctx.run_id,
            &InputRef {
                locator: location.url.clone(),
                note: Some(format!("acquired via {} for {work_key}", location.resolver)),
            },
            &ctx.permissions,
        )
        .await;
        match ingest {
            // Full text means readable text: a fetch whose extraction yielded
            // nothing must not block the abstract fallback (§9.3).
            Ok(ingested) if ingested.artifact.is_some() => {
                let source = backfill_source(ctx, ingested.source, work, location).await?;
                ctx.store
                    .set_work_source(&ctx.run_id, &work.work_key, &source.id)
                    .await?;
                record_retrieval(ctx, work, &source.id).await?;
                tracing::info!(
                    target: "uruk::search",
                    work_key,
                    resolver = location.resolver,
                    outcome = "full_text",
                    access = source.access.as_str(),
                    "search.acquire"
                );
                note_outcome(batch, ctx, work_key, "full text ingested", &source.id.0);
                return Ok(());
            }
            Ok(ingested) => {
                if ingested.source.access != AccessLevel::Unavailable {
                    unreadable = Some(ingested.source);
                }
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            // Extraction blowing up on a hostile document must degrade to
            // the abstract fallback, not fail the acquisition outright.
            Err(e) => {
                tracing::warn!(
                    target: "uruk::search",
                    work_key,
                    error = %e,
                    "full-text ingestion failed; falling back"
                );
            }
        }
    }

    // Abstract fallback: real evidence, honestly labelled (§9.3). The
    // existing AccessLevel::AbstractOnly machinery prevents over-claiming.
    if let Some(abstract_text) = work
        .abstract_text
        .as_deref()
        .filter(|a| !a.trim().is_empty())
    {
        let source = abstract_source(ctx, work, abstract_text).await?;
        ctx.store
            .set_work_source(&ctx.run_id, &work.work_key, &source.id)
            .await?;
        record_retrieval(ctx, work, &source.id).await?;
        tracing::info!(
            target: "uruk::search",
            work_key,
            outcome = "abstract_only",
            bytes = abstract_text.len(),
            "search.acquire"
        );
        note_outcome(batch, ctx, work_key, "abstract-only fallback", &source.id.0);
        return Ok(());
    }

    // Fetched but unreadable, and no abstract to fall back on: link the
    // metadata-only source so the retrieval stays in the audit trail,
    // honestly labelled by its own access level.
    if let Some(source) = unreadable {
        ctx.store
            .set_work_source(&ctx.run_id, &work.work_key, &source.id)
            .await?;
        record_retrieval(ctx, work, &source.id).await?;
        tracing::info!(
            target: "uruk::search",
            work_key,
            outcome = "metadata_only",
            access = source.access.as_str(),
            "search.acquire"
        );
        note_outcome(
            batch,
            ctx,
            work_key,
            "retrieved but no text could be extracted",
            &source.id.0,
        );
        return Ok(());
    }

    // No location, no abstract: an unavailable entry on the originating
    // search record, so coverage stays auditable (§9.4).
    let reason = format!("{work_key} {}: no open-access location", work.title);
    if let Some(search_id) = originating_search_id(work) {
        ctx.store
            .update_search_record(&ctx.run_id, &search_id, |record| {
                if !record.unavailable.contains(&reason) {
                    record.unavailable.push(reason.clone());
                }
            })
            .await?;
    }
    tracing::info!(
        target: "uruk::search",
        work_key,
        outcome = "unavailable",
        "search.acquire"
    );
    note_outcome(
        batch,
        ctx,
        work_key,
        "unavailable: no open-access location",
        "",
    );
    Ok(())
}

/// The search that first surfaced this work: the earliest hit recorded in
/// the work's own audit trail (`body.hits`).
fn originating_search_id(work: &WorkRecord) -> Option<SearchId> {
    work.hits.first().map(|h| h.search_id.clone())
}

/// Backfill bibliographic fields extraction did not find, from the merged
/// work record, and record the acquisition provenance.
async fn backfill_source(
    ctx: &AgentContext,
    mut source: Source,
    work: &WorkRecord,
    location: &FulltextLocation,
) -> Result<Source> {
    if source.identifier.is_none() {
        source.identifier = work.identifier();
    }
    if source.title.is_none() || source.title.as_deref() == Some(location.url.as_str()) {
        source.title = Some(work.title.clone());
    }
    if source.authors.is_none() && !work.authors.is_empty() {
        source.authors = Some(work.authors.join(", "));
    }
    if source.date.is_none() {
        source.date = work.year.map(|y| y.to_string());
    }
    if let Some(license) = &location.license {
        let note = format!(
            "open-access location ({}); license: {license}",
            location.resolver
        );
        source.access_limitations = Some(match source.access_limitations.take() {
            Some(existing) => format!("{existing}; {note}"),
            None => note,
        });
    }
    ctx.store.update_source(&source).await?;
    Ok(source)
}

/// Create an abstract-only source: the abstract stored as a text artifact,
/// chunked and indexed like any other text-bearing source.
async fn abstract_source(
    ctx: &AgentContext,
    work: &WorkRecord,
    abstract_text: &str,
) -> Result<Source> {
    let source_id = SourceId::new();
    let path = ctx
        .store
        .artifact_dir(&ctx.run_id)
        .join(format!("{source_id}.txt"));
    crate::store::write_atomic(&path, abstract_text.as_bytes()).await?;

    let artifact = Artifact {
        id: ArtifactId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: ctx.run_id.clone(),
        media_type: "text/plain".to_string(),
        content_hash: ContentHash::of_str(abstract_text),
        size_bytes: abstract_text.len() as u64,
        storage_path: ctx.store.storable_path(&path),
        produced_by: None,
        access: AccessClass::Open,
        label: Some(format!("abstract of {source_id}")),
        supersedes: None,
        superseded_reason: None,
        created_at: OffsetDateTime::now_utc(),
    };
    ctx.store.insert_artifact(&artifact).await?;

    let source = Source {
        id: source_id,
        schema_version: SCHEMA_VERSION,
        run_id: ctx.run_id.clone(),
        origin: match &work.landing_url {
            Some(url) => Origin::Url(url.clone()),
            None => Origin::Supplied,
        },
        title: Some(work.title.clone()),
        authors: Some(work.authors.join(", ")).filter(|a| !a.is_empty()),
        date: work.year.map(|y| y.to_string()),
        identifier: work.identifier(),
        retrieved_at: OffsetDateTime::now_utc(),
        content_hash: ContentHash::of_str(abstract_text),
        access: AccessLevel::AbstractOnly,
        access_limitations: Some(
            "full text not openly accessible; abstract from connector metadata".to_string(),
        ),
        text_artifact: Some(artifact.id.clone()),
    };
    ctx.store.insert_source(&source).await?;
    ctx.store
        .index_source_passages(&source, abstract_text)
        .await?;
    Ok(source)
}

/// Append the acquired source to the originating search record's
/// `results_retrieved`.
async fn record_retrieval(
    ctx: &AgentContext,
    work: &WorkRecord,
    source_id: &SourceId,
) -> Result<()> {
    if let Some(search_id) = originating_search_id(work) {
        ctx.store
            .update_search_record(&ctx.run_id, &search_id, |record| {
                if !record.results_retrieved.contains(source_id) {
                    record.results_retrieved.push(source_id.clone());
                }
            })
            .await?;
    }
    Ok(())
}

fn note_outcome(
    batch: &mut RecordBatch,
    ctx: &AgentContext,
    work_key: &str,
    outcome: &str,
    source_ref: &str,
) {
    batch.decisions.push(Decision {
        id: DecisionId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: ctx.run_id.clone(),
        kind: DecisionKind::Scheduling,
        actor: Actor::Agent("search".into()),
        reason: format!("acquisition of {work_key}: {outcome}"),
        referenced: if source_ref.is_empty() {
            vec![work_key.to_string()]
        } else {
            vec![work_key.to_string(), source_ref.to_string()]
        },
        payload: serde_json::json!({ "work_key": work_key, "outcome": outcome }),
        created_at: OffsetDateTime::now_utc(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(text: &str, year_from: Option<i32>, year_to: Option<i32>) -> PlannedQuery {
        PlannedQuery {
            text: text.to_string(),
            rationale: String::new(),
            year_from,
            year_to,
        }
    }

    #[test]
    fn planned_query_whitespace_is_collapsed() {
        let q =
            sanitize_planned_query(planned("  catalyst\n\n degradation\t x ", None, None)).unwrap();
        assert_eq!(q.text, "catalyst degradation x");
    }

    #[test]
    fn planned_query_text_is_capped_at_the_char_limit() {
        let long = "word ".repeat(200); // 1000 chars
        let q = sanitize_planned_query(planned(&long, None, None)).unwrap();
        assert!(
            q.text.chars().count() <= MAX_QUERY_CHARS,
            "{}",
            q.text.len()
        );
        assert!(
            !q.text.ends_with(' '),
            "no dangling whitespace after the cap"
        );
    }

    #[test]
    fn planned_query_blank_text_is_dropped() {
        assert!(sanitize_planned_query(planned("  \n\t ", None, None)).is_none());
    }

    #[test]
    fn planned_query_year_filters_outside_the_sane_range_are_dropped() {
        let q = sanitize_planned_query(planned("x", Some(-5), Some(99_999))).unwrap();
        assert_eq!((q.year_from, q.year_to), (None, None));
        let q = sanitize_planned_query(planned("x", Some(2019), Some(2024))).unwrap();
        assert_eq!((q.year_from, q.year_to), (Some(2019), Some(2024)));
    }

    #[test]
    fn planned_query_inverted_year_bounds_are_dropped_not_guessed() {
        let q = sanitize_planned_query(planned("x", Some(2024), Some(2019))).unwrap();
        assert_eq!((q.year_from, q.year_to), (None, None));
    }

    #[test]
    fn fallback_query_strips_stopwords_deterministically() {
        assert_eq!(
            fallback_query("What is known about catalyst degradation in fuel cells?"),
            "catalyst degradation fuel cells"
        );
        assert_eq!(fallback_query("the of and"), "the of and", "never empty");
        assert_eq!(
            fallback_query("What is known about X?"),
            fallback_query("What is known about X?"),
        );
    }
}
