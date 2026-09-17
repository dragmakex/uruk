//! The six runtime checks required before live model calls (SPEC §9.3).

mod common;

use common::*;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uruk::agents::{generation, ranking, reflection, supervisor};
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::report;
use uruk::runtime::{ConcurrencyLimits, Scheduler, SchedulerConfig, execute_task};
use uruk::store::{RecordBatch, Store};

/// A pending task the way the scheduler would build it.
fn pending_task(f: &Fixture, role: Role, strategy: &str, inputs: Vec<String>) -> Task {
    Task {
        id: TaskId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        role,
        strategy: strategy.into(),
        goal_id: f.goal.id.clone(),
        plan_id: f.plan.id.clone(),
        input_refs: inputs,
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
        created_at: OffsetDateTime::now_utc(),
        started_at: None,
        finished_at: None,
    }
}

/// §9.3(1): concurrent reviews obey global and tool limits while unrelated
/// work progresses independently.
#[tokio::test]
async fn check_1_concurrency_limits_hold() {
    let limits = ConcurrencyLimits::new(2, 1);
    let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..6 {
        let (limits, peak, live) = (limits.clone(), peak.clone(), live.clone());
        handles.push(tokio::spawn(async move {
            let _p = limits.acquire(Role::Reflection, None).await;
            use std::sync::atomic::Ordering;
            let n = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            live.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert!(
        peak.load(std::sync::atomic::Ordering::SeqCst) <= 2,
        "global concurrency limit was exceeded"
    );

    // A held tool slot must not block unrelated model work.
    let limits = ConcurrencyLimits::new(4, 1);
    let held = limits.acquire(Role::Tool, Some("shell")).await;
    let unrelated = tokio::time::timeout(
        Duration::from_millis(200),
        limits.acquire(Role::Reflection, None),
    )
    .await;
    assert!(
        unrelated.is_ok(),
        "unrelated work was blocked by a tool task"
    );
    drop(held);
}

/// §9.3(2): completed evidence selects new tasks and bounded iterations,
/// rather than only traversing a predeclared static pipeline. The work the
/// evidence selects must actually run.
#[tokio::test]
async fn check_2_new_evidence_changes_what_is_scheduled_and_runs() {
    let provider = MockProvider::new()
        .rule(anchor::RANKING_PAIRWISE, ranking_json("win_1"))
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "reviewed again", false));
    let f = fixture(Mode::Campaign, provider).await;
    let a = seed_item(&f, "candidate A").await;
    let b = seed_item(&f, "candidate B").await;
    for item in [&a, &b] {
        seed_review(&f, item).await;
        f.store.ensure_rating(&item.id, &f.plan.id).await.unwrap();
    }
    let weights = supervisor::Weights::default();

    let before = supervisor::select_work(&f.store, &f.run_id, &f.goal, &f.plan, &weights, 10, 0)
        .await
        .unwrap();
    assert!(
        !before.iter().any(|w| w.strategy == "refresh"),
        "nothing is stale yet: {before:?}"
    );

    // Contradicting external evidence arrives for A.
    let evidence = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        kind: EvidenceKind::ExternalExperiment,
        content: "independent replication failed".into(),
        method: "third-party lab".into(),
        inputs: vec![],
        source_ids: vec![],
        artifact_ids: vec![],
        limitations: None,
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_evidence(&evidence).await.unwrap();
    f.store
        .link_evidence(&EvidenceLink {
            evidence_id: evidence.id.clone(),
            item_id: a.id.clone(),
            claim: "candidate A claim".into(),
            relation: EvidenceRelation::Contradicts,
            justification: "did not replicate".into(),
            limits: None,
        })
        .await
        .unwrap();

    // The scheduler now wants the stale ranking refreshed against a peer, and
    // the evidence consumed by a review: what to do next came from the
    // evidence, not from a fixed pipeline position.
    let after = supervisor::select_work(&f.store, &f.run_id, &f.goal, &f.plan, &weights, 10, 1)
        .await
        .unwrap();
    let refresh = after
        .iter()
        .find(|w| w.role == Role::Ranking && w.strategy == "refresh")
        .unwrap_or_else(|| panic!("contradicting evidence must schedule a refresh: {after:?}"));
    assert_eq!(
        refresh.input_refs.len(),
        2,
        "a refresh is a comparison: {refresh:?}"
    );
    assert!(refresh.input_refs.contains(&a.id.0));
    assert!(
        after.iter().any(|w| w.role == Role::Reflection
            && w.strategy == "recurrent"
            && w.input_refs == vec![a.id.0.clone()]),
        "uncited evidence must be consumed by a recurrent review: {after:?}"
    );

    // And it actually runs.
    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    let refresh_task = tasks
        .iter()
        .find(|t| t.role == Role::Ranking && t.strategy == "refresh")
        .expect("refresh task exists");
    assert_eq!(
        refresh_task.state,
        TaskState::Completed,
        "{:?} / {summary:?}",
        refresh_task.error
    );
    let rating = f
        .store
        .get_rating(&a.id, &f.plan.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!rating.stale, "the refresh must clear the stale flag");
    assert!(
        tasks
            .iter()
            .any(|t| t.strategy == "recurrent" && t.state == TaskState::Completed),
        "the recurrent review must run"
    );
}

/// §9.3(3): kill/restart at the completion boundary without duplicate
/// research records, budget settlement, or Elo updates.
#[tokio::test]
async fn check_3_restart_does_not_duplicate_records() {
    let provider = MockProvider::new()
        .rule(
            anchor::GENERATION_LITERATURE,
            hypothesis_json("Drift", "drift explains it"),
        )
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "x", false));
    let f = fixture(Mode::Campaign, provider).await;
    seed_source(&f, "a", "Study A").await;

    // The scheduler's own identity for this work in round 0.
    let task = pending_task(&f, Role::Generation, "literature", vec![]);
    let key = format!("{}:generation:literature::iter0", f.plan.id);
    f.store.enqueue_task(&task, Some(&key)).await.unwrap();
    f.store
        .reserve_budget(
            &f.run_id,
            &task.id,
            &task.reservation,
            &f.goal.budget,
            false,
        )
        .await
        .unwrap();
    assert!(f.store.claim_task(&task.id).await.unwrap());

    // The task executes and produces an item, then the process dies before
    // the outcome is committed.
    let ctx = context(&f, CancellationToken::new());
    let outcome = execute_task(&ctx, &task).await.unwrap();
    assert_eq!(outcome.state, TaskState::Completed, "{:?}", outcome.error);
    assert_eq!(outcome.batch.items.len(), 1);
    assert!(
        f.store.list_items(&f.run_id).await.unwrap().is_empty(),
        "records commit with the outcome, never before it"
    );

    // --- restart ---
    let path = f.dir.path().join(".uruk/state.sqlite");
    f.store.close().await;
    let store = Store::open(&path).await.unwrap();
    let summary = Scheduler::new(
        store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();

    let items = store.list_items(&f.run_id).await.unwrap();
    let from_task: Vec<_> = items
        .iter()
        .filter(|i| i.produced_by.as_ref() == Some(&task.id))
        .collect();
    assert_eq!(
        from_task.len(),
        1,
        "the re-run task must produce exactly one item: {summary:?}"
    );
    let resumed = store.get_task(&task.id).await.unwrap();
    assert_eq!(resumed.attempts, 2, "the task was re-run, not duplicated");
    assert_eq!(resumed.state, TaskState::Completed);

    // Duplicate delivery of a committed outcome is harmless: Elo and
    // settlement stay exactly-once.
    let a = items[0].clone();
    let b = seed_item(
        &Fixture {
            store: store.clone(),
            ..f
        },
        "b",
    )
    .await;
    let match_task = pending_task(
        &Fixture {
            store: store.clone(),
            run_id: a.run_id.clone(),
            goal: store.current_goal(&a.run_id).await.unwrap(),
            plan: store.current_plan(&a.run_id).await.unwrap().unwrap(),
            provider: Arc::new(MockProvider::new()),
            dir: tempfile::tempdir().unwrap(),
        },
        Role::Ranking,
        "pairwise",
        vec![a.id.0.clone(), b.id.0.clone()],
    );
    store.enqueue_task(&match_task, None).await.unwrap();
    store.claim_task(&match_task.id).await.unwrap();
    let plan = store.current_plan(&a.run_id).await.unwrap().unwrap();
    let m = Match {
        id: MatchId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: a.run_id.clone(),
        item_a: a.id.clone(),
        item_b: b.id.clone(),
        rubric_id: plan.id.clone(),
        evidence_snapshot: ContentHash::of_str("snap"),
        order_randomized: false,
        method: MatchMethod::Pairwise,
        judge_model: "mock".into(),
        outcome: MatchOutcome::WinA,
        rationale: "a is better".into(),
        rating_a_before: 0.0,
        rating_b_before: 0.0,
        rating_a_after: 0.0,
        rating_b_after: 0.0,
        produced_by: Some(match_task.id.clone()),
        created_at: OffsetDateTime::now_utc(),
    };
    let mut batch = RecordBatch::default();
    batch.ratings.push((a.id.clone(), plan.id.clone()));
    batch.ratings.push((b.id.clone(), plan.id.clone()));
    batch.matches.push((m, "match-key-1".into()));
    let spent = CostActual {
        model_calls: 1,
        prompt_tokens: 50,
        completion_tokens: 10,
        ..Default::default()
    };
    let before = store.budget_usage(&a.run_id).await.unwrap();
    store
        .commit_task(
            &match_task.id,
            TaskState::Completed,
            &batch,
            spent.clone(),
            None,
        )
        .await
        .unwrap();
    let rating_once = store.get_rating(&a.id, &plan.id).await.unwrap().unwrap();
    store
        .commit_task(&match_task.id, TaskState::Completed, &batch, spent, None)
        .await
        .unwrap();
    let rating_twice = store.get_rating(&a.id, &plan.id).await.unwrap().unwrap();
    assert_eq!(
        rating_once.rating, rating_twice.rating,
        "Elo must not move on replay"
    );
    assert_eq!(rating_twice.matches_played, 1);
    let after = store.budget_usage(&a.run_id).await.unwrap();
    assert_eq!(
        after.model_calls,
        before.model_calls + 1,
        "settlement exactly once"
    );
}

/// §9.3(3), continued: an interrupted external effect is surfaced as uncertain
/// rather than blindly retried.
#[tokio::test]
async fn check_3b_uncertain_external_effects_are_surfaced() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let task = pending_task(&f, Role::Tool, "exec", vec![]);
    f.store.enqueue_task(&task, None).await.unwrap();
    f.store.claim_task(&task.id).await.unwrap();

    let uncertain = f.store.reconcile_interrupted(&f.run_id).await.unwrap();
    assert_eq!(uncertain, vec![task.id.clone()]);
    assert_eq!(
        f.store.get_task(&task.id).await.unwrap().state,
        TaskState::Uncertain
    );

    // An uncertain task is not eligible to run again on its own.
    let ready = f.store.ready_tasks(&f.run_id, 10).await.unwrap();
    assert!(
        !ready.iter().any(|t| t.id == task.id),
        "an uncertain external effect must not be silently retried"
    );
}

