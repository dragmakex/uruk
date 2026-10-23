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
use uruk::store::{QueueContext, QueueState, Store};

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
    let head = store.fair_head().await.unwrap().expect("a head");
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

    let entry = store.fair_head().await.unwrap().expect("head");
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
    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, head);
}

#[tokio::test]
async fn cancelling_a_queued_run_advances_the_head_in_the_same_transaction() {
    let (store, _dir) = open_store().await;
    let first = enqueue(&store, OffsetDateTime::now_utc()).await;
    let second = enqueue(&store, OffsetDateTime::now_utc()).await;
    let third = enqueue(&store, OffsetDateTime::now_utc()).await;

    assert!(store.try_cancel_run(&first).await.unwrap());

    let head = store.fair_head().await.unwrap().expect("head");
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

    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, next);
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

    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, next);
    let entries = store.list_queue().await.unwrap();
    assert!(
        entries.iter().all(|e| e.run_id != head),
        "a finished run is not in the live queue: {entries:?}"
    );
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
    let entry = store.fair_head().await.unwrap().expect("the head survives");
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
    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, second);
    assert!(store.activate_queue_entry(&second).await.unwrap());
    store.finish_queue_entry(&second).await.unwrap();
    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, third);
}

#[tokio::test]
async fn provider_wait_timing_survives_reopen() {
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
        // Its goal revision, as the pre-queue server stored it.
        let (_, mut goal) = queued_run(OffsetDateTime::now_utc());
        goal.id = GoalId::from_raw(format!("goal_{id}"));
        goal.run_id = RunId::from_raw(id.to_string());
        sqlx::query(
            "INSERT INTO goals (id, run_id, revision, body, created_at) VALUES (?, ?, 0, ?, ?)",
        )
        .bind(goal.id.as_str())
        .bind(id)
        .bind(serde_json::to_string(&goal).unwrap())
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
    let entries = store.list_queue().await.unwrap();
    assert!(
        entries.iter().all(|e| e.owner == uruk::store::ENGINE_OWNER),
        "ownerless legacy runs belong to the engine owner: {entries:?}"
    );
    assert_eq!(
        store.queue_turn().await.unwrap(),
        0,
        "no owner has been served"
    );
    // Migrated runs are `running`, not `queued`: their position still
    // comes from their live entry.
    let overviews = store.list_run_overviews().await.unwrap();
    let position = |id: &str| {
        overviews
            .iter()
            .find(|o| o.id.as_str() == id)
            .unwrap()
            .queue_position
    };
    assert_eq!(
        position("run_old_a"),
        Some(1),
        "a migrated running run shows its place on the queue"
    );
    assert_eq!(position("run_old_b"), Some(2));
    assert_eq!(position("run_old_done"), None);
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
    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, next);
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
        reservation: CostReservation { model_calls: 1 },
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

// --- round-robin fairness between owners (docs/WEB.md "Taking turns") ---

fn owner(name: char) -> uruk::store::OwnerDigest {
    uruk::store::OwnerDigest::from_token(&name.to_string().repeat(64))
}

/// Create and fully prepare a web run owned by `who`.
async fn enqueue_for(store: &Store, who: char) -> RunId {
    let (run, goal) = queued_run(OffsetDateTime::now_utc());
    store
        .create_run_queued(&run, &goal, Some(&owner(who)))
        .await
        .unwrap();
    assert!(store.ready_queue_entry(&run.id).await.unwrap());
    run.id
}

/// The projected execution order, as run ids.
async fn projected(store: &Store) -> Vec<RunId> {
    store
        .list_queue()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.run_id)
        .collect()
}

/// Activate the fair head, finish it, and return it.
async fn serve_next(store: &Store) -> RunId {
    let head = store
        .fair_head()
        .await
        .unwrap()
        .expect("something to serve");
    assert!(
        store.activate_queue_entry(&head.run_id).await.unwrap(),
        "the fair head is activatable: {head:?}"
    );
    assert!(
        store
            .finish_run(
                &head.run_id,
                RunState::Completed,
                StopCondition::WorkExhausted
            )
            .await
            .unwrap()
    );
    head.run_id
}

