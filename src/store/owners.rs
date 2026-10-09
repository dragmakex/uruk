//! Anonymous browser ownership of runs (see `docs/WEB.md`).
//!
//! The web API identifies a browser by an opaque bearer token in a cookie.
//! This module owns the durable side of that identity: the one-way
//! [`OwnerDigest`] derived from the token, the atomic binding of a
//! web-created run to exactly one owner, and the owner-scoped reads every
//! web handler must use.
//!
//! Two invariants are enforced here, not in handlers:
//!
//! - **The raw token is never persisted.** Only its SHA-256 digest reaches
//!   SQLite, so a copy of the database does not grant access to anyone's
//!   runs.
//! - **Web access fails closed.** A run without an owner row (created by
//!   an engine caller outside the web API, or before ownership existed) is reported as
//!   `not_found` by the owner-scoped reads — exactly like a run that does
//!   not exist — never adopted by or exposed to a web visitor.

use super::{Store, now, queries, to_rfc3339};
use crate::records::{AccessLevel, Goal, Run, RunId, Source};
use crate::store::RunOverview;
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

/// Metadata filters for the owner-scoped source library.
///
/// Filters combine conjunctively; `None` means no constraint. Every
/// constraint is applied inside the owner-scoped SQL query — never by
/// filtering a broader result afterwards.
#[derive(Debug, Clone, Default)]
pub struct SourceFilter {
    /// Keep only sources with exactly this access level.
    pub access: Option<AccessLevel>,
    /// Keep only sources whose origin kind matches; one of
    /// [`crate::records::Origin::KINDS`], validated by the caller.
    pub origin_kind: Option<String>,
    /// Keep only sources whose title, authors, or identifier contains this
    /// text (literal substring, ASCII case-insensitive).
    pub text: Option<String>,
}

/// One-way identifier of an anonymous browser: the lowercase-hex SHA-256
/// digest of the opaque cookie token. This is the only representation that
/// is stored or compared; the raw token lives solely in the browser's
/// cookie jar and in the request that carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerDigest(String);

impl OwnerDigest {
    /// Digest a raw bearer token into its stored form.
    pub fn from_token(token: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        Self(hex::encode(hasher.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Wrap a digest read back from storage (already the one-way form).
    pub(crate) fn from_digest(digest: String) -> Self {
        Self(digest)
    }
}

impl Store {
    /// Insert a run, its first goal revision, and its owner binding in one
    /// transaction: a web-created run is either fully owned or not created
    /// at all, so no crash window can leave an unowned web run behind.
    ///
    /// # Errors
    ///
    /// Fails with [`Error::Storage`] when any of the three inserts cannot
    /// be committed; nothing is persisted in that case.
    pub async fn create_run_owned(
        &self,
        run: &Run,
        goal: &Goal,
        owner: &OwnerDigest,
    ) -> Result<()> {
        let mut guard = self.begin_write().await?;
        super::writes::insert_run_and_goal_in_tx(guard.conn(), run, goal).await?;
        insert_owner_in_tx(guard.conn(), &run.id, owner).await?;
        guard.commit().await
    }

    /// Fetch a run if — and only if — `owner` owns it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] with the same message for a run that
    /// does not exist, a run owned by a different browser, and a run with
    /// no owner at all, so the web API cannot disclose which of those a
    /// probed id is.
    pub async fn get_run_owned(&self, run_id: &RunId, owner: &OwnerDigest) -> Result<Run> {
        let owned: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM run_owners WHERE run_id = ? AND owner_digest = ?")
                .bind(run_id.as_str())
                .bind(owner.as_str())
                .fetch_optional(self.pool())
                .await?;
        if owned.is_none() {
            // Identical to the unknown-run error from `get_run`.
            return Err(Error::not_found(format!("run {run_id}")));
        }
        self.get_run(run_id).await
    }

    /// The owner-scoped run listing: newest first, ties broken by id, runs
    /// of other owners and ownerless runs excluded.
    pub async fn list_run_overviews_owned(&self, owner: &OwnerDigest) -> Result<Vec<RunOverview>> {
        let rows = sqlx::query(
            "SELECT r.id, r.state, r.stop_condition, r.iterations, r.created_at, r.updated_at,
                    g.body AS goal
             FROM runs r
             JOIN goals g ON g.id = r.goal_id
             JOIN run_owners o ON o.run_id = r.id
             WHERE o.owner_digest = ?
             ORDER BY r.created_at DESC, r.id",
        )
        .bind(owner.as_str())
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(queries::overview_from_row).collect()
    }

    /// The owner-scoped source library: every source recorded by one of
    /// the owner's runs that passes `filter`, newest first, ties broken by
    /// id. The owner join and every filter run inside the one SQL query.
    ///
    /// The text filter uses `instr` on the stored metadata, so `%` and `_`
    /// are literal characters, not patterns; case folding is SQLite's
    /// `lower`, which is ASCII-only.
    pub async fn list_sources_owned(
        &self,
        owner: &OwnerDigest,
        filter: &SourceFilter,
    ) -> Result<Vec<Source>> {
        let mut sql = String::from(
            "SELECT s.body FROM sources s
             JOIN run_owners o ON o.run_id = s.run_id
             WHERE o.owner_digest = ?",
        );
        if filter.access.is_some() {
            sql.push_str(" AND s.access = ?");
        }
        if filter.origin_kind.is_some() {
            sql.push_str(" AND json_extract(s.body, '$.origin.kind') = ?");
        }
        if filter.text.is_some() {
            sql.push_str(
                " AND (instr(lower(coalesce(json_extract(s.body, '$.title'), '')), lower(?)) > 0
                   OR instr(lower(coalesce(json_extract(s.body, '$.authors'), '')), lower(?)) > 0
                   OR instr(lower(coalesce(json_extract(s.body, '$.identifier'), '')), lower(?)) > 0)",
            );
        }
        sql.push_str(" ORDER BY s.created_at DESC, s.id");

        let mut query = sqlx::query_scalar(&sql).bind(owner.as_str());
        if let Some(access) = filter.access {
            query = query.bind(access.as_str());
        }
        if let Some(kind) = &filter.origin_kind {
            query = query.bind(kind.as_str());
        }
        if let Some(text) = &filter.text {
            query = query
                .bind(text.as_str())
                .bind(text.as_str())
                .bind(text.as_str());
        }

        let rows = query.fetch_all(self.pool()).await?;
        queries::bodies(rows).await
    }
}

async fn insert_owner_in_tx(
    conn: &mut SqliteConnection,
    run_id: &RunId,
    owner: &OwnerDigest,
) -> Result<()> {
    sqlx::query("INSERT INTO run_owners (run_id, owner_digest, created_at) VALUES (?, ?, ?)")
        .bind(run_id.as_str())
        .bind(owner.as_str())
        .bind(to_rfc3339(now()))
        .execute(conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod from_token {
        use super::*;

        #[test]
        fn digests_to_the_known_sha256_of_the_token() {
            // SHA-256("abc"), the FIPS 180-2 test vector.
            assert_eq!(
                OwnerDigest::from_token("abc").as_str(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            );
        }

        #[test]
        fn never_echoes_the_raw_token() {
            let token = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            assert_ne!(OwnerDigest::from_token(token).as_str(), token);
        }
    }
}