/// §9.3(3), continued: a second process writing to the same store (as
/// `uruk approve` does during a run) queues rather than failing.
#[tokio::test]
async fn check_3c_cross_process_writers_queue() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let other = Store::open(f.dir.path().join(".uruk/state.sqlite"))
        .await
        .unwrap();

    let mut ids = Vec::new();
    for _ in 0..60 {
        let t = pending_task(&f, Role::Reflection, "initial", vec![]);
        f.store.enqueue_task(&t, None).await.unwrap();
        ids.push(t.id);
    }
    let mut handles = Vec::new();
    for (i, id) in ids.into_iter().enumerate() {
        let store = if i % 2 == 0 {
            f.store.clone()
        } else {
            other.clone()
        };
        handles.push(tokio::spawn(async move { store.claim_task(&id).await }));
    }
    for h in handles {
        assert!(
            h.await.unwrap().unwrap(),
            "a cross-process write must not fail"
        );
    }
}

/// §9.3(4): approval waits survive restart, accept only the pending version,
/// and respect denial.
#[tokio::test]
async fn check_4_approval_waits_survive_restart() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let path = f.dir.path().join(".uruk/state.sqlite");

    let mut task = pending_task(&f, Role::Tool, "exec", vec![]);
    task.state = TaskState::WaitingForHuman;
    f.store.enqueue_task(&task, None).await.unwrap();

    let payload = serde_json::json!({"program": "python", "args": ["analyze.py"]});
    let req = ApprovalRequest {
        id: RequestId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        task_id: Some(task.id.clone()),
        action: "execute analyze.py".into(),
        payload: payload.clone(),
        payload_hash: ContentHash::of_json(&payload).unwrap(),
        state: ApprovalState::Pending,
        decided_by: None,
        decided_reason: None,
        created_at: OffsetDateTime::now_utc(),
        decided_at: None,
    };
    f.store.insert_approval(&req).await.unwrap();
    f.store.close().await;

    // --- restart: the wait is a persisted record, not a live thread ---
    let store = Store::open(&path).await.unwrap();
    let pending = store
        .list_approvals(&f.run_id, Some(ApprovalState::Pending))
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "the approval wait must survive restart");

    // A changed payload cannot reuse this approval.
    let changed =
        ContentHash::of_json(&serde_json::json!({"program": "sh", "args": ["-c"]})).unwrap();
    assert!(
        store
            .decide_approval(&req.id, true, Some(&changed), None)
            .await
            .is_err(),
        "approval must bind to the exact pending payload"
    );

    // Denial is respected: the task is cancelled, not run.
    store
        .decide_approval(&req.id, false, Some(&req.payload_hash), Some("no".into()))
        .await
        .unwrap();
    assert_eq!(
        store.get_task(&task.id).await.unwrap().state,
        TaskState::Cancelled
    );
    assert_eq!(
        store.get_approval(&req.id).await.unwrap().state,
        ApprovalState::Denied
    );
}

