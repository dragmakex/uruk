//! Reads and the match commit path (SPEC §9.2, §7).

use super::{Store, from_rfc3339, now, to_rfc3339};
use crate::records::*;
use crate::{Error, Result};
use sqlx::{Row, SqliteConnection};
use std::collections::BTreeMap;

/// A compact item view for listings and reports.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ItemSummary {
    pub id: ItemId,
    pub kind: ItemKind,
    pub title: String,
    pub disposition: String,
    pub assessment: Assessment,
    pub rating: Option<f64>,
    pub matches_played: u32,
    pub stale_rating: bool,
    pub review_count: u32,
}

/// One row of the run listing: run lifecycle facts plus the current goal.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunOverview {
    pub id: RunId,
    pub state: RunState,
    pub stop_condition: Option<StopCondition>,
    pub iterations: u32,
    pub question: String,
    pub mode: Mode,
    pub created_at: String,
    pub updated_at: String,
}

pub(crate) async fn bodies<T: serde::de::DeserializeOwned>(rows: Vec<String>) -> Result<Vec<T>> {
    rows.iter().map(|b| Ok(serde_json::from_str(b)?)).collect()
}

/// Map one `runs ⨝ goals` row (as selected by the overview queries) into a
/// [`RunOverview`]. Shared with the owner-scoped listing in `owners.rs`.
pub(crate) fn overview_from_row(r: &sqlx::sqlite::SqliteRow) -> Result<RunOverview> {
    let state = RunState::parse(r.get::<String, _>("state").as_str())
        .ok_or_else(|| Error::validation("unknown run state"))?;
    let goal: Goal = serde_json::from_str(&r.get::<String, _>("goal"))?;
    Ok(RunOverview {
        id: RunId::from_raw(r.get::<String, _>("id")),
        state,
        stop_condition: r
            .get::<Option<String>, _>("stop_condition")
            .as_deref()
            .and_then(StopCondition::parse),
        iterations: r.get::<i64, _>("iterations") as u32,
        question: goal.question,
        mode: goal.mode,
        created_at: r.get::<String, _>("created_at"),
        updated_at: r.get::<String, _>("updated_at"),
    })
}

impl Store {
    pub async fn get_run(&self, run_id: &RunId) -> Result<Run> {
        let row = sqlx::query(
            "SELECT id, schema_version, project_id, goal_id, plan_id, state, stop_condition,
                    iterations, created_at, updated_at FROM runs WHERE id = ?",
        )
        .bind(run_id.as_str())
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| Error::not_found(format!("run {run_id}")))?;

        let state = RunState::parse(row.get::<String, _>("state").as_str())
            .ok_or_else(|| Error::validation("unknown run state"))?;
        let stop_condition = row
            .get::<Option<String>, _>("stop_condition")
            .as_deref()
            .and_then(StopCondition::parse);

