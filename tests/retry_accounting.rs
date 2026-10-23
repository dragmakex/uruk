//! Every logical model call is charged (SPEC §6: "Failed work still costs
//! what it consumed"; §9.2: preserve possible incurred cost). The adapter
//! claims a call before its first send, so a served call, a permanently
//! refused call, and a call cut off by cancellation all settle against the
//! run's model-call budget. Sends repeated after a transient upstream
//! failure are audited as `resend` ledger rows but claim nothing: an
//! outage cannot spend the budget.

mod common;

use common::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uruk::agents::{AgentContext, CallAllowance};
use uruk::provider::{ChatRequest, HttpProvider, Message, MockProvider, ProbeBackoff};
use uruk::records::*;
use uruk::runtime::{Scheduler, SchedulerConfig, execute_task};
use uruk::{Error, Result};

/// One scripted upstream response: answer, answer whatever role is
/// asking, or accept and never answer.
enum Step {
    Reply(u16, String),
    /// A served reply shaped for the prompt: a synthesis for the
    /// deliverable, a plain review for everything else.
    ByPrompt,
    Hang,
}

fn ok_body() -> String {
    completion("ok")
}

/// A served chat completion whose reply is `content`.
fn completion(content: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
    })
    .to_string()
}

/// Read one request; returns its body.
async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut tmp).await.unwrap();
        if n == 0 {
            return String::new();
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
    String::from_utf8_lossy(&buf[head_end..]).into_owned()
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
            let request_body = read_request(&mut sock).await;
            counter.fetch_add(1, Ordering::SeqCst);
            let step = match step {
                Step::ByPrompt if request_body.contains("Requested deliverable:") => {
                    Step::Reply(200, completion(&synthesis_json()))
                }
                Step::ByPrompt => {
                    Step::Reply(200, completion(&review_json("plausible", "fine", false)))
                }
                other => other,
            };
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
                Step::ByPrompt => unreachable!("resolved above"),
            }
        }
        std::future::pending::<()>().await;
    });
    (url, handle, received)
}

fn fast_probes() -> ProbeBackoff {
    ProbeBackoff::new(Duration::from_millis(1), Duration::from_millis(5))
}