#[tokio::test]
async fn owners_take_turns_while_each_owner_stays_fifo() {
    let (store, _dir) = open_store().await;
    let a1 = enqueue_for(&store, 'a').await;
    let a2 = enqueue_for(&store, 'a').await;
    let a3 = enqueue_for(&store, 'a').await;
    let b1 = enqueue_for(&store, 'b').await;
    let c1 = enqueue_for(&store, 'c').await;
    let b2 = enqueue_for(&store, 'b').await;

    let expected = vec![
        a1.clone(),
        b1.clone(),
        c1.clone(),
        a2.clone(),
        b2.clone(),
        a3.clone(),
    ];
    assert_eq!(
        projected(&store).await,
        expected,
        "one run per owner per cycle"
    );

    let mut served = Vec::new();
    for _ in 0..6 {
        served.push(serve_next(&store).await);
    }
    assert_eq!(served, expected);
    assert!(store.list_queue().await.unwrap().is_empty());
}

#[tokio::test]
async fn fifo_holds_within_one_owner() {
    let (store, _dir) = open_store().await;
    let a1 = enqueue_for(&store, 'a').await;
    let a2 = enqueue_for(&store, 'a').await;
    assert!(
        !store.activate_queue_entry(&a2).await.unwrap(),
        "a later run of the same owner never overtakes an earlier one"
    );
    assert!(store.activate_queue_entry(&a1).await.unwrap());
    assert_eq!(projected(&store).await, vec![a1, a2]);
}

#[tokio::test]
async fn the_turn_cursor_survives_reopen_and_never_resets_to_the_heavy_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".uruk/state.sqlite");
    let (a2, a3, b1) = {
        let store = Store::open(&path).await.unwrap();
        store
            .ensure_project(&ProjectId::from_raw("prj_queue"), "queue")
            .await
            .unwrap();
        let a1 = enqueue_for(&store, 'a').await;
        let a2 = enqueue_for(&store, 'a').await;
        let a3 = enqueue_for(&store, 'a').await;
        let b1 = enqueue_for(&store, 'b').await;
        assert_eq!(serve_next(&store).await, a1);
        assert_eq!(store.queue_turn().await.unwrap(), 1);
        store.close().await;
        (a2, a3, b1)
    };

    // --- restart ---
    let store = Store::open(&path).await.unwrap();
    assert_eq!(
        store.queue_turn().await.unwrap(),
        1,
        "the turn is persisted"
    );
    assert_eq!(
        store.fair_head().await.unwrap().unwrap().run_id,
        b1,
        "b has never been served: its turn comes before a's second run"
    );
    assert_eq!(serve_next(&store).await, b1);
    // A newcomer joining now gets a baseline level with a's last turn; a
    // has been waiting since, so a2 runs first, then the newcomer.
    let c1 = enqueue_for(&store, 'c').await;
    assert_eq!(
        projected(&store).await,
        vec![a2.clone(), c1.clone(), a3.clone()]
    );
    assert_eq!(serve_next(&store).await, a2);
    assert_eq!(serve_next(&store).await, c1);
    assert_eq!(store.queue_turn().await.unwrap(), 4);
}

#[tokio::test]
async fn a_newcomer_never_preempts_the_active_run() {
    let (store, _dir) = open_store().await;
    let a1 = enqueue_for(&store, 'a').await;
    let a2 = enqueue_for(&store, 'a').await;
    assert!(store.activate_queue_entry(&a1).await.unwrap());
    let b1 = enqueue_for(&store, 'b').await;
    assert_eq!(
        projected(&store).await,
        vec![a1.clone(), b1.clone(), a2.clone()],
        "the active run stays first; the newcomer takes the next turn"
    );
    assert!(!store.activate_queue_entry(&b1).await.unwrap());
    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, a1);
    assert_eq!(store.queue_status(&b1).await.unwrap().unwrap().ahead, 1);
    assert_eq!(store.queue_status(&a2).await.unwrap().unwrap().ahead, 2);
}

