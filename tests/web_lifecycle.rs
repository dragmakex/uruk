//! The web run lifecycle: Stop (a durable, resumable pause), Resume, and
//! Restart (a clean fresh run from the original configuration).
//!
//! Web stop is deliberately NOT the CLI's terminal cancel: an anonymous
//! browser pausing its own run must be able to change its mind. Everything
//! runs offline against a temporary SQLite store and the mock provider,
//! through `tower::ServiceExt::oneshot` (no sockets).
#![cfg(feature = "web")]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::runtime::{Scheduler, SchedulerConfig};
use uruk::web::{AppState, WebConfig, router};

/// A second, independent browser for isolation checks.
const OTHER_TOKEN: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A router plus its state over the fixture's store. The fixture run is
/// owned by the browser behind [`common::BROWSER_TOKEN`].
async fn seeded_app(mode: Mode) -> (axum::Router, AppState, common::Fixture) {
    let fixture = common::fixture_owned(mode, MockProvider::new(), common::BROWSER_TOKEN).await;
    let state = AppState::new(
        fixture.store.clone(),
        fixture.provider.clone(),
        WebConfig {
            sse_poll: Duration::from_millis(20),
            ..WebConfig::default()
        },
    );
    (router(state.clone()), state, fixture)
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

fn get_as(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::COOKIE, format!("uruk_browser={token}"))
        .body(Body::empty())
        .expect("request")
}

fn post_as(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("uruk_browser={token}"))
        .body(Body::from("{}"))
        .expect("request")
}

fn post(uri: &str) -> Request<Body> {
    post_as(uri, common::BROWSER_TOKEN)
}

fn post_body(uri: &str, body: serde_json::Value) -> Request<Body> {
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

fn pending_task(f: &common::Fixture, role: Role, strategy: &str) -> Task {
    Task {
        id: TaskId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        role,
        strategy: strategy.into(),
        goal_id: f.goal.id.clone(),
        plan_id: f.plan.id.clone(),
        input_refs: vec![],
        input_hash: ContentHash::of_str("in"),
        prompt_id: None,
        prompt_hash: None,
        model: None,
        provider: Some("mock".into()),
        payload: None,
        feedback_id: None,
        depends_on: vec![],
        priority: 1.0,
        priority_rationale: "test".into(),
        permissions: f.goal.permissions.clone(),
        reservation: CostReservation {
            model_calls: 1,
            tokens: 1000,
            tool_executions: 0,
        },
        state: TaskState::Pending,
        attempts: 0,
        max_attempts: 3,
        seed: 1,
        output_refs: vec![],
        actual_cost: CostActual::default(),
        error: None,
        created_at: time::OffsetDateTime::now_utc(),
        started_at: None,
        finished_at: None,
    }
}

/// Drive a run started through the API until it is terminal.
async fn wait_terminal(store: &uruk::store::Store, run_id: &RunId) -> Run {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let run = store.get_run(run_id).await.unwrap();
        if run.state.is_terminal() {
            return run;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run did not finish in time (state {})",
            run.state.as_str()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// An app whose provider is scripted so API-started runs complete quickly.
async fn completing_app() -> (axum::Router, AppState, tempfile::TempDir) {
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
        store,
        provider,
        WebConfig {
            sse_poll: Duration::from_millis(20),
            ..WebConfig::default()
        },
    );
    (router(state.clone()), state, dir)
}

async fn start_api_run(app: &axum::Router, body: serde_json::Value) -> RunId {
    let response = app
        .clone()
        .oneshot(post_body("/api/runs", body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    RunId::from_raw(body["run_id"].as_str().expect("run_id").to_string())
}

// ---------------------------------------------------------------------------
// Stop: a durable, resumable pause
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stop_pauses_a_running_web_run_instead_of_cancelling_it() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;

    let uri = format!("/api/runs/{}/stop", fixture.run_id);
    let response = app.oneshot(post(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["state"], "paused");

    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Paused);
    assert_eq!(
        run.stop_condition, None,
        "a pause is not a stopping condition; the run has not concluded"
    );
}

#[tokio::test]
async fn stop_leaves_pending_work_pending_for_a_later_resume() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;
    let task = pending_task(&fixture, Role::Generation, "literature");
    fixture.store.enqueue_task(&task, None).await.unwrap();

    let uri = format!("/api/runs/{}/stop", fixture.run_id);
    let response = app.oneshot(post(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(
        fixture.store.get_task(&task.id).await.unwrap().state,
        TaskState::Pending,
        "pause must not cancel queued work"
    );
}

#[tokio::test]
async fn stop_is_idempotent_and_records_exactly_one_pause_decision() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;

    let uri = format!("/api/runs/{}/stop", fixture.run_id);
    for _ in 0..2 {
        let response = app.clone().oneshot(post(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["state"], "paused");
    }

    let decisions = fixture
        .store
        .list_decisions_of_kind(&fixture.run_id, DecisionKind::Pause)
        .await
        .unwrap();
    assert_eq!(
        decisions.len(),
        1,
        "a repeated stop is a no-op, not a new fact"
    );
}

#[tokio::test]
async fn stop_of_a_finished_run_is_refused_and_the_final_state_kept() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;
    fixture
        .store
        .set_run_state(
            &fixture.run_id,
            RunState::Completed,
            Some(StopCondition::DeliverableSatisfied),
        )
        .await
        .unwrap();

    let uri = format!("/api/runs/{}/stop", fixture.run_id);
    let response = app.oneshot(post(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "validation");

    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Completed);
}

// ---------------------------------------------------------------------------
// Resume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resume_returns_a_paused_run_to_running_and_records_the_decision() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;

    let stop = format!("/api/runs/{}/stop", fixture.run_id);
    app.clone().oneshot(post(&stop)).await.unwrap();

    let resume = format!("/api/runs/{}/resume", fixture.run_id);
    let response = app.oneshot(post(&resume)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["state"], "running");

    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Running);

    let decisions = fixture
        .store
        .list_decisions_of_kind(&fixture.run_id, DecisionKind::Resume)
        .await
        .unwrap();
    assert_eq!(decisions.len(), 1);
}

#[tokio::test]
async fn resume_of_a_run_that_is_already_running_is_a_no_op() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;

    let resume = format!("/api/runs/{}/resume", fixture.run_id);
    let response = app.oneshot(post(&resume)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["state"], "running");

    let decisions = fixture
        .store
        .list_decisions_of_kind(&fixture.run_id, DecisionKind::Resume)
        .await
        .unwrap();
    assert!(decisions.is_empty(), "nothing changed, nothing is recorded");
}

