//! Normalized records shared by every connector.
//!
//! Terminology discipline: the connectors are **freely accessible public
//! APIs**, not open-source software — Uruk runs none of their code. Where an
//! operator also open-sources their backend or releases data under an open
//! license, that is a property of the data/operator, stated as such.

use crate::records::ContentHash;
use serde::{Deserialize, Serialize};

/// Canonical dedup key. Priority: DOI > arXiv ID > PMID > title fingerprint.
///
/// Rendered as e.g. `doi:10.1038/s41586-…`, `arxiv:2403.01234`,
/// `pmid:38012345`, `fp:<16-hex of ContentHash(title|year|first_author)>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkKey(pub String);

impl WorkKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One query as sent to a connector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    /// Default 25, hard cap 50 (§22.12: polite traffic, meaningful RRF lists).
    pub max_results: usize,
}

/// Per-connector result cap defaults.
pub const DEFAULT_RESULTS_PER_QUERY: usize = 25;
pub const MAX_RESULTS_PER_QUERY: usize = 50;

impl SearchQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            year_from: None,
            year_to: None,
            max_results: DEFAULT_RESULTS_PER_QUERY,
        }
    }

    /// The effective result count: capped, never zero.
    pub fn capped_results(&self) -> usize {
        self.max_results.clamp(1, MAX_RESULTS_PER_QUERY)
    }

    /// Year filters as JSON, for `SearchRecord.filters`.
    pub fn filters_json(&self) -> Option<String> {
        if self.year_from.is_none() && self.year_to.is_none() {
            return None;
        }
        Some(
            serde_json::json!({
                "year_from": self.year_from,
                "year_to": self.year_to,
            })
            .to_string(),
        )
    }
}

/// One hit in one connector's ranked list, pre-dedup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorHit {
    pub work: WorkRecord,
    /// 1-based rank within this connector's list.
    pub rank: usize,
    /// Deterministic content hash identifying this item within the persisted
    /// raw response: the re-serialized JSON item (keys sorted) for the JSON
    /// connectors, the entry's text content for arXiv Atom. Not a hash of the
    /// raw bytes — those live whole in the recorded response artifact.
    pub raw_hash: ContentHash,
}

/// One connector's response to one query: the retrieved page plus the
/// connector-reported total, which `SearchRecord.results_found` persists so
/// coverage reflects how much the query matched, not just the page size.
#[derive(Debug, Clone)]
pub struct SearchPage {
    pub hits: Vec<ConnectorHit>,
    /// Total matches the connector reported (OpenAlex `meta.count`, Crossref
    /// `total-results`, arXiv `opensearch:totalResults`), when parseable.
    pub total_found: Option<usize>,
}

/// One pre-dedup per-connector hit, carried inside the merged work's body
/// (`works.body.hits`): the audit trail behind RRF, re-derivable without a
/// relational hit table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkHit {
    pub connector: String,
    /// The `SearchRecord` whose (connector × query) list produced this hit.
    pub search_id: crate::records::SearchId,
    /// 1-based rank in that connector's list.
    pub rank: u32,
    /// Hash of the raw per-item JSON/XML payload.
    pub raw_hash: ContentHash,
}

/// A discovered work, normalized across connectors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkRecord {
    pub work_key: WorkKey,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub venue: Option<String>,
    /// Normalized: lowercase, no resolver prefix.
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub pmid: Option<String>,
    pub pmcid: Option<String>,
    pub openalex_id: Option<String>,
    pub abstract_text: Option<String>,
    pub is_oa: Option<bool>,
    /// Candidate full-text URL, if the API supplied one.
    pub oa_url: Option<String>,
    /// OpenAlex `best_oa_location.pdf_url`: a designated OA PDF.
    pub oa_pdf_url: Option<String>,
    /// OpenAlex `best_oa_location.landing_page_url`: a designated OA page.
    pub oa_landing_url: Option<String>,
    pub landing_url: Option<String>,
    pub cited_by_count: Option<u64>,
    /// Per-connector ranks and raw-item hashes, filled at fusion time.
    #[serde(default)]
    pub hits: Vec<WorkHit>,
}

impl WorkRecord {
    /// Identifier string for `Source.identifier` (doi > arxiv > pmid).
    pub fn identifier(&self) -> Option<String> {
        if let Some(doi) = &self.doi {
            return Some(format!("doi:{doi}"));
        }
        if let Some(id) = &self.arxiv_id {
            return Some(format!("arxiv:{id}"));
        }
        self.pmid.as_ref().map(|p| format!("pmid:{p}"))
    }
}

/// What full text is expected at a resolved location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedMedia {
    Pdf,
    Html,
    Xml,
}

/// A legal open-access full-text location designated by a connector.
/// Resolved locally from discovery data; resolution makes no network call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FulltextLocation {
    pub url: String,
    pub expected_media: ExpectedMedia,
    pub license: Option<String>,
    /// `"arxiv" | "openalex"`.
    pub resolver: &'static str,
}
