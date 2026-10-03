//! OpenAlex: primary discovery (§5.1).
//!
//! Freely accessible public API operated by OurResearch. The data is CC0 and
//! the backend is open-source, but Uruk consumes only the hosted API. No
//! credential; `mailto` joins the polite pool.

use super::connector::{ApiAccess, Connector, ConnectorCx, urlencode};
use super::dedup::{self, normalize_doi};
use super::types::{ConnectorHit, SearchPage, SearchQuery, WorkRecord};
use crate::Result;
use crate::records::ContentHash;
use std::time::Duration;

pub const DEFAULT_BASE: &str = "https://api.openalex.org";

#[derive(Debug, Clone)]
pub struct OpenAlex {
    pub base: String,
}

impl OpenAlex {
    pub fn new(base: Option<String>) -> Self {
        Self {
            base: base.unwrap_or_else(|| DEFAULT_BASE.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl Connector for OpenAlex {
    fn name(&self) -> &'static str {
        "openalex"
    }

    fn access(&self) -> ApiAccess {
        ApiAccess::OpenWithEmail { required: false }
    }

    /// Operator limits are 100,000 calls/day and 10 req/s; Uruk's floor is
    /// far more conservative.
    fn min_interval(&self) -> Duration {
        Duration::from_millis(250)
    }

    async fn search(&self, q: &SearchQuery, cx: &ConnectorCx) -> Result<SearchPage> {
        let mut filters = Vec::new();
        if let Some(from) = q.year_from {
            filters.push(format!("from_publication_date:{from}-01-01"));
        }
        if let Some(to) = q.year_to {
            filters.push(format!("to_publication_date:{to}-12-31"));
        }
        let mut url = format!(
            "{}/works?search={}&per-page={}&select=id,doi,title,display_name,publication_year,authorships,primary_location,open_access,best_oa_location,abstract_inverted_index,ids,cited_by_count,type",
            self.base,
            urlencode(&q.text),
            q.capped_results(),
        );
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
            .map_err(|e| crate::Error::Search(format!("openalex: invalid JSON: {e}")))?;
        Ok(SearchPage {
            hits: parse_works(&json),
            total_found: reported_total(&json),
        })
    }
}

/// The connector-reported total match count (`meta.count`).
pub fn reported_total(json: &serde_json::Value) -> Option<usize> {
    json["meta"]["count"].as_u64().map(|n| n as usize)
}

/// Parse an OpenAlex `/works` response into hits, relevance order preserved.
pub fn parse_works(json: &serde_json::Value) -> Vec<ConnectorHit> {
    let Some(results) = json["results"].as_array() else {
        return vec![];
    };
    results
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
    let title = item["display_name"]
        .as_str()
        .or_else(|| item["title"].as_str())?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }

    let authors: Vec<String> = item["authorships"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x["author"]["display_name"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let ids = &item["ids"];
    let pmid = ids["pmid"]
        .as_str()
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string());
    let pmcid = ids["pmcid"]
        .as_str()
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string());
    let arxiv_id = ids["arxiv"]
        .as_str()
        .and_then(dedup::normalize_arxiv_id)
        .or_else(|| {
            item["primary_location"]["landing_page_url"]
                .as_str()
                .filter(|u| u.contains("arxiv.org"))
                .and_then(dedup::normalize_arxiv_id)
        });

    let mut record = WorkRecord {
        title,
        authors,
        year: item["publication_year"].as_i64().map(|y| y as i32),
        venue: item["primary_location"]["source"]["display_name"]
            .as_str()
            .map(str::to_string),
        doi: item["doi"].as_str().and_then(normalize_doi),
        arxiv_id,
        pmid,
        pmcid,
        openalex_id: item["id"].as_str().map(str::to_string),
        abstract_text: reconstruct_abstract(&item["abstract_inverted_index"]),
        is_oa: item["open_access"]["is_oa"].as_bool(),
        oa_url: item["open_access"]["oa_url"].as_str().map(str::to_string),
        oa_pdf_url: item["best_oa_location"]["pdf_url"]
            .as_str()
            .map(str::to_string),
        oa_landing_url: item["best_oa_location"]["landing_page_url"]
            .as_str()
            .map(str::to_string),
        landing_url: item["primary_location"]["landing_page_url"]
            .as_str()
            .map(str::to_string),
        cited_by_count: item["cited_by_count"].as_u64(),
        ..Default::default()
    };
    record.work_key = dedup::work_key(&record);
    Some(record)
}

/// Invert OpenAlex's `abstract_inverted_index` (word → positions) back to
/// text: place each word at its positions and join with spaces.
pub fn reconstruct_abstract(index: &serde_json::Value) -> Option<String> {
    let map = index.as_object()?;
    let mut positions: Vec<(u64, &str)> = Vec::new();
    for (word, positions_json) in map {
        for p in positions_json.as_array()? {
            positions.push((p.as_u64()?, word.as_str()));
        }
    }
    if positions.is_empty() {
        return None;
    }
    positions.sort_unstable();
    Some(
        positions
            .into_iter()
            .map(|(_, w)| w)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/search/openalex_works.json");

    #[test]
    fn fixture_parses_to_golden_work_records() {
        let json: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let hits = parse_works(&json);
        assert_eq!(hits.len(), 3);
        assert_eq!(reported_total(&json), Some(3), "meta.count is the total");

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
        assert_eq!(first.work.pmid.as_deref(), Some("38012345"));
        assert_eq!(first.work.is_oa, Some(true));
        assert_eq!(
            first.work.oa_url.as_deref(),
            Some("https://example.org/oa/catalyst.pdf")
        );
        assert_eq!(first.work.cited_by_count, Some(42));
        assert_eq!(
            first.work.abstract_text.as_deref(),
            Some("Thermal cycling degrades catalyst activity over time")
        );

        let second = &hits[1];
        assert_eq!(second.rank, 2);
        assert_eq!(second.work.work_key.as_str(), "arxiv:2403.01234");
        assert_eq!(second.work.doi, None);
        assert_eq!(second.work.oa_pdf_url, None, "no best_oa_location given");

        // The designated OA locations come from `best_oa_location`.
        let third = &hits[2];
        assert_eq!(third.rank, 3);
        assert_eq!(
            third.work.work_key.as_str(),
            "doi:10.7777/openalexonly.2024.3"
        );
        assert_eq!(
            third.work.oa_pdf_url.as_deref(),
            Some("https://example.org/oa/coating.pdf")
        );
        assert_eq!(
            third.work.oa_landing_url.as_deref(),
            Some("https://example.org/oa/coating")
        );
        assert_eq!(
            third.work.abstract_text.as_deref(),
            Some("Coating adhesion improves with plasma treatment")
        );
    }

    #[test]
    fn inverted_abstract_round_trips_word_order() {
        let idx = serde_json::json!({
            "over": [4], "Thermal": [0], "cycling": [1], "degrades": [2],
            "activity": [3], "time": [5]
        });
        assert_eq!(
            reconstruct_abstract(&idx).as_deref(),
            Some("Thermal cycling degrades activity over time")
        );
        assert_eq!(reconstruct_abstract(&serde_json::Value::Null), None);
        assert_eq!(reconstruct_abstract(&serde_json::json!({})), None);
    }
}
