//! Indefinite waiting for a transiently unavailable upstream (SPEC §9.1).
//!
//! A transient failure — a connect, send, or response transport error
//! (including timeouts), or a 408, 425, 429, or 5xx — never ends a completion. The caller's
//! request waits until the upstream serves it, bounded only by the
//! caller's cancellation. A process-wide gate per upstream turns every
//! waiter into one half-open probe at a time, with capped backoff and
//! `Retry-After` honored. Permanent refusals (other 4xx, malformed
//! replies, bad configuration) fail at once.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uruk::provider::{
    Attempts, ChatRequest, HttpProvider, Message, ProbeBackoff, Provider, TransientFault,
    UpstreamGate,
};
use uruk::{Error, Result};

fn ok_body() -> String {
    serde_json::json!({
        "id": "chatcmpl-1", "object": "chat.completion", "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
    })
    .to_string()
}

fn request() -> ChatRequest {
    ChatRequest::new("m", vec![Message::user("hi")])
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

/// One scripted upstream answer.
#[derive(Clone)]
enum Step {
    /// Status, extra headers, body.
    Reply(u16, String, String),
    /// Declare a body of `claimed` bytes, send fewer, then close.
    TruncatedBody(usize),
}

async fn answer(sock: &mut tokio::net::TcpStream, step: &Step) {
    match step {
        Step::Reply(status, headers, body) => {
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(response.as_bytes()).await.unwrap();
        }
        Step::TruncatedBody(claimed) => {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {claimed}\r\nConnection: close\r\n\r\n{{\"id\":"
            );
            sock.write_all(response.as_bytes()).await.unwrap();
        }
    }
    sock.shutdown().await.ok();
}

/// Serve `steps` in order, one per connection; the last step repeats for
/// every later connection. Returns the URL, the server task, and the count
/// of requests received.
async fn serve(steps: Vec<Step>) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let received = Arc::new(AtomicUsize::new(0));
    let counter = received.clone();
    let handle = tokio::spawn(async move {
        let mut index = 0usize;
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            read_request(&mut sock).await;
            counter.fetch_add(1, Ordering::SeqCst);
            let step = steps[index.min(steps.len() - 1)].clone();
            index += 1;
            answer(&mut sock, &step).await;
        }
    });
    (url, handle, received)
}

fn transient(status: u16) -> Step {
    Step::Reply(status, String::new(), "{\"error\":\"later\"}".into())
}

fn ok() -> Step {
    Step::Reply(200, String::new(), ok_body())
}

fn provider(url: &str) -> HttpProvider {
    HttpProvider::new(url, None, "m")
        .unwrap()
        .with_probe_backoff(ProbeBackoff::new(
            Duration::from_millis(1),
            Duration::from_millis(5),
        ))
}

#[tokio::test]
async fn more_than_four_transient_failures_then_success_is_one_served_call() {
    let (url, server, received) = serve(vec![
        transient(503),
        transient(502),
        transient(429),
        transient(500),
        transient(503),
        transient(504),
        ok(),
    ])
    .await;
    let attempts = Attempts::default();

    let response = provider(&url)
        .complete_counted(request(), &attempts)
        .await
        .unwrap();

    assert_eq!(response.text(), "ok");
    assert_eq!(
        received.load(Ordering::SeqCst),
        7,
        "it kept going past four"
    );
    assert_eq!(
        attempts.count(),
        1,
        "one logical model call, however many sends it took"
    );
    assert_eq!(attempts.resends(), 6, "every resend is on record");
    server.abort();
}

#[tokio::test]
async fn http_request_timeout_and_too_early_statuses_are_transient() {
    let (url, server, received) = serve(vec![transient(408), transient(425), ok()]).await;
    provider(&url).complete(request()).await.unwrap();
    assert_eq!(received.load(Ordering::SeqCst), 3);
    server.abort();
}

#[tokio::test]
async fn a_response_cut_off_mid_body_is_transient() {
    let (url, server, received) = serve(vec![Step::TruncatedBody(4096), ok()]).await;
    let response = provider(&url).complete(request()).await.unwrap();
    assert_eq!(response.text(), "ok");
    assert_eq!(
        received.load(Ordering::SeqCst),
        2,
        "a response transport error is retried like a connect error"
    );
    server.abort();
}

