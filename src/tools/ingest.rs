//! Ingest supplied sources into the store (SPEC §4.2, §8, §12).
//!
//! Raw inputs stay read-only: extracted text is written to a task-owned
//! artifact, and the original is never modified. Every source records what
//! could actually be read, so a report never implies full-text verification
//! of material that was not extracted (SPEC §4.3).

use super::extract::{self, Coverage, Extracted};
use crate::records::*;
use crate::store::{Store, write_atomic};
use crate::{Error, Result};
use std::path::Path;
use std::time::Duration;
use time::OffsetDateTime;

/// A source plus the artifact holding its extracted text.
#[derive(Debug, Clone)]
pub struct Ingested {
    pub source: Source,
    pub artifact: Option<Artifact>,
}

/// Largest document retrieved from a URL. Larger material is recorded as
/// unavailable rather than partially read.
pub const MAX_RETRIEVAL_BYTES: u64 = 25 * 1024 * 1024;

/// Ingest one input into the store.
///
/// Network retrieval requires the `network` permission; a URL supplied without
/// it is recorded as unavailable rather than silently skipped (SPEC §12).
pub async fn ingest_input(
    store: &Store,
    run_id: &RunId,
    input: &InputRef,
    permissions: &Permissions,
) -> Result<Ingested> {
    let locator = input.locator.trim();

    if locator.starts_with("http://") || locator.starts_with("https://") {
        return ingest_url(store, run_id, input, permissions).await;
    }
    ingest_file(store, run_id, input, permissions).await
}

async fn ingest_file(
    store: &Store,
    run_id: &RunId,
    input: &InputRef,
    permissions: &Permissions,
) -> Result<Ingested> {
    let path = Path::new(&input.locator);

    // Enforce the read allowlist at the boundary, not by prompt (SPEC §12).
    if !permissions.read_paths.is_empty() {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let allowed = permissions.read_paths.iter().any(|p| {
            let base = Path::new(p)
                .canonicalize()
                .unwrap_or_else(|_| Path::new(p).to_path_buf());
            canonical.starts_with(&base)
        });
        if !allowed {
            return Err(Error::permission(format!(
                "{} is outside the permitted read paths",
                path.display()
            )));
        }
    }

    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|e| Error::validation(format!("cannot read input {}: {e}", path.display())))?;
    if !metadata.is_file() {
        return Err(Error::validation(format!(
            "{} is not a file",
            path.display()
        )));
    }

    let bytes = tokio::fs::read(path).await?;
    let content_hash = ContentHash::of_bytes(&bytes);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&input.locator)
        .to_string();

    let extracted = extract_by_extension(&extension, bytes).await?;

    let mut source = describe(
        run_id,
        Origin::LocalFile(input.locator.clone()),
        &extracted,
        content_hash,
    );
    if source.title.is_none() {
        source.title = Some(file_name);
    }
    finish(store, run_id, source, extracted).await
}

/// Extract text from file bytes by extension: the dispatch shared by
/// researcher-supplied local files and web uploads.
async fn extract_by_extension(extension: &str, bytes: Vec<u8>) -> Result<Extracted> {
    Ok(match extension {
        "txt" | "md" | "csv" | "json" | "tsv" | "rs" | "py" | "r" | "jsonl" => {
            Extracted::plain(String::from_utf8_lossy(&bytes).into_owned())
        }
        "pdf" => extract::pdf(bytes).await?,
        "html" | "htm" => extract::html(&String::from_utf8_lossy(&bytes), None),
        other => Extracted::none(format!(
            "no text extractor for .{other} files; only metadata was recorded"
        )),
    })
}

/// Ingest a web-uploaded file whose bytes the server already holds.
///
/// Unlike [`ingest_input`], no path allowlist applies: the storage
/// location was chosen by the server, never by the client. The
/// researcher-facing `file_name` becomes the recorded locator, so the
/// server's storage layout never appears in a source record.
pub async fn ingest_upload(
    store: &Store,
    run_id: &RunId,
    file_name: &str,
    bytes: Vec<u8>,
) -> Result<Ingested> {
    let content_hash = ContentHash::of_bytes(&bytes);
    let extension = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let extracted = extract_by_extension(&extension, bytes).await?;

    let mut source = describe(
        run_id,
        Origin::LocalFile(file_name.to_string()),
        &extracted,
        content_hash,
    );
    if source.title.is_none() {
        source.title = Some(file_name.to_string());
    }
    finish(store, run_id, source, extracted).await
}

