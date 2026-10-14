//! The OpenAI-compatible HTTP provider against a local stand-in server
//! (SPEC §9.1: wire shape, transient waiting, usage accounting). No live
//! network. Outage behaviour in depth is in `provider_availability.rs`.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uruk::provider::{ChatRequest, HttpProvider, Message, ProbeBackoff, Provider};

/// One captured HTTP request.
struct Captured {
    head: String,
    body: String,
}

/// Serve the given (status, body) responses in order, one per connection,
/// and return the captured requests.
async fn serve(responses: Vec<(u16, String)>) -> (String, tokio::task::JoinHandle<Vec<Captured>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut captured = Vec::new();
        for (status, body) in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let head_end = loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = find(&buf, b"\r\n\r\n") {
                    break Some(pos + 4);
                }
            };
            let head_end = head_end.expect("request head");
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
            let body_bytes = &buf[head_end..(head_end + length).min(buf.len())];
            captured.push(Captured {
                head,
                body: String::from_utf8_lossy(body_bytes).into_owned(),
            });
            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(response.as_bytes()).await.unwrap();
            sock.shutdown().await.ok();
        }
        captured
    });
    (url, handle)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn completion_body(text: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
    })
    .to_string()
}

async fn serve_stream(
    fragments: Vec<String>,
    hold_open: Option<Duration>,
) -> (String, tokio::task::JoinHandle<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let head_end = loop {
            let n = sock.read(&mut tmp).await.unwrap();
            assert_ne!(n, 0, "request ended before its head");
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = find(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let length: usize = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        while buf.len() < head_end + length {
            let n = sock.read(&mut tmp).await.unwrap();
            assert_ne!(n, 0, "request ended before its body");
            buf.extend_from_slice(&tmp[..n]);
        }
        let captured = Captured {
            head,
            body: String::from_utf8_lossy(&buf[head_end..head_end + length]).into_owned(),
        };
        sock.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
        for fragment in fragments {
            sock.write_all(fragment.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
            tokio::task::yield_now().await;
        }
        if let Some(duration) = hold_open {
            tokio::time::sleep(duration).await;
        }
        sock.shutdown().await.ok();
        captured
    });
    (url, handle)
}

#[tokio::test]
async fn streaming_completions_survive_fragmented_sse_and_preserve_usage() {
    let chunks = [
        serde_json::json!({
            "id": "chatcmpl-stream",
            "model": "test-model",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hello "}, "finish_reason": null}]
        }),
        serde_json::json!({
            "id": "chatcmpl-stream",
            "model": "test-model",
            "choices": [{"index": 0, "delta": {"content": "world"}, "finish_reason": "stop"}]
        }),
        serde_json::json!({
            "id": "chatcmpl-stream",
            "model": "test-model",
            "choices": [],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
        }),
    ];
    let wire = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunks[0], chunks[1], chunks[2]
    );
    let splits = [7, 31, 79, wire.len() - 3];
    let mut start = 0;
    let mut fragments = Vec::new();
    for end in splits.into_iter().chain(std::iter::once(wire.len())) {
        fragments.push(wire[start..end].to_string());
        start = end;
    }
    let (url, server) = serve_stream(fragments, None).await;
    let provider = HttpProvider::new(&url, None, "test-model")
        .unwrap()
        .with_streaming(true);

    let response = provider
        .complete(ChatRequest::new("test-model", vec![Message::user("hi")]))
        .await
        .unwrap();

    assert_eq!(response.text(), "hello world");
    assert_eq!(response.usage.total_tokens, 15);
    let captured = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn streaming_returns_at_done_without_waiting_for_http_eof() {
    let wire = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "id": "chatcmpl-stream",
            "model": "test-model",
            "choices": [{"index": 0, "delta": {"content": "done"}, "finish_reason": "stop"}]
        }),
        serde_json::json!({
            "choices": [],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
    );
    let (url, server) = serve_stream(vec![wire], Some(Duration::from_secs(1))).await;
    let provider = HttpProvider::new(&url, None, "test-model")
        .unwrap()
        .with_streaming(true);

    let response = tokio::time::timeout(
        Duration::from_millis(200),
        provider.complete(ChatRequest::new("test-model", vec![Message::user("hi")])),
    )
    .await
    .expect("[DONE] must finish before the server closes the HTTP body")
    .unwrap();
    assert_eq!(response.text(), "done");
    server.abort();
}