#[tokio::test]
async fn a_refused_connection_is_waited_out_until_the_upstream_listens() {
    // Reserve a port, release it, and start listening only later.
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);
    let url = format!("http://{addr}/v1");
    let listener = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let listener = TcpListener::bind(addr).await.unwrap();
        let (mut sock, _) = listener.accept().await.unwrap();
        read_request(&mut sock).await;
        answer(&mut sock, &ok()).await;
    });

    let response = provider(&url).complete(request()).await.unwrap();
    assert_eq!(response.text(), "ok");
    listener.await.unwrap();
}

#[tokio::test]
async fn permanent_client_errors_fail_promptly_without_a_retry() {
    for status in [400u16, 401, 403, 404, 422] {
        let (url, server, received) = serve(vec![
            Step::Reply(status, String::new(), "{\"error\":\"no\"}".into()),
            ok(),
        ])
        .await;
        let err = provider(&url).complete(request()).await.unwrap_err();
        assert!(matches!(err, Error::Provider(_)), "{status}: {err}");
        assert!(err.to_string().contains(&format!("HTTP {status}")), "{err}");
        assert_eq!(
            received.load(Ordering::SeqCst),
            1,
            "{status} is not retried"
        );
        server.abort();
    }
}

#[tokio::test]
async fn the_attempt_count_never_appears_as_a_terminal_reason() {
    let (url, server, _) = serve(vec![
        transient(503),
        transient(503),
        transient(503),
        transient(503),
        transient(503),
        Step::Reply(400, String::new(), "{\"error\":\"bad\"}".into()),
    ])
    .await;
    let err = provider(&url).complete(request()).await.unwrap_err();
    let message = err.to_string();
    assert!(message.contains("HTTP 400"), "{message}");
    assert!(
        !message.contains("attempt"),
        "no 'after N attempts' verdict exists any more: {message}"
    );
    server.abort();
}

#[tokio::test]
async fn retry_after_is_honored_before_the_next_probe() {
    let (url, server, _) = serve(vec![
        Step::Reply(429, "Retry-After: 1\r\n".into(), "slow down".into()),
        ok(),
    ])
    .await;
    let started = Instant::now();
    provider(&url).complete(request()).await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "Retry-After: 1 must hold the probe for about a second, took {:?}",
        started.elapsed()
    );
    server.abort();
}

/// Serve 503s (each held for `hold`) until `recovered` flips, then 200s;
/// records the peak number of simultaneously open requests after the
/// first failure.
async fn serve_outage(
    hold: Duration,
    recovered: Arc<AtomicBool>,
) -> (
    String,
    tokio::task::JoinHandle<()>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let received = Arc::new(AtomicUsize::new(0));
    let (in_flight_c, peak_c, received_c) = (in_flight.clone(), peak.clone(), received.clone());
    let handle = tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let recovered = recovered.clone();
            let in_flight = in_flight_c.clone();
            let peak = peak_c.clone();
            let received = received_c.clone();
            tokio::spawn(async move {
                read_request(&mut sock).await;
                let n = received.fetch_add(1, Ordering::SeqCst) + 1;
                if recovered.load(Ordering::SeqCst) {
                    answer(&mut sock, &ok()).await;
                    return;
                }
                let open = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                // The very first request cannot know the upstream is down;
                // overlap is measured from the second one on.
                if n > 1 {
                    peak.fetch_max(open, Ordering::SeqCst);
                }
                tokio::time::sleep(hold).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                answer(&mut sock, &transient(503)).await;
            });
        }
    });
    (url, handle, received, peak)
}