/// §9.3(5): cancellation reaches child computations and owned processes,
/// retaining labelled partial outputs.
#[tokio::test]
async fn check_5_cancellation_reaches_children() {
    let f = fixture(Mode::Campaign, MockProvider::new().default_reply("{}")).await;
    let cancel = CancellationToken::new();
    let ctx = context(&f, cancel.clone());

    // Cancel before the call: the provider must never be reached.
    cancel.cancel();
    let item = seed_item(&f, "candidate").await;
    let result = reflection::review(&ctx, &item, ReviewStrategy::Initial, None).await;

    assert!(
        matches!(result, Err(uruk::Error::Cancelled)),
        "a cancelled context must not issue a model call: {result:?}"
    );
    assert_eq!(
        f.provider.call_count(),
        0,
        "cancellation must stop the request before it is sent"
    );

    // Through the executor, a cancelled task keeps its context and settles
    // what it spent.
    let task = pending_task(&f, Role::Reflection, "initial", vec![item.id.0.clone()]);
    let outcome = execute_task(&ctx, &task).await.unwrap();
    assert_eq!(outcome.state, TaskState::Cancelled);
    assert!(outcome.error.as_deref().unwrap_or("").contains("cancelled"));
    assert!(outcome.batch.items.is_empty() && outcome.batch.reviews.is_empty());
}

