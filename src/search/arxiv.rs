//! arXiv: preprints with essentially guaranteed full text (§5.3).
//!
//! Freely accessible public API operated by arXiv (Cornell). Open-access
//! repository; per-paper licenses vary and are honored by storing text
//! locally for research use only. The API Terms of Use require no more than
//! one request every 3 seconds on a single connection — the limiter enforces
//! both.

use super::connector::{ApiAccess, Connector, ConnectorCx, urlencode};
use super::dedup::{self, normalize_doi};
use super::types::{ConnectorHit, SearchPage, SearchQuery, WorkRecord};
use crate::Result;
use crate::records::ContentHash;
use quick_xml::events::Event;
use std::time::Duration;

pub const DEFAULT_BASE: &str = "https://export.arxiv.org";

#[derive(Debug, Clone)]
pub struct Arxiv {
    pub base: String,
}

impl Arxiv {
    pub fn new(base: Option<String>) -> Self {
        Self {
            base: base.unwrap_or_else(|| DEFAULT_BASE.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl Connector for Arxiv {
    fn name(&self) -> &'static str {
        "arxiv"
    }

    fn access(&self) -> ApiAccess {
        ApiAccess::Open
    }

    /// arXiv API Terms of Use: no more than 1 request per 3 seconds.
    fn min_interval(&self) -> Duration {
        Duration::from_secs(3)
    }

    async fn search(&self, q: &SearchQuery, cx: &ConnectorCx) -> Result<SearchPage> {
        let url = format!(
            "{}/api/query?search_query=all:{}&start=0&max_results={}",
            self.base,
            urlencode(&q.text),
            q.capped_results(),
        );
        let _slot = cx.pace(&url, self.min_interval()).await;
        let body = cx
            .get_recorded(self.name(), &url, "application/atom+xml")
            .await?;
        let text = String::from_utf8_lossy(&body);
        let mut page = parse_atom(&text)?;
        // The API has no year filter; apply the bounds locally so the
        // recorded filter is honest. The reported total keeps the feed's
        // count: it describes what the query matched upstream.
        if q.year_from.is_some() || q.year_to.is_some() {
            page.hits.retain(|h| {
                let Some(year) = h.work.year else { return true };
                q.year_from.is_none_or(|from| year >= from) && q.year_to.is_none_or(|to| year <= to)
            });
            for (i, h) in page.hits.iter_mut().enumerate() {
                h.rank = i + 1;
            }
        }
        Ok(page)
    }
}

/// One `<entry>` being accumulated during Atom parsing.
#[derive(Default)]
struct Entry {
    id: String,
    title: String,
    summary: String,
    published: String,
    doi: String,
    authors: Vec<String>,
    pdf_url: Option<String>,
    raw: String,
}

/// Parse an arXiv Atom 1.0 feed into hits (feed order preserved) and the
/// feed's `opensearch:totalResults` count.
pub fn parse_atom(xml: &str) -> Result<SearchPage> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut hits = Vec::new();
    let mut total_found: Option<usize> = None;
    let mut entry: Option<Entry> = None;
    let mut path: Vec<String> = Vec::new();
    let mut rank = 0usize;

    loop {
        match reader
            .read_event()
            .map_err(|e| crate::Error::Search(format!("arxiv: invalid Atom XML: {e}")))?
        {
            Event::Start(start) => {
                let name = start.local_name().as_ref().to_string();
                if name == "entry" {
                    entry = Some(Entry::default());
                }
                path.push(name);
            }
            Event::Empty(start) => {
                let name = start.local_name().as_ref().to_string();
                if name == "link"
                    && let Some(e) = entry.as_mut()
                {
                    let mut href = None;
                    let mut is_pdf = false;
                    for attr in start.attributes().flatten() {
                        let key = attr.key.local_name().as_ref().to_string();
                        let value = attr
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .unwrap_or_else(|_| attr.value.clone())
                            .into_owned();
                        match key.as_str() {
                            "href" => href = Some(value),
                            "title" if value == "pdf" => is_pdf = true,
                            "type" if value == "application/pdf" => is_pdf = true,
                            _ => {}
                        }
                    }
                    if is_pdf && e.pdf_url.is_none() {
                        e.pdf_url = href;
                    }
                }
            }
            Event::Text(text) => {
                if entry.is_none() && path.last().map(String::as_str) == Some("totalResults") {
                    total_found = text.xml10_content().into_owned().trim().parse().ok();
                }
                if let Some(e) = entry.as_mut() {
                    let value = text.xml10_content().into_owned();
                    e.raw.push_str(&value);
                    match path.last().map(String::as_str) {
                        Some("id") if path.len() >= 2 && path[path.len() - 2] == "entry" => {
                            e.id.push_str(&value)
                        }
                        Some("title") => e.title.push_str(&value),
                        Some("summary") => e.summary.push_str(&value),
                        Some("published") => e.published.push_str(&value),
                        Some("doi") => e.doi.push_str(&value),
                        Some("name") if path.iter().any(|p| p == "author") => e.authors.push(value),
                        _ => {}
                    }
                }
            }
            Event::End(end) => {
                let name = end.local_name().as_ref().to_string();
                path.pop();
                if name == "entry"
                    && let Some(done) = entry.take()
                    && let Some(work) = finish_entry(&done)
                {
                    rank += 1;
                    hits.push(ConnectorHit {
                        work,
                        rank,
                        raw_hash: ContentHash::of_str(&done.raw),
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(SearchPage { hits, total_found })
}

fn finish_entry(e: &Entry) -> Option<WorkRecord> {
    let arxiv_id = dedup::normalize_arxiv_id(&e.id)?;
    let title = normalize_ws(&e.title);
    if title.is_empty() {
        return None;
    }
    let year = e.published.get(..4).and_then(|y| y.parse::<i32>().ok());
    let summary = normalize_ws(&e.summary);

    let mut record = WorkRecord {
        title,
        authors: e.authors.clone(),
        year,
        doi: Some(e.doi.as_str())
            .filter(|d| !d.trim().is_empty())
            .and_then(normalize_doi),
        arxiv_id: Some(arxiv_id),
        abstract_text: Some(summary).filter(|s| !s.is_empty()),
        oa_url: e.pdf_url.clone(),
        landing_url: Some(e.id.trim().to_string()).filter(|s| !s.is_empty()),
        ..Default::default()
    };
    record.work_key = dedup::work_key(&record);
    Some(record)
}

/// Collapse the whitespace arXiv wraps into titles and summaries.
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/search/arxiv_atom.xml");

    #[test]
    fn fixture_parses_to_golden_work_records() {
        let page = parse_atom(FIXTURE).unwrap();
        assert_eq!(page.total_found, Some(2), "opensearch:totalResults");
        let hits = page.hits;
        assert_eq!(hits.len(), 2);

        let first = &hits[0];
        assert_eq!(first.rank, 1);
        assert_eq!(first.work.work_key.as_str(), "arxiv:2403.01234");
        assert_eq!(
            first.work.title,
            "Deep learning surrogates for catalyst degradation"
        );
        assert_eq!(first.work.authors, vec!["Grace Hopper", "Alan Turing"]);
        assert_eq!(first.work.year, Some(2024));
        assert_eq!(
            first.work.abstract_text.as_deref(),
            Some("We study catalyst degradation with learned surrogates.")
        );
        assert_eq!(
            first.work.oa_url.as_deref(),
            Some("http://arxiv.org/pdf/2403.01234v2")
        );

        let second = &hits[1];
        // The entry carries a DOI, so the DOI outranks the arXiv id as key.
        assert_eq!(
            second.work.work_key.as_str(),
            "doi:10.1234/catalyst.2024.001"
        );
        assert_eq!(second.work.arxiv_id.as_deref(), Some("2401.00001"));
    }

    #[test]
    fn version_suffix_is_stripped_from_the_id() {
        let hits = parse_atom(FIXTURE).unwrap().hits;
        assert_eq!(hits[0].work.arxiv_id.as_deref(), Some("2403.01234"));
    }
}