#[tokio::test]
async fn callers_of_one_upstream_share_the_gate_and_probe_one_at_a_time() {
    let recovered = Arc::new(AtomicBool::new(false));
    let (url, server, received, peak) =
        serve_outage(Duration::from_millis(40), recovered.clone()).await;
    let first = provider(&url);
    let gate = first.upstream_gate().expect("http providers are gated");

    let leader = {
        let p = first.clone();
        tokio::spawn(async move { p.complete(request()).await })
    };
    // Wait for the first failure to close the gate.
    let closed_at = Instant::now();
    while gate.status().is_available() {
        assert!(
            closed_at.elapsed() < Duration::from_secs(5),
            "gate never closed"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Four more callers: a clone, and handles constructed independently
    // for the same upstream.
    let mut followers = Vec::new();
    for i in 0..4 {
        let p = if i % 2 == 0 {
            first.clone()
        } else {
            provider(&url)
        };
        followers.push(tokio::spawn(async move { p.complete(request()).await }));
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    let probes = received.load(Ordering::SeqCst);
    assert!(
        probes >= 3,
        "probing kept going during the outage: {probes}"
    );
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "while the upstream is down, exactly one probe is in flight at a time"
    );

    recovered.store(true, Ordering::SeqCst);
    let done = tokio::time::timeout(Duration::from_secs(5), async {
        leader.await.unwrap().unwrap();
        for f in followers {
            f.await.unwrap().unwrap();
        }
    })
    .await;
    assert!(
        done.is_ok(),
        "every waiter is served once the upstream recovers"
    );
    assert!(gate.status().is_available());
    server.abort();
}

#[tokio::test]
async fn cancellation_while_waiting_is_prompt_and_releases_the_probe() {
    let (url, server, _) = serve(vec![transient(503)]).await;
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_probe_backoff(ProbeBackoff::new(
            Duration::from_secs(30),
            Duration::from_secs(30),
        ));
    let gate = provider.upstream_gate().expect("gated");
    let f = common::fixture(
        uruk::records::Mode::Task,
        uruk::provider::MockProvider::new(),
    )
    .await;
    let cancel = CancellationToken::new();
    let ctx = uruk::agents::AgentContext::new(
        f.store.clone(),
        Arc::new(provider.clone()),
        Arc::new(uruk::prompts::PromptRegistry::new()),
        f.run_id.clone(),
        f.goal.clone(),
        f.plan.clone(),
        f.goal.permissions.clone(),
        7,
        cancel.clone(),
    );

    let call = tokio::spawn(async move { ctx.complete(request()).await });
    while gate.status().is_available() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let cancelled_at = Instant::now();
    cancel.cancel();
    let result: Result<_> = call.await.unwrap();
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert!(
        cancelled_at.elapsed() < Duration::from_millis(500),
        "a 30 s backoff must not delay the stop: {:?}",
        cancelled_at.elapsed()
    );
    assert!(
        !gate.status().probe_in_flight(),
        "a dropped waiter must not leave the half-open probe claimed"
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn probe_backoff_doubles_from_the_base_and_is_capped() {
    let gate = UpstreamGate::new();
    let backoff = ProbeBackoff::new(Duration::from_secs(1), Duration::from_secs(8));
    let fault = TransientFault::status(503, None, "overloaded");

    let mut delays = Vec::new();
    for _ in 0..6 {
        let admission = gate.admit().await;
        let before = tokio::time::Instant::now();
        gate.report_transient(admission, &fault, &backoff);
        let next = gate.status().next_probe_in().expect("a probe is scheduled");
        delays.push(next);
        tokio::time::sleep(next).await;
        assert!(before.elapsed() >= next);
    }
    assert_eq!(
        delays,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(8),
            Duration::from_secs(8),
        ],
        "doubling from the base, never past the cap"
    );
}

#[tokio::test(start_paused = true)]
async fn retry_after_overrides_a_shorter_backoff_and_success_reopens_the_gate() {
    let gate = UpstreamGate::new();
    let backoff = ProbeBackoff::new(Duration::from_secs(1), Duration::from_secs(8));
    let admission = gate.admit().await;
    gate.report_transient(
        admission,
        &TransientFault::status(429, Some(Duration::from_secs(20)), "rate limited"),
        &backoff,
    );
    assert_eq!(
        gate.status().next_probe_in(),
        Some(Duration::from_secs(20)),
        "the upstream's own instruction wins over the backoff"
    );
    assert!(!gate.status().is_available());

    let before = tokio::time::Instant::now();
    let admission = gate.admit().await;
    assert!(
        before.elapsed() >= Duration::from_secs(20),
        "nobody probes before Retry-After elapses"
    );
    gate.report_served(admission);
    assert!(gate.status().is_available());
    assert_eq!(gate.status().next_probe_in(), None);
}

#[tokio::test(start_paused = true)]
async fn a_dropped_probe_frees_the_half_open_slot_for_the_next_waiter() {
    let gate = Arc::new(UpstreamGate::new());
    let backoff = ProbeBackoff::new(Duration::from_secs(1), Duration::from_secs(1));
    let admission = gate.admit().await;
    gate.report_transient(
        admission,
        &TransientFault::status(503, None, "overloaded"),
        &backoff,
    );

    // The elected prober is cancelled while its request is in flight.
    let probe = gate.admit().await;
    assert!(gate.status().probe_in_flight());
    drop(probe);
    assert!(
        !gate.status().probe_in_flight(),
        "dropping the admission releases the probe"
    );

    // The next waiter is elected at once; nobody is left sleeping on a
    // probe that will never report.
    let next = tokio::time::timeout(Duration::from_millis(10), gate.admit()).await;
    assert!(next.is_ok(), "the slot is free for the next waiter");
    assert!(gate.status().probe_in_flight());
}
