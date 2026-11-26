use uruk::records::{ContentHash, SearchId};
use uruk::search::{self, ConnectorHit, ConnectorSearchOutcome, SearchQuery, WorkRecord};

fn hit(doi: Option<&str>, arxiv: Option<&str>, rank: usize) -> ConnectorHit {
    let mut work = WorkRecord {
        title: "A scoped study".into(),
        doi: doi.map(str::to_owned),
        arxiv_id: arxiv.map(str::to_owned),
        ..Default::default()
    };
    work.work_key = search::dedup::work_key(&work);
    ConnectorHit {
        work,
        rank,
        raw_hash: ContentHash::of_str(&rank.to_string()),
    }
}
fn outcome(connector: &'static str, hits: Vec<ConnectorHit>) -> ConnectorSearchOutcome {
    ConnectorSearchOutcome {
        connector,
        query: SearchQuery::new("scoped study"),
        hits,
        total_found: None,
        error: None,
        raw: vec![],
    }
}
#[test]
fn crosswalk_identifiers_deduplicate_even_when_primary_keys_differ() {
    let outcomes = vec![
        outcome("arxiv", vec![hit(None, Some("2403.01234v2"), 1)]),
        outcome(
            "openalex",
            vec![hit(Some("10.1234/study"), Some("2403.01234"), 2)],
        ),
    ];
    let ids = vec![SearchId::new(), SearchId::new()];
    let fused = search::fuse_outcomes(&outcomes, &ids);
    assert_eq!(
        fused.len(),
        1,
        "DOI/arXiv crosswalk must produce one canonical work"
    );
    assert_eq!(fused[0].record.work_key.as_str(), "doi:10.1234/study");
    assert_eq!(fused[0].record.hits.len(), 2);
    let mut reversed = outcomes;
    reversed.reverse();
    let mut ids = ids;
    ids.reverse();
    let replay = search::fuse_outcomes(&reversed, &ids);
    assert_eq!(replay[0].record.work_key, fused[0].record.work_key);
    assert_eq!(replay[0].rrf_score, fused[0].rrf_score);
}
#[test]
fn different_dois_with_the_same_title_are_not_fuzzy_merged() {
    let outcomes = vec![outcome(
        "crossref",
        vec![
            hit(Some("10.1234/one"), None, 1),
            hit(Some("10.1234/two"), None, 2),
        ],
    )];
    assert_eq!(
        search::fuse_outcomes(&outcomes, &[SearchId::new()]).len(),
        2
    );
}

#[test]
fn a_duplicate_within_one_list_cannot_boost_fusion_score() {
    let single = outcome("crossref", vec![hit(Some("10.1234/study"), None, 1)]);
    let duplicate = outcome(
        "crossref",
        vec![
            hit(Some("10.1234/study"), None, 1),
            hit(Some("10.1234/study"), None, 2),
        ],
    );
    let ids = vec![SearchId::new()];
    let expected = search::fuse_outcomes(&[single], &ids);
    let got = search::fuse_outcomes(&[duplicate], &ids);
    assert_eq!(got[0].rrf_score, expected[0].rrf_score);
    assert_eq!(got[0].record.hits.len(), 2, "duplicates remain auditable");
}