#[tokio::test]
async fn resume_of_a_finished_run_is_a_validation_error() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;
    fixture
        .store
        .set_run_state(
            &fixture.run_id,
            RunState::Cancelled,
            Some(StopCondition::Cancelled),
        )
        .await
        .unwrap();

    let resume = format!("/api/runs/{}/resume", fixture.run_id);
    let response = app.oneshot(post(&resume)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "validation");
}

#[tokio::test]
async fn paused_time_is_excluded_from_the_wall_clock_budget() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;

    let stop = format!("/api/runs/{}/stop", fixture.run_id);
    app.clone().oneshot(post(&stop)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let resume = format!("/api/runs/{}/resume", fixture.run_id);
    app.oneshot(post(&resume)).await.unwrap();

    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert!(
        run.paused_ms >= 50,
        "time spent paused must be accounted ({}ms recorded)",
        run.paused_ms
    );
}

// ---------------------------------------------------------------------------
// Owner isolation: unknown and foreign runs are indistinguishable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_routes_return_identical_not_found_for_foreign_and_unknown_runs() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;

    for action in ["stop", "resume", "restart"] {
        let foreign_uri = format!("/api/runs/{}/{action}", fixture.run_id);
        let foreign = app
            .clone()
            .oneshot(post_as(&foreign_uri, OTHER_TOKEN))
            .await
            .unwrap();
        let unknown_uri = format!("/api/runs/run_missing/{action}");
        let unknown = app
            .clone()
            .oneshot(post_as(&unknown_uri, OTHER_TOKEN))
            .await
            .unwrap();

        assert_eq!(foreign.status(), StatusCode::NOT_FOUND, "{action}");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND, "{action}");
        let foreign_body = body_json(foreign).await;
        let mut unknown_body = body_json(unknown).await;
        // Normalize the echoed id; the shape and kind must be identical.
        let patched = unknown_body["error"]
            .as_str()
            .unwrap()
            .replace("run_missing", fixture.run_id.as_str());
        unknown_body["error"] = serde_json::Value::String(patched);
        assert_eq!(
            foreign_body, unknown_body,
            "{action}: a foreign run must be indistinguishable from an unknown one"
        );
    }

    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(
        run.state,
        RunState::Running,
        "the foreign poke changed nothing"
    );
}

