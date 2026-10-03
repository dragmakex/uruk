//! Source provenance and citation locators (SPEC §4.2, §4.3).

use super::ids::*;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// How much of a source Uruk could actually read.
///
/// SPEC §4.3 forbids implying full-text verification when only an abstract was
/// accessible, so this is recorded per source and surfaced in reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    /// Full text was retrieved and read.
    FullText,
    /// Only an abstract or summary was available.
    AbstractOnly,
    /// Only metadata (title/authors/venue) was available.
    MetadataOnly,
    /// Retrieval or extraction failed.
    Unavailable,
}

impl AccessLevel {
    /// Whether a claim may cite this source as read in full.
    pub fn permits_fulltext_claim(self) -> bool {
        matches!(self, Self::FullText)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullText => "full_text",
            Self::AbstractOnly => "abstract_only",
            Self::MetadataOnly => "metadata_only",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Where a source came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "at")]
pub enum Origin {
    /// A file supplied by the researcher.
    LocalFile(String),
    /// A URL retrieved through an approved tool.
    Url(String),
    /// A dataset supplied by the researcher.
    Dataset(String),
    /// A code repository or file.
    Code(String),
    /// Supplied inline by the researcher (pasted text, an observation).
    Supplied,
}

/// An exact location within a source, for citation (SPEC §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "at")]
pub enum Locator {
    Page(u32),
    Section(String),
    Figure(String),
    Table(String),
    /// A location in code: path plus optional line range.
    CodeLocation {
        path: String,
        lines: Option<String>,
    },
    /// A slice of a dataset: column, row range, or query.
    DataSlice(String),
    /// A byte offset range into the UTF-8 extracted-text artifact.
    ///
    /// Offsets are byte positions in the canonical artifact file (always on
    /// UTF-8 character boundaries), matching `passages.byte_start/byte_end`,
    /// so a span citation resolves to an exact slice of the file.
    Span {
        start: usize,
        end: usize,
    },
}

impl Locator {
    /// Human-facing rendering for citations in reports.
    pub fn render(&self) -> String {
        match self {
            Self::Page(p) => format!("p. {p}"),
            Self::Section(s) => format!("§{s}"),
            Self::Figure(f) => format!("Fig. {f}"),
            Self::Table(t) => format!("Table {t}"),
            Self::CodeLocation { path, lines } => match lines {
                Some(l) => format!("{path}:{l}"),
                None => path.clone(),
            },
            Self::DataSlice(s) => format!("data[{s}]"),
            Self::Span { start, end } => format!("chars {start}..{end}"),
        }
    }
}

/// A retrieved or supplied source (SPEC §4.2 `Source`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub schema_version: u32,
    pub run_id: RunId,
    pub origin: Origin,
    pub title: Option<String>,
    pub authors: Option<String>,
    /// Publication or creation date, as reported by the source.
    pub date: Option<String>,
    /// DOI, arXiv ID, ISBN, or similar.
    pub identifier: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub retrieved_at: OffsetDateTime,
    /// Hash of the retrieved bytes, pinning exactly what was read.
    pub content_hash: ContentHash,
    pub access: AccessLevel,
    /// What could not be read: paywalled text, omitted figures, OCR failures.
    pub access_limitations: Option<String>,
    /// Extracted text, when available, stored as an artifact.
    pub text_artifact: Option<ArtifactId>,
}

impl Source {
    /// Short citation label for reports, e.g. "Gottweis et al. (2025)".
    pub fn short_citation(&self) -> String {
        match (&self.authors, &self.date) {
            (Some(a), Some(d)) => format!("{a} ({d})"),
            (Some(a), None) => a.clone(),
            (None, Some(d)) => format!("{} ({d})", self.title.as_deref().unwrap_or("untitled")),
            (None, None) => self
                .title
                .clone()
                .unwrap_or_else(|| self.id.as_str().to_string()),
        }
    }
}

/// A record of one executed search, so coverage is auditable (SPEC §4.3).
///
/// "Novel within searched sources" is not a universal novelty guarantee, so the
/// queries, dates, filters, and what was unavailable are all persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRecord {
    /// Durable identity, so a work's hits and later acquisition outcomes
    /// refer back to the exact search that produced them.
    pub id: SearchId,
    pub run_id: RunId,
    pub query: String,
    pub tool: String,
    #[serde(with = "time::serde::rfc3339")]
    pub executed_at: OffsetDateTime,
    pub filters: Option<String>,
    /// Connector-reported total matches (may exceed the page retrieved).
    pub results_found: usize,
    pub results_retrieved: Vec<SourceId>,
    /// Material identified but not retrievable, with the reason.
    pub unavailable: Vec<String>,
    /// Connector failures during this search; a partial federation is
    /// recorded, not hidden.
    pub connector_errors: Vec<String>,
}
