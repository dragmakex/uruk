//! Passage indexing and BM25 retrieval over SQLite FTS5 (§6, §10).
//!
//! The extracted-text artifact file is the canonical full text; `passages`
//! is a derived index. Each row carries the artifact's content hash so
//! drift between file and index is detectable, and byte offsets map every
//! passage back to an exact span of the canonical file — the same spans
//! `Locator::Span` citations use.

use super::{OwnerDigest, Store, now, to_rfc3339};
use crate::records::*;
use crate::search::chunk::{ChunkConfig, chunk_text};
use crate::{Error, Result};
use sqlx::Row;

/// One ranked passage hit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PassageHit {
    pub source_id: SourceId,
    pub seq: u32,
    pub byte_start: usize,
    pub byte_end: usize,
    pub text: String,
    /// FTS5 `snippet()`, 64-token window with `…` ellipsis.
    pub snippet: String,
    /// `-bm25(passages_fts)`; higher is better.
    pub score: f64,
}

/// One stored passage in artifact order, for browsing a source's text.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredPassage {
    pub seq: u32,
    /// Byte offsets into the canonical UTF-8 artifact text, on char
    /// boundaries; `text` is the exact slice they denote.
    pub byte_start: usize,
    pub byte_end: usize,
    pub text: String,
}

/// One ranked hit of an owner-wide passage search: a [`PassageHit`] plus
/// the run and source-title facts a cross-run listing needs.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OwnedPassageHit {
    pub run_id: RunId,
    pub source_id: SourceId,
    /// The source's recorded title, when it has one.
    pub source_title: Option<String>,
    pub seq: u32,
    pub byte_start: usize,
    pub byte_end: usize,
    pub text: String,
    /// FTS5 `snippet()`, 64-token window with `…` ellipsis.
    pub snippet: String,
    /// `-bm25(passages_fts)`; higher is better.
    pub score: f64,
}

