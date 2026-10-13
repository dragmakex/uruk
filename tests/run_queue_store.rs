//! The durable FIFO run queue in SQLite (docs/WEB.md "One run at a time").
//!
//! The queue is a dedicated sequence, not a timestamp order: a run's place
//! is the order in which it was enqueued, whatever its clock says. Every
//! transition here is a store fact, so a restarted server rebuilds exactly
//! the same queue.

mod common;

use std::sync::Arc;
use time::OffsetDateTime;
use uruk::records::*;
use uruk::store::{QueueState, Store};

/// A run and goal pair the way the web start path builds them: the run is
/// `queued` until the dispatcher activates it.
fn queued_run(created_at: OffsetDateTime) -> (Run, Goal) {
    let run_id = RunId::new();
    let goal_id = GoalId::new();
    let goal = Goal {
        id: goal_id.clone(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        revision: 0,
        question: "why do these measurements disagree".into(),
        mode: Mode::Task,
        deliverables: vec!["report".into()],
        inputs: vec![],
        scope: None,
        exclusions: vec![],
        known_facts: vec![],
        assumptions: vec![],
        rubric: Rubric::default(),
        permissions: Permissions::default(),
        budget: Budget {
            wall_clock_secs: 60,
            ..Budget::default()
        },
        profile: None,
        revision_reason: None,
        created_at,
    };
    let run = Run {
        id: run_id,
        schema_version: SCHEMA_VERSION,
        project_id: ProjectId::from_raw("prj_queue"),
        goal_id,
        plan_id: None,
        state: RunState::Queued,
        stop_condition: None,
        iterations: 0,
        created_at,
        updated_at: created_at,
    };
    (run, goal)
}

async fn open_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    store
        .ensure_project(&ProjectId::from_raw("prj_queue"), "queue")
        .await
        .unwrap();
    (store, dir)
}

/// Create a web run "at" `created_at` and finish its preparation, the
/// way a completed start request leaves it; returns its id.
async fn enqueue(store: &Store, created_at: OffsetDateTime) -> RunId {
    let (run, goal) = queued_run(created_at);
    store.create_run_queued(&run, &goal, None).await.unwrap();
    assert!(store.ready_queue_entry(&run.id).await.unwrap());
    run.id
}

#[tokio::test]
async fn runs_queue_in_enqueue_order_even_when_their_timestamps_disagree() {
    let (store, _dir) = open_store().await;
    let now = OffsetDateTime::now_utc();
    // Later enqueues carry *earlier* creation stamps: the sequence, not the
    // clock, decides the order.
    let first = enqueue(&store, now).await;
    let second = enqueue(&store, now - time::Duration::minutes(5)).await;
    let third = enqueue(&store, now - time::Duration::minutes(10)).await;

    let live: Vec<RunId> = store
        .list_queue()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.run_id)
        .collect();
    assert_eq!(live, vec![first.clone(), second.clone(), third.clone()]);
    let head = store.queue_head().await.unwrap().expect("a head");
    assert_eq!(head.run_id, first);
    assert_eq!(head.state, QueueState::Queued);

    for (run_id, ahead) in [(&first, 0), (&second, 1), (&third, 2)] {
        let status = store.queue_status(run_id).await.unwrap().expect("queued");
        assert_eq!(status.ahead, ahead, "{run_id} has {ahead} run(s) ahead");
        assert_eq!(status.state, QueueState::Queued);
    }
    let run = store.get_run(&first).await.unwrap();
    assert_eq!(run.state, RunState::Queued, "enqueued runs start queued");
}

#[tokio::test]
async fn exactly_one_concurrent_claimant_activates_the_head() {
    let (store, _dir) = open_store().await;
    let head = enqueue(&store, OffsetDateTime::now_utc()).await;
    enqueue(&store, OffsetDateTime::now_utc()).await;

    let store = Arc::new(store);
    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = Arc::clone(&store);
        let run_id = head.clone();
        handles.push(tokio::spawn(async move {
            store.activate_queue_entry(&run_id).await.unwrap()
        }));
    }
    let mut activated = 0;
    for handle in handles {
        if handle.await.unwrap() {
            activated += 1;
        }
    }
    assert_eq!(activated, 1, "the queued→active transition happens once");

    let entry = store.queue_head().await.unwrap().expect("head");
    assert_eq!(entry.run_id, head);
    assert_eq!(entry.state, QueueState::Active);
    assert!(entry.activated_at.is_some());
    let run = store.get_run(&head).await.unwrap();
    assert_eq!(run.state, RunState::Running, "activation starts the run");
    let timing = store.run_timing(&head).await.unwrap();
    assert!(timing.started_at.is_some(), "the research clock starts now");
}

