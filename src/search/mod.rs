//! Literature search: local passage retrieval and federated discovery over
//! freely accessible public scholarly APIs (OpenAlex, Crossref, arXiv).
//!
//! Uruk runs none of these operators' code: the connectors consume hosted
//! APIs. Search is an explicit opt-in (the `search:*` allowlist grants,
//! which require the network permission); the network permission alone
//! retains URL-fetch-only behavior and sends zero connector traffic. With search enabled, query
//! text, year filters, and the configured contact email are the only data
//! transmitted — never source contents, never the full goal record — and
//! every transmitted query is persisted verbatim in a `SearchRecord` so the
//! disclosure is auditable after the fact. Full-text resolution is a local
//! decision over discovery data; it makes no further network call.

pub mod arxiv;
pub mod chunk;
pub mod connector;
pub mod crossref;
pub mod dedup;
pub mod fuse;
pub mod openalex;
pub mod types;

pub use connector::{ApiAccess, Connector, ConnectorCx, RawResponse};
pub use types::{
    ConnectorHit, ExpectedMedia, FulltextLocation, SearchPage, SearchQuery, WorkHit, WorkKey,
    WorkRecord,
};

use crate::records::{Permissions, SearchId};

/// Contact email for the OpenAlex/Crossref polite pools (mailto + UA).
pub const ENV_CONTACT_EMAIL: &str = "URUK_CONTACT_EMAIL";

/// Allowlist entries gating each connector (`Permissions::allowed_tools`).
pub const TOOL_OPENALEX: &str = "search:openalex";
pub const TOOL_CROSSREF: &str = "search:crossref";
pub const TOOL_ARXIV: &str = "search:arxiv";

/// The MVP connector set, enabled by `--search` (narrowable with
/// `--search-connectors`).
pub const DEFAULT_CONNECTOR_TOOLS: [&str; 3] = [TOOL_OPENALEX, TOOL_CROSSREF, TOOL_ARXIV];

/// Connector names, for `--search-connectors` validation.
pub const CONNECTOR_NAMES: [&str; 3] = ["openalex", "crossref", "arxiv"];

/// Base-URL overrides, for offline tests against a local fixture server.
const ENV_OPENALEX_BASE: &str = "URUK_OPENALEX_BASE";
const ENV_CROSSREF_BASE: &str = "URUK_CROSSREF_BASE";
const ENV_ARXIV_BASE: &str = "URUK_ARXIV_BASE";

fn base_override(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.trim().is_empty())
}

/// The configured contact email, if any.
pub fn contact_email() -> Option<String> {
    std::env::var(ENV_CONTACT_EMAIL)
        .ok()
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
}

/// Whether any search connector is enabled for these permissions.
pub fn any_connector_enabled(permissions: &Permissions) -> bool {
    permissions.network
        && DEFAULT_CONNECTOR_TOOLS
            .iter()
            .any(|t| permissions.allows_tool(t))
}

/// Discovery connectors enabled by the permissions, in deterministic order.
pub fn discovery_connectors(permissions: &Permissions) -> Vec<Box<dyn Connector>> {
    let mut out: Vec<Box<dyn Connector>> = Vec::new();
    if !permissions.network {
        return out;
    }
    if permissions.allows_tool(TOOL_OPENALEX) {
        out.push(Box::new(openalex::OpenAlex::new(base_override(
            ENV_OPENALEX_BASE,
        ))));
    }
    if permissions.allows_tool(TOOL_CROSSREF) {
        out.push(Box::new(crossref::Crossref::new(base_override(
            ENV_CROSSREF_BASE,
        ))));
    }
    if permissions.allows_tool(TOOL_ARXIV) {
        out.push(Box::new(arxiv::Arxiv::new(base_override(ENV_ARXIV_BASE))));
    }
    out
}

