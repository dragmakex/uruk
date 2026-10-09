//! Library and citation inspection over the web API (docs/WEB.md).
//!
//! Source detail, per-source passage browsing and search, owner-wide
//! metadata filters and passage search, and honest structured citation
//! inspection. Every read is run- and owner-constrained in SQL — a probing
//! browser learns nothing it does not own, and spans are resolved against
//! the canonical UTF-8 artifact bytes in Rust, never fabricated.
//!
//! Everything runs offline against temporary SQLite stores and the mock
//! provider, through `tower::ServiceExt::oneshot`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;
use uruk::provider::MockProvider;
use uruk::records::{AccessLevel, Locator, Mode, ObservationNote, Origin};
use uruk::web::{AppState, WebConfig, router};

/// Deliberately multibyte source text: Greek, accented Latin, and CJK, so
/// every byte-offset assertion exercises non-ASCII UTF-8 boundaries.
const MULTIBYTE_TEXT: &str = "Η θερμική ολίσθηση κυριαρχεί στον προϋπολογισμό της μέτρησης. \
     Détente thermique du détecteur observée.\n\n\
     熱ドリフトが測定の誤差収支を支配する。 Second paragraph about calibration drift.";

/// A valid-format browser token: 64 lowercase hex characters.
fn token(fill: char) -> String {
    std::iter::repeat_n(fill, 64).collect()
}

async fn seeded_app(mode: Mode) -> (axum::Router, common::Fixture) {
    let fixture = common::fixture_owned(mode, MockProvider::new(), common::BROWSER_TOKEN).await;
    let state = AppState::new(
        fixture.store.clone(),
        fixture.provider.clone(),
        WebConfig::default(),
    );
    (router(state), fixture)
}

/// A router over a CLI-style fixture: the run has no owner record.
async fn cli_app() -> (axum::Router, common::Fixture) {
    let fixture = common::fixture(Mode::Task, MockProvider::new()).await;
    let state = AppState::new(
        fixture.store.clone(),
        fixture.provider.clone(),
        WebConfig::default(),
    );
    (router(state), fixture)
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response body is JSON")
}

fn get_as(uri: &str, cookie_token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::COOKIE, format!("uruk_browser={cookie_token}"))
        .body(Body::empty())
        .expect("request")
}

fn get(uri: &str) -> Request<Body> {
    get_as(uri, common::BROWSER_TOKEN)
}

mod source_detail {
    use super::*;

    #[tokio::test]
    async fn reports_the_source_record_and_its_passage_count() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source_adjusted(&fixture, "paper-el", MULTIBYTE_TEXT, |s| {
            s.access = AccessLevel::AbstractOnly;
            s.access_limitations = Some("figures were not extracted".into());
            s.identifier = Some("10.1000/el.2025".into());
        })
        .await;
        let indexed = fixture
            .store
            .index_source_passages(&source, MULTIBYTE_TEXT)
            .await
            .unwrap();
        assert!(indexed > 0, "fixture text must index at least one passage");

        let uri = format!("/api/runs/{}/sources/{}", fixture.run_id, source.id);
        let response = app.oneshot(get(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["source"]["id"], source.id.as_str());
        assert_eq!(body["source"]["access"], "abstract_only");
        assert_eq!(
            body["source"]["access_limitations"],
            "figures were not extracted"
        );
        assert_eq!(body["source"]["identifier"], "10.1000/el.2025");
        assert_eq!(body["source"]["content_hash"], source.content_hash.as_str());
        assert_eq!(body["passage_count"], indexed as u64);
    }

    #[tokio::test]
    async fn a_source_without_indexed_text_reports_zero_passages() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source_adjusted(&fixture, "meta-only", "unused", |s| {
            s.access = AccessLevel::MetadataOnly;
            s.text_artifact = None;
        })
        .await;