impl Store {
    /// Chunk and index one source's text artifact. One write transaction;
    /// idempotent per `(source_id, seq)` via the UNIQUE constraint.
    /// Returns how many passages the source now contributes.
    pub async fn index_source_passages(&self, source: &Source, text: &str) -> Result<usize> {
        let Some(artifact_id) = &source.text_artifact else {
            return Ok(0);
        };
        let chunks = chunk_text(text, &ChunkConfig::default());
        if chunks.is_empty() {
            return Ok(0);
        }
        let artifact_hash = ContentHash::of_str(text);
        let created = to_rfc3339(now());

        let mut guard = self.begin_write().await?;
        let conn = guard.conn();
        for chunk in &chunks {
            sqlx::query(
                "INSERT OR IGNORE INTO passages
                     (run_id, source_id, artifact_id, artifact_hash, seq,
                      byte_start, byte_end, text, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(source.run_id.as_str())
            .bind(source.id.as_str())
            .bind(artifact_id.as_str())
            .bind(artifact_hash.as_str())
            .bind(chunk.seq)
            .bind(chunk.byte_start as i64)
            .bind(chunk.byte_end as i64)
            .bind(&chunk.text)
            .bind(&created)
            .execute(&mut *conn)
            .await?;
        }
        guard.commit().await?;

        tracing::info!(
            target: "uruk::search",
            source_id = %source.id,
            passages = chunks.len(),
            bytes = text.len(),
            "passages.index"
        );
        Ok(chunks.len())
    }

    /// BM25 search scoped to a run. Deterministic order:
    /// `ORDER BY bm25(passages_fts), passages.id`.
    pub async fn search_passages(
        &self,
        run_id: &RunId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<PassageHit>> {
        let Some(match_query) = sanitize_match_query(query) else {
            return Ok(vec![]);
        };
        let rows = sqlx::query(
            "SELECT p.source_id, p.seq, p.byte_start, p.byte_end, p.text,
                    snippet(passages_fts, 0, '', '', '…', 64) AS snip,
                    bm25(passages_fts) AS rank
             FROM passages_fts
             JOIN passages p ON p.id = passages_fts.rowid
             WHERE passages_fts MATCH ? AND p.run_id = ?
             ORDER BY bm25(passages_fts), p.id
             LIMIT ?",
        )
        .bind(&match_query)
        .bind(run_id.as_str())
        .bind(limit as i64)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(PassageHit {
                    source_id: SourceId::from_raw(r.get::<String, _>("source_id")),
                    seq: r.get::<i64, _>("seq") as u32,
                    byte_start: r.get::<i64, _>("byte_start") as usize,
                    byte_end: r.get::<i64, _>("byte_end") as usize,
                    text: r.get::<String, _>("text"),
                    snippet: r.get::<String, _>("snip"),
                    score: -r.get::<f64, _>("rank"),
                })
            })
            .collect()
    }

    /// BM25 search constrained to one source of one run. Same contract as
    /// [`Store::search_passages`], narrowed in the SQL itself.
    pub async fn search_passages_in_source(
        &self,
        run_id: &RunId,
        source_id: &SourceId,
        query: &str,
        limit: usize,
    ) -> Result<Vec<PassageHit>> {
        let Some(match_query) = sanitize_match_query(query) else {
            return Ok(vec![]);
        };
        let rows = sqlx::query(
            "SELECT p.source_id, p.seq, p.byte_start, p.byte_end, p.text,
                    snippet(passages_fts, 0, '', '', '…', 64) AS snip,
                    bm25(passages_fts) AS rank
             FROM passages_fts
             JOIN passages p ON p.id = passages_fts.rowid
             WHERE passages_fts MATCH ? AND p.run_id = ? AND p.source_id = ?
             ORDER BY bm25(passages_fts), p.id
             LIMIT ?",
        )
        .bind(&match_query)
        .bind(run_id.as_str())
        .bind(source_id.as_str())
        .bind(limit as i64)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(PassageHit {
                    source_id: SourceId::from_raw(r.get::<String, _>("source_id")),
                    seq: r.get::<i64, _>("seq") as u32,
                    byte_start: r.get::<i64, _>("byte_start") as usize,
                    byte_end: r.get::<i64, _>("byte_end") as usize,
                    text: r.get::<String, _>("text"),
                    snippet: r.get::<String, _>("snip"),
                    score: -r.get::<f64, _>("rank"),
                })
            })
            .collect()
    }

    /// BM25 search across every run the owner created, for the library's
    /// owner-wide search. The owner constraint is a SQL join on
    /// `run_owners`: ownerless (CLI) runs and other owners' runs can never
    /// appear in the result.
    pub async fn search_passages_owned(
        &self,
        owner: &OwnerDigest,
        query: &str,
        limit: usize,
    ) -> Result<Vec<OwnedPassageHit>> {
        let Some(match_query) = sanitize_match_query(query) else {
            return Ok(vec![]);
        };
        let rows = sqlx::query(
            "SELECT p.run_id, p.source_id, p.seq, p.byte_start, p.byte_end, p.text,
                    json_extract(s.body, '$.title') AS source_title,
                    snippet(passages_fts, 0, '', '', '…', 64) AS snip,
                    bm25(passages_fts) AS rank
             FROM passages_fts
             JOIN passages p ON p.id = passages_fts.rowid
             JOIN run_owners o ON o.run_id = p.run_id
             JOIN sources s ON s.id = p.source_id
             WHERE passages_fts MATCH ? AND o.owner_digest = ?
             ORDER BY bm25(passages_fts), p.id
             LIMIT ?",
        )
        .bind(&match_query)
        .bind(owner.as_str())
        .bind(limit as i64)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(OwnedPassageHit {
                    run_id: RunId::from_raw(r.get::<String, _>("run_id")),
                    source_id: SourceId::from_raw(r.get::<String, _>("source_id")),
                    source_title: r.get::<Option<String>, _>("source_title"),
                    seq: r.get::<i64, _>("seq") as u32,
                    byte_start: r.get::<i64, _>("byte_start") as usize,
                    byte_end: r.get::<i64, _>("byte_end") as usize,
                    text: r.get::<String, _>("text"),
                    snippet: r.get::<String, _>("snip"),
                    score: -r.get::<f64, _>("rank"),
                })
            })
            .collect()
    }

    /// One source's passages in sequence order, for browsing. Run-constrained
    /// in the SQL: another run's source id yields nothing.
    pub async fn list_source_passages(
        &self,
        run_id: &RunId,
        source_id: &SourceId,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<StoredPassage>> {
        let rows = sqlx::query(
            "SELECT seq, byte_start, byte_end, text FROM passages
             WHERE run_id = ? AND source_id = ?
             ORDER BY seq
             LIMIT ? OFFSET ?",
        )
        .bind(run_id.as_str())
        .bind(source_id.as_str())
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(StoredPassage {
                    seq: r.get::<i64, _>("seq") as u32,
                    byte_start: r.get::<i64, _>("byte_start") as usize,
                    byte_end: r.get::<i64, _>("byte_end") as usize,
                    text: r.get::<String, _>("text"),
                })
            })
            .collect()
    }

    /// How many passages one source contributes, run-constrained like
    /// [`Store::list_source_passages`].
    pub async fn count_source_passages(&self, run_id: &RunId, source_id: &SourceId) -> Result<u64> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM passages WHERE run_id = ? AND source_id = ?")
                .bind(run_id.as_str())
                .bind(source_id.as_str())
                .fetch_one(self.pool())
                .await?;
        Ok(n as u64)
    }

    /// How many passages are indexed for a run.
    pub async fn count_passages(&self, run_id: &RunId) -> Result<u64> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM passages WHERE run_id = ?")
            .bind(run_id.as_str())
            .fetch_one(self.pool())
            .await?;
        Ok(n as u64)
    }

    /// Verify one passage span against the stored artifact hash, for span
    /// citation checks.
    pub async fn passage_artifact_hash(
        &self,
        source_id: &SourceId,
        seq: u32,
    ) -> Result<ContentHash> {
        let hash: Option<String> = sqlx::query_scalar(
            "SELECT artifact_hash FROM passages WHERE source_id = ? AND seq = ?",
        )
        .bind(source_id.as_str())
        .bind(seq)
        .fetch_optional(self.pool())
        .await?;
        hash.map(ContentHash)
            .ok_or_else(|| Error::not_found(format!("passage {source_id}#{seq}")))
    }
}