/// §9.3(5), continued: a cancelled subprocess tree is terminated and partial
/// output is retained with its context.
#[tokio::test]
#[cfg(unix)]
async fn check_5b_cancellation_terminates_subprocess() {
    use uruk::tools::{ExecRequest, execute_analysis};
    if !containment_available() {
        return;
    }

    let f = fixture(Mode::Task, MockProvider::new()).await;
    let cancel = CancellationToken::new();

    let permissions = Permissions {
        execute: true,
        allowed_tools: vec!["sh".into()],
        ..Default::default()
    };

    // A process that would outlive the test if it were not killed.
    let request = ExecRequest {
        program: "sh".into(),
        args: vec!["-c".into(), "sleep 30".into()],
        inputs: vec![],
        timeout_secs: 60,
        max_output_bytes: 1024,
        seed: None,
    };

    let task_id = TaskId::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        cancel_clone.cancel();
    });

    let started = std::time::Instant::now();
    let result = execute_analysis(
        &f.store,
        &f.run_id,
        &task_id,
        &request,
        &permissions,
        &cancel,
    )
    .await;

    assert!(
        matches!(result, Err(uruk::Error::Cancelled)),
        "cancellation must interrupt the execution: {result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the subprocess tree was not terminated promptly: {:?}",
        started.elapsed()
    );
}