        Ok(Run {
            id: RunId::from_raw(row.get::<String, _>("id")),
            schema_version: row.get::<i64, _>("schema_version") as u32,
            project_id: ProjectId::from_raw(row.get::<String, _>("project_id")),
            goal_id: GoalId::from_raw(row.get::<String, _>("goal_id")),
            plan_id: row
                .get::<Option<String>, _>("plan_id")
                .map(PlanId::from_raw),
            state,
            stop_condition,
            iterations: row.get::<i64, _>("iterations") as u32,
            created_at: from_rfc3339(&row.get::<String, _>("created_at"))?,
            updated_at: from_rfc3339(&row.get::<String, _>("updated_at"))?,
        })
    }

    /// List runs, newest first.
    pub async fn list_runs(&self) -> Result<Vec<(RunId, RunState, String)>> {
        let rows =
            sqlx::query("SELECT id, state, created_at FROM runs ORDER BY created_at DESC, id")
                .fetch_all(self.pool())
                .await?;
        rows.into_iter()
            .map(|r| {
                let state = RunState::parse(r.get::<String, _>("state").as_str())
                    .ok_or_else(|| Error::validation("unknown run state"))?;
                Ok((
                    RunId::from_raw(r.get::<String, _>("id")),
                    state,
                    r.get::<String, _>("created_at"),
                ))
            })
            .collect()
    }

    /// Runs joined with their current goal, newest first (ties broken by ID
    /// so the order is deterministic).
    pub async fn list_run_overviews(&self) -> Result<Vec<RunOverview>> {
        let rows = sqlx::query(
            "SELECT r.id, r.state, r.stop_condition, r.iterations, r.created_at, r.updated_at,
                    g.body AS goal
             FROM runs r JOIN goals g ON g.id = r.goal_id
             ORDER BY r.created_at DESC, r.id",
        )
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(overview_from_row).collect()
    }

    pub async fn current_goal(&self, run_id: &RunId) -> Result<Goal> {
        let body: String = sqlx::query_scalar(
            "SELECT g.body FROM goals g JOIN runs r ON r.goal_id = g.id WHERE r.id = ?",
        )
        .bind(run_id.as_str())
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| Error::not_found(format!("goal for run {run_id}")))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn get_goal(&self, goal_id: &GoalId) -> Result<Goal> {
        let body: String = sqlx::query_scalar("SELECT body FROM goals WHERE id = ?")
            .bind(goal_id.as_str())
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| Error::not_found(format!("goal {goal_id}")))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn current_plan(&self, run_id: &RunId) -> Result<Option<Plan>> {
        let body: Option<String> = sqlx::query_scalar(
            "SELECT p.body FROM plans p JOIN runs r ON r.plan_id = p.id WHERE r.id = ?",
        )
        .bind(run_id.as_str())
        .fetch_optional(self.pool())
        .await?;
        Ok(match body {
            Some(b) => Some(serde_json::from_str(&b)?),
            None => None,
        })
    }

    pub async fn next_goal_revision(&self, run_id: &RunId) -> Result<u32> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(revision) FROM goals WHERE run_id = ?")
                .bind(run_id.as_str())
                .fetch_one(self.pool())
                .await?;
        Ok((max.unwrap_or(-1) + 1) as u32)
    }

    pub async fn next_plan_revision(&self, run_id: &RunId) -> Result<u32> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(revision) FROM plans WHERE run_id = ?")
                .bind(run_id.as_str())
                .fetch_one(self.pool())
                .await?;
        Ok((max.unwrap_or(-1) + 1) as u32)
    }

    pub async fn list_sources(&self, run_id: &RunId) -> Result<Vec<Source>> {
        let rows =
            sqlx::query_scalar("SELECT body FROM sources WHERE run_id = ? ORDER BY created_at, id")
                .bind(run_id.as_str())
                .fetch_all(self.pool())
                .await?;
        bodies(rows).await
    }

    /// Every source in the project, newest first (ties broken by ID), for
    /// the cross-run library view.
    pub async fn list_sources_all(&self) -> Result<Vec<Source>> {
        let rows = sqlx::query_scalar("SELECT body FROM sources ORDER BY created_at DESC, id")
            .fetch_all(self.pool())
            .await?;
        bodies(rows).await
    }

    /// Fetch a source if — and only if — `run_id` recorded it.
    ///
    /// The run constraint lives in the SQL, not in a post-fetch check, so
    /// this is safe for owner-scoped web reads (the caller authorizes the
    /// run; this query cannot reach past it).
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] with the same message for a source that
    /// does not exist and one recorded by a different run, so a probing
    /// caller cannot distinguish the two.
    pub async fn get_source_in_run(&self, run_id: &RunId, id: &SourceId) -> Result<Source> {
        let body: Option<String> =
            sqlx::query_scalar("SELECT body FROM sources WHERE id = ? AND run_id = ?")
                .bind(id.as_str())
                .bind(run_id.as_str())
                .fetch_optional(self.pool())
                .await?;
        let body = body.ok_or_else(|| Error::not_found(format!("source {id}")))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn get_source(&self, id: &SourceId) -> Result<Source> {
        let body: String = sqlx::query_scalar("SELECT body FROM sources WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| Error::not_found(format!("source {id}")))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn list_search_records(&self, run_id: &RunId) -> Result<Vec<SearchRecord>> {
        let rows = sqlx::query_scalar(
            "SELECT body FROM search_records WHERE run_id = ? ORDER BY created_at, id",
        )
        .bind(run_id.as_str())
        .fetch_all(self.pool())
        .await?;
        bodies(rows).await
    }

    pub async fn get_item(&self, id: &ItemId) -> Result<ResearchItem> {
        let body: String = sqlx::query_scalar("SELECT body FROM items WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| Error::not_found(format!("item {id}")))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn list_items(&self, run_id: &RunId) -> Result<Vec<ResearchItem>> {
        let rows =
            sqlx::query_scalar("SELECT body FROM items WHERE run_id = ? ORDER BY created_at, id")
                .bind(run_id.as_str())
                .fetch_all(self.pool())
                .await?;
        bodies(rows).await
    }

    pub async fn list_items_of_kind(
        &self,
        run_id: &RunId,
        kind: ItemKind,
    ) -> Result<Vec<ResearchItem>> {
        let rows = sqlx::query_scalar(
            "SELECT body FROM items WHERE run_id = ? AND kind = ? ORDER BY created_at, id",
        )
        .bind(run_id.as_str())
        .bind(kind.as_str())
        .fetch_all(self.pool())
        .await?;
        bodies(rows).await
    }

    pub async fn get_disposition(&self, item_id: &ItemId) -> Result<Disposition> {
        let row =
            sqlx::query("SELECT disposition, duplicate_of FROM item_states WHERE item_id = ?")
                .bind(item_id.as_str())
                .fetch_optional(self.pool())
                .await?
                .ok_or_else(|| Error::not_found(format!("item state {item_id}")))?;

        Ok(match row.get::<String, _>("disposition").as_str() {
            "active" => Disposition::Active,
            "blocked" => Disposition::Blocked,
            "archived" => Disposition::Archived,
            "duplicate" => Disposition::Duplicate(ItemId::from_raw(
                row.get::<Option<String>, _>("duplicate_of")
                    .ok_or_else(|| Error::validation("duplicate without representative"))?,
            )),
            other => return Err(Error::validation(format!("unknown disposition {other}"))),
        })
    }

    pub async fn list_reviews_for_item(&self, item_id: &ItemId) -> Result<Vec<Review>> {
        let rows = sqlx::query_scalar(
            "SELECT body FROM reviews WHERE item_id = ? ORDER BY created_at, id",
        )
        .bind(item_id.as_str())
        .fetch_all(self.pool())
        .await?;
        bodies(rows).await
    }

    pub async fn list_reviews(&self, run_id: &RunId) -> Result<Vec<Review>> {
        let rows =
            sqlx::query_scalar("SELECT body FROM reviews WHERE run_id = ? ORDER BY created_at, id")
                .bind(run_id.as_str())
                .fetch_all(self.pool())
                .await?;
        bodies(rows).await
    }

    /// Review counts per item in one query, for scheduling and snapshots.
    pub async fn review_counts(&self, run_id: &RunId) -> Result<BTreeMap<String, u32>> {
        let rows = sqlx::query(
            "SELECT item_id, COUNT(*) AS n FROM reviews WHERE run_id = ? GROUP BY item_id",
        )
        .bind(run_id.as_str())
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get::<String, _>("item_id"), r.get::<i64, _>("n") as u32))
            .collect())
    }

    /// Current assessment of an item: the most recent recorded one.
    pub async fn current_assessment(&self, item_id: &ItemId) -> Result<Assessment> {
        let s: Option<String> = sqlx::query_scalar(
            "SELECT assessment FROM assessments WHERE item_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(item_id.as_str())
        .fetch_optional(self.pool())
        .await?;

        Ok(match s.as_deref() {
            None | Some("untested") => Assessment::Untested,
            Some("plausible") => Assessment::Plausible,
            Some("supported") => Assessment::Supported,
            Some("contested") => Assessment::Contested,
            Some("refuted") => Assessment::Refuted,
            Some("inconclusive") => Assessment::Inconclusive,
            Some(other) => return Err(Error::validation(format!("unknown assessment {other}"))),
        })
    }

    pub async fn list_evidence(&self, run_id: &RunId) -> Result<Vec<Evidence>> {
        let rows = sqlx::query_scalar(
            "SELECT body FROM evidence WHERE run_id = ? ORDER BY created_at, id",
        )
        .bind(run_id.as_str())
        .fetch_all(self.pool())
        .await?;
        bodies(rows).await
    }

    pub async fn get_evidence(&self, id: &EvidenceId) -> Result<Evidence> {
        let body: String = sqlx::query_scalar("SELECT body FROM evidence WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| Error::not_found(format!("evidence {id}")))?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Evidence linked to an item, with each link, in link order.
    pub async fn evidence_for_item(
        &self,
        item_id: &ItemId,
    ) -> Result<Vec<(Evidence, EvidenceLink)>> {
        let rows = sqlx::query(
            "SELECT l.body AS link, e.body AS evidence FROM evidence_links l
             JOIN evidence e ON e.id = l.evidence_id
             WHERE l.item_id = ? ORDER BY l.id",
        )
        .bind(item_id.as_str())
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    serde_json::from_str(&r.get::<String, _>("evidence"))?,
                    serde_json::from_str(&r.get::<String, _>("link"))?,
                ))
            })
            .collect()
    }

    pub async fn list_evidence_links(&self, item_id: &ItemId) -> Result<Vec<EvidenceLink>> {
        let rows =
            sqlx::query_scalar("SELECT body FROM evidence_links WHERE item_id = ? ORDER BY id")
                .bind(item_id.as_str())
                .fetch_all(self.pool())
                .await?;
        bodies(rows).await
    }

    /// Links originating from one evidence item, across every claim it bears
    /// on. An evidence item may support one claim and contradict another.
    pub async fn links_for_evidence(&self, evidence_id: &EvidenceId) -> Result<Vec<EvidenceLink>> {
        let rows =
            sqlx::query_scalar("SELECT body FROM evidence_links WHERE evidence_id = ? ORDER BY id")
                .bind(evidence_id.as_str())
                .fetch_all(self.pool())
                .await?;
        bodies(rows).await
    }

    pub async fn list_decisions(&self, run_id: &RunId) -> Result<Vec<Decision>> {
        let rows = sqlx::query_scalar(
            "SELECT body FROM decisions WHERE run_id = ? ORDER BY created_at, id",
        )
        .bind(run_id.as_str())
        .fetch_all(self.pool())
        .await?;
        bodies(rows).await
    }

    pub async fn list_decisions_of_kind(
        &self,
        run_id: &RunId,
        kind: DecisionKind,
    ) -> Result<Vec<Decision>> {
        let rows = sqlx::query_scalar(
            "SELECT body FROM decisions WHERE run_id = ? AND kind = ? ORDER BY created_at, id",
        )
        .bind(run_id.as_str())
        .bind(kind.as_str())
        .fetch_all(self.pool())
        .await?;
        bodies(rows).await
    }

    pub async fn list_artifacts(&self, run_id: &RunId) -> Result<Vec<Artifact>> {
        let rows = sqlx::query_scalar(
            "SELECT body FROM artifacts WHERE run_id = ? ORDER BY created_at, id",
        )
        .bind(run_id.as_str())
        .fetch_all(self.pool())
        .await?;
        bodies(rows).await
    }

    pub async fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact> {
        let body: String = sqlx::query_scalar("SELECT body FROM artifacts WHERE id = ?")
            .bind(id.as_str())
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| Error::not_found(format!("artifact {id}")))?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn list_experiments(&self, run_id: &RunId) -> Result<Vec<Experiment>> {
        let rows = sqlx::query_scalar(
            "SELECT body FROM experiments WHERE run_id = ? ORDER BY created_at, id",
        )
        .bind(run_id.as_str())
        .fetch_all(self.pool())
        .await?;
        bodies(rows).await
    }

    pub async fn list_clusters(&self, run_id: &RunId) -> Result<Vec<Cluster>> {
        let rows = sqlx::query_scalar(
            "SELECT body FROM clusters WHERE run_id = ? ORDER BY created_at, id",
        )
        .bind(run_id.as_str())
        .fetch_all(self.pool())
        .await?;
        bodies(rows).await
    }

    pub async fn latest_feedback(&self, run_id: &RunId) -> Result<Option<Feedback>> {
        let body: Option<String> = sqlx::query_scalar(
            "SELECT body FROM feedback WHERE run_id = ? ORDER BY revision DESC LIMIT 1",
        )
        .bind(run_id.as_str())
        .fetch_optional(self.pool())
        .await?;
        Ok(match body {
            Some(b) => Some(serde_json::from_str(&b)?),
            None => None,
        })
    }

    pub async fn next_feedback_revision(&self, run_id: &RunId) -> Result<u32> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(revision) FROM feedback WHERE run_id = ?")
                .bind(run_id.as_str())
                .fetch_one(self.pool())
                .await?;
        Ok((max.unwrap_or(-1) + 1) as u32)
    }

    pub async fn list_snapshots(&self, run_id: &RunId) -> Result<Vec<CampaignSnapshot>> {
        let rows =
            sqlx::query_scalar("SELECT body FROM snapshots WHERE run_id = ? ORDER BY iteration")
                .bind(run_id.as_str())
                .fetch_all(self.pool())
                .await?;
        bodies(rows).await
    }

    pub async fn list_matches(&self, run_id: &RunId) -> Result<Vec<Match>> {
        let rows =
            sqlx::query_scalar("SELECT body FROM matches WHERE run_id = ? ORDER BY created_at, id")
                .bind(run_id.as_str())
                .fetch_all(self.pool())
                .await?;
        bodies(rows).await
    }

    pub async fn get_rating(&self, item_id: &ItemId, rubric_id: &PlanId) -> Result<Option<Rating>> {
        let row = sqlx::query(
            "SELECT rating, matches_played, stale, updated_at FROM ratings
             WHERE item_id = ? AND rubric_id = ?",
        )
        .bind(item_id.as_str())
        .bind(rubric_id.as_str())
        .fetch_optional(self.pool())
        .await?;

        Ok(match row {
            None => None,
            Some(r) => Some(Rating {
                item_id: item_id.clone(),
                rubric_id: rubric_id.clone(),
                rating: r.get::<f64, _>("rating"),
                matches_played: r.get::<i64, _>("matches_played") as u32,
                stale: r.get::<i64, _>("stale") != 0,
                updated_at: from_rfc3339(&r.get::<String, _>("updated_at"))?,
            }),
        })
    }

    /// Ratings within one rubric cohort, best first (SPEC §7). Ties (every
    /// candidate starts at the same initial Elo) break on item id so the
    /// exported `tournament.jsonl` is reproducible.
    pub async fn leaderboard(&self, run_id: &RunId, rubric_id: &PlanId) -> Result<Vec<Rating>> {
        let rows = sqlx::query(
            "SELECT r.item_id, r.rating, r.matches_played, r.stale, r.updated_at
             FROM ratings r JOIN items i ON i.id = r.item_id
             WHERE i.run_id = ? AND r.rubric_id = ?
             ORDER BY r.rating DESC, r.item_id",
        )
        .bind(run_id.as_str())
        .bind(rubric_id.as_str())
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(Rating {
                    item_id: ItemId::from_raw(r.get::<String, _>("item_id")),
                    rubric_id: rubric_id.clone(),
                    rating: r.get::<f64, _>("rating"),
                    matches_played: r.get::<i64, _>("matches_played") as u32,
                    stale: r.get::<i64, _>("stale") != 0,
                    updated_at: from_rfc3339(&r.get::<String, _>("updated_at"))?,
                })
            })
            .collect()
    }

    /// Enrolled candidates in a cohort start at the paper's initial rating.
    pub async fn ensure_rating(&self, item_id: &ItemId, rubric_id: &PlanId) -> Result<()> {
        let mut guard = self.begin_write().await?;
        ensure_rating_in_tx(guard.conn(), item_id, rubric_id).await?;
        guard.commit().await
    }

    /// Commit a match and apply its rating update exactly once (SPEC §7, §9.2).
    ///
    /// Returns `false` when the match was already recorded.
    pub async fn commit_match(&self, m: &Match, dedupe_key: &str, k: f64) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let applied = commit_match_in_tx(guard.conn(), m, dedupe_key, k).await?;
        guard.commit().await?;
        Ok(applied)
    }

    /// Summaries for reporting: content, disposition, assessment, and rating.
    pub async fn item_summaries(
        &self,
        run_id: &RunId,
        rubric_id: Option<&PlanId>,
    ) -> Result<Vec<ItemSummary>> {
        let items = self.list_items(run_id).await?;
        let counts = self.review_counts(run_id).await?;
        let mut out = Vec::with_capacity(items.len());

        for item in items {
            let disposition = self.get_disposition(&item.id).await?;
            let assessment = self.current_assessment(&item.id).await?;
            let rating = match rubric_id {
                Some(rid) => self.get_rating(&item.id, rid).await?,
                None => None,
            };

            out.push(ItemSummary {
                review_count: counts.get(item.id.as_str()).copied().unwrap_or(0),
                id: item.id.clone(),
                kind: item.kind,
                title: item.title.clone(),
                disposition: disposition.as_str().to_string(),
                assessment,
                rating: rating.as_ref().map(|r| r.rating),
                matches_played: rating.as_ref().map(|r| r.matches_played).unwrap_or(0),
                stale_rating: rating.as_ref().map(|r| r.stale).unwrap_or(false),
            });
        }
        Ok(out)
    }
}

