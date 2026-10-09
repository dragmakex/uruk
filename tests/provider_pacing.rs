//! Request-start pacing for rate-limited upstreams (SwissAI defaults to
//! 14 requests/minute). The pacer spaces request *starts*; it never
//! serializes whole requests. Timer-only tests run under paused Tokio time.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use uruk::provider::{HttpProvider, Provider, StartPacer};

const INTERVAL: Duration = Duration::from_secs(4);

#[test]
fn swissai_endpoints_default_to_14_rpm() {
    for url in [
        "https://api.swissai.cscs.ch/v1",
        "https://SwissAI.cscs.ch/v1",
        "https://api.swiss-ai.example/v1",
    ] {
        let provider = HttpProvider::new(url, None, "m").unwrap();
        assert_eq!(
            provider.requests_per_minute(),
            Some(14),
            "{url} must default to the SwissAI pacing of 14 requests/minute"
        );
    }
}

#[test]
fn other_openai_compatible_endpoints_are_unpaced_by_default() {
    for url in [
        "https://api.openai.com/v1",
        "http://localhost:11434/v1",
        "https://notswissai.example.com/v1",
        "https://swissair.example.com/v1",
    ] {
        let provider = HttpProvider::new(url, None, "m").unwrap();
        assert_eq!(
            provider.requests_per_minute(),
            None,
            "{url} must keep its existing unpaced behavior"
        );
    }
}

#[test]
fn rpm_can_be_overridden_or_disabled() {
    let swissai = HttpProvider::new("https://api.swissai.cscs.ch/v1", None, "m").unwrap();
    let faster = swissai.with_requests_per_minute(NonZeroU32::new(30));
    assert_eq!(faster.requests_per_minute(), Some(30));
    let unpaced = faster.with_requests_per_minute(None);
    assert_eq!(
        unpaced.requests_per_minute(),
        None,
        "the SwissAI default must be explicitly disableable"
    );

    let openai = HttpProvider::new("https://api.openai.com/v1", None, "m")
        .unwrap()
        .with_requests_per_minute(NonZeroU32::new(60));
    assert_eq!(
        openai.requests_per_minute(),
        Some(60),
        "any provider may opt in to pacing"
    );
}

#[tokio::test(start_paused = true)]
async fn first_start_is_immediate() {
    let pacer = StartPacer::new(INTERVAL);
    let before = Instant::now();
    pacer.pace(None).await;
    assert_eq!(Instant::now(), before, "startup must not wait");
}

#[tokio::test(start_paused = true)]
async fn starts_are_spaced_by_the_interval() {
    let pacer = StartPacer::new(INTERVAL);
    let start = Instant::now();
    pacer.pace(None).await;
    pacer.pace(None).await;
    pacer.pace(None).await;
    assert_eq!(
        Instant::now() - start,
        INTERVAL * 2,
        "three starts need exactly two intervals"
    );
}

#[tokio::test(start_paused = true)]
async fn idle_time_does_not_accumulate_a_burst() {
    let pacer = StartPacer::new(INTERVAL);
    pacer.pace(None).await;
    tokio::time::sleep(INTERVAL * 10).await;
    let before = Instant::now();
    pacer.pace(None).await;
    pacer.pace(None).await;
    assert_eq!(
        Instant::now() - before,
        INTERVAL,
        "idle credit allows at most one immediate start"
    );
}

#[tokio::test(start_paused = true)]
async fn concurrent_workers_share_one_schedule() {
    let pacer = Arc::new(StartPacer::new(INTERVAL));
    let t0 = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let pacer = Arc::clone(&pacer);
        handles.push(tokio::spawn(async move {
            pacer.pace(None).await;
            Instant::now() - t0
        }));
    }
    let mut offsets = Vec::new();
    for h in handles {
        offsets.push(h.await.unwrap());
    }
    offsets.sort();
    assert_eq!(
        offsets,
        vec![Duration::ZERO, INTERVAL, INTERVAL * 2, INTERVAL * 3],
        "four workers must start one interval apart"
    );
}

#[tokio::test(start_paused = true)]
async fn a_later_floor_subsumes_the_interval_without_adding_to_it() {
    let pacer = StartPacer::new(INTERVAL);
    pacer.pace(None).await;
    // A Retry-After style floor beyond the next slot: wait exactly until the
    // floor, not floor + interval and not interval + floor.
    let floor = Instant::now() + Duration::from_secs(10);
    let before = Instant::now();
    pacer.pace(Some(floor)).await;
    assert_eq!(
        Instant::now() - before,
        Duration::from_secs(10),
        "the larger of (floor, next slot) is the only wait"
    );
}

#[tokio::test(start_paused = true)]
async fn an_earlier_floor_is_subsumed_by_the_interval() {
    let pacer = StartPacer::new(INTERVAL);
    pacer.pace(None).await;
    let floor = Instant::now() + Duration::from_secs(1);
    let before = Instant::now();
    pacer.pace(Some(floor)).await;
    assert_eq!(
        Instant::now() - before,
        INTERVAL,
        "a floor inside the next slot must not shorten or lengthen pacing"
    );
}

