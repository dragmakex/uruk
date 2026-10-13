//! Web runs execute one at a time, in the order they were started, and a
//! transient provider outage pauses the head instead of failing it
//! (docs/WEB.md "One run at a time").
//!
//! Everything runs offline: the mock provider's availability switch stands
//! in for a SwissAI outage, and the router is driven through
//! `tower::ServiceExt::oneshot`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;
use uruk::provider::{MockProvider, ProbeBackoff};
use uruk::records::{Mode, RunId, RunState, TaskState};
use uruk::runtime::{Scheduler, SchedulerConfig};
use uruk::store::Store;
use uruk::web::{AppState, WebConfig, router};

/// A provider scripted for a short task run, with a fast probe schedule so
/// an outage test does not wait on real backoff.
fn scripted_provider() -> MockProvider {
    MockProvider::new()
        .rule("Requested deliverable:", common::synthesis_json())
        .default_reply(common::review_json("inconclusive", "mock run", false))
        .with_probe_backoff(ProbeBackoff::new(
            Duration::from_millis(5),
            Duration::from_millis(20),
        ))
}

async fn app_over(store: &Store, provider: Arc<MockProvider>) -> (axum::Router, AppState) {
    let state = AppState::new(
        store.clone(),
        provider,
        WebConfig {
            sse_poll: Duration::from_millis(20),
            reconcile_interval: Duration::from_millis(50),
            ..WebConfig::default()
        },
    );
    (router(state.clone()), state)
}

