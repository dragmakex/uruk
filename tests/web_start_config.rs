//! The full safe research configuration on `POST /api/runs`.
//!
//! The web start path accepts everything the CLI accepts except grants
//! with side effects an anonymous cookie cannot answer for: local
//! subprocess execution and arbitrary tool names stay CLI-only, and no
//! request field ever names a server filesystem path. Files enter runs
//! only through owner-bound uploads (`tests/web_uploads.rs`); URLs are
//! fetched only under an explicit network grant; literature search is an
//! explicit opt-in on top of that, with the same disclosure semantics as
//! the CLI's `--search`.
//!
//! Everything runs offline against temporary SQLite stores through
//! `tower::ServiceExt::oneshot`.
#![cfg(feature = "web")]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use uruk::provider::MockProvider;
use uruk::records::{Mode, RunId};
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

fn get(uri: &str, cookie_token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::COOKIE, format!("uruk_browser={cookie_token}"))
        .body(Body::empty())
        .expect("request")
}

fn post_json(uri: &str, body: serde_json::Value, cookie_token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("uruk_browser={cookie_token}"))
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// Start a run and return its id, asserting creation succeeded.
async fn start(app: &axum::Router, payload: serde_json::Value, cookie_token: &str) -> RunId {
    let response = app
        .clone()
        .oneshot(post_json("/api/runs", payload, cookie_token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    RunId::from_raw(body["run_id"].as_str().expect("run_id").to_string())
}

/// Upload `bytes` as `name` and return the upload id.
async fn upload(app: &axum::Router, name: &str, bytes: &[u8], cookie_token: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uploads?name={name}"))
                .header(header::COOKIE, format!("uruk_browser={cookie_token}"))
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(bytes.to_vec()))
                .expect("request"),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    body["upload"]["id"]
        .as_str()
        .expect("upload id")
        .to_string()
}

/// Fetch the goal record behind a freshly started run.
async fn goal_of(store: &uruk::store::Store, run_id: &RunId) -> uruk::records::Goal {
    let run = store.get_run(run_id).await.expect("run exists");
    store.get_goal(&run.goal_id).await.expect("goal exists")
}

mod configuration {
    use super::*;

    #[tokio::test]
    async fn the_full_configuration_lands_in_the_goal_record() {
        let (app, store, _dir) = empty_app().await;
        let run_id = start(
            &app,
            serde_json::json!({
                "goal": "what limits the detector's energy resolution",
                "mode": "campaign",
                "ranking": "tournament",
                "profile": "materials-science",
                "deliverables": ["ranked hypotheses"],
                "preferences": ["grounded in the supplied sources"],
                "attributes": ["novel", "testable"],
                "constraints": ["cryogenic operation only"],
                "max_model_calls": 44,
                "max_seconds": 1200,
                "max_iterations": 6,
                "max_debate_turns": 7,
                "allow_network": true,
                "search": true,
                "max_acquisitions": 3,
            }),
            &token('a'),
        )
        .await;

        let goal = goal_of(&store, &run_id).await;
        assert_eq!(goal.mode, Mode::Campaign);
        assert_eq!(goal.profile.as_deref(), Some("materials-science"));
        assert_eq!(goal.deliverables, vec!["ranked hypotheses".to_string()]);
        assert_eq!(goal.rubric.attributes, vec!["novel", "testable"]);
        assert_eq!(
            goal.rubric.constraints,
            vec!["cryogenic operation only".to_string()]
        );
        assert_eq!(goal.budget.max_model_calls, 44);
        assert_eq!(goal.budget.wall_clock_secs, 1200);
        assert_eq!(goal.budget.max_iterations, 6);
        assert_eq!(goal.budget.max_debate_turns, 7);
        assert_eq!(goal.budget.max_acquisitions, 3);

        let p = &goal.permissions;
        assert!(p.network, "the network grant was given");
        assert!(!p.execute, "execution is never grantable over the web");
        assert!(p.write_paths.is_empty());
        assert!(p.require_approval_for.is_empty());
        let mut tools = p.allowed_tools.clone();
        tools.sort();
        assert_eq!(
            tools,
            vec!["search:arxiv", "search:crossref", "search:openalex"],
            "search enables exactly the three connectors"
        );
    }

    #[tokio::test]
    async fn defaults_grant_nothing_beyond_provider_disclosure() {
        let (app, store, _dir) = empty_app().await;
        let run_id = start(
            &app,
            serde_json::json!({"goal": "why do the measurements disagree"}),
            &token('a'),
        )
        .await;

        let goal = goal_of(&store, &run_id).await;
        let p = &goal.permissions;
        assert!(!p.network);
        assert!(!p.execute);
        assert!(p.allowed_tools.is_empty());
        assert!(p.read_paths.is_empty());
        assert!(p.disclose_to_provider);
        assert_eq!(goal.budget.max_debate_turns, 1, "default ranking is direct");
        assert_eq!(goal.budget.max_acquisitions, 8);
        assert_eq!(goal.profile, None);
    }

    #[tokio::test]
    async fn execution_and_path_grants_are_not_accepted_fields() {
        let (app, store, _dir) = empty_app().await;
        for (key, value) in [
            ("allow_execute", serde_json::json!(true)),
            ("tools", serde_json::json!(["shell"])),
            ("read_paths", serde_json::json!(["/etc"])),
            ("write_paths", serde_json::json!(["/tmp"])),
            ("inputs", serde_json::json!(["/etc/passwd"])),
        ] {
            let mut payload = serde_json::json!({"goal": "a real question"});
            payload[key] = value;
            let response = app
                .clone()
                .oneshot(post_json("/api/runs", payload, &token('a')))
                .await
                .unwrap();
            // Unknown fields are refused by deserialization: axum's JSON
            // rejection status, with the stable validation body.
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY, "{key}");
            let body = body_json(response).await;
            assert_eq!(body["ok"], false, "{key}");
            assert_eq!(body["kind"], "validation", "{key}");
        }
        assert!(
            store.list_runs().await.expect("list").is_empty(),
            "nothing was created"
        );
    }

    #[tokio::test]
    async fn debate_turns_must_match_the_ranking_method() {
        let (app, store, _dir) = empty_app().await;

        // Direct comparison is single-turn by definition.
        let response = app
            .clone()
            .oneshot(post_json(
                "/api/runs",
                serde_json::json!({"goal": "q", "ranking": "simple", "max_debate_turns": 3}),
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // Multi-turn debate caps at the published hard cap of 10.
        let response = app
            .clone()
            .oneshot(post_json(
                "/api/runs",
                serde_json::json!({"goal": "q", "ranking": "tournament", "max_debate_turns": 11}),
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // One turn is not a multi-turn debate; the truthful choice for that
        // is the direct method.
        let response = app
            .clone()
            .oneshot(post_json(
                "/api/runs",
                serde_json::json!({"goal": "q", "ranking": "tournament", "max_debate_turns": 1}),
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let run_id = start(
            &app,
            serde_json::json!({"goal": "q", "ranking": "tournament", "max_debate_turns": 2}),
            &token('a'),
        )
        .await;
        assert_eq!(goal_of(&store, &run_id).await.budget.max_debate_turns, 2);
    }

    #[tokio::test]
    async fn acquisition_and_profile_bounds_are_enforced() {
        let (app, _store, _dir) = empty_app().await;
        for payload in [
            serde_json::json!({"goal": "q", "max_acquisitions": 0}),
            serde_json::json!({"goal": "q", "max_acquisitions": 101}),
            serde_json::json!({"goal": "q", "profile": "x".repeat(200)}),
        ] {
            let response = app
                .clone()
                .oneshot(post_json("/api/runs", payload, &token('a')))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = body_json(response).await;
            assert_eq!(body["kind"], "validation");
        }
    }
}

mod search_disclosure {
    use super::*;

    #[tokio::test]
    async fn search_requires_the_network_grant() {
        let (app, _store, _dir) = empty_app().await;
        let response = app
            .oneshot(post_json(
                "/api/runs",
                serde_json::json!({"goal": "q", "search": true}),
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["kind"], "validation");
        assert!(
            body["error"]
                .as_str()
                .expect("prose")
                .contains("allow_network"),
            "says what is missing: {body}"
        );
    }

    #[tokio::test]
    async fn connector_selection_requires_search() {
        let (app, _store, _dir) = empty_app().await;
        let response = app
            .oneshot(post_json(
                "/api/runs",
                serde_json::json!({
                    "goal": "q",
                    "allow_network": true,
                    "search_connectors": ["arxiv"],
                }),
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["kind"], "validation");
        assert!(
            body["error"].as_str().expect("prose").contains("search"),
            "says what is missing: {body}"
        );
    }

    #[tokio::test]
    async fn unknown_connectors_are_refused() {
        let (app, _store, _dir) = empty_app().await;
        for connectors in [serde_json::json!(["scopus"]), serde_json::json!([])] {
            let response = app
                .clone()
                .oneshot(post_json(
                    "/api/runs",
                    serde_json::json!({
                        "goal": "q",
                        "allow_network": true,
                        "search": true,
                        "search_connectors": connectors,
                    }),
                    &token('a'),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = body_json(response).await;
            assert_eq!(body["kind"], "validation");
        }
    }

    #[tokio::test]
    async fn connector_selection_narrows_the_allowlist() {
        let (app, store, _dir) = empty_app().await;
        let run_id = start(
            &app,
            serde_json::json!({
                "goal": "q",
                "allow_network": true,
                "search": true,
                "search_connectors": ["arxiv", "openalex"],
            }),
            &token('a'),
        )
        .await;
        let goal = goal_of(&store, &run_id).await;
        let mut tools = goal.permissions.allowed_tools.clone();
        tools.sort();
        assert_eq!(tools, vec!["search:arxiv", "search:openalex"]);
    }
}

mod inputs {
    use super::*;

    #[tokio::test]
    async fn server_paths_and_non_http_urls_are_never_accepted() {
        let (app, store, _dir) = empty_app().await;
        for locator in [
            "/etc/passwd",
            "file:///etc/passwd",
            "C:\\data\\notes.txt",
            "ftp://host/file",
            "../relative/paper.pdf",
        ] {
            let response = app
                .clone()
                .oneshot(post_json(
                    "/api/runs",
                    serde_json::json!({"goal": "q", "input_urls": [locator]}),
                    &token('a'),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{locator}");
            let body = body_json(response).await;
            assert_eq!(body["kind"], "validation", "{locator}");
        }
        assert!(store.list_runs().await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn a_url_without_the_network_grant_is_recorded_unavailable() {
        let (app, _store, _dir) = empty_app().await;
        let run_id = start(
            &app,
            serde_json::json!({
                "goal": "q",
                "input_urls": ["https://example.org/paper.pdf"],
            }),
            &token('a'),
        )
        .await;

        let response = app
            .oneshot(get(&format!("/api/runs/{run_id}/sources"), &token('a')))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let sources = body["sources"].as_array().expect("sources");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["origin"]["kind"], "url");
        assert_eq!(sources[0]["access"], "unavailable");
        assert!(
            sources[0]["access_limitations"]
                .as_str()
                .expect("limitation prose")
                .contains("not permitted"),
            "the absence is explained: {body}"
        );
    }

    #[tokio::test]
    async fn an_uploaded_file_becomes_a_fully_read_source() {
        let (app, store, _dir) = empty_app().await;
        let upload_id = upload(
            &app,
            "drift-notes.txt",
            b"thermal drift dominates the measurement budget",
            &token('a'),
        )
        .await;

        let run_id = start(
            &app,
            serde_json::json!({"goal": "what dominates the budget", "upload_ids": [upload_id]}),
            &token('a'),
        )
        .await;

        // The source is recorded under the researcher-facing name, read in
        // full, and its text is passage-searchable.
        let response = app
            .clone()
            .oneshot(get(&format!("/api/runs/{run_id}/sources"), &token('a')))
            .await
            .unwrap();
        let body = body_json(response).await;
        let sources = body["sources"].as_array().expect("sources");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["origin"]["kind"], "local_file");
        assert_eq!(sources[0]["origin"]["at"], "drift-notes.txt");
        assert_eq!(sources[0]["access"], "full_text");

        let response = app
            .clone()
            .oneshot(get(
                &format!("/api/runs/{run_id}/passages?q=thermal+drift"),
                &token('a'),
            ))
            .await
            .unwrap();
        let body = body_json(response).await;
        assert!(
            !body["passages"].as_array().expect("passages").is_empty(),
            "uploaded text is indexed: {body}"
        );

        // The goal records the display name, never a server path.
        let goal = goal_of(&store, &run_id).await;
        assert_eq!(goal.inputs.len(), 1);
        assert_eq!(goal.inputs[0].locator, "drift-notes.txt");
        for path in &goal.permissions.read_paths {
            assert!(
                path.contains(".uruk/uploads"),
                "reads stay inside the managed upload store: {path}"
            );
        }

        // Starting a run does not consume the upload.
        let response = app.oneshot(get("/api/uploads", &token('a'))).await.unwrap();
        let body = body_json(response).await;
        assert_eq!(body["uploads"].as_array().expect("uploads").len(), 1);
    }

    #[tokio::test]
    async fn foreign_and_unknown_upload_ids_are_indistinguishable() {
        let (app, store, _dir) = empty_app().await;
        let theirs = upload(&app, "theirs.txt", b"owned by a", &token('a')).await;

        let foreign = app
            .clone()
            .oneshot(post_json(
                "/api/runs",
                serde_json::json!({"goal": "q", "upload_ids": [theirs]}),
                &token('b'),
            ))
            .await
            .unwrap();
        let unknown = app
            .clone()
            .oneshot(post_json(
                "/api/runs",
                serde_json::json!({"goal": "q", "upload_ids": ["upl_does_not_exist"]}),
                &token('b'),
            ))
            .await
            .unwrap();
        assert_eq!(foreign.status(), StatusCode::BAD_REQUEST);
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
        let foreign_body = body_json(foreign).await;
        let unknown_body = body_json(unknown).await;
        assert_eq!(foreign_body["kind"], "validation");
        assert_eq!(
            foreign_body["error"], unknown_body["error"],
            "probing discloses nothing"
        );
        assert!(store.list_runs().await.expect("list").is_empty());
    }
}

mod dry_run {
    use super::*;

    #[tokio::test]
    async fn a_dry_run_previews_the_plan_and_persists_no_run() {
        let (app, store, _dir) = empty_app().await;
        let upload_id = upload(&app, "notes.txt", b"contents", &token('a')).await;

        let response = app
            .clone()
            .oneshot(post_json(
                "/api/runs",
                serde_json::json!({
                    "goal": "what limits the resolution",
                    "mode": "campaign",
                    "input_urls": ["https://example.org/paper.pdf"],
                    "upload_ids": [upload_id],
                    "dry_run": true,
                }),
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "previewed, not created");
        let body = body_json(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["dry_run"], true);
        assert!(body.get("run_id").is_none_or(|v| v.is_null()));
        assert!(
            !body["plan"]["roles"].as_array().expect("roles").is_empty(),
            "the plan is previewed: {body}"
        );
        assert_eq!(
            body["inputs"],
            serde_json::json!(["https://example.org/paper.pdf", "notes.txt"]),
            "the would-be inputs are listed by display locator"
        );

        assert!(
            store.list_runs().await.expect("list").is_empty(),
            "a dry run persists nothing"
        );
    }

    #[tokio::test]
    async fn a_blocked_goal_is_refused_on_dry_run_without_persisting() {
        let (app, store, _dir) = empty_app().await;
        let response = app
            .oneshot(post_json(
                "/api/runs",
                serde_json::json!({
                    "goal": "how to bypass ethics review for the trial",
                    "dry_run": true,
                }),
                &token('a'),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = body_json(response).await;
        assert_eq!(body["kind"], "permission");
        assert!(store.list_runs().await.expect("list").is_empty());
    }
}
