//! Web API adapter checks: routes, view models, errors, and SSE.
//!
//! Everything runs offline against a temporary SQLite store and the mock
//! provider, through `tower::ServiceExt::oneshot` (no sockets).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use uruk::provider::MockProvider;
use uruk::records::{Mode, RunState};
use uruk::web::{AppState, WebConfig, router};

/// A router over a fresh on-disk store (so exports and resumes work), plus
/// the fixture that seeded it. The fixture run is owned by the browser
/// behind [`common::BROWSER_TOKEN`], which [`get`] and [`post_json`]
/// present; cross-browser isolation itself is covered in `web_owner.rs`.
async fn seeded_app(mode: Mode) -> (axum::Router, common::Fixture) {
    let fixture = common::fixture_owned(mode, MockProvider::new(), common::BROWSER_TOKEN).await;
    let state = AppState::new(
        fixture.store.clone(),
        fixture.provider.clone(),
        WebConfig {
            sse_poll: std::time::Duration::from_millis(20),
            ..WebConfig::default()
        },
    );
    (router(state), fixture)
}

async fn empty_app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let state = AppState::new(store, Arc::new(MockProvider::new()), WebConfig::default());
    (router(state), dir)
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

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(
            header::COOKIE,
            format!("uruk_browser={}", common::BROWSER_TOKEN),
        )
        .body(Body::empty())
        .expect("request")
}

fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::COOKIE,
            format!("uruk_browser={}", common::BROWSER_TOKEN),
        )
        .body(Body::from(body.to_string()))
        .expect("request")
}

#[tokio::test]
async fn health_reports_ok_and_version() {
    let (app, _dir) = empty_app().await;
    let response = app.oneshot(get("/api/health")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn run_listing_is_empty_for_a_fresh_project() {
    let (app, _dir) = empty_app().await;
    let response = app.oneshot(get("/api/runs")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["runs"], serde_json::json!([]));
}

#[tokio::test]
async fn run_listing_includes_goal_and_state() {
    let (app, fixture) = seeded_app(Mode::Campaign).await;
    let response = app.oneshot(get("/api/runs")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let runs = body["runs"].as_array().expect("runs array");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["id"], fixture.run_id.as_str());
    assert_eq!(runs[0]["state"], "running");
    assert_eq!(runs[0]["question"], fixture.goal.question);
    assert_eq!(runs[0]["mode"], "campaign");
}

#[tokio::test]
async fn unknown_run_returns_not_found_with_stable_error_kind() {
    let (app, _dir) = empty_app().await;
    let response = app.oneshot(get("/api/runs/run_missing")).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["kind"], "not_found");
    assert!(body["error"].as_str().unwrap_or("").contains("run_missing"));
}

#[tokio::test]
async fn unknown_route_returns_json_not_found() {
    let (app, _dir) = empty_app().await;
    let response = app.oneshot(get("/api/nope")).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["kind"], "not_found");
}

#[tokio::test]
async fn run_snapshot_reports_goal_plan_agents_and_budget_facts() {
    let (app, fixture) = seeded_app(Mode::Campaign).await;
    let uri = format!("/api/runs/{}", fixture.run_id);
    let response = app.oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(body["ok"], true);
    let snap = &body["snapshot"];
    assert_eq!(snap["run"]["id"], fixture.run_id.as_str());
    assert_eq!(snap["run"]["state"], "running");
    assert_eq!(snap["goal"]["question"], fixture.goal.question);
    assert_eq!(snap["goal"]["budget"]["max_model_calls"], 40);
    assert_eq!(snap["goal"]["budget"]["max_iterations"], 4);

    // The plan's roles become agent panels with the supervisor first. The
    // run is live but has no tasks yet: the supervisor is waiting and every
    // worker is idle.
    let agents = snap["agents"].as_array().expect("agents");
    assert_eq!(agents[0]["role"], "supervisor");
    assert_eq!(agents[0]["state"], "waiting");
    assert!(agents.len() > 1, "plan roles become agent panels");
    for agent in &agents[1..] {
        assert_eq!(agent["state"], "idle");
        assert_eq!(agent["counts"]["running"], 0);
    }

    // No invented facts: usage starts settled at zero with unknown cost.
    assert_eq!(snap["usage"]["model_calls"], 0);
    assert_eq!(snap["usage"]["cost_usd"], serde_json::Value::Null);
    assert_eq!(snap["stats"]["items"], 0);
    assert_eq!(snap["stats"]["leader"], serde_json::Value::Null);
}