#[tokio::test(start_paused = true)]
async fn a_floor_delays_workers_already_sleeping_on_a_reserved_slot() {
    let pacer = Arc::new(StartPacer::new(INTERVAL));
    let t0 = Instant::now();
    pacer.pace(None).await;
    // A worker reserves the t0+4s slot and goes to sleep on it.
    let waiter = tokio::spawn({
        let pacer = Arc::clone(&pacer);
        async move {
            pacer.pace(None).await;
            Instant::now() - t0
        }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    // A 429 elsewhere imposes Retry-After until t0+30s. The sleeping worker
    // must not fire inside the server's declared backoff window.
    let floor = t0 + Duration::from_secs(30);
    pacer.pace(Some(floor)).await;
    let waited = waiter.await.unwrap();
    assert!(
        waited >= Duration::from_secs(30),
        "an already-waiting worker must honor a later Retry-After floor \
         (started after {waited:?})"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_waiter_does_not_poison_the_schedule() {
    let pacer = Arc::new(StartPacer::new(INTERVAL));
    pacer.pace(None).await;
    let waiter = tokio::spawn({
        let pacer = Arc::clone(&pacer);
        async move { pacer.pace(None).await }
    });
    // Let the waiter reserve its slot, then cancel it mid-sleep.
    tokio::time::sleep(Duration::from_secs(1)).await;
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());

    // The next caller still gets a slot and still respects spacing.
    let before = Instant::now();
    tokio::time::timeout(INTERVAL * 3, pacer.pace(None))
        .await
        .unwrap();
    let waited = Instant::now() - before;
    assert!(
        waited <= INTERVAL * 2,
        "cancellation must not stall the schedule (waited {waited:?})"
    );
}

// ---------------------------------------------------------------------------
// End-to-end pacing through `HttpProvider::complete` against a local stand-in
// server. Real time, so assertions are mostly lower bounds: pacing guarantees
// minimum spacing between request starts.
// ---------------------------------------------------------------------------

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn completion_body() -> String {
    serde_json::json!({
        "id": "chatcmpl-1",
        "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
    .to_string()
}

fn http_response(status: u16, extra_headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra_headers}\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Read one full HTTP request (head + content-length body) from the socket.
async fn read_http_request(sock: &mut TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut tmp).await.unwrap();
        assert!(n > 0, "client hung up mid-request");
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
        assert!(n > 0, "client hung up mid-body");
        buf.extend_from_slice(&tmp[..n]);
    }
}

/// Serve the responses in order, one connection each, recording when each
/// connection was accepted.
async fn serve_timed(
    responses: Vec<(u16, String, String)>,
) -> (String, tokio::task::JoinHandle<Vec<Instant>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut accepted = Vec::new();
        for (status, extra_headers, body) in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            accepted.push(Instant::now());
            read_http_request(&mut sock).await;
            sock.write_all(http_response(status, &extra_headers, &body).as_bytes())
                .await
                .unwrap();
            sock.shutdown().await.ok();
        }
        accepted
    });
    (url, handle)
}

/// Accept `n` connections and read each request fully *before* answering any
/// of them, recording accept times. Completes only if `n` requests are in
/// flight simultaneously — proof the client is not single-flight.
async fn serve_gated(n: usize) -> (String, tokio::task::JoinHandle<Vec<Instant>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut socks = Vec::new();
        let mut accepted = Vec::new();
        for _ in 0..n {
            let (mut sock, _) = listener.accept().await.unwrap();
            accepted.push(Instant::now());
            read_http_request(&mut sock).await;
            socks.push(sock);
        }
        for mut sock in socks {
            sock.write_all(http_response(200, "", &completion_body()).as_bytes())
                .await
                .unwrap();
            sock.shutdown().await.ok();
        }
        accepted
    });
    (url, handle)
}

fn request() -> uruk::provider::ChatRequest {
    uruk::provider::ChatRequest::new("m", vec![uruk::provider::Message::user("hi")])
}

#[tokio::test]
async fn paced_first_attempts_are_spaced_across_cloned_handles() {
    let (url, server) = serve_gated(2).await;
    // 240 rpm = one start per 250 ms; cloned handles share the schedule.
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_requests_per_minute(NonZeroU32::new(240));
    let clone = provider.clone();

    let a = tokio::spawn(async move { provider.complete(request()).await });
    let b = tokio::spawn(async move { clone.complete(request()).await });
    let joined = tokio::time::timeout(Duration::from_secs(10), async {
        (a.await.unwrap(), b.await.unwrap())
    })
    .await
    .expect("paced requests must overlap in flight, not serialize");
    joined.0.unwrap();
    joined.1.unwrap();

    let accepted = server.await.unwrap();
    let gap = accepted[1] - accepted[0];
    assert!(
        gap >= Duration::from_millis(200),
        "first attempts from cloned handles must share pacing (gap {gap:?})"
    );
}