#[tokio::test]
async fn a_follower_cannot_be_activated_while_the_head_is_live() {
    let (store, _dir) = open_store().await;
    let head = enqueue(&store, OffsetDateTime::now_utc()).await;
    let follower = enqueue(&store, OffsetDateTime::now_utc()).await;

    assert!(
        !store.activate_queue_entry(&follower).await.unwrap(),
        "a queued head blocks its followers"
    );
    assert!(store.activate_queue_entry(&head).await.unwrap());
    assert!(
        !store.activate_queue_entry(&follower).await.unwrap(),
        "an active head (even one waiting for the provider) blocks its followers"
    );
    assert_eq!(
        store.get_run(&follower).await.unwrap().state,
        RunState::Queued,
        "a blocked follower is untouched"
    );
    assert_eq!(store.queue_head().await.unwrap().unwrap().run_id, head);
}

#[tokio::test]
async fn cancelling_a_queued_run_advances_the_head_in_the_same_transaction() {
    let (store, _dir) = open_store().await;
    let first = enqueue(&store, OffsetDateTime::now_utc()).await;
    let second = enqueue(&store, OffsetDateTime::now_utc()).await;
    let third = enqueue(&store, OffsetDateTime::now_utc()).await;

    assert!(store.try_cancel_run(&first).await.unwrap());

    let head = store.queue_head().await.unwrap().expect("head");
    assert_eq!(head.run_id, second, "the next run is now the head");
    assert_eq!(
        store.queue_status(&first).await.unwrap(),
        None,
        "a cancelled run holds no live queue place"
    );
    assert_eq!(store.queue_status(&third).await.unwrap().unwrap().ahead, 1);
    assert_eq!(
        store.get_run(&first).await.unwrap().state,
        RunState::Cancelled
    );
}

#[tokio::test]
async fn cancelling_the_active_head_closes_its_entry() {
    let (store, _dir) = open_store().await;
    let head = enqueue(&store, OffsetDateTime::now_utc()).await;
    let next = enqueue(&store, OffsetDateTime::now_utc()).await;
    assert!(store.activate_queue_entry(&head).await.unwrap());

    assert!(store.try_cancel_run(&head).await.unwrap());

    assert_eq!(store.queue_head().await.unwrap().unwrap().run_id, next);
    assert_eq!(store.queue_status(&head).await.unwrap(), None);
}

#[tokio::test]
async fn finishing_a_run_closes_its_queue_entry_atomically() {
    let (store, _dir) = open_store().await;
    let head = enqueue(&store, OffsetDateTime::now_utc()).await;
    let next = enqueue(&store, OffsetDateTime::now_utc()).await;
    assert!(store.activate_queue_entry(&head).await.unwrap());

    assert!(
        store
            .finish_run(&head, RunState::Completed, StopCondition::WorkExhausted)
            .await
            .unwrap()
    );

    assert_eq!(store.queue_head().await.unwrap().unwrap().run_id, next);
    let entries = store.list_queue().await.unwrap();
    assert!(
        entries.iter().all(|e| e.run_id != head),
        "a finished run is not in the live queue: {entries:?}"
    );
}

#[tokio::test]
async fn a_blocked_run_leaves_the_queue() {
    let (store, _dir) = open_store().await;
    let blocked = enqueue(&store, OffsetDateTime::now_utc()).await;
    let next = enqueue(&store, OffsetDateTime::now_utc()).await;

    store
        .set_run_state(
            &blocked,
            RunState::Blocked,
            Some(StopCondition::SafetyBoundary),
        )
        .await
        .unwrap();

    assert_eq!(store.queue_head().await.unwrap().unwrap().run_id, next);
    assert_eq!(store.queue_status(&blocked).await.unwrap(), None);
}

