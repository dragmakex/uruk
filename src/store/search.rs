//! Works and search-record updates.
//!
//! `works.body` holds the merged `WorkRecord`, whose `hits` carry the
//! pre-dedup per-connector ranks and raw-item hashes — the RRF computation
//! stays re-derivable without a relational hit table. `search_records`
//! keeps its JSON-body convention; rows are addressed by the record's own
//! `id` inside the body.

use super::{Store, from_rfc3339, now, to_rfc3339};
use crate::records::*;
use crate::search::types::{WorkKey, WorkRecord};
use crate::{Error, Result};
use sqlx::Row;
use time::OffsetDateTime;

/// A work as stored: the merged record plus fusion/acquisition state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredWork {
    pub work: WorkRecord,
    pub rrf_score: Option<f64>,
    pub source_id: Option<SourceId>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Store {
    /// Insert one deduplicated work with its fused score. Idempotent per
    /// `(run_id, work_key)`: a replayed discover task cannot duplicate rows.
    pub async fn insert_work(
        &self,
        run_id: &RunId,
        work: &WorkRecord,
        rrf_score: f64,
    ) -> Result<()> {
        let mut guard = self.begin_write().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO works
                 (work_key, run_id, doi, arxiv_id, pmid, title, year, rrf_score, body, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(work.work_key.as_str())
        .bind(run_id.as_str())
        .bind(&work.doi)
        .bind(&work.arxiv_id)
        .bind(&work.pmid)
        .bind(&work.title)
        .bind(work.year)
        .bind(rrf_score)
        .bind(serde_json::to_string(work)?)
        .bind(to_rfc3339(now()))
        .execute(guard.conn())
        .await?;
        guard.commit().await
    }

    /// Record that a work was acquired as `source_id`.
    pub async fn set_work_source(
        &self,
        run_id: &RunId,
        work_key: &WorkKey,
        source_id: &SourceId,
    ) -> Result<()> {
        let mut guard = self.begin_write().await?;
        sqlx::query("UPDATE works SET source_id = ? WHERE run_id = ? AND work_key = ?")
            .bind(source_id.as_str())
            .bind(run_id.as_str())
            .bind(work_key.as_str())
            .execute(guard.conn())
            .await?;
        guard.commit().await
    }

    pub async fn get_work(&self, run_id: &RunId, work_key: &str) -> Result<StoredWork> {
        let row = sqlx::query(
            "SELECT body, rrf_score, source_id, created_at FROM works
             WHERE run_id = ? AND work_key = ?",
        )
        .bind(run_id.as_str())
        .bind(work_key)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| Error::not_found(format!("work {work_key}")))?;
        stored_work(row)
    }

    /// Works for a run, best fused score first, ties by work_key.
    pub async fn list_works(&self, run_id: &RunId) -> Result<Vec<StoredWork>> {
        let rows = sqlx::query(
            "SELECT body, rrf_score, source_id, created_at FROM works
             WHERE run_id = ? ORDER BY rrf_score DESC, work_key",
        )
        .bind(run_id.as_str())
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(stored_work).collect()
    }

    /// Update a search record in place, addressed by the `id` inside its
    /// JSON body: acquisition outcomes (retrieved source, unavailable
    /// entries) land on the record that found the work, keeping coverage
    /// auditable. Returns `false` when no record matches.
    pub async fn update_search_record(
        &self,
        run_id: &RunId,
        search_id: &SearchId,
        apply: impl FnOnce(&mut SearchRecord),
    ) -> Result<bool> {
        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        let rows: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, body FROM search_records WHERE run_id = ?")
                .bind(run_id.as_str())
                .fetch_all(&mut *conn)
                .await?;
        for (rowid, body) in rows {
            let mut record: SearchRecord = serde_json::from_str(&body)?;
            if &record.id != search_id {
                continue;
            }
            apply(&mut record);
            sqlx::query("UPDATE search_records SET body = ? WHERE id = ?")
                .bind(serde_json::to_string(&record)?)
                .bind(rowid)
                .execute(&mut *conn)
                .await?;
            guard.commit().await?;
            return Ok(true);
        }
        Ok(false)
    }
}

fn stored_work(row: sqlx::sqlite::SqliteRow) -> Result<StoredWork> {
    Ok(StoredWork {
        work: serde_json::from_str(&row.get::<String, _>("body"))?,
        rrf_score: row.get::<Option<f64>, _>("rrf_score"),
        source_id: row
            .get::<Option<String>, _>("source_id")
            .map(SourceId::from_raw),
        created_at: from_rfc3339(&row.get::<String, _>("created_at"))?,
    })
}
