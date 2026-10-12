//! Every outbound model request is charged (SPEC §6: "Failed work still
//! costs what it consumed"; §9.2: preserve possible incurred cost). The
//! adapter counts attempts as they start, so attempts before an eventual
//! success, attempts of a terminal failure, and an attempt cut off by
//! cancellation all settle against the run's model-call budget.

mod common;

use common::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uruk::agents::{AgentContext, CallAllowance};
use uruk::provider::{ChatRequest, HttpProvider, Message, MockProvider};
use uruk::records::*;
use uruk::runtime::{Scheduler, SchedulerConfig, execute_task};
use uruk::{Error, Result};

/// One scripted upstream response: answer, or accept and never answer.
enum Step {
    Reply(u16, String),
    Hang,
}

fn ok_body() -> String {
    serde_json::json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
    })
    .to_string()
}

async fn read_request(sock: &mut tokio::net::TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut tmp).await.unwrap();
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let length: usize = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap())
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = sock.read(&mut tmp).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

/// Serve `steps` one per connection, then hold every hung socket open until
/// the returned task is aborted. The counter is the number of requests the
/// upstream actually received.
async fn serve(steps: Vec<Step>) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let received = Arc::new(AtomicUsize::new(0));
    let counter = received.clone();
    let handle = tokio::spawn(async move {
        let mut held = Vec::new();
        for step in steps {
            let (mut sock, _) = listener.accept().await.unwrap();
            read_request(&mut sock).await;
            counter.fetch_add(1, Ordering::SeqCst);
            match step {
                Step::Reply(status, body) => {
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    sock.write_all(response.as_bytes()).await.unwrap();
                    sock.shutdown().await.ok();
                }
                Step::Hang => held.push(sock),
            }
        }
        std::future::pending::<()>().await;
    });
    (url, handle, received)
}

fn http_context(
    f: &Fixture,
    url: &str,
    max_retries: u32,
    cancel: CancellationToken,
) -> AgentContext {
    let provider = HttpProvider::new(url, None, "m")
        .unwrap()
        .with_max_retries(max_retries)
        .with_retry_base(Duration::from_millis(1));
    AgentContext::new(
        f.store.clone(),
        Arc::new(provider),
        Arc::new(uruk::prompts::PromptRegistry::new()),
        f.run_id.clone(),
        f.goal.clone(),
        f.plan.clone(),
        f.goal.permissions.clone(),
        7,
        cancel,
    )
}

fn request() -> ChatRequest {
    ChatRequest::new("m", vec![Message::user("hi")])
}

#[tokio::test]
async fn every_attempt_before_an_eventual_success_is_charged() {
    let (url, server, _) = serve(vec![
        Step::Reply(503, "{}".into()),
        Step::Reply(503, "{}".into()),
        Step::Reply(200, ok_body()),
    ])
    .await;
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let ctx = http_context(&f, &url, 3, CancellationToken::new());

    let response = ctx.complete(request()).await.unwrap();
    assert_eq!(response.text(), "ok");
    let spent = ctx.spent();
    assert_eq!(spent.model_calls, 3, "three requests went out: {spent:?}");
    assert_eq!(
        spent.prompt_tokens, 12,
        "the reply's usage is still counted"
    );
    server.abort();
}

#[tokio::test]
async fn every_attempt_of_a_terminal_failure_is_charged() {
    let (url, server, _) = serve(vec![
        Step::Reply(500, "boom".into()),
        Step::Reply(500, "boom".into()),
    ])
    .await;
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let ctx = http_context(&f, &url, 1, CancellationToken::new());

    let err = ctx.complete(request()).await.unwrap_err();
    assert!(matches!(err, Error::Provider(_)), "{err}");
    assert_eq!(ctx.spent().model_calls, 2, "both requests went out");
    server.abort();
}

#[tokio::test]
async fn attempts_cut_off_by_cancellation_are_charged() {
    let (url, server, _) = serve(vec![Step::Reply(503, "{}".into()), Step::Hang]).await;
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let cancel = CancellationToken::new();
    let ctx = http_context(&f, &url, 3, cancel.clone());

    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        canceller.cancel();
    });

    let result: Result<_> = ctx.complete(request()).await;
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert_eq!(
        ctx.spent().model_calls,
        2,
        "the failed attempt and the one in flight at cancellation were both sent"
    );
    server.abort();
}