#[tokio::test]
async fn a_restart_resumes_the_active_head_first_then_the_queue_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".uruk/state.sqlite");
    let (head, second, third) = {
        let store = Store::open(&path).await.unwrap();
        store
            .ensure_project(&ProjectId::from_raw("prj_queue"), "queue")
            .await
            .unwrap();
        let head = enqueue(&store, OffsetDateTime::now_utc()).await;
        let second = enqueue(&store, OffsetDateTime::now_utc()).await;
        let third = enqueue(&store, OffsetDateTime::now_utc()).await;
        assert!(store.activate_queue_entry(&head).await.unwrap());
        store.close().await;
        (head, second, third)
    };

    // --- restart: nothing in memory survives ---
    let store = Store::open(&path).await.unwrap();
    let entry = store
        .queue_head()
        .await
        .unwrap()
        .expect("the head survives");
    assert_eq!(entry.run_id, head);
    assert_eq!(
        entry.state,
        QueueState::Active,
        "an interrupted active head is resumed, never re-queued behind others"
    );
    assert!(
        !store.activate_queue_entry(&second).await.unwrap(),
        "the stale active claim still belongs to the head"
    );
    assert_eq!(store.get_run(&head).await.unwrap().state, RunState::Running);

    store.finish_queue_entry(&head).await.unwrap();
    assert_eq!(store.queue_head().await.unwrap().unwrap().run_id, second);
    assert!(store.activate_queue_entry(&second).await.unwrap());
    store.finish_queue_entry(&second).await.unwrap();
    assert_eq!(store.queue_head().await.unwrap().unwrap().run_id, third);
}

#[tokio::test]
async fn provider_wait_timing_survives_reopen_and_extends_the_deadline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".uruk/state.sqlite");
    let store = Store::open(&path).await.unwrap();
    store
        .ensure_project(&ProjectId::from_raw("prj_queue"), "queue")
        .await
        .unwrap();
    let run_id = enqueue(&store, OffsetDateTime::now_utc()).await;
    assert!(store.activate_queue_entry(&run_id).await.unwrap());
    let started = store
        .run_timing(&run_id)
        .await
        .unwrap()
        .started_at
        .expect("started");

    let since = started + time::Duration::seconds(10);
    store
        .begin_provider_wait(&run_id, since, "HTTP 503: overloaded")
        .await
        .unwrap();
    store.close().await;

    // --- restart ---
    let store = Store::open(&path).await.unwrap();
    let timing = store.run_timing(&run_id).await.unwrap();
    assert_eq!(timing.paused_since, Some(since), "the open wait survives");
    assert_eq!(timing.pause_note.as_deref(), Some("HTTP 503: overloaded"));
    assert!(timing.is_paused());

    let until = since + time::Duration::seconds(30);
    assert!(store.end_provider_wait(&run_id, until).await.unwrap());
    let timing = store.run_timing(&run_id).await.unwrap();
    assert_eq!(timing.paused_since, None);
    assert_eq!(
        timing.paused_secs, 30,
        "the wait is settled in whole seconds"
    );
    assert_eq!(
        timing.deadline(60),
        Some(started + time::Duration::seconds(90)),
        "waiting for the provider does not consume the wall-clock budget"
    );
    assert!(
        !store.end_provider_wait(&run_id, until).await.unwrap(),
        "ending a wait that is not open changes nothing"
    );
}

#[tokio::test]
async fn re_enqueueing_a_run_keeps_its_first_start_time() {
    let (store, _dir) = open_store().await;
    let run_id = enqueue(&store, OffsetDateTime::now_utc()).await;
    assert!(store.activate_queue_entry(&run_id).await.unwrap());
    let first_start = store.run_timing(&run_id).await.unwrap().started_at;
    store.finish_queue_entry(&run_id).await.unwrap();

    assert!(
        store.enqueue_run(&run_id).await.unwrap(),
        "a run with no live entry can be re-enqueued"
    );
    assert!(
        !store.enqueue_run(&run_id).await.unwrap(),
        "a run with a live entry is not enqueued twice"
    );
    assert!(store.activate_queue_entry(&run_id).await.unwrap());
    assert_eq!(
        store.run_timing(&run_id).await.unwrap().started_at,
        first_start,
        "the research clock is not reset by a second activation"
    );
}

#[tokio::test]
async fn the_sweep_fixture_is_consistent_with_the_queue() {
    // A fixture run created the engine way (state `running`, no queue entry)
    // is exactly the shape of a run predating the queue: it has no live
    // entry until something enqueues it.
    let f = common::fixture(Mode::Task, uruk::provider::MockProvider::new()).await;
    assert_eq!(f.store.queue_status(&f.run_id).await.unwrap(), None);
    assert!(f.store.enqueue_run(&f.run_id).await.unwrap());
    assert_eq!(
        f.store
            .queue_status(&f.run_id)
            .await
            .unwrap()
            .unwrap()
            .ahead,
        0
    );
}