#[tokio::test]
async fn run_snapshot_ranks_items_by_rating_and_counts_reviews() {
    let (app, fixture) = seeded_app(Mode::Campaign).await;

    let low = common::seed_item(&fixture, "Low-rated candidate").await;
    let high = common::seed_item(&fixture, "High-rated candidate").await;
    common::seed_review(&fixture, &high).await;

    // Enrol both and play one deduped match so ratings diverge.
    let plan_id = fixture.plan.id.clone();
    fixture
        .store
        .ensure_rating(&low.id, &plan_id)
        .await
        .unwrap();
    fixture
        .store
        .ensure_rating(&high.id, &plan_id)
        .await
        .unwrap();
    let m = uruk::records::Match {
        id: uruk::records::MatchId::new(),
        schema_version: uruk::records::SCHEMA_VERSION,
        run_id: fixture.run_id.clone(),
        item_a: high.id.clone(),
        item_b: low.id.clone(),
        rubric_id: plan_id,
        evidence_snapshot: uruk::records::ContentHash::of_str("evidence"),
        order_randomized: false,
        method: uruk::records::MatchMethod::Pairwise,
        judge_model: "mock".into(),
        outcome: uruk::records::MatchOutcome::WinA,
        rationale: "test match".into(),
        rating_a_before: 0.0,
        rating_b_before: 0.0,
        rating_a_after: 0.0,
        rating_b_after: 0.0,
        produced_by: None,
        created_at: time::OffsetDateTime::now_utc(),
    };
    fixture
        .store
        .commit_match(&m, "web-api-test-match", uruk::records::DEFAULT_K)
        .await
        .unwrap();

    let uri = format!("/api/runs/{}", fixture.run_id);
    let response = app.oneshot(get(&uri)).await.unwrap();
    let body = body_json(response).await;
    let items = body["snapshot"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], high.id.as_str(), "winner ranks first");
    assert_eq!(items[0]["review_count"], 1);
    assert!(items[0]["rating"].as_f64().unwrap() > items[1]["rating"].as_f64().unwrap());

    let stats = &body["snapshot"]["stats"];
    assert_eq!(stats["items"], 2);
    assert_eq!(stats["matches"], 1);
    assert_eq!(stats["leader"]["item_id"], high.id.as_str());
}

#[tokio::test]
async fn stop_marks_the_run_cancelled_and_records_the_decision() {
    let (app, fixture) = seeded_app(Mode::Campaign).await;
    let uri = format!("/api/runs/{}/stop", fixture.run_id);
    let response = app
        .clone()
        .oneshot(post_json(&uri, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["ok"], true);

    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Cancelled);

    let decisions = fixture
        .store
        .list_decisions_of_kind(&fixture.run_id, uruk::records::DecisionKind::Stop)
        .await
        .unwrap();
    assert_eq!(decisions.len(), 1);
}