/// Parse `--search-connectors` (a csv subset of the three connectors) into
/// allowlist entries. The flag only narrows the set after `--search`;
/// omitting `--search` is how a run opts out of connectors entirely.
pub fn parse_connector_flag(value: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for part in value.trim().split(',') {
        let name = part.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        if !CONNECTOR_NAMES.contains(&name.as_str()) {
            return Err(format!(
                "unknown search connector {name:?}; known: {} \
                 (to run without connectors, omit --search)",
                CONNECTOR_NAMES.join(", ")
            ));
        }
        out.push(format!("search:{name}"));
    }
    if out.is_empty() {
        return Err(format!(
            "--search-connectors names no connector; known: {} \
             (to run without connectors, omit --search)",
            CONNECTOR_NAMES.join(", ")
        ));
    }
    Ok(out)
}

/// Outcome of one (connector × query) search.
#[derive(Debug)]
pub struct ConnectorSearchOutcome {
    pub connector: &'static str,
    pub query: SearchQuery,
    pub hits: Vec<ConnectorHit>,
    /// The connector-reported total match count, when the response carried
    /// one; `SearchRecord.results_found` persists it (§13).
    pub total_found: Option<usize>,
    /// Connector-level failure; a partial federation proceeds (§14).
    pub error: Option<String>,
    /// Raw response bodies this search produced, persisted as artifacts.
    pub raw: Vec<RawResponse>,
}

/// Execute every (connector × query) search. `charge` is called before each
/// connector call; returning `false` (budget or deadline exhausted) stops
/// further calls cleanly, which is reported, never hidden.
pub async fn federated_search(
    connectors: &[Box<dyn Connector>],
    queries: &[SearchQuery],
    cx: &ConnectorCx,
    charge: &(dyn Fn() -> bool + Sync),
) -> Vec<ConnectorSearchOutcome> {
    let mut outcomes = Vec::new();
    for connector in connectors {
        for query in queries {
            if !charge() {
                tracing::warn!(
                    connector = connector.name(),
                    executed = outcomes.len(),
                    "federation stopped early (budget exhausted or cancelled); \
                     remaining searches skipped"
                );
                return outcomes;
            }
            let started = std::time::Instant::now();
            let result = connector.search(query, cx).await;
            let raw = cx.take_raw();
            let query_hash = crate::records::ContentHash::of_str(&query.text);
            match result {
                Ok(page) => {
                    tracing::info!(
                        target: "uruk::search",
                        connector = connector.name(),
                        query_hash = query_hash.short(),
                        results = page.hits.len(),
                        total_found = page.total_found,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "search.discover"
                    );
                    tracing::debug!(connector = connector.name(), query = %query.text, "query text");
                    outcomes.push(ConnectorSearchOutcome {
                        connector: connector.name(),
                        query: query.clone(),
                        hits: page.hits,
                        total_found: page.total_found,
                        error: None,
                        raw,
                    });
                }
                Err(e) => {
                    tracing::warn!(connector = connector.name(), error = %e, "connector failed");
                    outcomes.push(ConnectorSearchOutcome {
                        connector: connector.name(),
                        query: query.clone(),
                        hits: vec![],
                        total_found: None,
                        error: Some(e.to_string()),
                        raw,
                    });
                }
            }
        }
    }
    outcomes
}

/// One work after dedup and fusion. The pre-dedup per-connector ranks and
/// raw-item hashes live in `record.hits` (persisted as `works.body.hits`).
#[derive(Debug, Clone)]
pub struct FusedWork {
    pub record: WorkRecord,
    pub rrf_score: f64,
}