#[tokio::test]
async fn an_interrupted_active_run_resumes_first_after_restart_whatever_the_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".uruk/state.sqlite");
    let (a1, b1) = {
        let store = Store::open(&path).await.unwrap();
        store
            .ensure_project(&ProjectId::from_raw("prj_queue"), "queue")
            .await
            .unwrap();
        let a1 = enqueue_for(&store, 'a').await;
        let b1 = enqueue_for(&store, 'b').await;
        assert!(store.activate_queue_entry(&a1).await.unwrap());
        store.close().await;
        (a1, b1)
    };
    let store = Store::open(&path).await.unwrap();
    let head = store.fair_head().await.unwrap().unwrap();
    assert_eq!(head.run_id, a1);
    assert_eq!(head.state, QueueState::Active);
    assert!(
        !store.activate_queue_entry(&b1).await.unwrap(),
        "nothing starts while the interrupted run is still active"
    );
    assert_eq!(projected(&store).await[0], a1);
}

#[tokio::test]
async fn a_preparing_entry_holds_its_place_without_blocking_other_owners() {
    let (store, _dir) = open_store().await;
    let (a1_run, a1_goal) = queued_run(OffsetDateTime::now_utc());
    store
        .create_run_queued(&a1_run, &a1_goal, Some(&owner('a')))
        .await
        .unwrap();
    let a1 = a1_run.id.clone();
    let a2 = enqueue_for(&store, 'a').await;
    let b1 = enqueue_for(&store, 'b').await;

    assert_eq!(
        projected(&store).await,
        vec![b1.clone(), a1.clone(), a2.clone()],
        "the projection starts with what can run now; the unprepared owner follows"
    );
    assert_eq!(
        store.fair_head().await.unwrap().unwrap().run_id,
        b1,
        "an owner whose oldest run is not ready is skipped, not waited for"
    );
    assert!(
        !store.activate_queue_entry(&a2).await.unwrap(),
        "the owner's later run does not overtake its unprepared earlier one"
    );
    assert!(!store.activate_queue_entry(&a1).await.unwrap());
    assert!(store.activate_queue_entry(&b1).await.unwrap());

    // Once prepared, it takes its place at the next selection.
    let plan = uruk::agents::supervisor::build_plan(&a1_goal, 0);
    assert!(store.insert_plan_and_ready(&plan).await.unwrap());
    store.finish_queue_entry(&b1).await.unwrap();
    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, a1);
}

#[tokio::test]
async fn cancelling_a_preparing_run_frees_its_owner_turn() {
    let (store, _dir) = open_store().await;
    let (a1_run, a1_goal) = queued_run(OffsetDateTime::now_utc());
    store
        .create_run_queued(&a1_run, &a1_goal, Some(&owner('a')))
        .await
        .unwrap();
    let a1 = a1_run.id.clone();
    let a2 = enqueue_for(&store, 'a').await;
    let b1 = enqueue_for(&store, 'b').await;

    assert!(store.try_cancel_run(&a1).await.unwrap());
    assert_eq!(store.queue_status(&a1).await.unwrap(), None);
    assert_eq!(
        projected(&store).await,
        vec![a2.clone(), b1.clone()],
        "neither owner has been served: a's oldest live run was enqueued first"
    );
    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, a2);
    assert_eq!(store.queue_status(&b1).await.unwrap().unwrap().ahead, 1);
}

#[tokio::test]
async fn queue_positions_follow_the_fair_order_not_the_insertion_sequence() {
    let (store, _dir) = open_store().await;
    let a1 = enqueue_for(&store, 'a').await;
    let a2 = enqueue_for(&store, 'a').await;
    let a3 = enqueue_for(&store, 'a').await;
    let b1 = enqueue_for(&store, 'b').await;
    let c1 = enqueue_for(&store, 'c').await;
    let b2 = enqueue_for(&store, 'b').await;
    for (run_id, ahead) in [(&a1, 0), (&b1, 1), (&c1, 2), (&a2, 3), (&b2, 4), (&a3, 5)] {
        assert_eq!(
            store.queue_status(run_id).await.unwrap().unwrap().ahead,
            ahead,
            "{run_id}"
        );
    }
    let overviews = store.list_run_overviews().await.unwrap();
    let position = |id: &RunId| {
        overviews
            .iter()
            .find(|o| &o.id == id)
            .unwrap()
            .queue_position
            .unwrap()
    };
    assert_eq!(position(&a2), 4, "the listing projects the fair order");
    assert_eq!(position(&b2), 5);
    assert_eq!(position(&a3), 6);
}