/// A `uruk stop` from another process is observed by a running scheduler:
/// the run ends Cancelled and stays Cancelled.
#[tokio::test]
async fn stop_request_is_observed_by_a_running_scheduler() {
    let provider = MockProvider::new()
        .default_reply(review_json("plausible", "slow review", false))
        .with_delay(Duration::from_millis(400));
    let f = fixture_with(Mode::Campaign, provider, |g| g.budget.max_iterations = 20).await;
    for i in 0..4 {
        seed_item(&f, &format!("candidate {i}")).await;
    }
    let other = Store::open(f.dir.path().join(".uruk/state.sqlite"))
        .await
        .unwrap();

    let scheduler = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    );
    let run_id = f.run_id.clone();
    let handle = tokio::spawn(async move { scheduler.run(&run_id).await });

    tokio::time::sleep(Duration::from_millis(150)).await;
    // Exactly what `uruk stop` does.
    other.cancel_pending_tasks(&f.run_id).await.unwrap();
    other
        .set_run_state(
            &f.run_id,
            RunState::Cancelled,
            Some(StopCondition::Cancelled),
        )
        .await
        .unwrap();

    let summary = handle.await.unwrap().unwrap();
    assert_eq!(summary.state, RunState::Cancelled, "{summary:?}");
    let run = other.get_run(&f.run_id).await.unwrap();
    assert_eq!(
        run.state,
        RunState::Cancelled,
        "the stop must not be overwritten"
    );
    assert_eq!(run.stop_condition, Some(StopCondition::Cancelled));
    assert!(
        f.provider.call_count() < 8,
        "in-flight work must stop, not run to completion: {} calls",
        f.provider.call_count()
    );
}

/// Failed and retried work is charged for what it consumed (SPEC §6).
#[tokio::test]
async fn failed_work_is_charged_and_retried() {
    let provider = MockProvider::new()
        .fail_first(1)
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "fine", false));
    // Task mode with a review-only plan: the review is the first and only
    // call in flight, so the injected fault lands on it.
    let f = fixture_with(Mode::Task, provider, |g| {
        g.question = "assess this candidate".into();
    })
    .await;
    let item = seed_item(&f, "candidate").await;

    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();

    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    let review_task = tasks
        .iter()
        .find(|t| t.role == Role::Reflection && t.input_refs == vec![item.id.0.clone()])
        .expect("the review task exists");
    assert_eq!(
        review_task.attempts, 2,
        "a provider fault is retried: {summary:?}"
    );
    assert_eq!(review_task.state, TaskState::Completed);

    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(
        usage.model_calls,
        f.provider.call_count(),
        "every attempted call is settled, including the failed one"
    );
    assert_eq!(usage.reserved_calls, 0);
}

/// §9.3(6): a report and evidence manifest export from persisted state without
/// a provider or external workflow service, under the project directory.
#[tokio::test]
async fn check_6_report_exports_without_a_provider() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let item = seed_item(&f, "candidate").await;
    seed_source(&f, "study-a", "We observed a 23% increase.").await;

    let evidence = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        kind: EvidenceKind::ComputationalResult,
        content: "bootstrap CI excludes zero".into(),
        method: "bootstrap".into(),
        inputs: vec!["data.csv".into()],
        source_ids: vec![],
        artifact_ids: vec![],
        limitations: Some("single dataset".into()),
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_evidence(&evidence).await.unwrap();
    f.store
        .link_evidence(&EvidenceLink {
            evidence_id: evidence.id.clone(),
            item_id: item.id.clone(),
            claim: "candidate claim".into(),
            relation: EvidenceRelation::Supports,
            justification: "interval excludes the null".into(),
            limits: Some("one dataset".into()),
        })
        .await
        .unwrap();
    f.store
        .set_run_state(
            &f.run_id,
            RunState::BudgetExhausted,
            Some(StopCondition::BudgetExhausted),
        )
        .await
        .unwrap();

    // Export runs with the provider untouched.
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    assert_eq!(
        f.provider.call_count(),
        0,
        "exporting a report must not require a model call"
    );
    assert!(
        export.dir.starts_with(f.dir.path()),
        "exports live under the project directory, not the CWD: {:?}",
        export.dir
    );

    assert!(export.files.contains(&"REPORT.md".to_string()));
    assert!(export.files.contains(&"manifest.json".to_string()));
    assert!(export.files.contains(&"research.jsonl".to_string()));
    assert!(export.files.contains(&"sources.jsonl".to_string()));

    let report_text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert!(report_text.contains("bootstrap CI excludes zero"));
    assert!(
        report_text.contains("single dataset"),
        "limitations must appear"
    );
    assert!(
        report_text.contains("(budget_exhausted)"),
        "the stop condition must be reported: {report_text}"
    );

    let manifest: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(export.dir.join("manifest.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["counts"]["evidence"], 1);
    assert_eq!(manifest["counts"]["sources"], 1);
    assert_eq!(manifest["stop_condition"], "budget_exhausted");
    assert!(
        manifest["usage"]["cost_usd"].is_null(),
        "unknown cost must be null, not zero"
    );
}

/// §6: budget exhaustion preserves completed evidence and permits a clearly
/// labelled partial report without another model call.
#[tokio::test]
async fn partial_completion_preserves_evidence() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let _item = seed_item(&f, "candidate").await;

    f.store
        .set_run_state(
            &f.run_id,
            RunState::BudgetExhausted,
            Some(StopCondition::BudgetExhausted),
        )
        .await
        .unwrap();

    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    assert!(
        export.partial,
        "an exhausted run must export a partial result"
    );
    assert_eq!(f.provider.call_count(), 0);

    let text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert!(
        text.contains("Partial result"),
        "a partial report must be clearly labelled"
    );
}