/// Dedup by WorkKey, merge field-wise, and fuse with RRF (§8).
/// `search_ids[i]` is the SearchRecord id for `outcomes[i]`, so each merged
/// work carries the audit trail of exactly which searches found it.
pub fn fuse_outcomes(
    outcomes: &[ConnectorSearchOutcome],
    search_ids: &[SearchId],
) -> Vec<FusedWork> {
    use std::collections::BTreeMap;

    assert_eq!(
        outcomes.len(),
        search_ids.len(),
        "one SearchRecord per outcome"
    );

    // Ranked lists per (connector × query), in outcome order.
    let lists: Vec<fuse::RankedList> = outcomes
        .iter()
        .map(|o| o.hits.iter().map(|h| h.work.work_key.clone()).collect())
        .collect();
    let fused = fuse::rrf_fuse(&lists);

    // Group hits by key for merge and audit.
    let mut by_key: BTreeMap<&WorkKey, Vec<(usize, &ConnectorHit)>> = BTreeMap::new();
    for (i, outcome) in outcomes.iter().enumerate() {
        for hit in &outcome.hits {
            by_key.entry(&hit.work.work_key).or_default().push((i, hit));
        }
    }

    fused
        .into_iter()
        .filter_map(|(key, score)| {
            let group = by_key.get(&key)?;
            let for_merge: Vec<(&str, &ConnectorHit)> = group
                .iter()
                .map(|(i, h)| (outcomes[*i].connector, *h))
                .collect();
            let mut record = dedup::merge_works(&for_merge);
            record.work_key = key.clone();
            record.hits = group
                .iter()
                .map(|(i, h)| WorkHit {
                    connector: outcomes[*i].connector.to_string(),
                    search_id: search_ids[*i].clone(),
                    rank: h.rank as u32,
                    raw_hash: h.raw_hash.clone(),
                })
                .collect();
            Some(FusedWork {
                record,
                rrf_score: score,
            })
        })
        .collect()
}