#[tokio::test]
async fn ownerless_runs_share_one_turn_as_the_engine_owner() {
    let (store, _dir) = open_store().await;
    let e1 = enqueue(&store, OffsetDateTime::now_utc()).await;
    let e2 = enqueue(&store, OffsetDateTime::now_utc()).await;
    let a1 = enqueue_for(&store, 'a').await;
    let entries = store.list_queue().await.unwrap();
    assert_eq!(entries[0].owner, uruk::store::ENGINE_OWNER);
    assert_eq!(
        projected(&store).await,
        vec![e1.clone(), a1.clone(), e2.clone()],
        "engine runs take turns as one owner, FIFO among themselves"
    );
    assert_eq!(serve_next(&store).await, e1);
    assert_eq!(serve_next(&store).await, a1);
    assert_eq!(serve_next(&store).await, e2);
}

// --- least-recently-served: the required regressions ---

#[tokio::test]
async fn an_owner_with_no_live_run_does_not_run_twice_while_another_owner_waits() {
    let (store, _dir) = open_store().await;
    let b1 = enqueue_for(&store, 'b').await;
    let a1 = enqueue_for(&store, 'a').await;
    assert_eq!(serve_next(&store).await, b1);
    assert_eq!(serve_next(&store).await, a1);
    // a has nothing live; a2 arrives before b2 is enqueued.
    let a2 = enqueue_for(&store, 'a').await;
    let b2 = enqueue_for(&store, 'b').await;
    assert_eq!(
        store.fair_head().await.unwrap().unwrap().run_id,
        b2,
        "b was served longer ago than a: b's turn, not a's again"
    );
    assert_eq!(projected(&store).await, vec![b2.clone(), a2.clone()]);
    assert_eq!(serve_next(&store).await, b2);
    assert_eq!(serve_next(&store).await, a2);
}

#[tokio::test]
async fn an_owner_whose_run_was_preparing_gets_one_turn_not_a_catch_up_burst() {
    let (store, _dir) = open_store().await;
    let (a1_run, a1_goal) = queued_run(OffsetDateTime::now_utc());
    store
        .create_run_queued(&a1_run, &a1_goal, Some(&owner('a')))
        .await
        .unwrap();
    let a1 = a1_run.id.clone();
    let b1 = enqueue_for(&store, 'b').await;
    let b2 = enqueue_for(&store, 'b').await;
    let b3 = enqueue_for(&store, 'b').await;
    let a2 = enqueue_for(&store, 'a').await;
    // b runs three times while a's oldest run is still being prepared.
    assert_eq!(serve_next(&store).await, b1);
    assert_eq!(serve_next(&store).await, b2);
    assert_eq!(serve_next(&store).await, b3);
    let b4 = enqueue_for(&store, 'b').await;
    let plan = uruk::agents::supervisor::build_plan(&a1_goal, 0);
    assert!(store.insert_plan_and_ready(&plan).await.unwrap());

    assert_eq!(
        projected(&store).await,
        vec![a1.clone(), b4.clone(), a2.clone()],
        "a's turn, then b's, then a's: never a1 and a2 back to back"
    );
    assert_eq!(serve_next(&store).await, a1);
    assert_eq!(serve_next(&store).await, b4);
    assert_eq!(serve_next(&store).await, a2);
}