// ---------------------------------------------------------------------------
// Scheduler and recovery behavior under pause
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_paused_run_is_not_picked_up_by_the_recovery_sweep() {
    let (app, state, fixture) = seeded_app(Mode::Campaign).await;

    let stop = format!("/api/runs/{}/stop", fixture.run_id);
    app.oneshot(post(&stop)).await.unwrap();

    let resumed = uruk::web::resume_orphaned_runs(&state).await.unwrap();
    assert!(
        resumed.is_empty(),
        "a pause must survive a server restart: {resumed:?}"
    );
    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Paused);
}

#[tokio::test]
async fn a_scheduler_handed_a_paused_run_winds_down_without_cancelling_anything() {
    let fixture = common::fixture(Mode::Campaign, MockProvider::new()).await;
    let task = pending_task(&fixture, Role::Generation, "literature");
    fixture.store.enqueue_task(&task, None).await.unwrap();
    assert!(fixture.store.try_pause_run(&fixture.run_id).await.unwrap());

    let scheduler = Scheduler::new(
        fixture.store.clone(),
        fixture.provider.clone(),
        SchedulerConfig::default(),
    );
    let summary = scheduler.run(&fixture.run_id).await.unwrap();

    assert_eq!(summary.state, RunState::Paused, "{summary:?}");
    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Paused, "the pause is durable");
    assert_eq!(
        fixture.store.get_task(&task.id).await.unwrap().state,
        TaskState::Pending,
        "queued work waits for resume"
    );
    assert_eq!(
        fixture.provider.call_count(),
        0,
        "a paused run must not consume budget"
    );
}

#[tokio::test]
async fn a_pause_wins_the_race_against_a_starting_scheduler() {
    let fixture = common::fixture(Mode::Campaign, MockProvider::new()).await;
    assert!(fixture.store.try_pause_run(&fixture.run_id).await.unwrap());

    // Exactly the guarded transition a starting scheduler performs.
    assert!(
        !fixture
            .store
            .mark_run_running(&fixture.run_id)
            .await
            .unwrap(),
        "a starting scheduler must not overwrite a durable pause"
    );
    let run = fixture.store.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Paused);
}

#[tokio::test]
async fn a_running_scheduler_observes_a_pause_and_leaves_the_run_resumable() {
    // Slow reviews keep the scheduler busy long enough for the pause to land
    // mid-flight, exactly like the durable-stop check it mirrors.
    let provider = MockProvider::new()
        .default_reply(common::review_json("plausible", "slow review", false))
        .with_delay(Duration::from_millis(400));
    let fixture =
        common::fixture_with(Mode::Campaign, provider, |g| g.budget.max_iterations = 20).await;
    for i in 0..4 {
        common::seed_item(&fixture, &format!("candidate {i}")).await;
    }
    let other = uruk::store::Store::open(fixture.dir.path().join(".uruk/state.sqlite"))
        .await
        .unwrap();

    let scheduler = Scheduler::new(
        fixture.store.clone(),
        fixture.provider.clone(),
        SchedulerConfig::default(),
    );
    let run_id = fixture.run_id.clone();
    let handle = tokio::spawn(async move { scheduler.run(&run_id).await });

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(other.try_pause_run(&fixture.run_id).await.unwrap());

    let summary = handle.await.unwrap().unwrap();
    assert_eq!(summary.state, RunState::Paused, "{summary:?}");
    let run = other.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(
        run.state,
        RunState::Paused,
        "the pause must not be overwritten"
    );
    assert_eq!(run.stop_condition, None);
}

