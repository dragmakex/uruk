//! One owner for provider failures (SPEC §6, §9.1).
//!
//! The provider adapter waits out a transient failure itself (see
//! `provider_availability.rs`); what reaches the scheduler is a permanent
//! refusal. The scheduler does not stack a retry loop on top of that: a
//! permanent refusal is final for the task, so it cannot multiply into
//! nested attempt counts, strand dependent work as queued, or keep a run
//! busy past its wall-clock deadline.

mod common;

use common::*;
use std::time::Duration;
use time::OffsetDateTime;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::runtime::{Scheduler, SchedulerConfig, request_stop};

/// A pending task the way the scheduler would build it.
fn pending_task(f: &Fixture, role: Role, strategy: &str) -> Task {
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

/// A provider that refuses every request permanently. The scheduler must
/// not re-run the task again and again; the task fails once with the
/// provider's own message, and the run winds down with no task left
/// queued.
#[tokio::test]
async fn a_permanent_provider_refusal_is_final_for_the_task() {
    let provider = MockProvider::new().fail_first(u32::MAX);
    let f = fixture_with(Mode::Task, provider, |g| {
        g.question = "assess this candidate".into();
    })
    .await;
    seed_item(&f, "candidate").await;

    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();

    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    assert!(!tasks.is_empty(), "the run attempted work: {summary:?}");
    for task in &tasks {
        assert_eq!(
            task.attempts, 1,
            "the scheduler must not retry a permanent provider refusal: {task:?}"
        );
        assert_eq!(task.state, TaskState::Failed, "{task:?}");
        let error = task.error.as_deref().unwrap_or("");
        assert!(
            error.contains("mock provider refused"),
            "the provider's own message is reported: {error:?}"
        );
        assert!(
            !error.contains("attempts exhausted"),
            "no second, nested attempt count: {error:?}"
        );
    }
    assert_eq!(
        f.provider.call_count(),
        tasks.iter().map(|t| t.attempts).sum::<u32>(),
        "exactly one provider call per task attempt"
    );
    assert!(
        summary.state.is_terminal(),
        "a refusing provider ends the run coherently: {summary:?}"
    );
    assert!(
        summary.final_output_skipped.is_some(),
        "a deliverable that already failed is reported, not silently retried: {summary:?}"
    );
}

/// Work that depends on a permanently failed task can never become ready;
/// leaving it `pending` shows "queued" forever. It is cancelled, naming the
/// prerequisite, as soon as the prerequisite fails.
#[tokio::test]
async fn dependents_of_a_permanently_failed_task_are_cancelled_not_left_queued() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let first = pending_task(&f, Role::Search, "discover");
    let mut second = pending_task(&f, Role::Search, "acquire");
    second.depends_on = vec![first.id.clone()];
    let mut third = pending_task(&f, Role::Search, "acquire");
    third.depends_on = vec![second.id.clone()];
    f.store.enqueue_task(&first, None).await.unwrap();
    f.store.enqueue_task(&second, None).await.unwrap();
    f.store.enqueue_task(&third, None).await.unwrap();

    assert!(f.store.claim_task(&first.id).await.unwrap());
    f.store
        .complete_task(
            &first.id,
            TaskState::Failed,
            vec![],
            CostActual::default(),
            Some("HTTP 401: unauthorized".into()),
        )
        .await
        .unwrap();

    let second = f.store.get_task(&second.id).await.unwrap();
    assert_eq!(second.state, TaskState::Cancelled, "{second:?}");
    let error = second.error.as_deref().unwrap_or("");
    assert!(
        error.contains(first.id.as_str()),
        "the cancellation names the failed prerequisite: {error:?}"
    );
    let third = f.store.get_task(&third.id).await.unwrap();
    assert_eq!(
        third.state,
        TaskState::Cancelled,
        "the cascade is transitive: {third:?}"
    );
    assert!(
        f.store.ready_tasks(&f.run_id, 10).await.unwrap().is_empty(),
        "nothing is left to dispatch"
    );
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(
        usage.reserved_calls, 0,
        "cancelled work releases its reservation"
    );
}