#[tokio::test]
async fn position_one_is_always_the_selectable_next_run() {
    let (store, _dir) = open_store().await;
    let (a1_run, a1_goal) = queued_run(OffsetDateTime::now_utc());
    store
        .create_run_queued(&a1_run, &a1_goal, Some(&owner('a')))
        .await
        .unwrap();
    let a1 = a1_run.id.clone();
    let b1 = enqueue_for(&store, 'b').await;

    // b1 is what would run now; a1 cannot, so it is not ahead of b1.
    assert_eq!(store.queue_status(&b1).await.unwrap().unwrap().ahead, 0);
    assert_eq!(store.queue_status(&a1).await.unwrap().unwrap().ahead, 1);
    let overviews = store.list_run_overviews().await.unwrap();
    let position = |id: &RunId| {
        overviews
            .iter()
            .find(|o| &o.id == id)
            .unwrap()
            .queue_position
            .unwrap()
    };
    assert_eq!(position(&b1), 1);
    assert_eq!(position(&a1), 2);

    // A newcomer joins behind the selectable run.
    let c1 = enqueue_for(&store, 'c').await;
    assert_eq!(
        projected(&store).await,
        vec![b1.clone(), c1.clone(), a1.clone()]
    );

    // Preparation completing changes the projection: a has never been
    // served and its run was enqueued first, so it is now next.
    let plan = uruk::agents::supervisor::build_plan(&a1_goal, 0);
    assert!(store.insert_plan_and_ready(&plan).await.unwrap());
    assert_eq!(
        projected(&store).await,
        vec![a1.clone(), b1.clone(), c1.clone()]
    );
    assert_eq!(store.queue_status(&a1).await.unwrap().unwrap().ahead, 0);

    // A parked engine run re-enqueued by the sweep joins as the engine
    // owner, behind the owners already waiting.
    let parked = enqueue(&store, OffsetDateTime::now_utc()).await;
    store.finish_queue_entry(&parked).await.unwrap();
    assert!(store.enqueue_run(&parked).await.unwrap());
    assert_eq!(
        projected(&store).await,
        vec![a1.clone(), b1.clone(), c1.clone(), parked.clone()]
    );
    assert_eq!(store.queue_status(&parked).await.unwrap().unwrap().ahead, 3);
}

// --- newcomer baselines: rotating identities cannot starve a waiting owner ---

#[tokio::test]
async fn a_newcomer_takes_the_next_turn_but_a_later_newcomer_waits_behind_an_older_owner() {
    let (store, _dir) = open_store().await;
    let a1 = enqueue_for(&store, 'a').await;
    let a2 = enqueue_for(&store, 'a').await;
    assert_eq!(serve_next(&store).await, a1);

    // b has never been seen: it goes next, ahead of the just-served a.
    let b1 = enqueue_for(&store, 'b').await;
    assert_eq!(
        store.owner_last_served(&owner('b')).await.unwrap(),
        Some(0),
        "a newcomer's baseline is persisted at enqueue: one turn behind the turn just served"
    );
    assert_eq!(store.fair_head().await.unwrap().unwrap().run_id, b1);
    assert_eq!(serve_next(&store).await, b1);

    // c is also new, but a has now waited a full turn: a2 runs before c1.
    let c1 = enqueue_for(&store, 'c').await;
    assert_eq!(store.owner_last_served(&owner('c')).await.unwrap(), Some(1));
    assert_eq!(projected(&store).await, vec![a2.clone(), c1.clone()]);
    assert_eq!(serve_next(&store).await, a2);
    assert_eq!(serve_next(&store).await, c1);
}

#[tokio::test]
async fn a_stream_of_fresh_identities_cannot_starve_a_waiting_owner() {
    let (store, _dir) = open_store().await;
    let a1 = enqueue_for(&store, 'a').await;
    let a2 = enqueue_for(&store, 'a').await;
    assert_eq!(serve_next(&store).await, a1);

    // Every served run is followed by a brand-new identity's first run.
    let mut newcomers_served_before_a2 = 0;
    for name in ['n', 'o', 'p', 'q', 'r'] {
        let fresh = enqueue_for(&store, name).await;
        let served = serve_next(&store).await;
        if served == a2 {
            break;
        }
        assert_eq!(served, fresh);
        newcomers_served_before_a2 += 1;
    }
    assert_eq!(
        newcomers_served_before_a2, 1,
        "exactly one newcomer may jump ahead; the next one waits behind a2"
    );
    assert_eq!(
        store.get_run(&a2).await.unwrap().state,
        RunState::Completed,
        "a2 was served"
    );
}