#[tokio::test]
async fn a_provider_wait_never_starts_before_the_run_did() {
    // The gate records when the *upstream* went down, which can predate
    // this run (the provider was already down during the previous run).
    // Only the part of the outage this run lived through may be credited.
    let (store, _dir) = open_store().await;
    let run_id = enqueue(&store, OffsetDateTime::now_utc()).await;
    assert!(store.activate_queue_entry(&run_id).await.unwrap());
    let started = store
        .run_timing(&run_id)
        .await
        .unwrap()
        .started_at
        .expect("started");

    let long_before = started - time::Duration::hours(2);
    store
        .begin_provider_wait(&run_id, long_before, "HTTP 503")
        .await
        .unwrap();
    let timing = store.run_timing(&run_id).await.unwrap();
    assert_eq!(
        timing.paused_since,
        Some(started),
        "the wait is clamped to the run's own start"
    );
    assert!(
        store
            .end_provider_wait(&run_id, started + time::Duration::seconds(5))
            .await
            .unwrap()
    );
    assert_eq!(store.run_timing(&run_id).await.unwrap().paused_secs, 5);
}

/// Build a database exactly as a pre-queue server left it (migrations
/// 0001–0005 applied and recorded, runs in `running` and `completed`),
/// then open it with the current store.
#[tokio::test]
async fn a_pre_queue_database_migrates_running_runs_onto_the_queue_in_creation_order() {
    use sha2::Digest;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".uruk/state.sqlite");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();

    // sqlx's own bookkeeping table, so the migrator sees 0001–0005 as
    // applied and only runs 0006.
    sqlx::query(
        "CREATE TABLE _sqlx_migrations (
             version BIGINT PRIMARY KEY,
             description TEXT NOT NULL,
             installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
             success BOOLEAN NOT NULL,
             checksum BLOB NOT NULL,
             execution_time BIGINT NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut files: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            name.ends_with(".sql") && name.as_str() < "0006"
        })
        .collect();
    files.sort();
    assert_eq!(files.len(), 5, "the five pre-queue migrations: {files:?}");
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let (version, rest) = name.split_once('_').unwrap();
        let description = rest.trim_end_matches(".sql").replace('_', " ");
        let sql = std::fs::read_to_string(&file).unwrap();
        sqlx::raw_sql(&sql).execute(&pool).await.unwrap();
        let checksum = sha2::Sha384::digest(sql.as_bytes()).to_vec();
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
             VALUES (?, ?, 1, ?, 0)",
        )
        .bind(version.parse::<i64>().unwrap())
        .bind(description)
        .bind(checksum)
        .execute(&pool)
        .await
        .unwrap();
    }

    // Two interrupted runs, created in the opposite order of their ids,
    // and one finished run.
    sqlx::query("INSERT INTO projects (id, name, created_at) VALUES ('prj_old', 'old', '2026-01-01T00:00:00Z')")
        .execute(&pool)
        .await
        .unwrap();
    for (id, state, created) in [
        ("run_old_b", "running", "2026-01-01T10:00:00Z"),
        ("run_old_a", "running", "2026-01-01T09:00:00Z"),
        ("run_old_done", "completed", "2026-01-01T08:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO runs (id, schema_version, project_id, goal_id, state, iterations,
                               created_at, updated_at)
             VALUES (?, 1, 'prj_old', ?, ?, 0, ?, ?)",
        )
        .bind(id)
        .bind(format!("goal_{id}"))
        .bind(state)
        .bind(created)
        .bind(created)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;

    // --- the current server opens it ---
    let store = Store::open(&path)
        .await
        .expect("migration 0006 applies cleanly");
    let queue: Vec<String> = store
        .list_queue()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.run_id.as_str().to_string())
        .collect();
    assert_eq!(
        queue,
        vec!["run_old_a".to_string(), "run_old_b".to_string()],
        "interrupted runs take the queue in creation order; finished ones do not"
    );
    for id in ["run_old_a", "run_old_b", "run_old_done"] {
        let timing = store
            .run_timing(&RunId::from_raw(id.to_string()))
            .await
            .unwrap();
        assert_eq!(
            timing.started_at,
            Some(timing.created_at),
            "{id}: a pre-queue run's clock keeps running from its creation"
        );
        assert_eq!(timing.paused_secs, 0);
        assert!(!timing.is_paused());
    }
    assert_eq!(
        store
            .queue_status(&RunId::from_raw("run_old_done".to_string()))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_new_web_run_is_preparing_and_cannot_be_activated_until_ready() {
    let (store, _dir) = open_store().await;
    let (run, goal) = queued_run(OffsetDateTime::now_utc());
    let entry = store.create_run_queued(&run, &goal, None).await.unwrap();
    assert_eq!(
        entry.state,
        QueueState::Preparing,
        "born preparing, durably"
    );
    assert_eq!(
        store.queue_status(&run.id).await.unwrap().unwrap().state,
        QueueState::Preparing
    );
    assert!(
        !store.activate_queue_entry(&run.id).await.unwrap(),
        "an unprepared head never executes"
    );

    let plan = uruk::agents::supervisor::build_plan(&goal, 0);
    assert!(
        store.insert_plan_and_ready(&plan).await.unwrap(),
        "the plan and the readiness commit together"
    );
    assert_eq!(
        store.queue_status(&run.id).await.unwrap().unwrap().state,
        QueueState::Queued
    );
    assert!(store.activate_queue_entry(&run.id).await.unwrap());
    assert!(
        !store.ready_queue_entry(&run.id).await.unwrap(),
        "readiness is a one-way transition"
    );
}

#[tokio::test]
async fn abandoning_an_interrupted_preparation_fails_the_run_and_frees_the_head() {
    let (store, _dir) = open_store().await;
    let (run, goal) = queued_run(OffsetDateTime::now_utc());
    store.create_run_queued(&run, &goal, None).await.unwrap();
    let next = enqueue(&store, OffsetDateTime::now_utc()).await;

    assert!(
        store
            .abandon_preparation(&run.id, "start request did not complete")
            .await
            .unwrap()
    );
    let failed = store.get_run(&run.id).await.unwrap();
    assert_eq!(failed.state, RunState::Failed, "never executes");
    assert_eq!(failed.stop_condition, Some(StopCondition::StartIncomplete));
    assert_eq!(store.queue_status(&run.id).await.unwrap(), None);
    assert_eq!(store.queue_head().await.unwrap().unwrap().run_id, next);
    let decisions = store.list_decisions(&run.id).await.unwrap();
    assert!(
        decisions
            .iter()
            .any(|d| d.kind == DecisionKind::Stop && d.reason.contains("did not complete")),
        "the abandonment is on record: {decisions:?}"
    );
    assert!(
        !store.abandon_preparation(&run.id, "again").await.unwrap(),
        "abandoning twice changes nothing"
    );
    assert!(
        !store.abandon_preparation(&next, "ready run").await.unwrap(),
        "a prepared run is never abandoned"
    );
    assert_eq!(store.get_run(&next).await.unwrap().state, RunState::Queued);
}

#[tokio::test]
async fn a_blocked_goal_is_created_blocked_with_no_queue_place() {
    let (store, _dir) = open_store().await;
    let (mut run, goal) = queued_run(OffsetDateTime::now_utc());
    run.state = RunState::Blocked;
    run.stop_condition = Some(StopCondition::SafetyBoundary);
    let decision = Decision {
        id: DecisionId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run.id.clone(),
        kind: DecisionKind::SafetyBlock,
        actor: Actor::System,
        reason: "blocked goal".into(),
        referenced: vec![],
        payload: serde_json::json!({}),
        created_at: OffsetDateTime::now_utc(),
    };
    store
        .create_run_blocked(&run, &goal, None, &decision)
        .await
        .unwrap();
    assert_eq!(
        store.get_run(&run.id).await.unwrap().state,
        RunState::Blocked
    );
    assert_eq!(store.queue_status(&run.id).await.unwrap(), None);
    assert!(store.list_queue().await.unwrap().is_empty());
    assert_eq!(store.list_decisions(&run.id).await.unwrap().len(), 1);
    assert!(
        !store.activate_queue_entry(&run.id).await.unwrap(),
        "a blocked run can never be activated"
    );
}

fn pending_task(run_id: &RunId, goal: &Goal, plan: &Plan) -> Task {
    Task {
        id: TaskId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        role: Role::Reflection,
        strategy: "initial".into(),
        goal_id: goal.id.clone(),
        plan_id: plan.id.clone(),
        input_refs: vec![],
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
        permissions: goal.permissions.clone(),
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
        created_at: OffsetDateTime::now_utc(),
        started_at: None,
        finished_at: None,
    }
}

#[tokio::test]
async fn stopping_a_run_cancels_it_its_tasks_and_records_the_decision_together() {
    let (store, _dir) = open_store().await;
    let (run, goal) = queued_run(OffsetDateTime::now_utc());
    store.create_run_queued(&run, &goal, None).await.unwrap();
    let plan = uruk::agents::supervisor::build_plan(&goal, 0);
    assert!(store.insert_plan_and_ready(&plan).await.unwrap());
    assert!(store.activate_queue_entry(&run.id).await.unwrap());
    let mut ids = Vec::new();
    for _ in 0..3 {
        let task = pending_task(&run.id, &goal, &plan);
        store.enqueue_task(&task, None).await.unwrap();
        store
            .reserve_budget(&run.id, &task.id, &task.reservation, &goal.budget, false)
            .await
            .unwrap();
        ids.push(task.id);
    }
    assert_eq!(store.budget_usage(&run.id).await.unwrap().reserved_calls, 3);

    let outcome = uruk::runtime::request_stop(&store, &run.id).await.unwrap();
    assert_eq!(outcome.cancelled_tasks, 3);

    let stopped = store.get_run(&run.id).await.unwrap();
    assert_eq!(stopped.state, RunState::Cancelled);
    assert_eq!(stopped.stop_condition, Some(StopCondition::Cancelled));
    assert_eq!(store.queue_status(&run.id).await.unwrap(), None);
    for id in &ids {
        assert_eq!(
            store.get_task(id).await.unwrap().state,
            TaskState::Cancelled
        );
    }
    assert_eq!(
        store.budget_usage(&run.id).await.unwrap().reserved_calls,
        0,
        "every reservation is released with the stop"
    );
    let decisions = store.list_decisions(&run.id).await.unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].id, outcome.decision_id);
    assert_eq!(decisions[0].payload["cancelled_tasks"], 3);

    // A retried stop tells the truth: nothing left to stop.
    let err = uruk::runtime::request_stop(&store, &run.id)
        .await
        .unwrap_err();
    assert!(matches!(err, uruk::Error::Validation(_)), "{err}");
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert_eq!(
        store.list_decisions(&run.id).await.unwrap().len(),
        1,
        "a refused retry records nothing"
    );
}

