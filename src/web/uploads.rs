//! Owner-bound file uploads: the only way files enter a web run.
//!
//! The browser posts bytes; the server picks the storage location under
//! `.uruk/uploads/<owner_digest>/<upload_id>` and hands back an opaque id
//! that `POST /api/runs` can reference. No client-supplied path is ever
//! used as a filesystem path: the file name is validated for display and
//! extractor choice only, and the storage path never crosses the wire.
//!
//! Bounds (enforced here, not by prompt): per-file size via a
//! route-scoped body limit ([`super::WebConfig::max_upload_bytes`]),
//! per-owner count inside the insert transaction
//! ([`super::WebConfig::max_uploads_per_owner`]), and a fixed allowlist
//! of text-extractable file types ([`ALLOWED_EXTENSIONS`]).

use super::AppState;
use super::error::{ApiError, ApiResult};
use super::owner::Owner;
use crate::records::{ContentHash, UploadId};
use crate::store::{Upload, write_atomic};
use crate::{Error, Result};
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// File types accepted for upload: exactly the set the ingest extractors
/// can turn into text. Everything else would be stored unread, so it is
/// refused instead.
pub const ALLOWED_EXTENSIONS: [&str; 12] = [
    "txt", "md", "csv", "tsv", "json", "jsonl", "pdf", "html", "htm", "rs", "py", "r",
];

/// Upper bound on the client-chosen file name, in characters.
const MAX_FILE_NAME_CHARS: usize = 160;

/// Upper bound on an upload id taken from the request path before it is
/// used in a query.
const MAX_UPLOAD_ID_CHARS: usize = 128;

#[derive(Debug, serde::Deserialize)]
pub struct UploadQuery {
    #[serde(default)]
    name: Option<String>,
}

/// Validate the client-chosen file name: display text and an extractor
/// hint, never a path. Returns the trimmed name.
///
/// # Errors
///
/// Returns [`Error::Validation`] for a missing, blank, oversized, or
/// path-like name, and for a file type outside [`ALLOWED_EXTENSIONS`].
fn validate_file_name(name: Option<&str>) -> Result<String> {
    let name = name.map(str::trim).unwrap_or_default();
    if name.is_empty() {
        return Err(Error::validation(
            "an upload needs a file name: POST /api/uploads?name=<file name>",
        ));
    }
    if name.chars().count() > MAX_FILE_NAME_CHARS {
        return Err(Error::validation(format!(
            "the file name is limited to {MAX_FILE_NAME_CHARS} characters"
        )));
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c.is_control())
        || name.contains("..")
    {
        return Err(Error::validation(
            "the file name must be a plain name, not a path: no separators, \
             no \"..\", no control characters",
        ));
    }
    // Match the extractor's Path::extension semantics: `.txt` is a
    // dotfile, not a text file with an extension.
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    match extension {
        Some(ext) if ALLOWED_EXTENSIONS.contains(&ext.as_str()) => Ok(name.to_string()),
        _ => Err(Error::validation(format!(
            "this file type is not accepted; the name must end in one of: {}",
            ALLOWED_EXTENSIONS.join(", ")
        ))),
    }
}

/// The wire form of an upload: everything the browser may know. The
/// storage path stays server-side.
fn wire(upload: &Upload) -> serde_json::Value {
    serde_json::json!({
        "id": upload.id.as_str(),
        "file_name": upload.file_name,
        "size_bytes": upload.size_bytes,
        "content_hash": upload.content_hash.0,
        "created_at": upload
            .created_at
            .format(&Rfc3339)
            .unwrap_or_else(|_| upload.created_at.to_string()),
    })
}

/// `POST /api/uploads?name=…`: store the request body as an owner-bound
/// file. The body is raw bytes; the route-level body limit has already
/// rejected anything over [`super::WebConfig::max_upload_bytes`].
pub async fn create(
    State(state): State<AppState>,
    Owner(owner): Owner,
    query: std::result::Result<Query<UploadQuery>, QueryRejection>,
    body: axum::body::Bytes,
) -> Response {
    let result = async {
        let Query(query) = query.map_err(|r| Error::validation(r.body_text()))?;
        let file_name = validate_file_name(query.name.as_deref())?;
        if body.is_empty() {
            return Err(Error::validation("the upload body is empty"));
        }

        let id = UploadId::new();
        let dir = state
            .store()
            .project_dir()
            .join(".uruk/uploads")
            .join(owner.as_str());
        let path = dir.join(id.as_str());
        // Bytes first, row second: a crash in between leaves an orphaned
        // file (harmless, invisible), never a row pointing at nothing.
        write_atomic(&path, &body).await?;

        let upload = Upload {
            id,
            owner_digest: owner.clone(),
            file_name,
            size_bytes: body.len() as u64,
            content_hash: ContentHash::of_bytes(&body),
            storage_path: state.store().storable_path(&path),
            created_at: OffsetDateTime::now_utc(),
        };
        if let Err(e) = state
            .store()
            .create_upload_bounded(&upload, state.config().max_uploads_per_owner)
            .await
        {
            // The row was refused; do not leave the bytes behind.
            let _ = tokio::fs::remove_file(&path).await;
            return Err(e);
        }
        Ok(upload)
    }
    .await;

    match result {
        Ok(upload) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"ok": true, "upload": wire(&upload)})),
        )
            .into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// `GET /api/uploads`: this browser's uploads, newest first.
