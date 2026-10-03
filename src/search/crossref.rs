//! Crossref: DOI authority and second ranked list (§5.2).
//!
//! Freely accessible public API operated by Crossref, a nonprofit membership
//! organisation. Bibliographic metadata is openly reusable; abstracts carry
//! publisher-dependent terms and are stored for local analysis only. No OA
//! resolution data here; acquisition relies on arXiv and OpenAlex locations.

use super::connector::{ApiAccess, Connector, ConnectorCx, urlencode};
use super::dedup::{self, normalize_doi};
use super::types::{ConnectorHit, SearchPage, SearchQuery, WorkRecord};
use crate::Result;
use crate::records::ContentHash;
use std::time::Duration;

pub const DEFAULT_BASE: &str = "https://api.crossref.org";

#[derive(Debug, Clone)]
pub struct Crossref {
    pub base: String,
}

impl Crossref {
    pub fn new(base: Option<String>) -> Self {
        Self {
            base: base.unwrap_or_else(|| DEFAULT_BASE.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl Connector for Crossref {
    fn name(&self) -> &'static str {
        "crossref"
    }

    fn access(&self) -> ApiAccess {
        ApiAccess::OpenWithEmail { required: false }
    }

    /// Crossref publishes per-response `X-Rate-Limit-*` headers; Uruk's
    /// static floor stays below any advertised limit.
    fn min_interval(&self) -> Duration {
        Duration::from_millis(500)
    }

    async fn search(&self, q: &SearchQuery, cx: &ConnectorCx) -> Result<SearchPage> {
        let mut url = format!(
            "{}/works?query.bibliographic={}&rows={}&select=DOI,title,author,issued,container-title,abstract,score,type",
            self.base,
            urlencode(&q.text),
            q.capped_results(),
        );
        let mut filters = Vec::new();
        if let Some(from) = q.year_from {
            filters.push(format!("from-pub-date:{from}-01-01"));
        }
        if let Some(to) = q.year_to {
            filters.push(format!("until-pub-date:{to}-12-31"));
        }
        if !filters.is_empty() {
            url.push_str(&format!("&filter={}", urlencode(&filters.join(","))));
        }
        if let Some(email) = &cx.contact_email {
            url.push_str(&format!("&mailto={}", urlencode(email)));
        }

        let _slot = cx.pace(&url, self.min_interval()).await;
        let body = cx
            .get_recorded(self.name(), &url, "application/json")
            .await?;
        let json: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|e| crate::Error::Search(format!("crossref: invalid JSON: {e}")))?;
        Ok(SearchPage {
            hits: parse_works(&json),
            total_found: reported_total(&json),
        })
    }
}

/// The connector-reported total match count (`message.total-results`).
pub fn reported_total(json: &serde_json::Value) -> Option<usize> {
    json["message"]["total-results"]
        .as_u64()
        .map(|n| n as usize)
}

/// Parse a Crossref `/works` response; `score` order is the connector rank.
pub fn parse_works(json: &serde_json::Value) -> Vec<ConnectorHit> {
    let Some(items) = json["message"]["items"].as_array() else {
        return vec![];
    };
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let work = parse_work(item)?;
            Some(ConnectorHit {
                work,
                rank: i + 1,
                raw_hash: ContentHash::of_str(&item.to_string()),
            })
        })
        .collect()
}

fn parse_work(item: &serde_json::Value) -> Option<WorkRecord> {
    let title = item["title"]
        .as_array()
        .and_then(|t| t.first())
        .and_then(|t| t.as_str())?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }

    let authors: Vec<String> = item["author"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let family = x["family"].as_str()?;
                    Some(match x["given"].as_str() {
                        Some(given) => format!("{given} {family}"),
                        None => family.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let year = item["issued"]["date-parts"]
        .as_array()
        .and_then(|p| p.first())
        .and_then(|p| p.as_array())
        .and_then(|p| p.first())
        .and_then(|y| y.as_i64())
        .map(|y| y as i32);

    let mut record = WorkRecord {
        title,
        authors,
        year,
        venue: item["container-title"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(|c| c.as_str())
            .map(str::to_string),
        doi: item["DOI"].as_str().and_then(normalize_doi),
        abstract_text: item["abstract"].as_str().map(strip_jats),
        ..Default::default()
    };
    record.work_key = dedup::work_key(&record);
    Some(record)
}

/// Strip JATS XML tags from a Crossref abstract, collapsing whitespace.
pub fn strip_jats(jats: &str) -> String {
    let mut out = String::with_capacity(jats.len());
    let mut in_tag = false;
    for c in jats.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/search/crossref_works.json");

    #[test]
    fn fixture_parses_to_golden_work_records() {
        let json: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let hits = parse_works(&json);
        assert_eq!(hits.len(), 3);
        assert_eq!(
            reported_total(&json),
            Some(40),
            "total-results can exceed the retrieved page"
        );

        let first = &hits[0];
        assert_eq!(first.rank, 1);
        assert_eq!(
            first.work.work_key.as_str(),
            "doi:10.1234/catalyst.2024.001"
        );
        assert_eq!(
            first.work.title,
            "Catalyst degradation under thermal cycling"
        );
        assert_eq!(first.work.authors, vec!["Ada Lovelace", "Charles Babbage"]);
        assert_eq!(first.work.year, Some(2024));
        assert_eq!(first.work.venue.as_deref(), Some("Journal of Catalysis"));
        assert_eq!(
            first.work.abstract_text.as_deref(),
            Some("Abstract Thermal cycling degrades catalyst activity.")
        );

        let second = &hits[1];
        assert_eq!(second.work.work_key.as_str(), "doi:10.5555/other.2023.9");
        assert_eq!(
            second.work.abstract_text.as_deref(),
            Some("Electrode fouling abstract only.")
        );

        let third = &hits[2];
        assert_eq!(third.work.work_key.as_str(), "doi:10.9999/paywalled.2022.7");
        assert_eq!(third.work.abstract_text, None);
        assert_eq!(third.work.year, Some(2022));
    }

    #[test]
    fn jats_tags_are_stripped_from_abstracts() {
        assert_eq!(
            strip_jats("<jats:p>Some <jats:italic>text</jats:italic> &amp; more</jats:p>"),
            "Some text & more"
        );
    }
}