/// Build a safe FTS5 MATCH string from a raw query (§10).
///
/// Raw strings never reach MATCH: the query is tokenized to alphanumeric
/// terms, each term is double-quoted, and terms join with OR (recall-biased;
/// BM25 handles precision). Double-quoted phrases in the input are preserved
/// as FTS5 phrase queries. Empty after sanitization → `None`, not an error.
pub fn sanitize_match_query(raw: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut rest = raw;

    // Pull out double-quoted phrases first.
    while let Some(open) = rest.find('"') {
        let before = &rest[..open];
        collect_terms(before, &mut terms);
        let after = &rest[open + 1..];
        match after.find('"') {
            Some(close) => {
                let phrase: Vec<String> = after[..close]
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect();
                if !phrase.is_empty() {
                    terms.push(format!("\"{}\"", phrase.join(" ")));
                }
                rest = &after[close + 1..];
            }
            None => {
                // Unbalanced quote: treat the remainder as plain terms.
                rest = after;
                break;
            }
        }
    }
    collect_terms(rest, &mut terms);

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

fn collect_terms(text: &str, terms: &mut Vec<String>) {
    for token in text.split(|c: char| !c.is_alphanumeric()) {
        if !token.is_empty() {
            terms.push(format!("\"{token}\""));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terms_are_quoted_and_or_joined() {
        assert_eq!(
            sanitize_match_query("catalyst degradation").as_deref(),
            Some("\"catalyst\" OR \"degradation\"")
        );
    }

    #[test]
    fn injection_attempts_become_safe_match_strings() {
        // FTS5 operators and column filters must not survive as syntax.
        let q = sanitize_match_query("\"a\" OR rowid").unwrap();
        assert_eq!(q, "\"a\" OR \"OR\" OR \"rowid\"");

        let q = sanitize_match_query("text: NEAR(x y) AND {col}*^").unwrap();
        assert!(!q.contains('('), "{q}");
        assert!(!q.contains('{'), "{q}");
        assert!(!q.contains('*'), "{q}");
        assert!(!q.contains(':'), "{q}");
        for term in q.split(" OR ") {
            assert!(
                term.starts_with('"') && term.ends_with('"'),
                "unquoted term {term:?} in {q}"
            );
        }
    }

    #[test]
    fn quoted_phrases_are_preserved() {
        assert_eq!(
            sanitize_match_query("\"thermal cycling\" fatigue").as_deref(),
            Some("\"thermal cycling\" OR \"fatigue\"")
        );
    }

    #[test]
    fn unbalanced_quotes_degrade_to_plain_terms() {
        assert_eq!(
            sanitize_match_query("broken \"quote here").as_deref(),
            Some("\"broken\" OR \"quote\" OR \"here\"")
        );
    }

    #[test]
    fn empty_after_sanitization_is_none() {
        assert_eq!(sanitize_match_query(""), None);
        assert_eq!(sanitize_match_query("()^:*"), None);
        assert_eq!(sanitize_match_query("\"\""), None);
    }
}