#[tokio::test]
async fn streaming_falls_back_to_a_json_completion_when_the_server_ignores_stream() {
    let (url, server) = serve(vec![(200, completion_body("fallback"))]).await;
    let provider = HttpProvider::new(&url, None, "test-model")
        .unwrap()
        .with_streaming(true);

    let response = tokio::time::timeout(
        Duration::from_millis(200),
        provider.complete(ChatRequest::new("test-model", vec![Message::user("hi")])),
    )
    .await
    .expect("JSON fallback must not enter the provider retry loop")
    .unwrap();
    assert_eq!(response.text(), "fallback");
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn posts_chat_completions_with_auth_and_parses_usage() {
    let (url, server) = serve(vec![(200, completion_body("hello"))]).await;
    let provider = HttpProvider::new(&url, Some("secret-key".into()), "test-model").unwrap();

    let response = provider
        .complete(ChatRequest::new("test-model", vec![Message::user("hi")]).with_seed(7))
        .await
        .unwrap();
    assert_eq!(response.text(), "hello");
    assert_eq!(response.usage.total_tokens, 15);
    let cost = response.cost();
    assert_eq!(cost.model_calls, 1);
    assert_eq!(cost.prompt_tokens, 12);
    assert_eq!(cost.cost_usd, None, "cost stays unknown unless reported");

    let captured = server.await.unwrap();
    assert_eq!(captured.len(), 1);
    let req = &captured[0];
    assert!(
        req.head.starts_with("POST /v1/chat/completions HTTP/1.1"),
        "{}",
        req.head
    );
    assert!(
        req.head
            .to_ascii_lowercase()
            .contains("authorization: bearer secret-key"),
        "{}",
        req.head
    );
    let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hi");
    assert_eq!(body["seed"], 7);
    assert!(
        body.get("reasoning_effort").is_none(),
        "not sent unless configured"
    );
    assert_eq!(
        provider.name(),
        format!(
            "openai-compatible@{}",
            url.trim_start_matches("http://").trim_end_matches("/v1")
        )
    );
}

#[tokio::test]
async fn reasoning_effort_is_sent_when_configured() {
    let (url, server) = serve(vec![(200, completion_body("ok"))]).await;
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_reasoning_effort(Some("none".into()));
    provider
        .complete(ChatRequest::new("m", vec![Message::user("hi")]))
        .await
        .unwrap();
    let captured = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&captured[0].body).unwrap();
    assert_eq!(body["reasoning_effort"], "none");
    assert_eq!(body["model"], "m", "the rest of the request is unchanged");
}

#[tokio::test]
async fn transient_failures_are_waited_out_until_served() {
    let (url, server) = serve(vec![
        (503, "{\"error\": \"overloaded\"}".into()),
        (429, "{\"error\": \"rate limited\"}".into()),
        (200, completion_body("finally")),
    ])
    .await;
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_probe_backoff(ProbeBackoff::new(
            Duration::from_millis(5),
            Duration::from_millis(5),
        ));

    let response = provider
        .complete(ChatRequest::new("m", vec![Message::user("hi")]))
        .await
        .unwrap();
    assert_eq!(response.text(), "finally");
    assert_eq!(server.await.unwrap().len(), 3, "two probes, then served");
}

#[tokio::test]
async fn a_persistent_outage_never_ends_the_call_on_its_own() {
    let (url, server) = serve((0..12).map(|_| (500, "boom".to_string())).collect()).await;
    let provider = HttpProvider::new(&url, None, "m")
        .unwrap()
        .with_probe_backoff(ProbeBackoff::new(
            Duration::from_millis(5),
            Duration::from_millis(5),
        ));

    let outcome = tokio::time::timeout(
        Duration::from_millis(600),
        provider.complete(ChatRequest::new("m", vec![Message::user("hi")])),
    )
    .await;
    assert!(
        outcome.is_err(),
        "the call is still waiting for the upstream, not failed: {outcome:?}"
    );
    server.abort();
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let (url, server) = serve(vec![(400, "{\"error\": \"bad request\"}".into())]).await;
    let provider = HttpProvider::new(&url, None, "m").unwrap();

    let err = provider
        .complete(ChatRequest::new("m", vec![Message::user("hi")]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("HTTP 400"), "{err}");
    assert_eq!(server.await.unwrap().len(), 1, "a 4xx must not be retried");
}

#[tokio::test]
async fn unparseable_success_is_a_provider_error() {
    let (url, _server) = serve(vec![(200, "not json".into())]).await;
    let provider = HttpProvider::new(&url, None, "m").unwrap();
    let err = provider
        .complete(ChatRequest::new("m", vec![Message::user("hi")]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unparseable"), "{err}");
}

#[test]
fn from_env_requires_a_model_when_a_url_is_set() {
    // Environment is process-global; run the checks in one test.
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var(uruk::provider::ENV_URL, "http://127.0.0.1:1/v1") };
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var(uruk::provider::ENV_MODEL) };
    assert!(
        HttpProvider::from_env().is_err(),
        "a URL without a model is a misconfiguration"
    );

    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var(uruk::provider::ENV_MODEL, "m") };
    assert!(HttpProvider::from_env().unwrap().is_some());

    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var(uruk::provider::ENV_REASONING, "none") };
    assert_eq!(
        HttpProvider::from_env()
            .unwrap()
            .unwrap()
            .reasoning_effort(),
        Some("none")
    );
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var(uruk::provider::ENV_REASONING) };

    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var(uruk::provider::ENV_RPM, "20") };
    assert_eq!(
        HttpProvider::from_env()
            .unwrap()
            .unwrap()
            .requests_per_minute(),
        Some(20),
        "{} opts any provider in to pacing",
        uruk::provider::ENV_RPM
    );

    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var(uruk::provider::ENV_URL, "https://api.swissai.cscs.ch/v1") };
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var(uruk::provider::ENV_RPM, "0") };
    assert_eq!(
        HttpProvider::from_env()
            .unwrap()
            .unwrap()
            .requests_per_minute(),
        None,
        "0 disables pacing, including the SwissAI default"
    );
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var(uruk::provider::ENV_RPM) };
    assert_eq!(
        HttpProvider::from_env()
            .unwrap()
            .unwrap()
            .requests_per_minute(),
        Some(14),
        "a SwissAI URL defaults to 14 requests/minute without any RPM setting"
    );
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var(uruk::provider::ENV_RPM, "not-a-number") };
    assert!(
        HttpProvider::from_env().is_err(),
        "an unparseable RPM is a misconfiguration, not a silent default"
    );
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var(uruk::provider::ENV_RPM) };

    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var(uruk::provider::ENV_URL) };
    assert!(
        HttpProvider::from_env().unwrap().is_none(),
        "no URL means no HTTP provider"
    );
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var(uruk::provider::ENV_MODEL) };
}