#[tokio::test]
async fn stop_of_an_unknown_run_is_not_found() {
    let (app, _dir) = empty_app().await;
    let response = app
        .oneshot(post_json("/api/runs/run_ghost/stop", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "not_found");
}

#[tokio::test]
async fn start_run_rejects_an_empty_goal() {
    let (app, _dir) = empty_app().await;
    let response = app
        .oneshot(post_json(
            "/api/runs",
            serde_json::json!({"goal": "   ", "mode": "campaign"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["kind"], "validation");
}

#[tokio::test]
async fn start_run_rejects_unknown_mode_and_ranking() {
    let (app, _dir) = empty_app().await;
    for payload in [
        serde_json::json!({"goal": "a real question", "mode": "swarm"}),
        serde_json::json!({"goal": "a real question", "ranking": "elaborate"}),
    ] {
        let response = app
            .clone()
            .oneshot(post_json("/api/runs", payload))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["kind"], "validation");
    }
}

#[tokio::test]
async fn start_run_rejects_oversized_bodies() {
    let (app, _dir) = empty_app().await;
    let huge = "x".repeat(200 * 1024);
    let response = app
        .oneshot(post_json("/api/runs", serde_json::json!({"goal": huge})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_json(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["kind"], "validation");
}

#[tokio::test]
async fn start_run_creates_and_dispatches_a_run_to_completion() {
    // The mock provider's unscripted replies make a short honest run: the
    // scheduler still exercises planning, tasks, and the final export.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let provider = Arc::new(
        MockProvider::new()
            .rule("Requested deliverable:", common::synthesis_json())
            .default_reply(common::review_json("inconclusive", "mock run", false)),
    );
    let state = AppState::new(
        store.clone(),
        provider,
        WebConfig {
            sse_poll: std::time::Duration::from_millis(20),
            ..WebConfig::default()
        },
    );
    let app = router(state);

    let response = app
        .clone()
        .oneshot(post_json(
            "/api/runs",
            serde_json::json!({
                "goal": "compare the two supplied approaches",
                "mode": "task",
                "ranking": "simple",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    assert_eq!(body["ok"], true);
    let run_id = uruk::records::RunId::from_raw(
        body["run_id"]
            .as_str()
            .expect("run_id in response")
            .to_string(),
    );

    // The scheduler runs in the background; poll the store until terminal.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let run = store.get_run(&run_id).await.unwrap();
        if run.state.is_terminal() {
            assert_eq!(run.state, RunState::Completed);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run did not finish in time (state {})",
            run.state.as_str()
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Completion exports the report, so the report route serves it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let response = app
            .clone()
            .oneshot(get(&format!("/api/runs/{run_id}/report")))
            .await
            .unwrap();
        if response.status() == StatusCode::OK {
            let body = body_json(response).await;
            assert_eq!(body["ok"], true);
            let markdown = body["markdown"].as_str().expect("markdown");
            assert!(markdown.contains("#"), "report is markdown");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "report was not exported after completion"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn report_of_an_unexported_run_is_not_found() {
    let (app, fixture) = seeded_app(Mode::Task).await;
    let uri = format!("/api/runs/{}/report", fixture.run_id);
    let response = app.oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "not_found");
}

#[tokio::test]
async fn sources_and_library_list_seeded_sources() {
    let (app, fixture) = seeded_app(Mode::Task).await;
    let source = common::seed_source(&fixture, "paper-a", "measured effect text").await;

    let uri = format!("/api/runs/{}/sources", fixture.run_id);
    let response = app.clone().oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let sources = body["sources"].as_array().expect("sources");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["id"], source.id.as_str());
    assert_eq!(sources[0]["access"], "full_text");

    let response = app.oneshot(get("/api/library")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let entries = body["sources"].as_array().expect("library sources");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["run_id"], fixture.run_id.as_str());
}

#[tokio::test]
async fn passages_route_returns_hits_for_indexed_text() {
    let (app, fixture) = seeded_app(Mode::Task).await;
    let source = common::seed_source(&fixture, "paper-b", "thermal drift dominates").await;
    fixture
        .store
        .index_source_passages(&source, "thermal drift dominates the measurement budget")
        .await
        .unwrap();

    let uri = format!(
        "/api/runs/{}/passages?q=thermal+drift&limit=5",
        fixture.run_id
    );
    let response = app.clone().oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let passages = body["passages"].as_array().expect("passages");
    assert_eq!(passages.len(), 1);
    assert_eq!(passages[0]["source_id"], source.id.as_str());

    // A blank query is a validation error, not an empty success.
    let uri = format!("/api/runs/{}/passages?q=&limit=5", fixture.run_id);
    let response = app.oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sse_emits_an_initial_snapshot_event_immediately() {
    let (app, fixture) = seeded_app(Mode::Campaign).await;
    let uri = format!("/api/runs/{}/events", fixture.run_id);
    let response = app.oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );

    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        use futures_util::StreamExt;
        let mut buffer = Vec::new();
        while let Some(chunk) = body.next().await {
            buffer.extend_from_slice(&chunk.expect("sse chunk"));
            if buffer.windows(2).any(|w| w == b"\n\n") {
                break;
            }
        }
        String::from_utf8(buffer).expect("utf8 sse frame")
    })
    .await
    .expect("first SSE frame arrives promptly");

    assert!(first.contains("event: snapshot"), "frame: {first}");
    let data_line = first
        .lines()
        .find(|l| l.starts_with("data: "))
        .expect("data line");
    let snapshot: serde_json::Value =
        serde_json::from_str(data_line.trim_start_matches("data: ")).expect("snapshot JSON");
    assert_eq!(snapshot["run"]["id"], fixture.run_id.as_str());
}

#[tokio::test]
async fn sse_settles_with_an_end_event_once_the_run_is_terminal() {
    let (app, fixture) = seeded_app(Mode::Campaign).await;
    fixture
        .store
        .set_run_state(
            &fixture.run_id,
            RunState::Cancelled,
            Some(uruk::records::StopCondition::Cancelled),
        )
        .await
        .unwrap();

    let uri = format!("/api/runs/{}/events", fixture.run_id);
    let response = app.oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // A terminal run's stream yields the snapshot, then `end`, then closes.
    let whole = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        response
            .into_body()
            .collect()
            .await
            .expect("stream closes")
            .to_bytes()
    })
    .await
    .expect("terminal SSE stream closes on its own");
    let text = String::from_utf8(whole.to_vec()).expect("utf8");
    assert!(text.contains("event: snapshot"), "stream: {text}");
    assert!(text.contains("event: end"), "stream: {text}");
}

#[tokio::test]
async fn sse_for_an_unknown_run_is_not_found() {
    let (app, _dir) = empty_app().await;
    let response = app
        .oneshot(get("/api/runs/run_ghost/events"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn responses_carry_a_request_id() {
    let (app, _dir) = empty_app().await;
    let response = app.oneshot(get("/api/health")).await.unwrap();
    assert!(
        response.headers().contains_key("x-request-id"),
        "request id header is set"
    );
}

#[tokio::test]
async fn stop_of_a_finished_run_is_a_validation_error() {
    let (app, fixture) = seeded_app(Mode::Campaign).await;
    fixture
        .store
        .set_run_state(
            &fixture.run_id,
            RunState::Completed,
            Some(uruk::records::StopCondition::DeliverableSatisfied),
        )
        .await
        .unwrap();

    let uri = format!("/api/runs/{}/stop", fixture.run_id);
    let response = app
        .oneshot(post_json(&uri, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "validation");

    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Completed, "finished state is kept");
}

#[tokio::test]
async fn a_wrong_method_still_gets_the_stable_json_error_body() {
    let (app, _dir) = empty_app().await;
    let response = app
        .oneshot(post_json("/api/health", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let body = body_json(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["kind"], "validation");
}

#[tokio::test]
async fn a_request_timeout_still_gets_the_stable_json_error_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let state = AppState::new(
        store,
        Arc::new(MockProvider::new()),
        WebConfig {
            request_timeout: std::time::Duration::ZERO,
            ..WebConfig::default()
        },
    );
    let response = router(state).oneshot(get("/api/runs")).await.unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    let body = body_json(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["kind"], "timeout");
}

#[tokio::test]
async fn starts_use_their_own_bounded_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let state = AppState::new(
        store,
        Arc::new(MockProvider::new()),
        WebConfig {
            request_timeout: std::time::Duration::ZERO,
            start_timeout: std::time::Duration::from_secs(30),
            ..WebConfig::default()
        },
    );
    let response = router(state)
        .oneshot(post_json(
            "/api/runs",
            serde_json::json!({"goal": "preview me", "dry_run": true}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let dir = tempfile::tempdir().expect("tempdir");
    let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let state = AppState::new(
        store,
        Arc::new(MockProvider::new()),
        WebConfig {
            start_timeout: std::time::Duration::ZERO,
            ..WebConfig::default()
        },
    );
    let response = router(state)
        .oneshot(post_json(
            "/api/runs",
            serde_json::json!({"goal": "start me"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "timeout");
}

#[tokio::test]
async fn a_maximum_sized_valid_start_body_is_accepted() {
    let (app, _dir) = empty_app().await;
    let list: Vec<String> = (0..32)
        .map(|i| format!("{i:02}{}", "x".repeat(498)))
        .collect();
    let urls: Vec<String> = (0..16)
        .map(|i| format!("https://example.org/{i:02}/{}", "x".repeat(1975)))
        .collect();
    let body = serde_json::json!({
        "goal": "g".repeat(8_000),
        "deliverables": list,
        "preferences": list,
        "attributes": list,
        "constraints": list,
        "input_urls": urls,
        "allow_network": true,
        "dry_run": true
    });
    assert!(
        body.to_string().len() > 64 * 1024,
        "exercises the raised cap"
    );
    let response = app.oneshot(post_json("/api/runs", body)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_unparsable_passages_query_is_a_json_validation_error() {
    let (app, fixture) = seeded_app(Mode::Task).await;
    let uri = format!("/api/runs/{}/passages?q=x&limit=abc", fixture.run_id);
    let response = app.oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["kind"], "validation");
}

#[tokio::test]
async fn web_started_runs_share_the_cli_project_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let state = AppState::new(
        store.clone(),
        Arc::new(MockProvider::new()),
        WebConfig::default(),
    );
    let response = router(state)
        .oneshot(post_json(
            "/api/runs",
            serde_json::json!({"goal": "what explains the drift"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    let run_id =
        uruk::records::RunId::from_raw(body["run_id"].as_str().expect("run_id").to_string());

    let run = store.get_run(&run_id).await.unwrap();
    assert_eq!(
        run.project_id,
        store.project_identity().0,
        "web runs land in the same project row the CLI uses"
    );
}

#[tokio::test]
async fn interrupted_runs_resume_when_the_server_starts() {
    // A run left `running` by a killed process: the serve-side reconciler
    // must pick it up, because `uruk serve` is the project's only
    // scheduler owner.
    let provider = MockProvider::new()
        .rule("Requested deliverable:", common::synthesis_json())
        .default_reply(common::review_json("inconclusive", "mock resume", false));
    let fixture = common::fixture(Mode::Task, provider).await;
    let state = AppState::new(
        fixture.store.clone(),
        fixture.provider.clone(),
        WebConfig::default(),
    );

    let resumed = uruk::web::resume_orphaned_runs(&state).await.unwrap();
    assert_eq!(resumed, vec![fixture.run_id.clone()]);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
        if run.state.is_terminal() {
            assert_eq!(run.state, RunState::Completed);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "orphaned run was not driven to completion (state {})",
            run.state.as_str()
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Terminal and parked runs are left alone on the next sweep.
    let resumed_again = uruk::web::resume_orphaned_runs(&state).await.unwrap();
    assert!(
        resumed_again.is_empty(),
        "finished runs are not re-dispatched"
    );
}

// --- report downloads: REPORT.pdf and REPORT.md as files ---

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

fn header<'r>(response: &'r axum::response::Response, name: &str) -> &'r str {
    response
        .headers()
        .get(name)
        .unwrap_or_else(|| panic!("{name} header present"))
        .to_str()
        .expect("header is ASCII")
}

#[tokio::test]
async fn report_pdf_download_carries_pdf_headers_and_magic() {
    let (app, fixture) = seeded_app(Mode::Task).await;
    uruk::report::export_run(&fixture.store, &fixture.run_id)
        .await
        .unwrap();

    let response = app
        .oneshot(get(&format!("/api/runs/{}/report.pdf", fixture.run_id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, "content-type"), "application/pdf");
    assert_eq!(
        header(&response, "content-disposition"),
        format!(
            "attachment; filename=\"uruk-report-{}.pdf\"",
            fixture.run_id
        )
    );
    let bytes = body_bytes(response).await;
    assert!(bytes.starts_with(b"%PDF-"), "PDF magic number");
    assert!(
        bytes.len() > 1_000,
        "nontrivial size ({} bytes)",
        bytes.len()
    );
}

#[tokio::test]
async fn report_pdf_of_an_unexported_run_is_not_found() {
    let (app, fixture) = seeded_app(Mode::Task).await;
    let response = app
        .oneshot(get(&format!("/api/runs/{}/report.pdf", fixture.run_id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "not_found");
}

#[tokio::test]
async fn report_pdf_is_derived_on_demand_for_markdown_only_exports() {
    // Runs exported before the PDF twin existed have only REPORT.md on
    // disk; the route derives the same deterministic bytes on demand.
    let (app, fixture) = seeded_app(Mode::Task).await;
    uruk::report::export_run(&fixture.store, &fixture.run_id)
        .await
        .unwrap();
    let dir = fixture.store.run_dir(&fixture.run_id);
    let exported = tokio::fs::read(dir.join("REPORT.pdf")).await.unwrap();
    tokio::fs::remove_file(dir.join("REPORT.pdf"))
        .await
        .unwrap();

    let response = app
        .oneshot(get(&format!("/api/runs/{}/report.pdf", fixture.run_id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_bytes(response).await,
        exported,
        "on-demand rendering reproduces the exported bytes exactly"
    );
}

#[tokio::test]
async fn report_pdf_never_serves_a_stale_or_tampered_twin() {
    let (app, fixture) = seeded_app(Mode::Task).await;
    uruk::report::export_run(&fixture.store, &fixture.run_id)
        .await
        .unwrap();
    let dir = fixture.store.run_dir(&fixture.run_id);
    let canonical_markdown = tokio::fs::read_to_string(dir.join("REPORT.md"))
        .await
        .unwrap();
    tokio::fs::write(dir.join("REPORT.pdf"), b"not the canonical PDF")
        .await
        .unwrap();

    let response = app
        .oneshot(get(&format!("/api/runs/{}/report.pdf", fixture.run_id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, "x-content-type-options"), "nosniff");
    assert_eq!(header(&response, "cache-control"), "private, no-store");
    assert_eq!(
        body_bytes(response).await,
        uruk::report::render_report_pdf(&canonical_markdown),
        "REPORT.md is the sole source of truth"
    );
}

#[tokio::test]
async fn report_markdown_download_serves_the_exact_canonical_bytes() {
    let (app, fixture) = seeded_app(Mode::Task).await;
    uruk::report::export_run(&fixture.store, &fixture.run_id)
        .await
        .unwrap();
    let canonical = tokio::fs::read(fixture.store.run_dir(&fixture.run_id).join("REPORT.md"))
        .await
        .unwrap();

    let response = app
        .oneshot(get(&format!("/api/runs/{}/report.md", fixture.run_id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, "content-type"),
        "text/markdown; charset=utf-8"
    );
    assert_eq!(
        header(&response, "content-disposition"),
        format!("attachment; filename=\"uruk-report-{}.md\"", fixture.run_id)
    );
    assert_eq!(
        body_bytes(response).await,
        canonical,
        "the download is the canonical file, byte for byte"
    );
}

#[tokio::test]
async fn report_markdown_download_of_an_unexported_run_is_not_found() {
    let (app, fixture) = seeded_app(Mode::Task).await;
    let response = app
        .oneshot(get(&format!("/api/runs/{}/report.md", fixture.run_id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "not_found");
}

#[tokio::test]
async fn report_download_ids_cannot_traverse_outside_the_runs_directory() {
    // A traversal-shaped id must be answered exactly like a missing run,
    // before any filesystem path is formed from it.
    let (app, _dir) = empty_app().await;
    for uri in [
        "/api/runs/..%2F..%2F..%2Fetc%2Fpasswd/report.pdf",
        "/api/runs/..%2F..%2F..%2Fetc%2Fpasswd/report.md",
    ] {
        let response = app.clone().oneshot(get(uri)).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{uri} must be indistinguishable from a missing run"
        );
        let body = body_json(response).await;
        assert_eq!(body["kind"], "not_found");
    }
}