/// A permanent provider refusal is terminal for that logical work: the
/// supervisor must not propose the same work again under a new task id,
/// which would only repeat the refusal.
#[tokio::test]
async fn permanent_provider_refusals_are_not_re_proposed() {
    let provider = MockProvider::new().fail_first(u32::MAX);
    let f = fixture_with(Mode::Task, provider, |g| {
        g.question = "assess this candidate".into();
    })
    .await;
    seed_item(&f, "candidate").await;

    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();

    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    let mut by_work: std::collections::BTreeMap<(String, String, Vec<String>), u32> =
        Default::default();
    for task in &tasks {
        *by_work
            .entry((
                task.role.as_str().to_string(),
                task.strategy.clone(),
                task.input_refs.clone(),
            ))
            .or_default() += 1;
    }
    assert!(
        by_work.values().all(|&n| n == 1),
        "each logical work item is attempted exactly once against a refusing provider: {by_work:?} ({summary:?})"
    );
    assert_eq!(
        f.provider.call_count(),
        tasks.len() as u32,
        "one outbound call per logical work item"
    );
}

/// Legitimate adaptation is preserved: a failure in the role's output (the
/// model answered, but not acceptably) is a stochastic miss, and the same
/// work is proposed again.
#[tokio::test]
async fn output_failures_are_still_re_proposed() {
    let provider = MockProvider::new()
        .push_reply("this is not the structured review that was asked for")
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "fine", false));
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
    let missed = tasks
        .iter()
        .find(|t| t.state == TaskState::Failed)
        .expect("the garbage reply failed one task");
    assert_eq!(missed.failure, Some(FailureKind::Output), "{missed:?}");
    assert_eq!(missed.input_refs, vec![item.id.0.clone()]);
    let same_work: Vec<&Task> = tasks
        .iter()
        .filter(|t| {
            t.role == missed.role
                && t.strategy == missed.strategy
                && t.input_refs == missed.input_refs
        })
        .collect();
    assert_eq!(
        same_work.len(),
        2,
        "an output miss is re-proposed once: {summary:?}"
    );
    assert!(same_work.iter().any(|t| t.state == TaskState::Completed));
}

/// A durable stop request that lands while the final meta-review is running
/// must win: the run ends `cancelled`, not `budget-exhausted` written over
/// the stop by a scheduler holding a stale stop condition.
#[tokio::test]
async fn a_stop_during_the_final_meta_review_wins() {
    let provider = MockProvider::new()
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "needs work", false))
        .with_delay(Duration::from_millis(1500));
    let f = fixture_with(Mode::Campaign, provider, |g| {
        g.budget.max_iterations = 1;
    })
    .await;
    seed_item(&f, "candidate").await;

    let store = f.store.clone();
    let run_id = f.run_id.clone();
    let stopper = tokio::spawn(async move {
        loop {
            let tasks = store.list_tasks(&run_id).await.unwrap();
            if tasks
                .iter()
                .any(|t| t.strategy == "deliverable" && t.state == TaskState::Running)
            {
                request_stop(&store, &run_id).await.unwrap();
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });

    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    stopper.await.unwrap();

    assert_eq!(summary.state, RunState::Cancelled, "{summary:?}");
    let run = f.store.get_run(&f.run_id).await.unwrap();
    assert_eq!(
        run.state,
        RunState::Cancelled,
        "the durable stop is not overwritten"
    );
    let skipped = summary.final_output_skipped.as_deref().unwrap_or("");
    assert!(
        skipped.contains("cancelled"),
        "the interrupted deliverable is reported: {summary:?}"
    );
    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    assert!(tasks.iter().all(|t| t.state.is_terminal()), "{tasks:?}");
}