#[tokio::test]
async fn returning_owners_keep_their_durable_last_served_turn() {
    let (store, _dir) = open_store().await;
    let a1 = enqueue_for(&store, 'a').await;
    let b1 = enqueue_for(&store, 'b').await;
    assert_eq!(serve_next(&store).await, a1);
    assert_eq!(serve_next(&store).await, b1);
    assert_eq!(store.owner_last_served(&owner('a')).await.unwrap(), Some(1));
    // a returns after both were served: its own history, not a fresh
    // baseline, decides its place.
    let a2 = enqueue_for(&store, 'a').await;
    assert_eq!(store.owner_last_served(&owner('a')).await.unwrap(), Some(1));
    let b2 = enqueue_for(&store, 'b').await;
    assert_eq!(projected(&store).await, vec![a2, b2]);
}

#[tokio::test]
async fn first_owners_at_turn_zero_still_tie_by_start_order() {
    let (store, _dir) = open_store().await;
    let b1 = enqueue_for(&store, 'b').await;
    let a1 = enqueue_for(&store, 'a').await;
    let c1 = enqueue_for(&store, 'c').await;
    for who in ['a', 'b', 'c'] {
        assert_eq!(store.owner_last_served(&owner(who)).await.unwrap(), Some(0));
    }
    assert_eq!(projected(&store).await, vec![b1, a1, c1]);
}

// --- positions come from the live entry, not the run's lifecycle state ---

#[tokio::test]
async fn the_queue_context_reads_position_and_the_heads_wait_as_one_snapshot() {
    let (store, _dir) = open_store().await;
    let now = OffsetDateTime::now_utc();
    let head = enqueue(&store, now).await;
    let follower = enqueue(&store, now).await;
    let stranger = {
        let (run, goal) = queued_run(now);
        store.create_run(&run, &goal).await.unwrap();
        run.id
    };

    // Nothing active yet: both facts come back together, the wait false.
    let ctx = store.queue_context(&follower).await.unwrap();
    assert_eq!(
        ctx,
        QueueContext {
            status: Some(uruk::store::QueueStatus {
                state: QueueState::Queued,
                ahead: 1
            }),
            active_provider_waiting: false,
        }
    );
    assert_eq!(
        store.queue_context(&stranger).await.unwrap(),
        QueueContext {
            status: None,
            active_provider_waiting: false
        },
        "a run with no live entry is not on the queue and learns nothing"
    );

    assert!(store.activate_queue_entry(&head).await.unwrap());
    let started = store.run_timing(&head).await.unwrap().started_at.unwrap();
    let since = started + time::Duration::seconds(5);
    store
        .begin_provider_wait(&head, since, "HTTP 503")
        .await
        .unwrap();

    let ctx = store.queue_context(&follower).await.unwrap();
    assert_eq!(
        ctx.status.map(|s| (s.state, s.ahead)),
        Some((QueueState::Queued, 1))
    );
    assert!(
        ctx.active_provider_waiting,
        "the follower sees the head's wait"
    );
    let ctx = store.queue_context(&head).await.unwrap();
    assert_eq!(ctx.status.map(|s| s.state), Some(QueueState::Active));
    assert!(
        !ctx.active_provider_waiting,
        "the head's own wait is its `provider_wait`, not the queue's"
    );
    assert!(
        !store
            .queue_context(&stranger)
            .await
            .unwrap()
            .active_provider_waiting,
        "only a run on the queue is told about the head"
    );

    assert!(
        store
            .end_provider_wait(&head, since + time::Duration::seconds(1))
            .await
            .unwrap()
    );
    let ctx = store.queue_context(&follower).await.unwrap();
    assert_eq!(ctx.status.map(|s| s.ahead), Some(1));
    assert!(!ctx.active_provider_waiting, "recovery clears the bit");

    // A stale open wait on a run that left the queue is not the queue's.
    store
        .begin_provider_wait(&head, since, "HTTP 503")
        .await
        .unwrap();
    store.finish_queue_entry(&head).await.unwrap();
    let ctx = store.queue_context(&follower).await.unwrap();
    assert_eq!(
        ctx.status.map(|s| s.ahead),
        Some(0),
        "the follower moved up"
    );
    assert!(!ctx.active_provider_waiting, "nothing is active");
}