/// The scheduler drives a task-mode run to a deliverable without a tournament.
#[tokio::test]
async fn scheduler_completes_a_task_run() {
    let provider = MockProvider::new()
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "needs replication", false));

    let f = fixture(Mode::Task, provider).await;
    seed_source(&f, "study-a", "We observed a 23% increase (p=0.03).").await;

    let scheduler = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig {
            limits: ConcurrencyLimits::new(2, 1),
            ..Default::default()
        },
    );

    let summary = scheduler.run(&f.run_id).await.unwrap();
    assert_eq!(summary.state, RunState::Completed, "{summary:?}");
    assert_eq!(
        summary.matches, 0,
        "a literature task must not generate a tournament (SPEC §13)"
    );
    assert!(
        summary.items >= 1,
        "the deliverable should be recorded: {summary:?}"
    );
    assert_eq!(summary.tasks_failed, 0, "{summary:?}");
}

/// In task mode with generation, research work runs first, the deliverable
/// is scheduled last, and the deliverable is never itself reviewed.
#[tokio::test]
async fn task_mode_delivers_last_and_never_reviews_the_deliverable() {
    let provider = MockProvider::new()
        .rule(
            anchor::GENERATION_LITERATURE,
            hypothesis_json("Drift", "drift explains it"),
        )
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "unchecked assumption", false));
    let f = fixture_with(Mode::Task, provider, |g| {
        g.question = "propose hypotheses for why these measurements disagree".into();
    })
    .await;

    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    assert_eq!(summary.state, RunState::Completed, "{summary:?}");
    assert_eq!(summary.tasks_failed, 0, "{summary:?}");

    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    let generated = tasks
        .iter()
        .filter(|t| t.role == Role::Generation && t.state == TaskState::Completed)
        .count();
    assert!(
        (1..=supervisor::TASK_CANDIDATE_CAP).contains(&generated),
        "task mode generates a bounded number of candidates: {generated}"
    );
    let deliverable = tasks
        .iter()
        .find(|t| t.strategy == "deliverable")
        .expect("deliverable produced");
    assert!(
        tasks.iter().all(|t| t.created_at <= deliverable.created_at),
        "the deliverable must come after generation and review"
    );
    let items = f.store.list_items(&f.run_id).await.unwrap();
    let synthesis: Vec<_> = items
        .iter()
        .filter(|i| i.kind == ItemKind::Synthesis)
        .map(|i| i.id.0.clone())
        .collect();
    assert_eq!(synthesis.len(), 1);
    assert!(
        !tasks
            .iter()
            .any(|t| t.role == Role::Reflection
                && t.input_refs.iter().any(|r| synthesis.contains(r))),
        "the deliverable is not a candidate and must not be reviewed"
    );
    assert_eq!(
        f.store
            .current_assessment(&ItemId::from_raw(synthesis[0].clone()))
            .await
            .unwrap(),
        Assessment::Untested,
        "a synthesis carries no epistemic assessment"
    );
}