/// The executor records why a task failed as a typed fact, so scheduling
/// policy can tell a provider that would not serve the work from a model
/// reply that merely missed, without parsing display strings.
#[tokio::test]
async fn provider_failures_are_recorded_with_a_typed_kind() {
    let f = fixture(Mode::Campaign, MockProvider::new().fail_first(u32::MAX)).await;
    let item = seed_item(&f, "candidate").await;
    let ctx = context(&f, CancellationToken::new());
    let mut task = Task {
        id: TaskId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        role: Role::Reflection,
        strategy: "initial".into(),
        goal_id: f.goal.id.clone(),
        plan_id: f.plan.id.clone(),
        input_refs: vec![item.id.0.clone()],
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
        failure: None,
        created_at: time::OffsetDateTime::now_utc(),
        started_at: None,
        finished_at: None,
    };
    f.store.enqueue_task(&task, None).await.unwrap();
    assert!(f.store.claim_task(&task.id).await.unwrap());
    task.attempts = 1;

    let outcome = execute_task(&ctx, &task).await.unwrap();
    assert_eq!(outcome.state, TaskState::Failed);
    assert_eq!(outcome.failure, Some(FailureKind::Provider));

    f.store
        .commit_task(
            &task.id,
            outcome.state,
            &outcome.batch,
            outcome.cost,
            outcome.error,
            outcome.failure,
        )
        .await
        .unwrap();
    let stored = f.store.get_task(&task.id).await.unwrap();
    assert_eq!(
        stored.failure,
        Some(FailureKind::Provider),
        "the kind is persisted with the task: {stored:?}"
    );
}

/// Drive a review-only task run against an upstream that only ever answers
/// 503, with `calls` model calls in the whole budget and an adapter willing
/// to retry far more often than that.
async fn outage_run_with_budget(
    calls: u32,
) -> (usize, Vec<Task>, uruk::runtime::RunSummary, Fixture) {
    let (url, server, received) = serve(
        (0..8)
            .map(|_| Step::Reply(503, "{\"error\":\"overloaded\"}".into()))
            .collect(),
    )
    .await;
    let f = fixture_with(Mode::Task, MockProvider::new(), |g| {
        g.question = "assess this candidate".into();
        g.budget.max_model_calls = calls;
        g.budget.reserve_calls_for_output = 0;
    })
    .await;
    seed_item(&f, "candidate").await;
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_max_retries(3)
        .with_retry_base(Duration::from_millis(1));

    let summary = Scheduler::new(
        f.store.clone(),
        Arc::new(provider),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    server.abort();
    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    (received.load(Ordering::SeqCst), tasks, summary, f)
}

async fn assert_outbound_requests_never_exceed_the_budget(calls: u32) {
    let (received, tasks, summary, f) = outage_run_with_budget(calls).await;
    assert_eq!(
        received as u32, calls,
        "the adapter may only send what the budget still allows: {summary:?}"
    );
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert!(
        usage.model_calls <= calls,
        "settled calls stay within the hard budget: {usage:?}"
    );
    assert_eq!(usage.model_calls, calls, "every sent attempt is settled");

    let review = tasks
        .iter()
        .find(|t| t.role == Role::Reflection)
        .expect("the review ran");
    assert_eq!(review.state, TaskState::Failed, "{review:?}");
    assert_eq!(
        review.failure,
        Some(FailureKind::Budget),
        "the task reports budget exhaustion, not a provider outage: {review:?}"
    );
    let error = review.error.as_deref().unwrap_or("");
    assert!(error.contains("budget"), "{error:?}");
    assert!(
        !error.contains("after 4 attempts"),
        "the adapter never got to exhaust its own bound: {error:?}"
    );
    assert_eq!(summary.state, RunState::BudgetExhausted, "{summary:?}");
}

#[tokio::test]
async fn with_one_call_left_an_outage_sends_exactly_one_request() {
    assert_outbound_requests_never_exceed_the_budget(1).await;
}

#[tokio::test]
async fn with_two_calls_left_an_outage_sends_exactly_two_requests() {
    assert_outbound_requests_never_exceed_the_budget(2).await;
}

/// Concurrent tasks draw unreserved headroom through the store, in one
/// transaction per attempt, so together they can never start more attempts
/// than the run has calls left, whichever process drives them.
#[tokio::test]
async fn two_tasks_share_durable_headroom_without_overrunning_it() {
    const CEILING: u32 = 3;
    let f = fixture_with(Mode::Task, MockProvider::new(), |g| {
        g.budget.max_model_calls = CEILING;
        g.budget.reserve_calls_for_output = 0;
    })
    .await;
    let item = seed_item(&f, "candidate").await;
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let task = review_task(&f, &item);
        f.store.enqueue_task(&task, None).await.unwrap();
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
        tasks.push(f.store.get_task(&task.id).await.unwrap());
    }
    let a = CallAllowance::for_task(f.store.clone(), &tasks[0], CEILING);
    let b = CallAllowance::for_task(f.store.clone(), &tasks[1], CEILING);

    assert!(a.claim(1).await.unwrap(), "a's own reservation");
    assert!(b.claim(1).await.unwrap(), "b's own reservation");
    let third = a.claim(2).await.unwrap();
    let fourth = b.claim(2).await.unwrap();
    assert!(
        third ^ fourth,
        "exactly one of them leases the single unreserved call"
    );
    assert!(!a.claim(3).await.unwrap() && !b.claim(3).await.unwrap());

    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(
        usage.committed_calls(),
        CEILING,
        "the lease is a durable reservation, visible to any process: {usage:?}"
    );
}

