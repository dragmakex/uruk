//! Owner-bound file uploads for the web API (docs/WEB.md).
//!
//! An upload is a file a browser posted over the API: the server chose
//! where its bytes live (`.uruk/uploads/<owner_digest>/<upload_id>`), so
//! no client-supplied path ever reaches the filesystem. Like runs, every
//! upload belongs to exactly one anonymous browser identity, and every
//! read or delete here is owner-scoped: a foreign id answers exactly like
//! an id that does not exist, so probing discloses nothing.

use super::{Store, from_rfc3339, to_rfc3339};
use crate::records::{ContentHash, UploadId};
use crate::store::OwnerDigest;
use crate::{Error, Result};
use sqlx::Row;
use time::OffsetDateTime;

/// A stored upload: the durable metadata of one owner-bound file.
///
/// `storage_path` is relative to the project directory (resolve with
/// [`Store::resolve_path`]) and is server-internal: it must never be
/// serialized onto the wire.
#[derive(Debug, Clone)]
pub struct Upload {
    pub id: UploadId,
    pub owner_digest: OwnerDigest,
    /// The name the browser gave the file, kept for display and for
    /// choosing a text extractor — never used as a filesystem path.
    pub file_name: String,
    pub size_bytes: u64,
    pub content_hash: ContentHash,
    pub storage_path: String,
    pub created_at: OffsetDateTime,
}

/// The one message every missing, foreign, or unknown upload gets, so a
/// probe cannot tell those cases apart.
pub(crate) const NO_SUCH_UPLOAD: &str = "no such upload";

impl Store {
    /// Insert an upload, enforcing the per-owner count cap inside the
    /// write transaction so two concurrent uploads cannot both squeeze
    /// under the limit.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] when the owner already holds
    /// `max_per_owner` uploads, and [`Error::Storage`] when the insert
    /// cannot be committed.
    pub async fn create_upload_bounded(&self, upload: &Upload, max_per_owner: usize) -> Result<()> {
        let mut guard = self.begin_write().await?;
        let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM uploads WHERE owner_digest = ?")
            .bind(upload.owner_digest.as_str())
            .fetch_one(guard.conn())
            .await?;
        if held as usize >= max_per_owner {
            return Err(Error::validation(format!(
                "this browser already holds {held} uploads, the maximum is \
                 {max_per_owner}; delete one before uploading more"
            )));
        }
        sqlx::query(
            "INSERT INTO uploads
                 (id, owner_digest, file_name, size_bytes, content_hash,
                  storage_path, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(upload.id.as_str())
        .bind(upload.owner_digest.as_str())
        .bind(&upload.file_name)
        .bind(upload.size_bytes as i64)
        .bind(&upload.content_hash.0)
        .bind(&upload.storage_path)
        .bind(to_rfc3339(upload.created_at))
        .execute(guard.conn())
        .await?;
        guard.commit().await
    }

    /// The owner's uploads, newest first, ties broken by id.
    pub async fn list_uploads_owned(&self, owner: &OwnerDigest) -> Result<Vec<Upload>> {
        let rows = sqlx::query(
            "SELECT id, owner_digest, file_name, size_bytes, content_hash,
                    storage_path, created_at
             FROM uploads WHERE owner_digest = ?
             ORDER BY created_at DESC, id",
        )
        .bind(owner.as_str())
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(upload_from_row).collect()
    }

    /// Fetch an upload if — and only if — `owner` owns it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] with one fixed message for an unknown
    /// id and a foreign id alike, so the web API cannot disclose which of
    /// those a probed id is.
    pub async fn get_upload_owned(&self, id: &UploadId, owner: &OwnerDigest) -> Result<Upload> {
        let row = sqlx::query(
            "SELECT id, owner_digest, file_name, size_bytes, content_hash,
                    storage_path, created_at
             FROM uploads WHERE id = ? AND owner_digest = ?",
        )
        .bind(id.as_str())
        .bind(owner.as_str())
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => upload_from_row(&row),
            None => Err(Error::not_found(NO_SUCH_UPLOAD)),
        }
    }

    /// Delete an upload the owner holds and return its row, so the caller
    /// can remove the stored bytes after the commit.
    ///
    /// # Errors
    ///
    /// Same non-disclosing [`Error::NotFound`] as [`Store::get_upload_owned`].
    pub async fn delete_upload_owned(&self, id: &UploadId, owner: &OwnerDigest) -> Result<Upload> {
        let upload = self.get_upload_owned(id, owner).await?;
        let mut guard = self.begin_write().await?;
        sqlx::query("DELETE FROM uploads WHERE id = ? AND owner_digest = ?")
            .bind(id.as_str())
            .bind(owner.as_str())
            .execute(guard.conn())
            .await?;
        guard.commit().await?;
        Ok(upload)
    }
}

fn upload_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Upload> {
    Ok(Upload {
        id: UploadId::from_raw(row.get::<String, _>("id")),
        owner_digest: OwnerDigest::from_digest(row.get::<String, _>("owner_digest")),
        file_name: row.get("file_name"),
        size_bytes: row.get::<i64, _>("size_bytes") as u64,
        content_hash: ContentHash(row.get("content_hash")),
        storage_path: row.get("storage_path"),
        created_at: from_rfc3339(&row.get::<String, _>("created_at"))?,
    })
}