pub async fn list(
    State(state): State<AppState>,
    Owner(owner): Owner,
) -> ApiResult<Json<serde_json::Value>> {
    let uploads = state.store().list_uploads_owned(&owner).await?;
    let uploads: Vec<serde_json::Value> = uploads.iter().map(wire).collect();
    Ok(Json(serde_json::json!({"ok": true, "uploads": uploads})))
}

/// `DELETE /api/uploads/{upload_id}`: remove an upload this browser owns.
/// A foreign or unknown id gets one identical `not_found`.
pub async fn remove(
    State(state): State<AppState>,
    Owner(owner): Owner,
    Path(raw): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    if raw.chars().count() > MAX_UPLOAD_ID_CHARS {
        // Bounded echo: an absurd path segment is not reflected back.
        return Err(ApiError(Error::not_found("no such upload")));
    }
    let id = UploadId::from_raw(raw);
    let upload = state.store().delete_upload_owned(&id, &owner).await?;
    // The row is gone; remove the bytes. A missing file is already the
    // desired end state, and any other failure must not resurrect the
    // deleted upload — log it and move on.
    let path = state.store().resolve_path(&upload.storage_path);
    if let Err(e) = tokio::fs::remove_file(&path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::error!(upload = %upload.id, error = %e, "deleting upload bytes failed");
    }
    Ok(Json(serde_json::json!({
        "ok": true,
        "upload_id": upload.id.as_str(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod validate_file_name {
        use super::*;

        #[test]
        fn accepts_a_plain_allowed_name_and_trims_it() {
            assert_eq!(
                validate_file_name(Some("  notes.txt ")).expect("valid"),
                "notes.txt"
            );
        }

        #[test]
        fn accepts_uppercase_extensions() {
            assert!(validate_file_name(Some("paper.PDF")).is_ok());
        }

        #[test]
        fn rejects_missing_and_blank_names() {
            assert!(validate_file_name(None).is_err());
            assert!(validate_file_name(Some("   ")).is_err());
        }

        #[test]
        fn rejects_every_path_like_spelling() {
            for bad in [
                "a/b.txt",
                "/etc/passwd",
                "..",
                "..txt",
                "a\\b.txt",
                "a\u{0}b.txt",
                "../up.txt",
            ] {
                assert!(
                    validate_file_name(Some(bad)).is_err(),
                    "must reject {bad:?}"
                );
            }
        }

        #[test]
        fn rejects_disallowed_and_missing_extensions() {
            for bad in [
                "payload.exe",
                "archive.zip",
                "noextension",
                "trailingdot.",
                ".txt",
            ] {
                let err = validate_file_name(Some(bad)).expect_err("rejected");
                assert!(err.to_string().contains("pdf"), "names the allowed set");
            }
        }

        #[test]
        fn rejects_an_oversized_name() {
            let long = format!("{}.txt", "x".repeat(MAX_FILE_NAME_CHARS));
            assert!(validate_file_name(Some(&long)).is_err());
        }
    }

    mod wire {
        use super::*;
        use crate::store::OwnerDigest;

        #[test]
        fn never_carries_the_storage_path() {
            let upload = Upload {
                id: UploadId::new(),
                owner_digest: OwnerDigest::from_token("t"),
                file_name: "notes.txt".into(),
                size_bytes: 9,
                content_hash: ContentHash::of_str("contents"),
                storage_path: ".uruk/uploads/abc/upl_x".into(),
                created_at: OffsetDateTime::now_utc(),
            };
            let serialized = wire(&upload).to_string();
            assert!(!serialized.contains(".uruk"), "no path: {serialized}");
            assert!(!serialized.contains("storage"), "no path: {serialized}");
        }
    }
}