#[tokio::test]
async fn a_pause_resumed_mid_wind_down_leaves_the_run_running_not_cancelled() {
    // The watcher cancels in-flight work when it observes a pause. If the
    // browser changes its mind and resumes before the scheduler finishes
    // winding down, the scheduler must not mistake the cancelled token plus
    // the running state for a terminal cancel request.
    let provider = MockProvider::new()
        .default_reply(common::review_json("plausible", "slow review", false))
        .with_delay(Duration::from_millis(400));
    let fixture =
        common::fixture_with(Mode::Campaign, provider, |g| g.budget.max_iterations = 20).await;
    for i in 0..4 {
        common::seed_item(&fixture, &format!("candidate {i}")).await;
    }
    let other = uruk::store::Store::open(fixture.dir.path().join(".uruk/state.sqlite"))
        .await
        .unwrap();

    let scheduler = Scheduler::new(
        fixture.store.clone(),
        fixture.provider.clone(),
        SchedulerConfig::default(),
    );
    let token = scheduler.cancellation_token();
    let run_id = fixture.run_id.clone();
    let handle = tokio::spawn(async move { scheduler.run(&run_id).await });

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(other.try_pause_run(&fixture.run_id).await.unwrap());
    // The watcher observes the pause and cancels in-flight work...
    tokio::time::timeout(Duration::from_secs(5), token.cancelled())
        .await
        .expect("the stop watcher observes the pause");
    // ...and the browser changes its mind before the wind-down completes.
    assert!(other.try_resume_run(&fixture.run_id).await.unwrap());

    let summary = handle.await.unwrap().unwrap();
    assert_ne!(
        summary.state,
        RunState::Cancelled,
        "a resumed pause is not a cancel request: {summary:?}"
    );
    let run = other.get_run(&fixture.run_id).await.unwrap();
    assert_eq!(
        run.state,
        RunState::Running,
        "the resume must survive the old scheduler's wind-down"
    );
    assert_eq!(run.stop_condition, None);
}

#[tokio::test]
async fn stop_pauses_a_waiting_for_human_run_and_resume_returns_it_to_running() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;
    fixture
        .store
        .set_run_state(&fixture.run_id, RunState::WaitingForHuman, None)
        .await
        .unwrap();

    let stop = format!("/api/runs/{}/stop", fixture.run_id);
    let response = app.clone().oneshot(post(&stop)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fixture.store.get_run(&fixture.run_id).await.unwrap().state,
        RunState::Paused
    );

    let resume = format!("/api/runs/{}/resume", fixture.run_id);
    let response = app.oneshot(post(&resume)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fixture.store.get_run(&fixture.run_id).await.unwrap().state,
        RunState::Running
    );
}

#[tokio::test]
async fn resume_of_a_blocked_run_is_a_conflict() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;
    fixture
        .store
        .set_run_state(
            &fixture.run_id,
            RunState::Blocked,
            Some(StopCondition::SafetyBoundary),
        )
        .await
        .unwrap();

    let resume = format!("/api/runs/{}/resume", fixture.run_id);
    let response = app.oneshot(post(&resume)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "conflict");
}