pub(crate) async fn ensure_rating_in_tx(
    conn: &mut SqliteConnection,
    item_id: &ItemId,
    rubric_id: &PlanId,
) -> Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO ratings (item_id, rubric_id, rating, matches_played, stale, updated_at)
             VALUES (?, ?, ?, 0, 0, ?)",
    )
    .bind(item_id.as_str())
    .bind(rubric_id.as_str())
    .bind(INITIAL_ELO)
    .bind(to_rfc3339(now()))
    .execute(conn)
    .await?;
    Ok(())
}

/// `dedupe_key` makes duplicate delivery harmless: a replayed result hits
/// the unique index and the Elo update is not applied a second time.
pub(crate) async fn commit_match_in_tx(
    conn: &mut SqliteConnection,
    m: &Match,
    dedupe_key: &str,
    k: f64,
) -> Result<bool> {
    let already: Option<String> = sqlx::query_scalar("SELECT id FROM matches WHERE dedupe_key = ?")
        .bind(dedupe_key)
        .fetch_optional(&mut *conn)
        .await?;
    if already.is_some() {
        return Ok(false);
    }

    // Read current ratings inside the transaction so concurrent matches
    // cannot interleave a lost update.
    let (mut ra, mut played_a) = fetch_rating(conn, &m.item_a, &m.rubric_id).await?;
    let (mut rb, mut played_b) = fetch_rating(conn, &m.item_b, &m.rubric_id).await?;

    let mut stored = m.clone();
    stored.rating_a_before = ra;
    stored.rating_b_before = rb;

    if m.outcome.updates_ratings() {
        let (na, nb) = apply_elo(ra, rb, m.outcome, k);
        ra = na;
        rb = nb;
        played_a += 1;
        played_b += 1;
    }
    stored.rating_a_after = ra;
    stored.rating_b_after = rb;

    sqlx::query(
        "INSERT INTO matches (id, run_id, item_a, item_b, rubric_id, outcome,
                              dedupe_key, body, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(stored.id.as_str())
    .bind(stored.run_id.as_str())
    .bind(stored.item_a.as_str())
    .bind(stored.item_b.as_str())
    .bind(stored.rubric_id.as_str())
    .bind(stored.outcome.as_str())
    .bind(dedupe_key)
    .bind(serde_json::to_string(&stored)?)
    .bind(to_rfc3339(stored.created_at))
    .execute(&mut *conn)
    .await?;

    if m.outcome.updates_ratings() {
        // A refreshed comparison clears the stale flag for both sides.
        upsert_rating(conn, &m.item_a, &m.rubric_id, ra, played_a).await?;
        upsert_rating(conn, &m.item_b, &m.rubric_id, rb, played_b).await?;
    }
    Ok(true)
}

async fn fetch_rating(
    conn: &mut SqliteConnection,
    item_id: &ItemId,
    rubric_id: &PlanId,
) -> Result<(f64, u32)> {
    let row = sqlx::query(
        "SELECT rating, matches_played FROM ratings WHERE item_id = ? AND rubric_id = ?",
    )
    .bind(item_id.as_str())
    .bind(rubric_id.as_str())
    .fetch_optional(conn)
    .await?;

    Ok(match row {
        Some(r) => (
            r.get::<f64, _>("rating"),
            r.get::<i64, _>("matches_played") as u32,
        ),
        None => (INITIAL_ELO, 0),
    })
}

async fn upsert_rating(
    conn: &mut SqliteConnection,
    item_id: &ItemId,
    rubric_id: &PlanId,
    rating: f64,
    played: u32,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO ratings (item_id, rubric_id, rating, matches_played, stale, updated_at)
         VALUES (?, ?, ?, ?, 0, ?)
         ON CONFLICT(item_id, rubric_id) DO UPDATE SET
             rating = excluded.rating,
             matches_played = excluded.matches_played,
             stale = 0,
             updated_at = excluded.updated_at",
    )
    .bind(item_id.as_str())
    .bind(rubric_id.as_str())
    .bind(rating)
    .bind(played)
    .bind(to_rfc3339(now()))
    .execute(conn)
    .await?;
    Ok(())
}