        let uri = format!("/api/runs/{}/sources/{}", fixture.run_id, source.id);
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["passage_count"], 0);
    }

    #[tokio::test]
    async fn a_source_from_another_owned_run_is_not_served_under_this_run() {
        // Both runs belong to the same browser; the source is still only
        // reachable under its own run. This fails if the handler fetched the
        // source globally and filtered afterwards.
        let (app, fixture) = seeded_app(Mode::Task).await;
        let other_run = super::start_run(&app, common::BROWSER_TOKEN).await;
        let foreign = common::seed_source_adjusted(&fixture, "other-run", "text", |s| {
            s.run_id = uruk::records::RunId::from_raw(other_run);
        })
        .await;

        let uri = format!("/api/runs/{}/sources/{}", fixture.run_id, foreign.id);
        let response = app.oneshot(get(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_existing_but_unowned_source_answers_exactly_like_an_unknown_one() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper-a", "measured effect").await;

        // Browser B probes A's run and source: the run-level check answers.
        let uri = format!("/api/runs/{}/sources/{}", fixture.run_id, source.id);
        let foreign = app
            .clone()
            .oneshot(get_as(&uri, &token('b')))
            .await
            .unwrap();
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
        let foreign_body = body_json(foreign).await;

        // The owner probes an unknown source id in their own run: same
        // not_found kind, so probing ids discloses nothing.
        let unknown_uri = format!("/api/runs/{}/sources/src_unknown", fixture.run_id);
        let unknown = app.oneshot(get(&unknown_uri)).await.unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        let unknown_body = body_json(unknown).await;
        assert_eq!(unknown_body["kind"], foreign_body["kind"]);
    }

    #[tokio::test]
    async fn source_ids_of_a_cli_run_are_unreachable() {
        let (app, fixture) = cli_app().await;
        let source = common::seed_source(&fixture, "cli-paper", "cli text").await;
        let uri = format!("/api/runs/{}/sources/{}", fixture.run_id, source.id);
        let response = app.oneshot(get_as(&uri, &token('a'))).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

mod source_passages {
    use super::*;

    #[tokio::test]
    async fn browsing_lists_passages_in_sequence_order_with_byte_offsets() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper-el", MULTIBYTE_TEXT).await;
        fixture
            .store
            .index_source_passages(&source, MULTIBYTE_TEXT)
            .await
            .unwrap();

        let uri = format!(
            "/api/runs/{}/sources/{}/passages",
            fixture.run_id, source.id
        );
        let response = app.oneshot(get(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["ok"], true);
        let passages = body["passages"].as_array().expect("passages");
        assert!(!passages.is_empty());
        assert_eq!(body["total"], passages.len() as u64);

        for (i, p) in passages.iter().enumerate() {
            assert_eq!(p["seq"], i as u64, "sequence order");
            // The offsets are byte offsets into the canonical UTF-8 artifact
            // and the server resolved the text; the client never slices.
            let start = p["byte_start"].as_u64().expect("byte_start") as usize;
            let end = p["byte_end"].as_u64().expect("byte_end") as usize;
            assert!(MULTIBYTE_TEXT.is_char_boundary(start));
            assert!(MULTIBYTE_TEXT.is_char_boundary(end));
            assert_eq!(
                p["text"].as_str().expect("text"),
                &MULTIBYTE_TEXT[start..end],
                "server-resolved text matches the exact byte span"
            );
        }
    }

    #[tokio::test]
    async fn browsing_pages_with_offset_and_limit() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        // Three well-separated paragraphs, each too big to pack together.
        let text = format!(
            "{a}\n\n{b}\n\n{c}",
            a = "alpha ".repeat(350),
            b = "beta métrologie ".repeat(140),
            c = "gamma δοκιμή ".repeat(160),
        );
        let source = common::seed_source(&fixture, "paged", &text).await;
        let total = fixture
            .store
            .index_source_passages(&source, &text)
            .await
            .unwrap();
        assert!(total >= 3, "need at least three passages, got {total}");

        let uri = format!(
            "/api/runs/{}/sources/{}/passages?offset=1&limit=1",
            fixture.run_id, source.id
        );
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        assert_eq!(body["total"], total as u64);
        let passages = body["passages"].as_array().expect("passages");
        assert_eq!(passages.len(), 1);
        assert_eq!(passages[0]["seq"], 1, "offset skips the first passage");
    }

    #[tokio::test]
    async fn an_offset_past_the_end_is_an_empty_page_not_an_error() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "small", "tiny text").await;
        fixture
            .store
            .index_source_passages(&source, "tiny text")
            .await
            .unwrap();

        let uri = format!(
            "/api/runs/{}/sources/{}/passages?offset=999",
            fixture.run_id, source.id
        );
        let response = app.oneshot(get(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["passages"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn searching_is_scoped_to_the_source_not_the_run() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let here = common::seed_source(&fixture, "here", "catalyst degradation observed").await;
        let elsewhere =
            common::seed_source(&fixture, "elsewhere", "catalyst degradation elsewhere").await;
        for (s, t) in [
            (&here, "catalyst degradation observed"),
            (&elsewhere, "catalyst degradation elsewhere"),
        ] {
            fixture.store.index_source_passages(s, t).await.unwrap();
        }

        let uri = format!(
            "/api/runs/{}/sources/{}/passages?q=catalyst",
            fixture.run_id, here.id
        );
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        let passages = body["passages"].as_array().expect("passages");
        assert_eq!(passages.len(), 1, "only this source's hits: {passages:?}");
        assert_eq!(passages[0]["text"], "catalyst degradation observed");
    }

    #[tokio::test]
    async fn search_matches_accented_terms_via_diacritic_folding() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper-el", MULTIBYTE_TEXT).await;
        fixture
            .store
            .index_source_passages(&source, MULTIBYTE_TEXT)
            .await
            .unwrap();

        let uri = format!(
            "/api/runs/{}/sources/{}/passages?q=detente",
            fixture.run_id, source.id
        );
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        let passages = body["passages"].as_array().expect("passages");
        assert_eq!(passages.len(), 1, "ASCII query finds Détente: {passages:?}");
        let text = passages[0]["text"].as_str().expect("text");
        assert!(text.contains("Détente"), "verbatim bytes survive: {text}");
    }

    #[tokio::test]
    async fn a_blank_search_query_is_a_validation_error() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper", "text").await;
        let uri = format!(
            "/api/runs/{}/sources/{}/passages?q=%20",
            fixture.run_id, source.id
        );
        let response = app.oneshot(get(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["kind"], "validation");
    }

    #[tokio::test]
    async fn an_unparsable_limit_is_a_json_validation_error() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper", "text").await;
        let uri = format!(
            "/api/runs/{}/sources/{}/passages?limit=abc",
            fixture.run_id, source.id
        );
        let response = app.oneshot(get(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["kind"], "validation");
    }

    #[tokio::test]
    async fn foreign_browsers_get_not_found_for_passages() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper", "text").await;
        let uri = format!(
            "/api/runs/{}/sources/{}/passages",
            fixture.run_id, source.id
        );
        let response = app.oneshot(get_as(&uri, &token('b'))).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

mod library_filters {
    use super::*;

    async fn filterable_fixture() -> (axum::Router, common::Fixture) {
        let (app, fixture) = seeded_app(Mode::Task).await;
        common::seed_source_adjusted(&fixture, "full-url", "full text here", |s| {
            s.origin = Origin::Url("https://example.org/a".into());
            s.access = AccessLevel::FullText;
            s.authors = Some("Ηρώ Παπαδάκη".into());
        })
        .await;
        common::seed_source_adjusted(&fixture, "abstract-file", "abstract only", |s| {
            s.origin = Origin::LocalFile("b.md".into());
            s.access = AccessLevel::AbstractOnly;
            s.identifier = Some("arXiv:2501.01234".into());
        })
        .await;
        (app, fixture)
    }

    #[tokio::test]
    async fn the_access_filter_returns_only_matching_sources() {
        let (app, _fixture) = filterable_fixture().await;
        let body = body_json(
            app.oneshot(get("/api/library?access=abstract_only"))
                .await
                .unwrap(),
        )
        .await;
        let sources = body["sources"].as_array().expect("sources");
        assert_eq!(sources.len(), 1, "{sources:?}");
        assert_eq!(sources[0]["access"], "abstract_only");
    }

    #[tokio::test]
    async fn the_origin_filter_returns_only_matching_sources() {
        let (app, _fixture) = filterable_fixture().await;
        let body = body_json(app.oneshot(get("/api/library?origin=url")).await.unwrap()).await;
        let sources = body["sources"].as_array().expect("sources");
        assert_eq!(sources.len(), 1, "{sources:?}");
        assert_eq!(sources[0]["origin"]["kind"], "url");
    }

    #[tokio::test]
    async fn the_text_filter_matches_title_authors_and_identifier() {
        let (app, _fixture) = filterable_fixture().await;
        for (query, expected_title) in [
            ("full-url", "full-url"),        // title
            ("Παπαδάκη", "full-url"),        // authors, non-ASCII
            ("arxiv:2501", "abstract-file"), // identifier, case-insensitive
        ] {
            let uri = format!("/api/library?q={}", urlencode(query));
            let body = body_json(app.clone().oneshot(get(&uri)).await.unwrap()).await;
            let sources = body["sources"].as_array().expect("sources");
            assert_eq!(sources.len(), 1, "query {query:?}: {sources:?}");
            assert_eq!(sources[0]["title"], expected_title, "query {query:?}");
        }
    }

    #[tokio::test]
    async fn filters_combine_conjunctively() {
        let (app, _fixture) = filterable_fixture().await;
        let body = body_json(
            app.oneshot(get("/api/library?origin=url&access=abstract_only"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["sources"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn an_unknown_access_value_is_a_validation_error() {
        let (app, _fixture) = filterable_fixture().await;
        let response = app.oneshot(get("/api/library?access=total")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["kind"], "validation");
    }

    #[tokio::test]
    async fn an_unknown_origin_value_is_a_validation_error() {
        let (app, _fixture) = filterable_fixture().await;
        let response = app
            .oneshot(get("/api/library?origin=telepathy"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["kind"], "validation");
    }

    #[tokio::test]
    async fn filtered_results_never_include_other_owners_sources() {
        let (app, fixture) = filterable_fixture().await;
        // Both seeded sources match broad filters for their owner; a
        // different browser gets nothing, filtered or not. Keep the fixture
        // alive while issuing requests: it owns the temporary project
        // directory backing the router's still-open SQLite pool.
        let _fixture = fixture;
        for uri in [
            "/api/library",
            "/api/library?access=full_text",
            "/api/library?origin=url",
            "/api/library?q=full",
        ] {
            let body =
                body_json(app.clone().oneshot(get_as(uri, &token('b'))).await.unwrap()).await;
            assert_eq!(
                body["sources"],
                serde_json::json!([]),
                "{uri} leaked a foreign source"
            );
        }
    }
}

mod library_search {
    use super::*;

    #[tokio::test]
    async fn owner_wide_search_spans_the_owners_sources_and_names_them() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let a = common::seed_source(&fixture, "paper-a", "catalyst degradation in reactor A").await;
        let b = common::seed_source(&fixture, "paper-b", "catalyst poisoning in reactor B").await;
        for (s, t) in [
            (&a, "catalyst degradation in reactor A"),
            (&b, "catalyst poisoning in reactor B"),
        ] {
            fixture.store.index_source_passages(s, t).await.unwrap();
        }

        let body = body_json(
            app.oneshot(get("/api/library/passages?q=catalyst"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["ok"], true);
        let passages = body["passages"].as_array().expect("passages");
        assert_eq!(passages.len(), 2, "{passages:?}");
        for p in passages {
            assert_eq!(p["run_id"], fixture.run_id.as_str());
            let source_id = p["source_id"].as_str().expect("source_id");
            assert!(source_id == a.id.as_str() || source_id == b.id.as_str());
            assert!(
                p["source_title"] == "paper-a" || p["source_title"] == "paper-b",
                "the hit names its source: {p:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_blank_query_is_a_validation_error() {
        let (app, _fixture) = seeded_app(Mode::Task).await;
        let response = app
            .oneshot(get("/api/library/passages?q=%20%20"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["kind"], "validation");
    }

    #[tokio::test]
    async fn foreign_and_cli_passages_are_invisible_to_a_searching_browser() {
        // One store with an owned run (browser A) and a CLI run, both with
        // indexed passages matching the query.
        let (app, fixture) = seeded_app(Mode::Task).await;
        let owned = common::seed_source(&fixture, "owned", "shared keyword catalyst").await;
        fixture
            .store
            .index_source_passages(&owned, "shared keyword catalyst")
            .await
            .unwrap();

        let body = body_json(
            app.oneshot(get_as("/api/library/passages?q=catalyst", &token('b')))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            body["passages"],
            serde_json::json!([]),
            "browser B sees no foreign passages"
        );
    }

    #[tokio::test]
    async fn cli_run_passages_are_invisible_to_every_browser() {
        let (app, fixture) = cli_app().await;
        let source = common::seed_source(&fixture, "cli-paper", "catalyst data").await;
        fixture
            .store
            .index_source_passages(&source, "catalyst data")
            .await
            .unwrap();

        let body = body_json(
            app.oneshot(get_as("/api/library/passages?q=catalyst", &token('a')))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["passages"], serde_json::json!([]));
    }
}

mod citations {
    use super::*;

    /// The byte range of `needle` within `hay`, as exact UTF-8 offsets.
    fn span_of(hay: &str, needle: &str) -> (usize, usize) {
        let start = hay.find(needle).expect("needle present");
        (start, start + needle.len())
    }

    #[tokio::test]
    async fn a_deliverable_span_citation_resolves_to_the_exact_bytes() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper-el", MULTIBYTE_TEXT).await;
        let (start, end) = span_of(MULTIBYTE_TEXT, "Détente thermique du détecteur");
        common::seed_deliverable(
            &fixture,
            &serde_json::json!({
                "title": "Synthesis",
                "body": "Grounded synthesis.",
                "claims": [{
                    "claim": "the detector shows thermal detente",
                    "basis": "source_reported",
                    "citations": [{
                        "source_id": source.id.as_str(),
                        "locator": format!("chars {start}..{end}"),
                        "supports": "reports the observation"
                    }]
                }],
                "disagreements": [],
                "limitations": "",
                "next_actions": []
            }),
        )
        .await;

        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let response = app.oneshot(get(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["ok"], true);
        let citations = body["citations"].as_array().expect("citations");
        assert_eq!(citations.len(), 1, "{citations:?}");
        let c = &citations[0];
        assert_eq!(c["origin"]["kind"], "deliverable_claim");
        assert_eq!(c["origin"]["basis"], "source_reported");
        assert_eq!(c["source_id"], source.id.as_str());
        assert_eq!(c["source_title"], "paper-el");
        assert_eq!(c["resolution"]["kind"], "span");
        assert_eq!(c["resolution"]["start"], start as u64);
        assert_eq!(c["resolution"]["end"], end as u64);
        assert_eq!(
            c["resolution"]["text"], "Détente thermique du détecteur",
            "the span resolves to the exact artifact bytes"
        );
        assert_eq!(c["resolution"]["truncated"], false);
    }

    #[tokio::test]
    async fn a_coarse_locator_resolves_to_the_source_not_to_bytes() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper-a", "plain text").await;
        common::seed_deliverable(
            &fixture,
            &serde_json::json!({
                "title": "Synthesis",
                "body": "b",
                "claims": [{
                    "claim": "reported at page three",
                    "basis": "source_reported",
                    "citations": [{"source_id": source.id.as_str(), "locator": "p. 3"}]
                }],
            }),
        )
        .await;

        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        let c = &body["citations"][0];
        assert_eq!(c["resolution"]["kind"], "source_locator");
        assert_eq!(c["locator"], "p. 3");
    }

    #[tokio::test]
    async fn a_citation_of_an_unrecorded_source_is_unresolved_not_invented() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        common::seed_deliverable(
            &fixture,
            &serde_json::json!({
                "title": "Synthesis",
                "body": "b",
                "claims": [{
                    "claim": "cites a ghost",
                    "basis": "source_reported",
                    "citations": [{"source_id": "src_ghost", "locator": "p. 1"}]
                }],
            }),
        )
        .await;

        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        let c = &body["citations"][0];
        assert_eq!(c["resolution"]["kind"], "unresolved");
        assert_eq!(c["source_title"], serde_json::Value::Null);
        let reason = c["resolution"]["reason"].as_str().expect("reason");
        assert!(
            reason.contains("not recorded"),
            "the reason is honest: {reason}"
        );
    }

    #[tokio::test]
    async fn observation_citations_are_reported_with_their_review() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper-el", MULTIBYTE_TEXT).await;
        let item = common::seed_item(&fixture, "Candidate").await;
        let (start, end) = span_of(MULTIBYTE_TEXT, "熱ドリフトが測定の誤差収支を支配する。");
        let review = common::seed_observation_review(
            &fixture,
            &item,
            vec![
                ObservationNote {
                    source_id: source.id.clone(),
                    locator: Locator::Span { start, end },
                    observation: "the CJK passage reports drift".into(),
                    cause_established: false,
                    consistent_with_hypothesis: true,
                    expected_regardless: false,
                    alternative_explanations: vec![],
                    label: uruk::records::ExplanatoryLabel::Neutral,
                },
                ObservationNote {
                    source_id: source.id.clone(),
                    locator: Locator::Page(2),
                    observation: "a page-level observation".into(),
                    cause_established: false,
                    consistent_with_hypothesis: true,
                    expected_regardless: false,
                    alternative_explanations: vec![],
                    label: uruk::records::ExplanatoryLabel::Neutral,
                },
            ],
        )
        .await;

        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        let citations = body["citations"].as_array().expect("citations");
        assert_eq!(citations.len(), 2, "{citations:?}");

        let span = &citations[0];
        assert_eq!(span["origin"]["kind"], "review_observation");
        assert_eq!(span["origin"]["review_id"], review.id.as_str());
        assert_eq!(span["claim"], "the CJK passage reports drift");
        assert_eq!(span["resolution"]["kind"], "span");
        assert_eq!(
            span["resolution"]["text"],
            "熱ドリフトが測定の誤差収支を支配する。"
        );

        let page = &citations[1];
        assert_eq!(page["resolution"]["kind"], "source_locator");
        assert_eq!(page["locator"], "p. 2");
    }

    #[tokio::test]
    async fn a_span_inside_a_multibyte_character_is_unresolved() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "paper-el", MULTIBYTE_TEXT).await;
        // Byte 1 is inside the two-byte Greek capital Eta at the start.
        assert!(!MULTIBYTE_TEXT.is_char_boundary(1));
        let item = common::seed_item(&fixture, "Candidate").await;
        common::seed_observation_review(
            &fixture,
            &item,
            vec![ObservationNote {
                source_id: source.id.clone(),
                locator: Locator::Span { start: 1, end: 20 },
                observation: "mid-character span".into(),
                cause_established: false,
                consistent_with_hypothesis: true,
                expected_regardless: false,
                alternative_explanations: vec![],
                label: uruk::records::ExplanatoryLabel::Neutral,
            }],
        )
        .await;

        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        let c = &body["citations"][0];
        assert_eq!(c["resolution"]["kind"], "unresolved");
        let reason = c["resolution"]["reason"].as_str().expect("reason");
        assert!(
            reason.contains("character boundary"),
            "names the boundary problem: {reason}"
        );
    }

    #[tokio::test]
    async fn a_span_past_the_end_of_the_artifact_is_unresolved() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "short", "short text").await;
        let item = common::seed_item(&fixture, "Candidate").await;
        common::seed_observation_review(
            &fixture,
            &item,
            vec![ObservationNote {
                source_id: source.id.clone(),
                locator: Locator::Span {
                    start: 0,
                    end: 10_000,
                },
                observation: "out of bounds".into(),
                cause_established: false,
                consistent_with_hypothesis: true,
                expected_regardless: false,
                alternative_explanations: vec![],
                label: uruk::records::ExplanatoryLabel::Neutral,
            }],
        )
        .await;

        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        assert_eq!(body["citations"][0]["resolution"]["kind"], "unresolved");
    }

    #[tokio::test]
    async fn artifact_drift_makes_a_span_unresolved_instead_of_serving_new_bytes() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "drifty", "original artifact words").await;
        let item = common::seed_item(&fixture, "Candidate").await;
        common::seed_observation_review(
            &fixture,
            &item,
            vec![ObservationNote {
                source_id: source.id.clone(),
                locator: Locator::Span { start: 0, end: 8 },
                observation: "cites the original".into(),
                cause_established: false,
                consistent_with_hypothesis: true,
                expected_regardless: false,
                alternative_explanations: vec![],
                label: uruk::records::ExplanatoryLabel::Neutral,
            }],
        )
        .await;

        // The file on disk changes after the citation was recorded.
        tokio::fs::write(
            fixture.dir.path().join("drifty.txt"),
            "REWRITTEN artifact words",
        )
        .await
        .unwrap();

        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        let c = &body["citations"][0];
        assert_eq!(
            c["resolution"]["kind"], "unresolved",
            "drifted bytes must not be served as the citation"
        );
        let reason = c["resolution"]["reason"].as_str().expect("reason");
        assert!(reason.contains("hash"), "names the drift: {reason}");
    }

    #[tokio::test]
    async fn a_missing_artifact_file_is_unresolved() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let source = common::seed_source(&fixture, "vanishing", "will be deleted").await;
        let item = common::seed_item(&fixture, "Candidate").await;
        common::seed_observation_review(
            &fixture,
            &item,
            vec![ObservationNote {
                source_id: source.id.clone(),
                locator: Locator::Span { start: 0, end: 4 },
                observation: "cites a deleted file".into(),
                cause_established: false,
                consistent_with_hypothesis: true,
                expected_regardless: false,
                alternative_explanations: vec![],
                label: uruk::records::ExplanatoryLabel::Neutral,
            }],
        )
        .await;
        tokio::fs::remove_file(fixture.dir.path().join("vanishing.txt"))
            .await
            .unwrap();

        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let body = body_json(app.oneshot(get(&uri)).await.unwrap()).await;
        assert_eq!(body["citations"][0]["resolution"]["kind"], "unresolved");
    }

    #[tokio::test]
    async fn a_run_without_citations_reports_an_empty_list() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let response = app.oneshot(get(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["citations"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn foreign_browsers_get_not_found_for_citations() {
        let (app, fixture) = seeded_app(Mode::Task).await;
        let uri = format!("/api/runs/{}/citations", fixture.run_id);
        let response = app.oneshot(get_as(&uri, &token('b'))).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

mod store_level {
    use super::*;
    use uruk::store::{OwnerDigest, SourceFilter};

    #[tokio::test]
    async fn get_source_in_run_refuses_a_source_of_another_run() {
        let fixture = common::fixture(Mode::Task, MockProvider::new()).await;
        let source = common::seed_source(&fixture, "paper", "text").await;

        let other = uruk::records::RunId::from_raw("run_other");
        let err = fixture
            .store
            .get_source_in_run(&other, &source.id)
            .await
            .expect_err("a run must not serve another run's source");
        assert_eq!(err.kind(), "not_found");

        // The same message as a source that does not exist at all, so the
        // two cases are indistinguishable.
        let missing = fixture
            .store
            .get_source_in_run(
                &fixture.run_id,
                &uruk::records::SourceId::from_raw("src_no"),
            )
            .await
            .expect_err("unknown source");
        assert_eq!(
            err.to_string().replace(source.id.as_str(), "<id>"),
            missing.to_string().replace("src_no", "<id>"),
        );
    }

    #[tokio::test]
    async fn source_passage_queries_are_run_constrained() {
        let fixture = common::fixture(Mode::Task, MockProvider::new()).await;
        let source = common::seed_source(&fixture, "paper", "catalyst degradation text").await;
        fixture
            .store
            .index_source_passages(&source, "catalyst degradation text")
            .await
            .unwrap();

        let other = uruk::records::RunId::from_raw("run_other");
        assert_eq!(
            fixture
                .store
                .count_source_passages(&other, &source.id)
                .await
                .unwrap(),
            0
        );
        assert!(
            fixture
                .store
                .list_source_passages(&other, &source.id, 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                .store
                .search_passages_in_source(&other, &source.id, "catalyst", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn owner_wide_passage_search_is_owner_constrained_in_sql() {
        let fixture =
            common::fixture_owned(Mode::Task, MockProvider::new(), common::BROWSER_TOKEN).await;
        let source = common::seed_source(&fixture, "paper", "catalyst degradation text").await;
        fixture
            .store
            .index_source_passages(&source, "catalyst degradation text")
            .await
            .unwrap();

        let owner = OwnerDigest::from_token(common::BROWSER_TOKEN);
        let hits = fixture
            .store
            .search_passages_owned(&owner, "catalyst", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].run_id, fixture.run_id);
        assert_eq!(hits[0].source_title.as_deref(), Some("paper"));

        let stranger = OwnerDigest::from_token(&token('b'));
        assert!(
            fixture
                .store
                .search_passages_owned(&stranger, "catalyst", 10)
                .await
                .unwrap()
                .is_empty(),
            "a different owner finds nothing"
        );
    }

    #[tokio::test]
    async fn the_metadata_text_filter_treats_instr_literally_not_as_a_pattern() {
        // `%` and `_` are LIKE metacharacters; the filter must treat them as
        // literal text and match nothing here.
        let fixture =
            common::fixture_owned(Mode::Task, MockProvider::new(), common::BROWSER_TOKEN).await;
        common::seed_source(&fixture, "plain-title", "text").await;

        let owner = OwnerDigest::from_token(common::BROWSER_TOKEN);
        let filter = SourceFilter {
            text: Some("%".into()),
            ..SourceFilter::default()
        };
        assert!(
            fixture
                .store
                .list_sources_owned(&owner, &filter)
                .await
                .unwrap()
                .is_empty(),
            "a literal % matches nothing in 'plain-title'"
        );
    }
}

/// Start a run owned by `cookie_token` over the API and return its id.
async fn start_run(app: &axum::Router, cookie_token: &str) -> String {
    let request = Request::builder()
        .method("POST")
        .uri("/api/runs")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("uruk_browser={cookie_token}"))
        .body(Body::from(
            serde_json::json!({"goal": "library test run"}).to_string(),
        ))
        .expect("request");
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    body_json(response).await["run_id"]
        .as_str()
        .expect("run_id")
        .to_string()
}

/// Percent-encode a query value (tests only; std has no encoder).
fn urlencode(raw: &str) -> String {
    let mut out = String::new();
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