#[tokio::test]
async fn resume_drives_a_paused_api_run_onward_to_completion() {
    let (app, _state, _dir) = completing_app().await;
    let run_id = start_api_run(
        &app,
        serde_json::json!({"goal": "compare the approaches", "mode": "task"}),
    )
    .await;

    // Pause immediately, then resume; the run must still reach its natural
    // terminal state afterwards.
    let stop = format!("/api/runs/{run_id}/stop");
    let response = app.clone().oneshot(post(&stop)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let resume = format!("/api/runs/{run_id}/resume");
    let response = app.clone().oneshot(post(&resume)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Poll the snapshot route until the run reports a terminal state.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let response = app
            .clone()
            .oneshot(get_as(
                &format!("/api/runs/{run_id}"),
                common::BROWSER_TOKEN,
            ))
            .await
            .unwrap();
        let body = body_json(response).await;
        let state = body["snapshot"]["run"]["state"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if ["completed", "budget-exhausted", "failed"].contains(&state.as_str()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "resumed run did not finish (state {state})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// SSE under pause
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sse_reports_the_paused_state_and_keeps_the_stream_open() {
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;

    let uri = format!("/api/runs/{}/events", fixture.run_id);
    let response = app
        .clone()
        .oneshot(get_as(&uri, common::BROWSER_TOKEN))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();

    // First frame: the running snapshot.
    let first = next_frame(&mut stream).await;
    assert!(first.contains("\"state\":\"running\""), "frame: {first}");

    assert!(fixture.store.try_pause_run(&fixture.run_id).await.unwrap());

    // Next frame: the paused snapshot, with no end event — the stream stays
    // open because a paused run can come back.
    let second = next_frame(&mut stream).await;
    assert!(second.contains("\"state\":\"paused\""), "frame: {second}");
    assert!(
        !second.contains("event: end"),
        "a paused run must not settle the stream: {second}"
    );
}

/// Read one SSE frame (terminated by a blank line) from a body stream.
async fn next_frame(
    stream: &mut (impl futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
) -> String {
    use futures_util::StreamExt;
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut buffer = Vec::new();
        while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk.expect("sse chunk"));
            if buffer.windows(2).any(|w| w == b"\n\n") {
                break;
            }
        }
        String::from_utf8(buffer).expect("utf8 sse frame")
    })
    .await
    .expect("SSE frame arrives promptly")
}

// ---------------------------------------------------------------------------
// Restart: a clean fresh run from the original configuration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restart_creates_a_fresh_owned_run_with_new_ids_and_recorded_lineage() {
    let (app, state, _dir) = completing_app().await;
    let original = start_api_run(
        &app,
        serde_json::json!({
            "goal": "compare the two supplied approaches",
            "mode": "task",
            "ranking": "simple",
            "max_model_calls": 30,
        }),
    )
    .await;
    wait_terminal(state.store(), &original).await;

    let uri = format!("/api/runs/{original}/restart");
    let response = app.clone().oneshot(post(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["restarted_from"], original.as_str());
    let new_id = RunId::from_raw(body["run_id"].as_str().expect("run_id").to_string());
    assert_ne!(new_id, original, "a restart is a new run, not a reopening");

    // The canonical configuration is copied.
    let original_run = state.store().get_run(&original).await.unwrap();
    let old_goal = state.store().get_goal(&original_run.goal_id).await.unwrap();
    let new_goal = state.store().current_goal(&new_id).await.unwrap();
    assert_eq!(new_goal.question, old_goal.question);
    assert_eq!(new_goal.mode, old_goal.mode);
    assert_eq!(new_goal.budget.max_model_calls, 30);
    assert_ne!(new_goal.id, old_goal.id, "fresh goal identity");

    // Generated results are not copied: the fresh run starts with none of
    // the original's records.
    let old_items: Vec<String> = state
        .store()
        .list_items(&original)
        .await
        .unwrap()
        .iter()
        .map(|i| i.id.0.clone())
        .collect();
    let new_items = state.store().list_items(&new_id).await.unwrap();
    assert!(
        new_items.iter().all(|i| !old_items.contains(&i.id.0)),
        "no research records cross a restart"
    );

    // Lineage is recorded on the new run.
    let lineage = state
        .store()
        .list_decisions_of_kind(&new_id, DecisionKind::Restart)
        .await
        .unwrap();
    assert_eq!(lineage.len(), 1);
    assert_eq!(lineage[0].payload["restarted_from"], original.as_str());

    // The original run's state and history are untouched.
    let old_run = state.store().get_run(&original).await.unwrap();
    assert!(
        old_run.state.is_terminal(),
        "the original is left as it was"
    );

    // Both runs belong to the same browser.
    let response = app
        .oneshot(get_as("/api/runs", common::BROWSER_TOKEN))
        .await
        .unwrap();
    let runs = body_json(response).await["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(runs.contains(&original.as_str().to_string()));
    assert!(runs.contains(&new_id.as_str().to_string()));
}

#[tokio::test]
async fn restart_reingests_the_owned_uploads_rather_than_copying_results() {
    let (app, state, _dir) = completing_app().await;

    // Store one owner-bound upload, then start a run that uses it.
    let upload_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uploads?name=notes.txt")
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(
                    header::COOKIE,
                    format!("uruk_browser={}", common::BROWSER_TOKEN),
                )
                .body(Body::from("the measured drift is thermal"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload_response.status(), StatusCode::CREATED);
    let upload_id = body_json(upload_response).await["upload"]["id"]
        .as_str()
        .expect("upload id")
        .to_string();

    let original = start_api_run(
        &app,
        serde_json::json!({
            "goal": "what explains the drift in the notes",
            "mode": "task",
            "upload_ids": [upload_id],
        }),
    )
    .await;
    wait_terminal(state.store(), &original).await;
    let old_sources: Vec<String> = state
        .store()
        .list_sources(&original)
        .await
        .unwrap()
        .iter()
        .map(|s| s.id.as_str().to_string())
        .collect();
    assert!(
        !old_sources.is_empty(),
        "the upload was ingested originally"
    );

    let uri = format!("/api/runs/{original}/restart");
    let response = app.clone().oneshot(post(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let new_id = RunId::from_raw(
        body_json(response).await["run_id"]
            .as_str()
            .expect("run_id")
            .to_string(),
    );

    // The upload is re-ingested for the new run under fresh identifiers;
    // the intent (the goal's input list) is copied, the results are not.
    let new_goal = state.store().current_goal(&new_id).await.unwrap();
    assert!(
        new_goal.inputs.iter().any(|i| i.locator == "notes.txt"),
        "the intended input survives the restart: {:?}",
        new_goal.inputs
    );
    let new_sources = state.store().list_sources(&new_id).await.unwrap();
    assert_eq!(new_sources.len(), 1, "re-ingested, not copied");
    assert!(
        !old_sources.contains(&new_sources[0].id.as_str().to_string()),
        "fresh source identity"
    );
}

#[tokio::test]
async fn restart_fails_cleanly_when_an_original_upload_was_deleted() {
    let (app, state, _dir) = completing_app().await;

    let upload_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uploads?name=notes.txt")
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(
                    header::COOKIE,
                    format!("uruk_browser={}", common::BROWSER_TOKEN),
                )
                .body(Body::from("the measured drift is thermal"))
                .unwrap(),
        )
        .await
        .unwrap();
    let upload_id = body_json(upload_response).await["upload"]["id"]
        .as_str()
        .expect("upload id")
        .to_string();

    let original = start_api_run(
        &app,
        serde_json::json!({
            "goal": "what explains the drift in the notes",
            "mode": "task",
            "upload_ids": [upload_id.clone()],
        }),
    )
    .await;
    wait_terminal(state.store(), &original).await;

    let delete = Request::builder()
        .method("DELETE")
        .uri(format!("/api/uploads/{upload_id}"))
        .header(
            header::COOKIE,
            format!("uruk_browser={}", common::BROWSER_TOKEN),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(delete).await.unwrap().status(),
        StatusCode::OK
    );

    let uri = format!("/api/runs/{original}/restart");
    let response = app.oneshot(post(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "validation");
}

#[tokio::test]
async fn restart_of_an_actively_running_original_is_refused_until_it_pauses() {
    // A restart of a run that is still working would silently double its
    // spend; the browser must pause it (or let it finish) first.
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;
    fixture
        .store
        .save_web_run_config(
            &fixture.run_id,
            &serde_json::json!({"goal": "replay the seeded run", "mode": "task"}).to_string(),
        )
        .await
        .unwrap();

    let uri = format!("/api/runs/{}/restart", fixture.run_id);
    let response = app.clone().oneshot(post(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(response).await["kind"], "conflict");

    assert!(fixture.store.try_pause_run(&fixture.run_id).await.unwrap());
    let response = app.oneshot(post(&uri)).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "a paused original may be restarted"
    );
    let body = body_json(response).await;
    assert_ne!(
        body["run_id"].as_str().expect("run_id"),
        fixture.run_id.as_str()
    );
}

#[tokio::test]
async fn restart_of_a_run_without_a_stored_configuration_is_a_validation_error() {
    // Fixture runs are seeded directly in the store, like runs created
    // before restart existed: there is no canonical request to replay.
    let (app, _state, fixture) = seeded_app(Mode::Campaign).await;

    let uri = format!("/api/runs/{}/restart", fixture.run_id);
    let response = app.oneshot(post(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["kind"], "validation");
}