/// Resolve a legal open-access full-text location for one work — locally,
/// from the discovery data already in hand; no network call is made:
/// arXiv PDF, then OpenAlex `best_oa_location` (PDF, then landing page),
/// then OpenAlex `oa_url`. Every candidate URL was designated by a
/// connector as an open-access location.
pub fn resolve_fulltext(work: &WorkRecord) -> Option<FulltextLocation> {
    if let Some(id) = &work.arxiv_id {
        // Prefer the PDF URL the API supplied (it already points at the
        // right host, which matters when the base is overridden in tests).
        let url = work
            .oa_url
            .clone()
            .filter(|u| u.contains("/pdf/"))
            .unwrap_or_else(|| format!("https://arxiv.org/pdf/{id}"));
        return Some(FulltextLocation {
            url,
            expected_media: ExpectedMedia::Pdf,
            license: None,
            resolver: "arxiv",
        });
    }
    if let Some(url) = &work.oa_pdf_url {
        return Some(FulltextLocation {
            url: url.clone(),
            expected_media: ExpectedMedia::Pdf,
            license: None,
            resolver: "openalex",
        });
    }
    if let Some(url) = &work.oa_landing_url {
        return Some(FulltextLocation {
            url: url.clone(),
            expected_media: ExpectedMedia::Html,
            license: None,
            resolver: "openalex",
        });
    }
    work.oa_url.as_ref().map(|url| FulltextLocation {
        url: url.clone(),
        expected_media: if url.to_ascii_lowercase().contains(".pdf") {
            ExpectedMedia::Pdf
        } else {
            ExpectedMedia::Html
        },
        license: None,
        resolver: "openalex",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::ContentHash;

    fn hit(key: &str, rank: usize) -> ConnectorHit {
        let mut work = WorkRecord {
            title: format!("work {key}"),
            ..Default::default()
        };
        work.work_key = WorkKey(key.to_string());
        ConnectorHit {
            work,
            rank,
            raw_hash: ContentHash::of_str(key),
        }
    }

    #[test]
    fn fusing_outcomes_dedups_and_keeps_the_audit_trail_in_body_hits() {
        let outcomes = vec![
            ConnectorSearchOutcome {
                connector: "openalex",
                query: SearchQuery::new("q"),
                hits: vec![hit("doi:10.1/a", 1), hit("doi:10.1/b", 2)],
                total_found: Some(2),
                error: None,
                raw: vec![],
            },
            ConnectorSearchOutcome {
                connector: "crossref",
                query: SearchQuery::new("q"),
                hits: vec![hit("doi:10.1/a", 1)],
                total_found: Some(1),
                error: None,
                raw: vec![],
            },
        ];
        let ids = vec![SearchId::new(), SearchId::new()];
        let fused = fuse_outcomes(&outcomes, &ids);
        assert_eq!(fused.len(), 2, "same DOI from two connectors is one work");
        assert_eq!(fused[0].record.work_key.as_str(), "doi:10.1/a");
        assert_eq!(fused[0].record.hits.len(), 2, "ranks live in the record");
        assert_eq!(fused[0].record.hits[0].connector, "openalex");
        assert_eq!(fused[0].record.hits[0].search_id, ids[0]);
        assert_eq!(fused[0].record.hits[1].connector, "crossref");
        assert_eq!(fused[0].record.hits[1].search_id, ids[1]);
        assert_eq!(fused[1].record.hits.len(), 1);
        assert!(fused[0].rrf_score > fused[1].rrf_score);
    }

    #[test]
    fn connector_flag_narrows_but_cannot_disable() {
        assert_eq!(
            parse_connector_flag("openalex, arxiv").unwrap(),
            vec!["search:openalex".to_string(), "search:arxiv".to_string()]
        );
        assert!(
            parse_connector_flag("scopus").is_err(),
            "paid APIs are not connectors"
        );
        assert!(
            parse_connector_flag("none").is_err(),
            "opting out of search is done by omitting --search, not via the narrowing flag"
        );
        assert!(
            parse_connector_flag("unpaywall").is_err(),
            "not an MVP connector"
        );
        assert!(parse_connector_flag("").is_err());
    }

    #[test]
    fn connectors_are_gated_on_network_and_allowlist() {
        let none = Permissions::default();
        assert!(discovery_connectors(&none).is_empty());
        assert!(!any_connector_enabled(&none));

        let network_only = Permissions {
            network: true,
            ..Default::default()
        };
        assert!(
            discovery_connectors(&network_only).is_empty(),
            "the network permission alone enables nothing without the search allowlist entries"
        );
        assert!(!any_connector_enabled(&network_only));

        let enabled = Permissions {
            network: true,
            allowed_tools: DEFAULT_CONNECTOR_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ..Default::default()
        };
        let connectors = discovery_connectors(&enabled);
        assert_eq!(connectors.len(), 3);
        assert!(any_connector_enabled(&enabled));
    }

    #[test]
    fn resolution_is_local_and_ordered() {
        let arxiv_work = WorkRecord {
            arxiv_id: Some("2403.01234".into()),
            oa_pdf_url: Some("https://x/best.pdf".into()),
            ..Default::default()
        };
        let loc = resolve_fulltext(&arxiv_work).unwrap();
        assert_eq!(loc.resolver, "arxiv");
        assert_eq!(loc.url, "https://arxiv.org/pdf/2403.01234");

        let openalex_pdf = WorkRecord {
            oa_pdf_url: Some("https://x/best.pdf".into()),
            oa_landing_url: Some("https://x/landing".into()),
            oa_url: Some("https://x/oa".into()),
            ..Default::default()
        };
        let loc = resolve_fulltext(&openalex_pdf).unwrap();
        assert_eq!(
            (loc.resolver, loc.url.as_str()),
            ("openalex", "https://x/best.pdf")
        );
        assert_eq!(loc.expected_media, ExpectedMedia::Pdf);

        let landing_only = WorkRecord {
            oa_landing_url: Some("https://x/landing".into()),
            oa_url: Some("https://x/oa".into()),
            ..Default::default()
        };
        let loc = resolve_fulltext(&landing_only).unwrap();
        assert_eq!(
            (loc.resolver, loc.url.as_str()),
            ("openalex", "https://x/landing")
        );
        assert_eq!(loc.expected_media, ExpectedMedia::Html);

        let oa_only = WorkRecord {
            oa_url: Some("https://x/oa".into()),
            ..Default::default()
        };
        assert_eq!(resolve_fulltext(&oa_only).unwrap().url, "https://x/oa");

        assert!(resolve_fulltext(&WorkRecord::default()).is_none());
    }
}
