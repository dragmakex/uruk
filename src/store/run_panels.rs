//! Bounded read models for run panels. LIMIT is applied before JSON is decoded.
use super::{
    Store,
    queries::{AssessmentBasis, AssessmentEvidence, ItemSummary, bodies},
};
use crate::{Error, Result, records::*};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite};
use std::collections::HashMap;

/// A single requested slice. History reads can pin an insertion watermark so
/// later (even backdated) records cannot shift an older page.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRequest {
    #[serde(default)]
    pub offset: u32,
    #[serde(default = "default_limit")]
    pub limit: u32,
    pub through: Option<i64>,
}
fn default_limit() -> u32 {
    5
}
impl Default for PageRequest {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: default_limit(),
            through: None,
        }
    }
}
impl PageRequest {
    pub fn validate(self) -> Result<Self> {
        if !(1..=100).contains(&self.limit) {
            return Err(Error::validation("limit must be between 1 and 100"));
        }
        if self
            .through
            .is_some_and(|n| !(0..=9_007_199_254_740_991).contains(&n))
        {
            return Err(Error::validation(
                "through must be a nonnegative safe integer",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PageInfo {
    pub total: u32,
    pub offset: u32,
    pub limit: u32,
    pub through: Option<i64>,
}
impl PageInfo {
    fn new(total: i64, request: PageRequest, through: Option<i64>) -> Self {
        let total = total as u32;
        // A stale page after a dataset shrinks resolves to its last real page.
        let offset = if total == 0 {
            0
        } else if request.offset >= total {
            (total - 1) / request.limit * request.limit
        } else {
            request.offset
        };
        Self {
            total,
            offset,
            limit: request.limit,
            through,
        }
    }
}

pub struct RecordPage<T> {
    pub page: PageInfo,
    pub records: Vec<T>,
}

/// Counts only: none of the history or candidate bodies are read.
pub struct RunRecordCounts {
    pub items: u32,
    pub reviews: u32,
    pub matches: u32,
    pub clusters: u32,
    pub sources: u32,
    pub works: u32,
    pub searches: u32,
}

impl Store {
    pub async fn match_page(
        &self,
        run_id: &RunId,
        request: PageRequest,
    ) -> Result<RecordPage<Match>> {
        self.history_page(run_id, request, true).await
    }

    pub async fn decision_page(
        &self,
        run_id: &RunId,
        request: PageRequest,
    ) -> Result<RecordPage<Decision>> {
        self.history_page(run_id, request, false).await
    }

    async fn history_page<T: serde::de::DeserializeOwned>(
        &self,
        run_id: &RunId,
        request: PageRequest,
        matches: bool,
    ) -> Result<RecordPage<T>> {
        let request = request.validate()?;
        // Both SQL fragments are selected locally, never interpolated from input.
        let (table, order) = if matches {
            ("matches", "created_at DESC, id DESC")
        } else {
            ("decisions", "created_at, id")
        };
        let mut tx = self.pool().begin().await?;
        let latest: i64 = sqlx::query_scalar(&format!(
            "SELECT COALESCE(MAX(rowid), 0) FROM {table} WHERE run_id = ?"
        ))
        .bind(run_id.as_str())
        .fetch_one(&mut *tx)
        .await?;
        let through = request.through.unwrap_or(latest).min(latest);
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {table} WHERE run_id = ? AND rowid <= ?"
        ))
        .bind(run_id.as_str())
        .bind(through)
        .fetch_one(&mut *tx)
        .await?;
        let page = PageInfo::new(total, request, Some(through));
        let rows = sqlx::query_scalar(&format!(
            // Prefer streaming the requested order even on a small/new store:
            // SQLite can otherwise choose the rowid index and sort every body.
            "SELECT body FROM {table} INDEXED BY idx_{table}_run_page WHERE run_id = ? AND rowid <= ? ORDER BY {order} LIMIT ? OFFSET ?"))
            .bind(run_id.as_str()).bind(through).bind(page.limit).bind(page.offset)
            .fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(RecordPage {
            page,
            records: bodies(rows).await?,
        })
    }

    /// Fetch just the titles used by a page of matches, not every candidate's
    /// reviews, assessments, or source/evidence inventory.
    pub async fn item_titles(
        &self,
        run_id: &RunId,
        ids: &[ItemId],
    ) -> Result<HashMap<String, String>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT id, json_extract(body, '$.title') AS title FROM items WHERE run_id = ",
        );
        query.push_bind(run_id.as_str()).push(" AND id IN (");
        let mut ids_query = query.separated(", ");
        for id in ids {
            ids_query.push_bind(id.as_str());
        }
        ids_query.push_unseparated(")");
        let rows = query.build().fetch_all(self.pool()).await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get("id"), r.get("title")))
            .collect())
    }

    /// Best-first SQL pagination in the current rubric; unrated candidates are
    /// last and equal ratings break by ID. The overview additionally excludes
    /// archived/non-proposal items before LIMIT, so five really means top five.
    pub async fn candidate_page(
        &self,
        run_id: &RunId,
        rubric_id: Option<&PlanId>,
        request: PageRequest,
        overview: bool,
    ) -> Result<RecordPage<ItemSummary>> {
        let request = request.validate()?;
        if request.through.is_some() {
            return Err(Error::validation(
                "through applies only to append-only histories",
            ));
        }
        let mut tx = self.pool().begin().await?;
        let eligible = "FROM items i JOIN item_states s ON s.item_id = i.id
            WHERE i.run_id = ? AND (? = 0 OR (s.disposition = 'active' AND i.kind IN ('hypothesis', 'proposal')))";
        let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) {eligible}"))
            .bind(run_id.as_str())
            .bind(overview)
            .fetch_one(&mut *tx)
            .await?;
        let page = PageInfo::new(total, request, None);
        let rows = sqlx::query(
            "SELECT i.id, i.kind, json_extract(i.body, '$.title') AS title, s.disposition,
                    r.rating, COALESCE(r.matches_played, 0) AS matches_played, COALESCE(r.stale, 0) AS stale,
                    (SELECT COUNT(*) FROM reviews v WHERE v.item_id = i.id AND v.run_id = i.run_id) AS review_count,
                    (SELECT a.body FROM assessments a WHERE a.item_id = i.id ORDER BY a.id DESC LIMIT 1) AS assessment
             FROM items i JOIN item_states s ON s.item_id = i.id
             LEFT JOIN ratings r ON r.item_id = i.id AND r.rubric_id = ?
             WHERE i.run_id = ? AND (? = 0 OR (s.disposition = 'active' AND i.kind IN ('hypothesis', 'proposal')))
             ORDER BY r.rating IS NULL, r.rating DESC, i.id LIMIT ? OFFSET ?")
            .bind(rubric_id.map(PlanId::as_str)).bind(run_id.as_str()).bind(overview)
            .bind(page.limit).bind(page.offset).fetch_all(&mut *tx).await?;
        // Resolve only evidence named by these assessments, in this same read
        // transaction. No source/evidence/candidate inventory is materialized.
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let assessment: Option<AssessmentRecord> = row
                .get::<Option<String>, _>("assessment")
                .map(|body| serde_json::from_str(&body))
                .transpose()?;
            let mut basis = None;
            if let Some(record) = &assessment {
                let mut named = Vec::with_capacity(record.evidence.len());
                for link in &record.evidence {
                    let body: String =
                        sqlx::query_scalar("SELECT body FROM evidence WHERE id = ? AND run_id = ?")
                            .bind(link.evidence_id.as_str())
                            .bind(run_id.as_str())
                            .fetch_optional(&mut *tx)
                            .await?
                            .ok_or_else(|| {
                                Error::validation("assessment evidence is not recorded by this run")
                            })?;
                    let evidence: Evidence = serde_json::from_str(&body)?;
                    let mut titles = Vec::new();
                    for id in &evidence.source_ids {
                        let title: Option<String> = sqlx::query_scalar("SELECT COALESCE(json_extract(body, '$.title'), id) FROM sources WHERE id = ? AND run_id = ?")
                            .bind(id.as_str()).bind(run_id.as_str()).fetch_optional(&mut *tx).await?;
                        if let Some(title) = title {
                            titles.push(title);
                        }
                    }
                    named.push(AssessmentEvidence {
                        id: evidence.id,
                        kind: evidence.kind,
                        label: if titles.is_empty() {
                            evidence.method
                        } else {
                            titles.join("; ")
                        },
                        relation: link.relation,
                        claim: link.claim.clone(),
                        limits: link.limits.clone(),
                    });
                }
                basis = Some(AssessmentBasis {
                    rationale: record.rationale.clone(),
                    review_id: record.basis_review.clone(),
                    decision_id: record.basis_decision.clone(),
                    evidence: named,
                });
            }
            let kind: String = row.get("kind");
            records.push(ItemSummary {
                id: ItemId::from_raw(row.get::<String, _>("id")),
                kind: serde_json::from_value(serde_json::Value::String(kind))?,
                title: row.get("title"),
                disposition: row.get("disposition"),
                assessment: assessment.map_or(Assessment::Untested, |a| a.assessment),
                assessment_basis: basis,
                rating: row.get("rating"),
                matches_played: row.get::<i64, _>("matches_played") as u32,
                stale_rating: row.get::<i64, _>("stale") != 0,
                review_count: row.get::<i64, _>("review_count") as u32,
            });
        }
        tx.commit().await?;
        Ok(RecordPage { page, records })
    }

    pub async fn run_record_counts(&self, run_id: &RunId) -> Result<RunRecordCounts> {
        let row = sqlx::query(
            "SELECT (SELECT COUNT(*) FROM items WHERE run_id = ?1) AS items,
                    (SELECT COUNT(*) FROM reviews WHERE run_id = ?1) AS reviews,
                    (SELECT COUNT(*) FROM matches WHERE run_id = ?1) AS matches,
                    (SELECT COUNT(*) FROM clusters WHERE run_id = ?1) AS clusters,
                    (SELECT COUNT(*) FROM sources WHERE run_id = ?1) AS sources,
                    (SELECT COUNT(*) FROM works WHERE run_id = ?1) AS works,
                    (SELECT COUNT(*) FROM search_records WHERE run_id = ?1) AS searches",
        )
        .bind(run_id.as_str())
        .fetch_one(self.pool())
        .await?;
        Ok(RunRecordCounts {
            items: row.get::<i64, _>("items") as u32,
            reviews: row.get::<i64, _>("reviews") as u32,
            matches: row.get::<i64, _>("matches") as u32,
            clusters: row.get::<i64, _>("clusters") as u32,
            sources: row.get::<i64, _>("sources") as u32,
            works: row.get::<i64, _>("works") as u32,
            searches: row.get::<i64, _>("searches") as u32,
        })
    }

    /// At most one checkpoint, and only the five overview ratings in its rubric.
    pub async fn overview_baseline(
        &self,
        run_id: &RunId,
        iteration: u32,
        rubric: &PlanId,
        ids: &[ItemId],
    ) -> Result<Option<std::collections::BTreeMap<String, f64>>> {
        // Project just the requested ratings inside SQLite; a checkpoint can
        // contain the entire board, which must not cross this read boundary.
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT EXISTS (SELECT 1 FROM json_each(s.body, '$.ratings') r WHERE json_extract(r.value, '$.rubric_id') = ",
        );
        query.push_bind(rubric.as_str()).push(") AS has_rubric, (SELECT json_group_object(json_extract(r.value, '$.item_id'), json_extract(r.value, '$.rating')) FROM json_each(s.body, '$.ratings') r WHERE json_extract(r.value, '$.rubric_id') = ")
            .push_bind(rubric.as_str()).push(" AND json_extract(r.value, '$.item_id') IN (");
        let mut selected = query.separated(", ");
        if ids.is_empty() {
            selected.push("NULL");
        } else {
            for id in ids {
                selected.push_bind(id.as_str());
            }
        }
        selected.push_unseparated(")) AS ratings FROM snapshots s WHERE s.run_id = ");
        query
            .push_bind(run_id.as_str())
            .push(" AND s.iteration = ")
            .push_bind(iteration)
            .push(" ORDER BY s.id LIMIT 1");
        let row = query.build().fetch_optional(self.pool()).await?;
        let Some(row) = row.filter(|r| r.get::<bool, _>("has_rubric")) else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(
            &row.get::<String, _>("ratings"),
        )?))
    }
}