fn review_task(f: &Fixture, item: &ResearchItem) -> Task {
    Task {
        id: TaskId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        role: Role::Reflection,
        strategy: "initial".into(),
        goal_id: f.goal.id.clone(),
        plan_id: f.plan.id.clone(),
        input_refs: vec![item.id.0.clone()],
        input_hash: ContentHash::of_str("in"),
        prompt_id: None,
        prompt_hash: None,
        model: None,
        provider: Some("http".into()),
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
        failure: None,
        created_at: time::OffsetDateTime::now_utc(),
        started_at: None,
        finished_at: None,
    }
}

/// A crash after attempts were sent but before the task settled must not
/// lose those attempts: recovery charges every attempt that was started
/// (SPEC §9.2: preserve possible incurred cost), and the redispatch cannot
/// spend what the conservative charge already consumed.
#[tokio::test]
async fn crash_after_send_charges_every_started_attempt_on_recovery() {
    const CEILING: u32 = 4;
    let (url, server, received) = serve(
        (0..8)
            .map(|_| Step::Reply(503, "{\"error\":\"overloaded\"}".into()))
            .collect(),
    )
    .await;
    let f = fixture_with(Mode::Task, MockProvider::new(), |g| {
        g.budget.max_model_calls = CEILING;
        g.budget.reserve_calls_for_output = 0;
    })
    .await;
    let item = seed_item(&f, "candidate").await;
    let task = review_task(&f, &item);
    f.store.enqueue_task(&task, None).await.unwrap();
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
    let claimed = f.store.get_task(&task.id).await.unwrap();

    // First process: the adapter sends what the budget allows, then the
    // process dies before the task outcome is committed.
    let mut ctx = http_context(&f, &url, 3, CancellationToken::new());
    ctx.set_call_allowance(&claimed, CEILING);
    let err = ctx.complete(request()).await.unwrap_err();
    assert!(matches!(err, Error::Provider(_)), "{err}");
    let sent_before_crash = received.load(Ordering::SeqCst) as u32;
    assert_eq!(
        sent_before_crash, CEILING,
        "the budget, not the retry bound, stopped it"
    );
    drop(ctx);

    // Second process: recovery.
    let uncertain = f.store.reconcile_interrupted(&f.run_id).await.unwrap();
    assert!(uncertain.is_empty(), "a model-only task is safe to redo");
    let recovered = f.store.get_task(&task.id).await.unwrap();
    assert_eq!(recovered.state, TaskState::Pending);
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(
        usage.model_calls, sent_before_crash,
        "every attempt that was started is settled on recovery: {usage:?}"
    );
    assert!(
        usage.cost_partially_unknown,
        "the cost of the lost attempts is reported unknown, not zero: {usage:?}"
    );
    assert!(
        recovered
            .error
            .as_deref()
            .is_some_and(|e| e.contains(&sent_before_crash.to_string())),
        "the recovery note is auditable: {recovered:?}"
    );

    // Redispatch: the recovered task gets no fresh allowance for calls the
    // run no longer has.
    assert!(f.store.claim_task(&task.id).await.unwrap());
    let reclaimed = f.store.get_task(&task.id).await.unwrap();
    let mut ctx = http_context(&f, &url, 3, CancellationToken::new());
    ctx.set_call_allowance(&reclaimed, CEILING);
    let err = ctx.complete(request()).await.unwrap_err();
    assert!(matches!(err, Error::Budget(_)), "{err}");
    assert_eq!(
        received.load(Ordering::SeqCst) as u32,
        CEILING,
        "no outbound request beyond the hard ceiling, across both processes"
    );
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert!(usage.model_calls <= CEILING, "{usage:?}");
    server.abort();
}

async fn attempt_rows(f: &Fixture, task: &TaskId) -> u32 {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM budget_ledger WHERE task_id = ? AND kind = 'attempt'",
    )
    .bind(task.as_str())
    .fetch_one(f.store.pool())
    .await
    .unwrap();
    u32::try_from(n).unwrap()
}