#[tokio::test]
async fn retries_go_through_the_shared_pacer() {
    let (url, server) = serve_timed(vec![
        (500, String::new(), "boom".into()),
        (200, String::new(), completion_body()),
    ])
    .await;
    // Backoff base of 1 ms: any observed spacing comes from the pacer.
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_requests_per_minute(NonZeroU32::new(240))
        .with_max_retries(2)
        .with_retry_base(Duration::from_millis(1));

    provider.complete(request()).await.unwrap();

    let accepted = server.await.unwrap();
    let gap = accepted[1] - accepted[0];
    assert!(
        gap >= Duration::from_millis(200),
        "a retry must wait for the shared start schedule (gap {gap:?})"
    );
}

#[tokio::test]
async fn retry_after_seconds_is_honored_on_429() {
    let (url, server) = serve_timed(vec![
        (429, "Retry-After: 1\r\n".into(), "slow down".into()),
        (200, String::new(), completion_body()),
    ])
    .await;
    // No pacing and a 1 ms backoff base: only Retry-After can explain a wait.
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_max_retries(2)
        .with_retry_base(Duration::from_millis(1));

    provider.complete(request()).await.unwrap();

    let accepted = server.await.unwrap();
    let gap = accepted[1] - accepted[0];
    assert!(
        gap >= Duration::from_millis(950),
        "Retry-After: 1 must delay the retry by about a second (gap {gap:?})"
    );
}

#[tokio::test]
async fn retry_after_and_the_pacer_wait_concurrently_not_additively() {
    let (url, server) = serve_timed(vec![
        (429, "Retry-After: 1\r\n".into(), "slow down".into()),
        (200, String::new(), completion_body()),
    ])
    .await;
    // 30 rpm = one start per 2 s. The retry must start at the next pacer
    // slot (~2 s, which already satisfies Retry-After: 1), not at ~3 s as a
    // pacer wait followed by a separate Retry-After sleep would.
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_requests_per_minute(NonZeroU32::new(30))
        .with_max_retries(2)
        .with_retry_base(Duration::from_millis(1));

    provider.complete(request()).await.unwrap();

    let accepted = server.await.unwrap();
    let gap = accepted[1] - accepted[0];
    assert!(
        gap >= Duration::from_millis(1800),
        "the pacer slot must still be respected (gap {gap:?})"
    );
    assert!(
        gap < Duration::from_millis(2900),
        "Retry-After and the pacer must not sleep back to back (gap {gap:?})"
    );
}

#[tokio::test]
async fn unpaced_providers_keep_starting_concurrently() {
    let (url, server) = serve_gated(2).await;
    let provider = HttpProvider::new(&url, None, "m").unwrap();
    let clone = provider.clone();

    let a = tokio::spawn(async move { provider.complete(request()).await });
    let b = tokio::spawn(async move { clone.complete(request()).await });
    let joined = tokio::time::timeout(Duration::from_secs(10), async {
        (a.await.unwrap(), b.await.unwrap())
    })
    .await
    .expect("unpaced concurrent requests must both reach the server");
    joined.0.unwrap();
    joined.1.unwrap();

    let accepted = server.await.unwrap();
    let gap = accepted[1] - accepted[0];
    assert!(
        gap < Duration::from_millis(1000),
        "a provider without an RPM must not be paced (gap {gap:?})"
    );
}

#[test]
fn differing_intervals_for_one_upstream_share_one_schedule() {
    let loose = StartPacer::shared("pacing-conflict.example", Duration::from_secs(2));
    let strict = StartPacer::shared("pacing-conflict.example", Duration::from_secs(4));
    assert!(
        Arc::ptr_eq(&loose, &strict),
        "one upstream identity must never get independent schedules, \
         whatever rates construction received"
    );
    assert_eq!(
        loose.interval(),
        Duration::from_secs(4),
        "the stricter interval must win even when the looser one resolved first"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stricter_rate_binds_handles_that_configured_a_looser_one() {
    let loose = StartPacer::shared("pacing-conflict-binds.example", INTERVAL / 4);
    let _strict = StartPacer::shared("pacing-conflict-binds.example", INTERVAL);
    let start = Instant::now();
    loose.pace(None).await;
    loose.pace(None).await;
    assert_eq!(
        Instant::now() - start,
        INTERVAL,
        "the stricter configured allowance must bind every handle of the identity"
    );
}

#[tokio::test(start_paused = true)]
async fn shared_registry_returns_one_pacer_per_upstream_identity() {
    let a = StartPacer::shared("pacing-test.example", INTERVAL);
    let b = StartPacer::shared("pacing-test.example", INTERVAL);
    let other = StartPacer::shared("pacing-test.other.example", INTERVAL);
    assert!(Arc::ptr_eq(&a, &b), "same upstream must share one schedule");
    assert!(
        !Arc::ptr_eq(&a, &other),
        "distinct upstreams must not share a schedule"
    );
}