/// A debate stops at its cap without fabricating a winner (SPEC §6, §13).
#[tokio::test]
async fn debate_stops_at_the_cap_without_fabricating() {
    // A panel that never converges.
    let never_converges = serde_json::json!({
        "status": "continue",
        "contribution": "I have further questions before concluding."
    })
    .to_string();

    let f = fixture(
        Mode::Campaign,
        MockProvider::new().default_reply(never_converges),
    )
    .await;
    let ctx = context(&f, CancellationToken::new());

    let result = generation::through_debate(&ctx, 4, 3, None).await.unwrap();

    match result {
        generation::DebateResult::Converged(_) => {
            panic!("an unconverged debate must not produce a hypothesis")
        }
        generation::DebateResult::Incomplete {
            turns_used,
            unresolved,
            ..
        } => {
            assert_eq!(turns_used, 4, "the cap must be enforced in Rust");
            assert!(!unresolved.is_empty());
        }
    }
    assert_eq!(
        f.provider.call_count(),
        4,
        "no eleventh call may be started to force a conclusion"
    );
    assert_eq!(ctx.spent().model_calls, 4, "every turn is charged");
}

/// A judge sees substantive reviews but never scores, Elo, or credits (§7).
#[tokio::test]
async fn judge_inputs_exclude_scores_and_ratings() {
    let f = fixture(
        Mode::Campaign,
        MockProvider::new().default_reply(ranking_json("win_1")),
    )
    .await;

    let a = seed_item(&f, "alpha").await;
    let b = seed_item(&f, "beta").await;

    // Reviews carrying numeric scores and a substantive objection.
    for (item, score) in [(&a, 9.5_f32), (&b, 2.0_f32)] {
        let review = Review {
            id: ReviewId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: f.run_id.clone(),
            item_id: item.id.clone(),
            item_hash: item.content_hash.clone(),
            strategy: ReviewStrategy::Full,
            assessment_text: "SUBSTANTIVE_FINDING_MARKER".into(),
            proposed_assessment: Assessment::Plausible,
            evidence: vec![],
            objections: vec![Objection {
                description: "OBJECTION_MARKER".into(),
                fatal: false,
                targets: None,
                suggested_check: None,
            }],
            unknowns: vec![],
            next_actions: vec![],
            execution_requests: vec![],
            observations: vec![],
            explanatory_label: None,
            score: Some(score),
            author: Author::Agent {
                role: "reflection".into(),
                strategy: "full".into(),
            },
            produced_by: None,
            created_at: OffsetDateTime::now_utc(),
        };
        f.store.insert_review(&review).await.unwrap();
    }

    // Give the items distinct, non-initial ratings.
    f.store.ensure_rating(&a.id, &f.plan.id).await.unwrap();
    f.store.ensure_rating(&b.id, &f.plan.id).await.unwrap();
    let warmup = Match {
        id: MatchId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        item_a: a.id.clone(),
        item_b: b.id.clone(),
        rubric_id: f.plan.id.clone(),
        evidence_snapshot: ContentHash::of_str("s"),
        order_randomized: false,
        method: MatchMethod::Pairwise,
        judge_model: "mock".into(),
        outcome: MatchOutcome::WinA,
        rationale: "warmup".into(),
        rating_a_before: 0.0,
        rating_b_before: 0.0,
        rating_a_after: 0.0,
        rating_b_after: 0.0,
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store
        .commit_match(&warmup, "warmup", DEFAULT_K)
        .await
        .unwrap();

    let ctx = context(&f, CancellationToken::new());
    ranking::compare(&ctx, &a, &b, MatchMethod::Pairwise, 3, None)
        .await
        .unwrap();

    let prompts = f.provider.seen_prompts();
    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];

    // Substance is present.
    assert!(
        prompt.contains("SUBSTANTIVE_FINDING_MARKER"),
        "reviews must reach the judge"
    );
    assert!(
        prompt.contains("OBJECTION_MARKER"),
        "objections must reach the judge"
    );

    // Scores, ratings, and credits are not.
    assert!(
        !prompt.contains("9.5"),
        "reviewer scores must be withheld (SPEC §7)"
    );
    assert!(
        !prompt.contains("2.0"),
        "reviewer scores must be withheld (SPEC §7)"
    );
    assert!(
        !prompt.contains("1216"),
        "Elo must be withheld from the judge"
    );
    assert!(
        !prompt.contains("1184"),
        "Elo must be withheld from the judge"
    );
    assert!(
        !prompt.to_lowercase().contains("elo"),
        "the judge must not see ratings at all"
    );
}