async fn fresh_app(provider: MockProvider) -> (axum::Router, AppState, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let (app, state) = app_over(&store, Arc::new(provider)).await;
    (app, state, store, dir)
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

async fn start(app: &axum::Router, goal: &str) -> RunId {
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/runs",
            serde_json::json!({"goal": goal, "mode": "task", "ranking": "simple"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    RunId::from_raw(body["run_id"].as_str().expect("run_id").to_string())
}

async fn snapshot(app: &axum::Router, run_id: &RunId) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(get(&format!("/api/runs/{run_id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["snapshot"].clone()
}

/// Poll until `done` holds or `limit` passes; panics with `what` otherwise.
async fn wait_until<F, Fut>(limit: Duration, what: &str, mut done: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + limit;
    while !done().await {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn all_terminal(store: Store, runs: Vec<RunId>) -> bool {
    for id in &runs {
        if !store.get_run(id).await.unwrap().state.is_terminal() {
            return false;
        }
    }
    true
}

async fn started_at(store: &Store, run_id: &RunId) -> time::OffsetDateTime {
    store
        .run_timing(run_id)
        .await
        .unwrap()
        .started_at
        .unwrap_or_else(|| panic!("{run_id} never started"))
}

async fn no_task_failed(store: &Store, run_id: &RunId) {
    let tasks = store.list_tasks(run_id).await.unwrap();
    assert!(
        tasks.iter().all(|t| t.state != TaskState::Failed),
        "no task of {run_id} fails during an outage: {tasks:?}"
    );
}

#[tokio::test]
async fn three_runs_from_different_browsers_execute_fifo_one_at_a_time() {
    let provider = scripted_provider().with_delay(Duration::from_millis(60));
    let (app, state, store, _dir) = fresh_app(provider).await;
    let a = start(&app, "run a").await;
    let b = start(&app, "run b").await;
    let c = start(&app, "run c").await;
    for id in [&a, &b, &c] {
        assert_eq!(store.get_run(id).await.unwrap().state, RunState::Queued);
    }
    assert!(
        state.start_dispatcher(),
        "the first start spawns the dispatcher"
    );
    assert!(!state.start_dispatcher(), "there is exactly one dispatcher");

    // Never more than one run executes at a time.
    let watcher = tokio::spawn({
        let store = store.clone();
        let runs = vec![a.clone(), b.clone(), c.clone()];
        async move {
            let mut peak = 0usize;
            while !all_terminal(store.clone(), runs.clone()).await {
                let mut running = 0;
                for id in &runs {
                    if store.get_run(id).await.unwrap().state == RunState::Running {
                        running += 1;
                    }
                }
                peak = peak.max(running);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            peak
        }
    });
    wait_until(Duration::from_secs(60), "all three runs finish", || {
        all_terminal(store.clone(), vec![a.clone(), b.clone(), c.clone()])
    })
    .await;
    assert_eq!(watcher.await.unwrap(), 1, "only the head executes");

    for id in [&a, &b, &c] {
        assert_eq!(store.get_run(id).await.unwrap().state, RunState::Completed);
    }
    let (ta, tb, tc) = (
        started_at(&store, &a).await,
        started_at(&store, &b).await,
        started_at(&store, &c).await,
    );
    assert!(ta < tb && tb < tc, "FIFO: {ta} < {tb} < {tc}");
    let finished_a = store.get_run(&a).await.unwrap().updated_at;
    assert!(finished_a <= tb, "b started only after a finished");
    assert!(
        store.list_queue().await.unwrap().is_empty(),
        "nothing is left queued"
    );
}

#[tokio::test]
async fn an_outage_keeps_the_head_running_and_followers_queued_then_resumes_in_order() {
    let provider = scripted_provider();
    provider.set_available(false);
    let (app, state, store, _dir) = fresh_app(provider.clone()).await;
    let a = start(&app, "run a").await;
    let b = start(&app, "run b").await;
    let c = start(&app, "run c").await;
    state.start_dispatcher();

    // The head reaches the provider and starts waiting.
    wait_until(
        Duration::from_secs(10),
        "the head waits for the provider",
        || {
            let store = store.clone();
            let a = a.clone();
            async move { store.run_timing(&a).await.unwrap().is_paused() }
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let run_a = store.get_run(&a).await.unwrap();
    assert_eq!(run_a.state, RunState::Running, "the head is not failed");
    no_task_failed(&store, &a).await;
    assert!(
        provider.transient_failures() > 4,
        "the head kept probing past four attempts: {}",
        provider.transient_failures()
    );
    for (id, position) in [(&b, 2), (&c, 3)] {
        let run = store.get_run(id).await.unwrap();
        assert_eq!(run.state, RunState::Queued, "{id} waits its turn");
        assert_eq!(
            store.queue_status(id).await.unwrap().unwrap().ahead + 1,
            position
        );
        assert!(
            store.list_tasks(id).await.unwrap().is_empty(),
            "no provider-dependent work starts behind a waiting head"
        );
    }

    // The API tells the truth: queued position, waiting for the provider,
    // nothing failed.
    let snap_a = snapshot(&app, &a).await;
    assert_eq!(snap_a["run"]["state"], "running");
    assert!(
        snap_a["run"]["provider_wait"].is_object(),
        "the wait is visible: {}",
        snap_a["run"]
    );
    assert!(
        snap_a["run"]["provider_wait"]["note"]
            .as_str()
            .is_some_and(|n| !n.is_empty()),
        "the last transient failure is reported: {}",
        snap_a["run"]
    );
    assert_eq!(snap_a["run"]["queue_position"], serde_json::Value::Null);
    let snap_b = snapshot(&app, &b).await;
    assert_eq!(snap_b["run"]["state"], "queued");
    assert_eq!(snap_b["run"]["queue_position"], 2);
    assert_eq!(snap_b["run"]["provider_wait"], serde_json::Value::Null);
    let listing = body_json(app.clone().oneshot(get("/api/runs")).await.unwrap()).await;
    let runs = listing["runs"].as_array().expect("runs");
    let overview = |id: &RunId| {
        runs.iter()
            .find(|r| r["id"] == id.as_str())
            .unwrap_or_else(|| panic!("{id} listed"))
            .clone()
    };
    assert_eq!(overview(&c)["queue_position"], 3);
    assert_eq!(overview(&c)["state"], "queued");
    assert!(overview(&a)["paused_since"].is_string());
    assert_eq!(overview(&a)["queue_position"], serde_json::Value::Null);

    // Recovery: the head finishes first, then the followers in order.
    provider.set_available(true);
    wait_until(
        Duration::from_secs(60),
        "all runs finish after recovery",
        || all_terminal(store.clone(), vec![a.clone(), b.clone(), c.clone()]),
    )
    .await;
    for id in [&a, &b, &c] {
        assert_eq!(store.get_run(id).await.unwrap().state, RunState::Completed);
        no_task_failed(&store, id).await;
    }
    let (ta, tb, tc) = (
        started_at(&store, &a).await,
        started_at(&store, &b).await,
        started_at(&store, &c).await,
    );
    assert!(
        ta < tb && tb < tc,
        "FIFO after recovery: {ta} < {tb} < {tc}"
    );
    let timing = store.run_timing(&a).await.unwrap();
    assert!(!timing.is_paused(), "the wait is settled once served");
}

#[tokio::test]
async fn stop_works_promptly_for_queued_and_provider_waiting_runs() {
    let provider = scripted_provider();
    provider.set_available(false);
    let (app, state, store, _dir) = fresh_app(provider.clone()).await;
    let a = start(&app, "run a").await;
    let b = start(&app, "run b").await;
    let c = start(&app, "run c").await;
    state.start_dispatcher();
    wait_until(
        Duration::from_secs(10),
        "the head waits for the provider",
        || {
            let store = store.clone();
            let a = a.clone();
            async move { store.run_timing(&a).await.unwrap().is_paused() }
        },
    )
    .await;

    // Stop a queued follower: it leaves the queue at once.
    let response = app
        .clone()
        .oneshot(post_json(
            &format!("/api/runs/{b}/stop"),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(store.get_run(&b).await.unwrap().state, RunState::Cancelled);
    assert_eq!(store.queue_status(&b).await.unwrap(), None);
    assert_eq!(store.queue_status(&c).await.unwrap().unwrap().ahead, 1);

    // Stop the waiting head: the wait ends promptly, not after a probe.
    let stopped_at = Instant::now();
    let response = app
        .clone()
        .oneshot(post_json(
            &format!("/api/runs/{a}/stop"),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    wait_until(Duration::from_secs(5), "the head run winds down", || {
        let store = store.clone();
        let a = a.clone();
        async move { store.queue_status(&a).await.unwrap().is_none() }
    })
    .await;
    assert!(
        stopped_at.elapsed() < Duration::from_secs(3),
        "stop reached the waiting head in {:?}",
        stopped_at.elapsed()
    );
    assert_eq!(store.get_run(&a).await.unwrap().state, RunState::Cancelled);

    // The survivor runs once the provider is back.
    provider.set_available(true);
    wait_until(Duration::from_secs(60), "the survivor finishes", || {
        let store = store.clone();
        let c = c.clone();
        async move { store.get_run(&c).await.unwrap().state.is_terminal() }
    })
    .await;
    assert_eq!(store.get_run(&c).await.unwrap().state, RunState::Completed);
}

#[tokio::test]
async fn waiting_for_the_provider_consumes_neither_the_deadline_nor_the_call_budget() {
    let provider = scripted_provider();
    provider.set_available(false);
    let f = common::fixture_with(Mode::Task, provider.clone(), |g| {
        g.question = "assess this candidate".into();
        g.budget.wall_clock_secs = 2;
        g.budget.max_model_calls = 6;
        g.budget.reserve_calls_for_output = 1;
    })
    .await;
    common::seed_item(&f, "candidate").await;

    let scheduler = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    );
    let run_id = f.run_id.clone();
    let handle = tokio::spawn(async move { scheduler.run(&run_id).await });

    // The outage outlives the whole wall-clock budget.
    tokio::time::sleep(Duration::from_millis(3200)).await;
    assert!(
        !handle.is_finished(),
        "the run must not end while the provider is unavailable"
    );
    let run = f.store.get_run(&f.run_id).await.unwrap();
    assert_eq!(run.state, RunState::Running, "{run:?}");
    let timing = f.store.run_timing(&f.run_id).await.unwrap();
    assert!(timing.is_paused(), "the wait is recorded: {timing:?}");
    assert!(
        provider.transient_failures() > 4,
        "probing continued: {}",
        provider.transient_failures()
    );

    provider.set_available(true);
    let summary = tokio::time::timeout(Duration::from_secs(60), handle)
        .await
        .expect("the run finishes after recovery")
        .unwrap()
        .unwrap();
    assert_eq!(summary.state, RunState::Completed, "{summary:?}");
    assert_eq!(summary.tasks_failed, 0, "{summary:?}");
    assert!(
        summary.final_output_skipped.is_none(),
        "the deliverable ran: {summary:?}"
    );

    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(
        usage.model_calls,
        provider.served_count(),
        "only served calls are charged against the logical budget: {usage:?}"
    );
    assert!(
        usage.model_calls < provider.transient_failures(),
        "unserved probes are not model calls: {usage:?}"
    );
    let timing = f.store.run_timing(&f.run_id).await.unwrap();
    assert!(timing.paused_secs >= 2, "the wait is settled: {timing:?}");
    assert!(!timing.is_paused());
    let resends: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM budget_ledger WHERE run_id = ? AND kind = 'resend'",
    )
    .bind(f.run_id.as_str())
    .fetch_one(f.store.pool())
    .await
    .unwrap();
    assert!(resends > 0, "every resend is an auditable ledger fact");
}

#[tokio::test]
async fn a_restart_resumes_the_queue_in_order_with_the_interrupted_head_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".uruk/state.sqlite");
    let (a, b, c) = {
        let store = Store::open(&path).await.expect("open store");
        let provider = scripted_provider();
        provider.set_available(false);
        let (app, state) = app_over(&store, Arc::new(provider)).await;
        let a = start(&app, "run a").await;
        let b = start(&app, "run b").await;
        let c = start(&app, "run c").await;
        state.start_dispatcher();
        wait_until(Duration::from_secs(10), "the head is active", || {
            let store = store.clone();
            let a = a.clone();
            async move { store.run_timing(&a).await.unwrap().is_paused() }
        })
        .await;
        // The process dies with the head mid-wait and two runs queued.
        drop(state);
        store.close().await;
        (a, b, c)
    };

    let store = Store::open(&path).await.expect("reopen store");
    assert_eq!(store.get_run(&a).await.unwrap().state, RunState::Running);
    assert_eq!(store.get_run(&b).await.unwrap().state, RunState::Queued);
    let (_app, state) = app_over(&store, Arc::new(scripted_provider())).await;
    let resumed = uruk::web::resume_orphaned_runs(&state).await.unwrap();
    assert!(
        resumed.is_empty(),
        "queued runs are already on the durable queue: {resumed:?}"
    );
    state.start_dispatcher();

    wait_until(Duration::from_secs(60), "the whole queue drains", || {
        all_terminal(store.clone(), vec![a.clone(), b.clone(), c.clone()])
    })
    .await;
    for id in [&a, &b, &c] {
        assert_eq!(store.get_run(id).await.unwrap().state, RunState::Completed);
    }
    let (ta, tb, tc) = (
        started_at(&store, &a).await,
        started_at(&store, &b).await,
        started_at(&store, &c).await,
    );
    assert!(
        ta < tb && tb < tc,
        "FIFO across the restart: {ta} < {tb} < {tc}"
    );
    let finished_a = store.get_run(&a).await.unwrap().updated_at;
    assert!(
        finished_a <= tb,
        "the interrupted head finished before the next run started"
    );
    assert!(
        !store.run_timing(&a).await.unwrap().is_paused(),
        "the pre-restart wait is settled once the provider serves again"
    );
}

#[tokio::test]
async fn an_engine_run_left_running_is_enqueued_by_the_sweep_not_dispatched_directly() {
    let fixture = common::fixture(Mode::Task, scripted_provider()).await;
    let (_app, state) = app_over(&fixture.store, fixture.provider.clone()).await;

    let resumed = uruk::web::resume_orphaned_runs(&state).await.unwrap();
    assert_eq!(resumed, vec![fixture.run_id.clone()]);
    assert_eq!(
        fixture
            .store
            .queue_status(&fixture.run_id)
            .await
            .unwrap()
            .unwrap()
            .ahead,
        0
    );
    assert!(
        uruk::web::resume_orphaned_runs(&state)
            .await
            .unwrap()
            .is_empty(),
        "a run already on the queue is not enqueued twice"
    );

    state.start_dispatcher();
    wait_until(Duration::from_secs(30), "the orphaned run finishes", || {
        let store = fixture.store.clone();
        let id = fixture.run_id.clone();
        async move { store.get_run(&id).await.unwrap().state.is_terminal() }
    })
    .await;
    assert_eq!(
        fixture.store.get_run(&fixture.run_id).await.unwrap().state,
        RunState::Completed
    );
}

/// A run and goal exactly as `POST /api/runs` persists them before the
/// start request has prepared the run.
fn unprepared_run(goal_text: &str) -> (uruk::records::Run, uruk::records::Goal) {
    use uruk::records::*;
    let run_id = RunId::new();
    let goal_id = GoalId::new();
    let now = time::OffsetDateTime::now_utc();
    let goal = Goal {
        id: goal_id.clone(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        revision: 0,
        question: goal_text.into(),
        mode: Mode::Task,
        deliverables: vec!["report".into()],
        inputs: vec![],
        scope: None,
        exclusions: vec![],
        known_facts: vec![],
        assumptions: vec![],
        rubric: Rubric::default(),
        permissions: Permissions {
            disclose_to_provider: true,
            ..Permissions::default()
        },
        budget: Budget::default(),
        profile: None,
        revision_reason: None,
        created_at: now,
    };
    let run = Run {
        id: run_id,
        schema_version: SCHEMA_VERSION,
        project_id: ProjectId::from_raw("prj_test"),
        goal_id,
        plan_id: None,
        state: RunState::Queued,
        stop_condition: None,
        iterations: 0,
        created_at: now,
        updated_at: now,
    };
    (run, goal)
}

#[tokio::test]
async fn a_start_interrupted_after_its_commit_never_executes_and_the_queue_moves_on() {
    let (app, state, store, _dir) = fresh_app(scripted_provider()).await;
    // The first start committed its run and queue place, then died (a
    // crash, a client timeout) before preparing it: no plan, no safety
    // decision. Nothing in memory remembers it.
    store
        .ensure_project(&uruk::records::ProjectId::from_raw("prj_test"), "test")
        .await
        .unwrap();
    let (run, goal) = unprepared_run("an interrupted start");
    store.create_run_queued(&run, &goal, None).await.unwrap();
    let a = run.id.clone();
    // A later, complete start is the selectable run: the unprepared one
    // is not eligible, so it is listed behind, never ahead.
    let b = start(&app, "run b").await;
    assert_eq!(store.queue_status(&b).await.unwrap().unwrap().ahead, 0);
    assert_eq!(store.queue_status(&a).await.unwrap().unwrap().ahead, 1);

    state.start_dispatcher();
    wait_until(Duration::from_secs(30), "the complete run finishes", || {
        let store = store.clone();
        let b = b.clone();
        async move { store.get_run(&b).await.unwrap().state.is_terminal() }
    })
    .await;

    let abandoned = store.get_run(&a).await.unwrap();
    assert_eq!(abandoned.state, RunState::Failed, "{abandoned:?}");
    assert_eq!(
        abandoned.stop_condition,
        Some(uruk::records::StopCondition::StartIncomplete)
    );
    assert!(
        store.list_tasks(&a).await.unwrap().is_empty(),
        "an unprepared run never schedules work"
    );
    assert!(
        store.run_timing(&a).await.unwrap().started_at.is_none(),
        "it never started"
    );
    assert_eq!(store.get_run(&b).await.unwrap().state, RunState::Completed);
    assert!(store.list_queue().await.unwrap().is_empty());
    let snap = snapshot(&app, &b).await;
    assert_eq!(snap["run"]["state"], "completed");
}

#[tokio::test]
async fn a_start_cut_off_by_the_request_timeout_leaves_no_executable_run_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let state = AppState::new(
        store.clone(),
        Arc::new(scripted_provider()),
        WebConfig {
            sse_poll: Duration::from_millis(20),
            reconcile_interval: Duration::from_millis(50),
            // The start deadline passes while the handler is still working.
            start_timeout: Duration::from_millis(1),
            ..WebConfig::default()
        },
    );
    let app = router(state.clone());
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/runs",
            serde_json::json!({"goal": "a start that times out", "mode": "task"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);

    state.start_dispatcher();
    // Whatever the timeout cut off, the queue settles: no run is left
    // executable without having been fully prepared.
    wait_until(Duration::from_secs(30), "the queue settles", || {
        let store = store.clone();
        async move {
            let runs = store.list_run_overviews().await.unwrap();
            runs.iter().all(|r| r.state.is_terminal())
        }
    })
    .await;
    let runs = store.list_run_overviews().await.unwrap();
    for run in &runs {
        let plan = store.current_plan(&run.id).await.unwrap();
        match run.state {
            RunState::Failed => {
                assert_eq!(
                    run.stop_condition,
                    Some(uruk::records::StopCondition::StartIncomplete),
                    "{run:?}"
                );
                assert!(
                    store.list_tasks(&run.id).await.unwrap().is_empty(),
                    "an abandoned start scheduled no work: {run:?}"
                );
            }
            RunState::Completed => {
                assert!(
                    plan.is_some(),
                    "only a fully prepared run executes: {run:?}"
                );
            }
            other => panic!("unexpected state {other:?} for {run:?}"),
        }
    }
    assert!(store.list_queue().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_scheduler_error_keeps_the_head_in_place_and_retries() {
    let (app, state, store, _dir) = fresh_app(scripted_provider()).await;
    let a = start(&app, "run a").await;
    let b = start(&app, "run b").await;
    // Corrupt one of a's persisted tasks: recovery fails to read it, so
    // the scheduler errors out before doing any work.
    sqlx::query(
        "INSERT INTO tasks (id, run_id, role, strategy, state, priority, attempts, seed,
                            input_hash, body, created_at, updated_at)
         VALUES ('task_corrupt', ?, 'reflection', 'initial', 'running', 1.0, 1, 1,
                 'h', 'not json', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    )
    .bind(a.as_str())
    .execute(store.pool())
    .await
    .unwrap();

    state.start_dispatcher();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let head = store.fair_head().await.unwrap().expect("a head");
    assert_eq!(head.run_id, a, "the failing head keeps its place");
    assert_eq!(head.state, uruk::store::QueueState::Active);
    assert_eq!(store.get_run(&a).await.unwrap().state, RunState::Running);
    assert_eq!(store.get_run(&b).await.unwrap().state, RunState::Queued);
    assert_eq!(store.queue_status(&b).await.unwrap().unwrap().ahead, 1);
    assert!(
        uruk::web::resume_orphaned_runs(&state)
            .await
            .unwrap()
            .is_empty(),
        "the sweep must not re-enqueue a run that still holds the head"
    );

    // Repair the store: the dispatcher's next retry resumes a in place,
    // and b follows only afterwards.
    sqlx::query("DELETE FROM tasks WHERE id = 'task_corrupt'")
        .execute(store.pool())
        .await
        .unwrap();
    wait_until(Duration::from_secs(60), "both runs finish", || {
        all_terminal(store.clone(), vec![a.clone(), b.clone()])
    })
    .await;
    assert_eq!(store.get_run(&a).await.unwrap().state, RunState::Completed);
    assert_eq!(store.get_run(&b).await.unwrap().state, RunState::Completed);
    assert!(started_at(&store, &a).await < started_at(&store, &b).await);
}

#[tokio::test]
async fn the_sweep_settles_tasks_orphaned_by_a_crash_after_a_stop() {
    let fixture = common::fixture(Mode::Task, scripted_provider()).await;
    let (_app, state) = app_over(&fixture.store, fixture.provider.clone()).await;
    let item = common::seed_item(&fixture, "candidate").await;
    let task = {
        use uruk::records::*;
        Task {
            id: TaskId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: fixture.run_id.clone(),
            role: Role::Reflection,
            strategy: "initial".into(),
            goal_id: fixture.goal.id.clone(),
            plan_id: fixture.plan.id.clone(),
            input_refs: vec![item.id.0.clone()],
            input_hash: ContentHash::of_str("in"),
            prompt_id: None,
            prompt_hash: None,
            model: None,
            provider: None,
            payload: None,
            feedback_id: None,
            depends_on: vec![],
            priority: 1.0,
            priority_rationale: "test".into(),
            permissions: fixture.goal.permissions.clone(),
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
            failure: None,
            created_at: time::OffsetDateTime::now_utc(),
            started_at: None,
            finished_at: None,
        }
    };
    fixture.store.enqueue_task(&task, None).await.unwrap();
    assert!(fixture.store.claim_task(&task.id).await.unwrap());
    // The researcher stopped the run; the process died before the
    // scheduler could settle the running task.
    uruk::runtime::request_stop(&fixture.store, &fixture.run_id)
        .await
        .unwrap();

    uruk::web::resume_orphaned_runs(&state).await.unwrap();
    let task = fixture.store.get_task(&task.id).await.unwrap();
    assert_eq!(task.state, TaskState::Cancelled, "{task:?}");
    assert!(
        fixture
            .store
            .list_tasks(&fixture.run_id)
            .await
            .unwrap()
            .iter()
            .all(|t| t.state.is_terminal()),
        "a stopped run holds no open work after the sweep"
    );
    assert_eq!(
        fixture.store.queue_status(&fixture.run_id).await.unwrap(),
        None,
        "a cancelled run is never put back on the queue"
    );
}

fn post_json_as(uri: &str, body: serde_json::Value, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("uruk_browser={token}"))
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn start_as(app: &axum::Router, goal: &str, token: &str) -> RunId {
    let response = app
        .clone()
        .oneshot(post_json_as(
            "/api/runs",
            serde_json::json!({"goal": goal, "mode": "task", "ranking": "simple"}),
            token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    RunId::from_raw(body["run_id"].as_str().expect("run_id").to_string())
}

#[tokio::test]
async fn browsers_take_turns_and_positions_show_the_projected_order() {
    let provider = scripted_provider();
    provider.set_available(false);
    let (app, state, store, _dir) = fresh_app(provider.clone()).await;
    let alice = "a".repeat(64);
    let bob = "b".repeat(64);
    let carol = "c".repeat(64);
    let a1 = start_as(&app, "alice one", &alice).await;
    let a2 = start_as(&app, "alice two", &alice).await;
    let a3 = start_as(&app, "alice three", &alice).await;
    let b1 = start_as(&app, "bob one", &bob).await;
    let c1 = start_as(&app, "carol one", &carol).await;
    let b2 = start_as(&app, "bob two", &bob).await;
    state.start_dispatcher();
    wait_until(
        Duration::from_secs(10),
        "the first run waits for the provider",
        || {
            let store = store.clone();
            let a1 = a1.clone();
            async move { store.run_timing(&a1).await.unwrap().is_paused() }
        },
    )
    .await;

    // Positions are the projected fair order, not the start order.
    for (id, position) in [(&b1, 2), (&c1, 3), (&a2, 4), (&b2, 5), (&a3, 6)] {
        let snap = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/runs/{id}"))
                    .header(
                        header::COOKIE,
                        format!(
                            "uruk_browser={}",
                            match position {
                                2 | 5 => &bob,
                                3 => &carol,
                                _ => &alice,
                            }
                        ),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(snap.status(), StatusCode::OK);
        let body = body_json(snap).await;
        assert_eq!(body["snapshot"]["run"]["queue_position"], position, "{id}");
    }

    provider.set_available(true);
    let all = vec![
        a1.clone(),
        a2.clone(),
        a3.clone(),
        b1.clone(),
        c1.clone(),
        b2.clone(),
    ];
    wait_until(Duration::from_secs(120), "every run finishes", || {
        all_terminal(store.clone(), all.clone())
    })
    .await;
    let mut by_start: Vec<(time::OffsetDateTime, RunId)> = Vec::new();
    for id in &all {
        by_start.push((started_at(&store, id).await, id.clone()));
    }
    by_start.sort();
    let order: Vec<RunId> = by_start.into_iter().map(|(_, id)| id).collect();
    assert_eq!(
        order,
        vec![a1, b1, c1, a2, b2, a3],
        "one run per browser per cycle"
    );
}

#[tokio::test]
async fn the_listing_shows_the_position_of_a_parked_run_put_back_on_the_queue() {
    let fixture =
        common::fixture_owned(Mode::Task, scripted_provider(), common::BROWSER_TOKEN).await;
    let (app, state) = app_over(&fixture.store, fixture.provider.clone()).await;
    // Parked for a person, then released: the sweep gives it a new turn.
    fixture
        .store
        .set_run_state(&fixture.run_id, RunState::WaitingForHuman, None)
        .await
        .unwrap();
    let enqueued = uruk::web::resume_orphaned_runs(&state).await.unwrap();
    assert_eq!(enqueued, vec![fixture.run_id.clone()]);

    let listing = body_json(app.clone().oneshot(get("/api/runs")).await.unwrap()).await;
    let run = &listing["runs"][0];
    assert_eq!(run["id"], fixture.run_id.as_str());
    assert_eq!(run["state"], "waiting-for-human");
    assert_eq!(
        run["queue_position"], 1,
        "the position comes from the live queue entry: {run}"
    );
}