async fn ingest_url(
    store: &Store,
    run_id: &RunId,
    input: &InputRef,
    permissions: &Permissions,
) -> Result<Ingested> {
    let url = input.locator.trim().to_string();

    // Retrieval is a capability, not an assumption. Without it the source is
    // recorded as unavailable so its absence is visible in the report.
    if !permissions.network {
        return unavailable(
            store,
            run_id,
            &url,
            "network retrieval is not permitted for this run; the source was not \
             retrieved and nothing in it has been read",
        )
        .await;
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(concat!("uruk/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Error::Provider(format!("cannot build HTTP client: {e}")))?;

    let mut response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            return unavailable(store, run_id, &url, &format!("retrieval failed: {e}")).await;
        }
    };
    let status = response.status();
    if !status.is_success() {
        return unavailable(
            store,
            run_id,
            &url,
            &format!("retrieval failed: HTTP {status}"),
        )
        .await;
    }
    if response.content_length().unwrap_or(0) > MAX_RETRIEVAL_BYTES {
        return unavailable(
            store,
            run_id,
            &url,
            &format!("document exceeds the {MAX_RETRIEVAL_BYTES}-byte retrieval limit"),
        )
        .await;
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Stream with the cap enforced as bytes arrive: a response without a
    // Content-Length header must not buffer unboundedly before a post-hoc
    // check. Oversized material is recorded as unavailable, never read.
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() as u64 + chunk.len() as u64 > MAX_RETRIEVAL_BYTES {
                    return unavailable(
                        store,
                        run_id,
                        &url,
                        &format!("document exceeds the {MAX_RETRIEVAL_BYTES}-byte retrieval limit"),
                    )
                    .await;
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                return unavailable(
                    store,
                    run_id,
                    &url,
                    &format!("retrieval failed while reading: {e}"),
                )
                .await;
            }
        }
    }
    let content_hash = ContentHash::of_bytes(&bytes);

    // Retrieved material is untrusted data: it is extracted, hashed, and
    // fenced when rendered, never treated as instructions (SPEC §12).
    let is_pdf = content_type.contains("application/pdf")
        || bytes.starts_with(b"%PDF")
        || url.to_ascii_lowercase().ends_with(".pdf");
    let is_html = content_type.contains("text/html")
        || content_type.contains("application/xhtml")
        || (content_type.is_empty() && bytes.trim_ascii_start().starts_with(b"<"));
    let extracted = if is_pdf {
        extract::pdf(bytes.to_vec()).await?
    } else if is_html {
        extract::html(&String::from_utf8_lossy(&bytes), Some(&url))
    } else if content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
    {
        Extracted::plain(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        Extracted::none(format!(
            "no text extractor for content type {:?}; only metadata was recorded",
            if content_type.is_empty() {
                "unknown"
            } else {
                &content_type
            }
        ))
    };

    let mut source = describe(run_id, Origin::Url(url.clone()), &extracted, content_hash);
    if source.title.is_none() {
        source.title = Some(url.clone());
    }
    if let Some(doi) = url.split("doi.org/").nth(1).filter(|d| !d.is_empty()) {
        source.identifier = Some(format!("doi:{doi}"));
    }
    finish(store, run_id, source, extracted).await
}

/// Record a URL that could not be retrieved, with the reason.
async fn unavailable(store: &Store, run_id: &RunId, url: &str, reason: &str) -> Result<Ingested> {
    let source = Source {
        id: SourceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        origin: Origin::Url(url.to_string()),
        title: Some(url.to_string()),
        authors: None,
        date: None,
        identifier: None,
        retrieved_at: OffsetDateTime::now_utc(),
        content_hash: ContentHash::of_str(url),
        access: AccessLevel::Unavailable,
        access_limitations: Some(reason.to_string()),
        text_artifact: None,
    };
    store.insert_source(&source).await?;
    Ok(Ingested {
        source,
        artifact: None,
    })
}

/// Build the source record from what extraction actually yielded.
fn describe(
    run_id: &RunId,
    origin: Origin,
    extracted: &Extracted,
    content_hash: ContentHash,
) -> Source {
    let (access, limitation) = match &extracted.coverage {
        Coverage::Full => (AccessLevel::FullText, None),
        Coverage::Partial {
            extracted: n,
            total,
        } => (
            AccessLevel::FullText,
            Some(format!(
                "text was extracted from {n} of {total} pages; the others yielded none \
                 (scanned or image-only) and were not read"
            )),
        ),
        Coverage::None { reason } => (AccessLevel::MetadataOnly, Some(reason.clone())),
    };
    Source {
        id: SourceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        origin,
        title: extracted.title.clone(),
        authors: extracted.authors.clone(),
        date: extracted.date.clone(),
        identifier: None,
        retrieved_at: OffsetDateTime::now_utc(),
        content_hash,
        access,
        access_limitations: limitation,
        text_artifact: None,
    }
}

/// Write the extracted text, if any, then commit the source and index its
/// passages. One code path for every text-bearing source — supplied files,
/// supplied URLs, and connector acquisitions — so local passage retrieval
/// works with zero connectors enabled.
async fn finish(
    store: &Store,
    run_id: &RunId,
    mut source: Source,
    extracted: Extracted,
) -> Result<Ingested> {
    let artifact = if extracted.text.trim().is_empty() {
        None
    } else {
        Some(write_text_artifact(store, run_id, &source.id, &extracted.text).await?)
    };
    source.text_artifact = artifact.as_ref().map(|a| a.id.clone());
    store.insert_source(&source).await?;
    if artifact.is_some() {
        store
            .index_source_passages(&source, &extracted.text)
            .await?;
    }
    Ok(Ingested { source, artifact })
}

async fn write_text_artifact(
    store: &Store,
    run_id: &RunId,
    source_id: &SourceId,
    text: &str,
) -> Result<Artifact> {
    let path = store.artifact_dir(run_id).join(format!("{source_id}.txt"));
    // Finalize the file before its reference is committed (SPEC §9.2).
    write_atomic(&path, text.as_bytes()).await?;

    let artifact = Artifact {
        id: ArtifactId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        media_type: "text/plain".to_string(),
        content_hash: ContentHash::of_str(text),
        size_bytes: text.len() as u64,
        storage_path: store.storable_path(&path),
        produced_by: None,
        access: AccessClass::Open,
        label: Some(format!("extracted text of {source_id}")),
        supersedes: None,
        superseded_reason: None,
        created_at: OffsetDateTime::now_utc(),
    };
    store.insert_artifact(&artifact).await?;
    Ok(artifact)
}
