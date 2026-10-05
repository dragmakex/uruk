//! Anonymous browser ownership for the web API.
//!
//! One persistent, HttpOnly browser cookie identifies a visitor; every
//! web-created run belongs to exactly one such identity, different cookie
//! jars are isolated from each other, and runs without an owner record
//! (CLI runs, pre-cookie runs) are never exposed over the web.
//!
//! Everything runs offline against temporary SQLite stores through
//! `tower::ServiceExt::oneshot`, with cookies carried by hand: each
//! distinct token below is an independent "browser".
#![cfg(feature = "web")]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use uruk::provider::MockProvider;
use uruk::records::Mode;
use uruk::web::{AppState, WebConfig, router};

/// A valid-format browser token: 64 lowercase hex characters (256 bits).
fn token(fill: char) -> String {
    std::iter::repeat_n(fill, 64).collect()
}

async fn empty_app() -> (axum::Router, uruk::store::Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let state = AppState::new(
        store.clone(),
        Arc::new(MockProvider::new()),
        WebConfig::default(),
    );
    (router(state), store, dir)
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

fn get(uri: &str, cookie_token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(uri);
    if let Some(t) = cookie_token {
        builder = builder.header(header::COOKIE, format!("uruk_browser={t}"));
    }
    builder.body(Body::empty()).expect("request")
}

fn post_json(uri: &str, body: serde_json::Value, cookie_token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = cookie_token {
        builder = builder.header(header::COOKIE, format!("uruk_browser={t}"));
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

fn set_cookie(response: &axum::response::Response) -> Option<&str> {
    response
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().expect("set-cookie is ascii"))
}

/// Start a run owned by `cookie_token` and return its id.
async fn start_run(app: &axum::Router, cookie_token: &str) -> String {
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/runs",
            serde_json::json!({"goal": "what explains the measured drift"}),
            Some(cookie_token),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    body["run_id"].as_str().expect("run_id").to_string()
}

mod cookie_minting {
    use super::*;

    #[tokio::test]
    async fn the_first_identity_request_sets_a_persistent_host_only_cookie() {
        let (app, _store, _dir) = empty_app().await;
        let response = app.oneshot(get("/api/runs", None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let cookie = set_cookie(&response).expect("a cookie is minted");
        assert!(
            cookie.starts_with("uruk_browser="),
            "names the browser identity: {cookie}"
        );
        assert!(cookie.contains("HttpOnly"), "HttpOnly: {cookie}");
        assert!(cookie.contains("Path=/"), "Path=/: {cookie}");
        assert!(cookie.contains("SameSite=Lax"), "SameSite: {cookie}");
        assert!(cookie.contains("Max-Age=31536000"), "Max-Age: {cookie}");
        assert!(
            !cookie.contains("Domain"),
            "host-only, no Domain attribute: {cookie}"
        );
    }

    #[tokio::test]
    async fn the_minted_token_is_64_lowercase_hex_characters() {
        let (app, _store, _dir) = empty_app().await;
        let response = app.oneshot(get("/api/runs", None)).await.unwrap();
        let cookie = set_cookie(&response).expect("a cookie is minted");
        let value = cookie
            .trim_start_matches("uruk_browser=")
            .split(';')
            .next()
            .expect("cookie value");
        assert_eq!(value.len(), 64, "256 bits as hex: {value}");
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "lowercase hex: {value}"
        );
    }

    #[tokio::test]
    async fn two_mints_produce_different_tokens() {
        let (app, _store, _dir) = empty_app().await;
        let first = app.clone().oneshot(get("/api/runs", None)).await.unwrap();
        let second = app.oneshot(get("/api/runs", None)).await.unwrap();
        assert_ne!(
            set_cookie(&first).expect("first cookie"),
            set_cookie(&second).expect("second cookie"),
            "tokens are random per browser"
        );
    }

    #[tokio::test]
    async fn a_valid_cookie_is_kept_not_reissued() {
        let (app, _store, _dir) = empty_app().await;
        let t = token('a');
        let response = app.oneshot(get("/api/runs", Some(&t))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            set_cookie(&response),
            None,
            "an existing identity is not replaced"
        );
    }

    #[tokio::test]
    async fn the_secure_attribute_follows_the_configured_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
            .await
            .expect("open store");
        let state = AppState::new(
            store,
            Arc::new(MockProvider::new()),
            WebConfig {
                cookie_secure: true,
                ..WebConfig::default()
            },
        );
        let response = router(state).oneshot(get("/api/runs", None)).await.unwrap();
        let cookie = set_cookie(&response).expect("a cookie is minted");
        assert!(
            cookie.contains("; Secure"),
            "Secure when configured: {cookie}"
        );
    }

    #[tokio::test]
    async fn health_does_not_mint_a_cookie() {
        let (app, _store, _dir) = empty_app().await;
        let response = app.oneshot(get("/api/health", None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(set_cookie(&response), None, "health stays cookie-free");
    }

    #[tokio::test]
    async fn unknown_routes_do_not_mint_a_cookie() {
        let (app, _store, _dir) = empty_app().await;
        let response = app.oneshot(get("/api/nope", None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(set_cookie(&response), None, "404s stay cookie-free");
    }

    #[tokio::test]
    async fn a_malformed_cookie_is_replaced_with_a_fresh_identity() {
        let (app, _store, _dir) = empty_app().await;
        for bad in [
            "short",
            "UPPERCASE00aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "",
        ] {
            let response = app
                .clone()
                .oneshot(get("/api/runs", Some(bad)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let cookie = set_cookie(&response)
                .unwrap_or_else(|| panic!("malformed cookie {bad:?} is replaced"));
            let value = cookie
                .trim_start_matches("uruk_browser=")
                .split(';')
                .next()
                .expect("cookie value");
            assert_ne!(value, bad, "a fresh token replaces {bad:?}");
        }
    }

    #[tokio::test]
    async fn an_oversized_cookie_header_is_treated_as_absent() {
        let (app, _store, _dir) = empty_app().await;
        let huge = format!(
            "junk={}; uruk_browser={}",
            "x".repeat(16 * 1024),
            token('a')
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/runs")
                    .header(header::COOKIE, huge)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            set_cookie(&response).is_some(),
            "parsing is bounded: an absurd header gets a fresh identity"
        );
    }
}

mod owner_isolation {
    use super::*;

    #[tokio::test]
    async fn a_visitor_only_lists_their_own_runs() {
        let (app, _store, _dir) = empty_app().await;
        let run_id = start_run(&app, &token('a')).await;

        let mine = body_json(
            app.clone()
                .oneshot(get("/api/runs", Some(&token('a'))))
                .await
                .unwrap(),
        )
        .await;
        let runs = mine["runs"].as_array().expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["id"], run_id.as_str());

        let theirs = body_json(
            app.oneshot(get("/api/runs", Some(&token('b'))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            theirs["runs"],
            serde_json::json!([]),
            "another browser sees nothing"
        );
    }

    #[tokio::test]
    async fn foreign_run_detail_is_not_found() {
        let (app, _store, _dir) = empty_app().await;
        let run_id = start_run(&app, &token('a')).await;

        let response = app
            .oneshot(get(&format!("/api/runs/{run_id}"), Some(&token('b'))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await["kind"], "not_found");
    }

    #[tokio::test]
    async fn foreign_and_unknown_runs_are_indistinguishable() {
        let (app, _store, _dir) = empty_app().await;
        let run_id = start_run(&app, &token('a')).await;

        let foreign = app
            .clone()
            .oneshot(get(&format!("/api/runs/{run_id}"), Some(&token('b'))))
            .await
            .unwrap();
        let unknown = app
            .oneshot(get("/api/runs/run_missing", Some(&token('b'))))
            .await
            .unwrap();

        assert_eq!(foreign.status(), unknown.status());
        let foreign_body = body_json(foreign).await;
        let unknown_body = body_json(unknown).await;
        assert_eq!(foreign_body["kind"], unknown_body["kind"]);
        assert_eq!(
            foreign_body["error"]
                .as_str()
                .expect("prose")
                .replace(run_id.as_str(), "<id>"),
            unknown_body["error"]
                .as_str()
                .expect("prose")
                .replace("run_missing", "<id>"),
            "the two responses differ only by the echoed id"
        );
    }

    #[tokio::test]
    async fn foreign_stop_is_refused_and_changes_nothing() {
        let (app, store, _dir) = empty_app().await;
        let run_id = start_run(&app, &token('a')).await;

        let response = app
            .oneshot(post_json(
                &format!("/api/runs/{run_id}/stop"),
                serde_json::json!({}),
                Some(&token('b')),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let run = store
            .get_run(&uruk::records::RunId::from_raw(run_id))
            .await
            .unwrap();
        assert_ne!(
            run.state,
            uruk::records::RunState::Cancelled,
            "a foreign stop request must not cancel the run"
        );
    }

    #[tokio::test]
    async fn the_owner_can_stop_their_run() {
        let (app, _store, _dir) = empty_app().await;
        let run_id = start_run(&app, &token('a')).await;

        let response = app
            .oneshot(post_json(
                &format!("/api/runs/{run_id}/stop"),
                serde_json::json!({}),
                Some(&token('a')),
            ))
            .await
            .unwrap();
        // The background scheduler may already have completed the mock run;
        // both outcomes prove authorization passed.
        assert!(
            response.status() == StatusCode::OK || response.status() == StatusCode::BAD_REQUEST,
            "owner stop is authorized, got {}",
            response.status()
        );
    }

    #[tokio::test]
    async fn foreign_report_sources_passages_and_events_are_not_found() {
        let (app, _store, _dir) = empty_app().await;
        let run_id = start_run(&app, &token('a')).await;

        for uri in [
            format!("/api/runs/{run_id}/report"),
            format!("/api/runs/{run_id}/sources"),
            format!("/api/runs/{run_id}/passages?q=drift"),
            format!("/api/runs/{run_id}/events"),
        ] {
            let response = app
                .clone()
                .oneshot(get(&uri, Some(&token('b'))))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{uri} must not disclose a foreign run"
            );
        }
    }

    #[tokio::test]
    async fn the_owner_can_follow_their_runs_event_stream() {
        let (app, _store, _dir) = empty_app().await;
        let run_id = start_run(&app, &token('a')).await;

        let response = app
            .oneshot(get(
                &format!("/api/runs/{run_id}/events"),
                Some(&token('a')),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
    }

    #[tokio::test]
    async fn the_library_only_shows_sources_from_the_visitors_runs() {
        // Seed a CLI-style run with a source: no owner record exists, so no
        // web visitor may see the source in the library.
        let (app, fixture) = {
            let fixture = common::fixture(Mode::Task, MockProvider::new()).await;
            let state = AppState::new(
                fixture.store.clone(),
                fixture.provider.clone(),
                WebConfig::default(),
            );
            (router(state), fixture)
        };
        common::seed_source(&fixture, "paper-a", "measured effect text").await;

        let body = body_json(
            app.oneshot(get("/api/library", Some(&token('b'))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            body["sources"],
            serde_json::json!([]),
            "CLI-run sources are not in a web visitor's library"
        );
    }
}

mod cli_runs_stay_closed {
    use super::*;

    #[tokio::test]
    async fn runs_without_an_owner_record_are_invisible_and_unreadable() {
        let fixture = common::fixture(Mode::Task, MockProvider::new()).await;
        let state = AppState::new(
            fixture.store.clone(),
            fixture.provider.clone(),
            WebConfig::default(),
        );
        let app = router(state);

        let listing = body_json(
            app.clone()
                .oneshot(get("/api/runs", Some(&token('a'))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            listing["runs"],
            serde_json::json!([]),
            "a pre-cookie run is not claimable by the first visitor"
        );

        let detail = app
            .oneshot(get(
                &format!("/api/runs/{}", fixture.run_id),
                Some(&token('a')),
            ))
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::NOT_FOUND);
    }
}

mod durability {
    use super::*;

    #[tokio::test]
    async fn ownership_survives_a_server_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join(".uruk/state.sqlite");

        let run_id = {
            let store = uruk::store::Store::open(&db).await.expect("open store");
            let state = AppState::new(
                store.clone(),
                Arc::new(MockProvider::new()),
                WebConfig::default(),
            );
            let app = router(state);
            let run_id = start_run(&app, &token('a')).await;
            store.close().await;
            run_id
        };

        // A fresh store and router over the same database: the same cookie
        // still owns the run, a different cookie still sees nothing.
        let store = uruk::store::Store::open(&db).await.expect("reopen store");
        let state = AppState::new(store, Arc::new(MockProvider::new()), WebConfig::default());
        let app = router(state);

        let mine = body_json(
            app.clone()
                .oneshot(get("/api/runs", Some(&token('a'))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(mine["runs"][0]["id"], run_id.as_str());

        let theirs = body_json(
            app.oneshot(get("/api/runs", Some(&token('b'))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(theirs["runs"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn the_store_keeps_a_digest_of_the_token_never_the_token() {
        let (app, store, _dir) = empty_app().await;
        let t = token('a');
        start_run(&app, &t).await;

        let stored: String = sqlx::query_scalar("SELECT owner_digest FROM run_owners")
            .fetch_one(store.pool())
            .await
            .expect("one owner row exists");

        assert_ne!(stored, t, "the raw bearer token is never persisted");
        let expected = uruk::records::ContentHash::of_str(&t);
        assert_eq!(
            stored,
            expected.as_str(),
            "the stored owner id is the SHA-256 digest of the token"
        );
    }
}