fn http_context(f: &Fixture, url: &str, cancel: CancellationToken) -> AgentContext {
    let provider = HttpProvider::new(url, None, "m")
        .unwrap()
        .with_probe_backoff(fast_probes());
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
async fn a_call_served_after_transient_failures_is_charged_once() {
    let (url, server, received) = serve(vec![
        Step::Reply(503, "{}".into()),
        Step::Reply(503, "{}".into()),
        Step::Reply(200, ok_body()),
    ])
    .await;
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let ctx = http_context(&f, &url, CancellationToken::new());

    let response = ctx.complete(request()).await.unwrap();
    assert_eq!(response.text(), "ok");
    assert_eq!(received.load(Ordering::SeqCst), 3, "three sends went out");
    let spent = ctx.spent();
    assert_eq!(
        spent.model_calls, 1,
        "one logical call, whatever the upstream needed: {spent:?}"
    );
    assert_eq!(spent.prompt_tokens, 12, "the reply's usage is counted");
    server.abort();
}

#[tokio::test]
async fn a_permanently_refused_call_is_charged() {
    let (url, server, received) = serve(vec![Step::Reply(401, "no".into())]).await;
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let ctx = http_context(&f, &url, CancellationToken::new());

    let err = ctx.complete(request()).await.unwrap_err();
    assert!(matches!(err, Error::Provider(_)), "{err}");
    assert_eq!(received.load(Ordering::SeqCst), 1, "no retry of a refusal");
    assert_eq!(
        ctx.spent().model_calls,
        1,
        "the refused request still went out and is charged"
    );
    server.abort();
}

#[tokio::test]
async fn a_call_cut_off_by_cancellation_is_charged_once() {
    let (url, server, received) = serve(vec![Step::Reply(503, "{}".into()), Step::Hang]).await;
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let cancel = CancellationToken::new();
    let ctx = http_context(&f, &url, cancel.clone());

    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        canceller.cancel();
    });

    let result: Result<_> = ctx.complete(request()).await;
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert_eq!(
        received.load(Ordering::SeqCst),
        2,
        "a probe, then the hung send"
    );
    assert_eq!(
        ctx.spent().model_calls,
        1,
        "the call in flight at cancellation is charged, its resend is not a second call"
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
        reservation: CostReservation { model_calls: 1 },
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

/// Drive a review-then-deliverable task run against an upstream that
/// answers 503 to its first `outage` requests and serves every later one
/// (a review, then syntheses), with `calls` model calls in the whole
/// budget.
async fn run_through_an_outage(
    outage: usize,
    calls: u32,
) -> (usize, Vec<Task>, uruk::runtime::RunSummary, Fixture) {
    let mut steps: Vec<Step> = (0..outage)
        .map(|_| Step::Reply(503, "{\"error\":\"overloaded\"}".into()))
        .collect();
    steps.extend((0..8).map(|_| Step::ByPrompt));
    let (url, server, received) = serve(steps).await;
    let f = fixture_with(Mode::Task, MockProvider::new(), |g| {
        g.question = "assess this candidate".into();
        g.budget.max_model_calls = calls;
        g.budget.reserve_calls_for_output = 0;
    })
    .await;
    seed_item(&f, "candidate").await;
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_probe_backoff(fast_probes());

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

/// An outage longer than the whole call budget does not spend it: the
/// sends that the upstream did not serve are audited, not charged, and
/// the run finishes its work once the upstream is back.
#[tokio::test]
async fn an_outage_longer_than_the_budget_does_not_spend_it() {
    const CALLS: u32 = 4;
    let (received, tasks, summary, f) = run_through_an_outage(6, CALLS).await;
    assert!(
        received > CALLS as usize,
        "the upstream saw every probe: {received} sends"
    );
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert!(
        usage.model_calls <= CALLS,
        "settled calls stay within the hard budget: {usage:?}"
    );
    let review = tasks
        .iter()
        .find(|t| t.role == Role::Reflection)
        .expect("the review ran");
    assert_eq!(review.state, TaskState::Completed, "{review:?}");
    assert_eq!(
        review.actual_cost.model_calls, 1,
        "the review is one logical call: {review:?}"
    );
    assert!(
        !tasks.iter().any(|t| t.state == TaskState::Failed),
        "an outage fails nothing: {tasks:?}"
    );
    assert_eq!(summary.state, RunState::Completed, "{summary:?}");
    let resends: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM budget_ledger WHERE run_id = ? AND kind = 'resend'",
    )
    .bind(f.run_id.as_str())
    .fetch_one(f.store.pool())
    .await
    .unwrap();
    assert_eq!(resends, 6, "every unserved send is an auditable fact");
}

/// With no call left, nothing goes out at all, outage or not: the budget
/// is consulted before the first send.
#[tokio::test]
async fn with_no_call_left_nothing_is_sent() {
    let (received, tasks, summary, _f) = run_through_an_outage(2, 0).await;
    assert_eq!(received, 0, "the adapter never reached the upstream");
    assert!(
        tasks.iter().all(|t| t.state != TaskState::Completed),
        "{tasks:?}"
    );
    assert_eq!(summary.state, RunState::BudgetExhausted, "{summary:?}");
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
        reservation: CostReservation { model_calls: 1 },
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

/// A crash while a call is waiting out an outage must not lose that call:
/// recovery charges the one logical call that was claimed (SPEC §9.2:
/// preserve possible incurred cost) — not one per probe — and the
/// redispatch gets exactly the headroom that is left.
#[tokio::test]
async fn crash_during_an_outage_charges_the_claimed_call_once_on_recovery() {
    const CEILING: u32 = 3;
    let (url, server, received) = serve(
        (0..64)
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

    // First process: the call waits out the outage, probing; then the
    // process dies mid-wait.
    let mut ctx = http_context(&f, &url, CancellationToken::new());
    ctx.set_call_allowance(&claimed, CEILING);
    let waiting = tokio::spawn(async move { ctx.complete(request()).await });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while received.load(Ordering::SeqCst) < 4 {
        assert!(std::time::Instant::now() < deadline, "probing stalled");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    waiting.abort();
    let _ = waiting.await;
    let probes = received.load(Ordering::SeqCst) as u32;
    assert!(probes >= 4, "the budget did not stop the probing: {probes}");

    // Second process: recovery.
    let uncertain = f.store.reconcile_interrupted(&f.run_id).await.unwrap();
    assert!(uncertain.is_empty(), "a model-only task is safe to redo");
    let recovered = f.store.get_task(&task.id).await.unwrap();
    assert_eq!(recovered.state, TaskState::Pending);
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(
        usage.model_calls, 1,
        "the one claimed call is charged, not one per probe: {usage:?}"
    );
    assert!(
        usage.cost_partially_unknown,
        "the cost of the lost call is reported unknown, not zero: {usage:?}"
    );
    assert!(
        recovered
            .error
            .as_deref()
            .is_some_and(|e| e.contains("1 model call")),
        "the recovery note is auditable: {recovered:?}"
    );
    let resends: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM budget_ledger WHERE task_id = ? AND kind = 'resend'",
    )
    .bind(task.id.as_str())
    .fetch_one(f.store.pool())
    .await
    .unwrap();
    assert!(resends >= 3, "the probes stay on record: {resends}");

    // Redispatch: the recovered task may claim the calls the run has left,
    // and not one more.
    assert!(f.store.claim_task(&task.id).await.unwrap());
    let reclaimed = f.store.get_task(&task.id).await.unwrap();
    let allowance = CallAllowance::for_task(f.store.clone(), &reclaimed, CEILING);
    assert!(
        allowance.claim(1).await.unwrap(),
        "the task's own reservation"
    );
    assert!(
        allowance.claim(2).await.unwrap(),
        "the last unreserved call"
    );
    assert!(
        !allowance.claim(3).await.unwrap(),
        "nothing beyond the hard ceiling, across both processes"
    );
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
/// logical call across all of them gets its own ledger row, resends are
/// audited under their call, the lease coverage is never confused between
/// completions, and the hard cap binds across the whole task attempt, not
/// per completion.
#[tokio::test]
async fn calls_across_completions_of_one_task_are_distinct_and_capped() {
    const CEILING: u32 = 2;
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
    let mut ctx = http_context(&f, &url, CancellationToken::new());
    ctx.set_call_allowance(&claimed, CEILING);

    // Turn 1: two transient failures, then served (3 sends, 1 call).
    let first = ctx.complete(request()).await.unwrap();
    assert_eq!(first.text(), "ok");
    assert_eq!(received.load(Ordering::SeqCst), 3);
    assert_eq!(
        attempt_rows(&f, &task.id).await,
        1,
        "one row per logical call"
    );
    assert_eq!(resend_rows(&f, &task.id).await, 2, "one row per resend");

    // Turn 2: the last call the budget covers; its resend is not a new one.
    let second = ctx.complete(request()).await.unwrap();
    assert_eq!(second.text(), "ok");
    assert_eq!(received.load(Ordering::SeqCst), 5);
    assert_eq!(attempt_rows(&f, &task.id).await, CEILING);
    assert_eq!(resend_rows(&f, &task.id).await, 3);

    // Turn 3: refused before anything goes out.
    let err = ctx.complete(request()).await.unwrap_err();
    assert!(matches!(err, Error::Budget(_)), "{err}");
    assert_eq!(
        received.load(Ordering::SeqCst),
        5,
        "the hard cap binds across completions of one task attempt"
    );
    assert_eq!(ctx.spent().model_calls, CEILING, "both calls are charged");
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(
        usage.committed_calls(),
        CEILING,
        "one base reservation plus one lease: {usage:?}"
    );

    // Crash before commit: recovery charges exactly the two calls.
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

async fn resend_rows(f: &Fixture, task: &TaskId) -> u32 {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM budget_ledger WHERE task_id = ? AND kind = 'resend'",
    )
    .bind(task.as_str())
    .fetch_one(f.store.pool())
    .await
    .unwrap();
    u32::try_from(n).unwrap()
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