/// One task attempt makes several completions (a debate's turns). Every
/// outbound request across all of them gets its own ledger row, the lease
/// coverage is never confused between completions, and the hard cap binds
/// across the whole task attempt, not per completion.
#[tokio::test]
async fn attempts_across_completions_of_one_task_are_distinct_and_capped() {
    const CEILING: u32 = 4;
    let (url, server, received) = serve(vec![
        Step::Reply(503, "{}".into()),
        Step::Reply(503, "{}".into()),
        Step::Reply(200, ok_body()),
        Step::Reply(503, "{}".into()),
        Step::Reply(200, ok_body()),
        Step::Reply(200, ok_body()),
    ])
    .await;
    let f = fixture_with(Mode::Task, MockProvider::new(), |g| {
        g.budget.max_model_calls = CEILING;
        g.budget.reserve_calls_for_output = 0;
    })
    .await;
    let item = seed_item(&f, "candidate").await;
    let task = review_task(&f, &item);
    f.store.enqueue_task(&task, None).await.unwrap();
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
    let claimed = f.store.get_task(&task.id).await.unwrap();
    let mut ctx = http_context(&f, &url, 3, CancellationToken::new());
    ctx.set_call_allowance(&claimed, CEILING);

    // Turn 1: two transient failures, then success (3 requests).
    let first = ctx.complete(request()).await.unwrap();
    assert_eq!(first.text(), "ok");
    assert_eq!(received.load(Ordering::SeqCst), 3);
    assert_eq!(attempt_rows(&f, &task.id).await, 3, "one row per request");

    // Turn 2: the 4th request is the last the budget covers; its retry is
    // refused before it goes out.
    let err = ctx.complete(request()).await.unwrap_err();
    assert!(matches!(err, Error::Budget(_)), "{err}");
    assert_eq!(
        received.load(Ordering::SeqCst) as u32,
        CEILING,
        "the hard cap binds across completions of one task attempt"
    );
    assert_eq!(
        attempt_rows(&f, &task.id).await,
        CEILING,
        "every outbound request of the task attempt has its own row"
    );
    assert_eq!(ctx.spent().model_calls, CEILING, "all four are charged");
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(
        usage.committed_calls(),
        CEILING,
        "one base reservation plus three leases: {usage:?}"
    );

    // Crash before commit: recovery charges exactly the four that went out.
    drop(ctx);
    f.store.reconcile_interrupted(&f.run_id).await.unwrap();
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(usage.model_calls, CEILING, "{usage:?}");
    assert_eq!(
        usage.reserved_calls, 1,
        "leases released, base kept: {usage:?}"
    );
    server.abort();
}

/// A provider written against the original trait implements only
/// `complete`. It must still compile, and counted dispatch must record and
/// charge exactly one attempt for it.
#[derive(Debug)]
struct LegacyProvider;

#[async_trait::async_trait]
impl uruk::provider::Provider for LegacyProvider {
    fn name(&self) -> &str {
        "legacy"
    }

    fn default_model(&self) -> &str {
        "legacy-model"
    }

    async fn complete(&self, _request: ChatRequest) -> Result<uruk::provider::ChatResponse> {
        Ok(serde_json::from_str(&ok_body()).unwrap())
    }
}

#[tokio::test]
async fn a_legacy_provider_implementing_only_complete_is_counted_as_one_attempt() {
    const CEILING: u32 = 2;
    let f = fixture_with(Mode::Task, MockProvider::new(), |g| {
        g.budget.max_model_calls = CEILING;
        g.budget.reserve_calls_for_output = 0;
    })
    .await;
    let item = seed_item(&f, "candidate").await;
    let task = review_task(&f, &item);
    f.store.enqueue_task(&task, None).await.unwrap();
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
    let claimed = f.store.get_task(&task.id).await.unwrap();
    let mut ctx = AgentContext::new(
        f.store.clone(),
        Arc::new(LegacyProvider),
        Arc::new(uruk::prompts::PromptRegistry::new()),
        f.run_id.clone(),
        f.goal.clone(),
        f.plan.clone(),
        f.goal.permissions.clone(),
        7,
        CancellationToken::new(),
    );
    ctx.set_call_allowance(&claimed, CEILING);

    let response = ctx.complete(request()).await.unwrap();
    assert_eq!(response.text(), "ok");
    assert_eq!(ctx.spent().model_calls, 1);
    assert_eq!(attempt_rows(&f, &task.id).await, 1);

    // The second call leases the one remaining; a third is refused before
    // the legacy provider is reached.
    ctx.complete(request()).await.unwrap();
    let err = ctx.complete(request()).await.unwrap_err();
    assert!(matches!(err, Error::Budget(_)), "{err}");
    assert_eq!(attempt_rows(&f, &task.id).await, 2);
}