#[tokio::test]
async fn running_tasks_orphaned_by_a_crash_after_a_stop_are_settled() {
    let (store, _dir) = open_store().await;
    let (run, goal) = queued_run(OffsetDateTime::now_utc());
    store.create_run_queued(&run, &goal, None).await.unwrap();
    let plan = uruk::agents::supervisor::build_plan(&goal, 0);
    assert!(store.insert_plan_and_ready(&plan).await.unwrap());
    assert!(store.activate_queue_entry(&run.id).await.unwrap());
    let task = pending_task(&run.id, &goal, &plan);
    store.enqueue_task(&task, None).await.unwrap();
    store
        .reserve_budget(&run.id, &task.id, &task.reservation, &goal.budget, false)
        .await
        .unwrap();
    assert!(store.claim_task(&task.id).await.unwrap());
    // One model call had been started when the process died.
    assert!(
        store
            .lease_model_call(&run.id, &task.id, 1, 1, 10, true)
            .await
            .unwrap()
    );
    uruk::runtime::request_stop(&store, &run.id).await.unwrap();
    assert_eq!(
        store.get_task(&task.id).await.unwrap().state,
        TaskState::Running,
        "a live scheduler would settle it; here the process is gone"
    );

    let settled = store.settle_orphaned_tasks(&run.id).await.unwrap();
    assert_eq!(settled, 1);
    let task = store.get_task(&task.id).await.unwrap();
    assert_eq!(task.state, TaskState::Cancelled, "{task:?}");
    assert!(
        task.error
            .as_deref()
            .is_some_and(|e| e.contains("1 model call")),
        "the conservative charge is on record: {task:?}"
    );
    let usage = store.budget_usage(&run.id).await.unwrap();
    assert_eq!(
        usage.model_calls, 1,
        "the started call is charged: {usage:?}"
    );
    assert!(usage.cost_partially_unknown);
    assert_eq!(usage.reserved_calls, 0, "nothing stays reserved: {usage:?}");
    assert_eq!(
        store.settle_orphaned_tasks(&run.id).await.unwrap(),
        0,
        "settling is idempotent"
    );
}
