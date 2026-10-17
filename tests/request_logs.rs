//! Durable diagnostics are independently queried from SQLite, not tracing output.
mod common;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use common::*;
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;
use uruk::{
    provider::{ChatRequest, HttpProvider, Message, MockProvider, ProbeBackoff},
    records::Mode,
    web::{AppState, WebConfig, router},
};

#[tokio::test]
async fn every_inbound_request_has_a_persistent_correlated_result() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let app = router(AppState::new(
        f.store.clone(),
        f.provider.clone(),
        WebConfig::default(),
    ));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs?token=QUERY_SECRET")
                .header("content-type", "application/json")
                .header("authorization", "Bearer AUTH_SECRET")
                .header("cookie", "private=COOKIE_SECRET")
                .body(Body::from(r#"{"goal":"","password":"BODY_SECRET"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(response.status(), 422);
    to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let (logged_id, status, outcome, request, response): (String, i64, String, String, String) =
        sqlx::query_as("SELECT request_id,status,outcome,request,response FROM request_logs WHERE direction='inbound'")
            .fetch_one(f.store.pool()).await.expect("every request must be recorded in SQLite");
    assert_eq!(logged_id, request_id);
    assert_eq!(status, 422);
    assert_eq!(outcome, "http_error");
    assert!(
        response.contains("password"),
        "error detail was lost: {response}"
    );
    for secret in [
        "QUERY_SECRET",
        "AUTH_SECRET",
        "COOKIE_SECRET",
        "BODY_SECRET",
    ] {
        assert!(
            !format!("{request}{response}").contains(secret),
            "persisted credential: {secret}"
        );
    }
    assert!(request.contains("REDACTED"));
    assert!(request.contains("goal"));
}

#[tokio::test]
async fn every_model_retry_preserves_response_detail_and_run_correlation() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for (status, body) in [(503, "diagnostic upstream overload".to_string()), (200,
            serde_json::json!({"choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}).to_string())] {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 8192];
                let n = sock.read(&mut bytes).await.unwrap();
                assert!(n > 0, "client closed before sending its request");
                request.extend_from_slice(&bytes[..n]);
                if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]);
                    let length = head.lines().find_map(|line| line.to_ascii_lowercase()
                        .strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    if request.len() >= end + 4 + length { break; }
                }
            }
            sock.write_all(format!("HTTP/1.1 {status} OK\r\nx-request-id: upstream-trace\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let mut ctx = context(&f, Default::default());
    ctx.provider = Arc::new(
        HttpProvider::new(url, Some("PROVIDER_SECRET".into()), "m")
            .unwrap()
            .with_probe_backoff(ProbeBackoff::new(
                Duration::from_millis(1),
                Duration::from_millis(2),
            )),
    );
    assert_eq!(
        ctx.complete(ChatRequest::new(
            "m",
            vec![Message::user("diagnostic prompt")]
        ))
        .await
        .unwrap()
        .text(),
        "ok"
    );
    server.await.unwrap();
    let rows: Vec<(String, String, i64, String, String, String)> = sqlx::query_as(
        "SELECT logical_id,run_id,status,outcome,request,response FROM request_logs WHERE direction='provider' ORDER BY started_at,id")
        .fetch_all(f.store.pool()).await.expect("each physical attempt must be logged");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, rows[1].0);
    assert_eq!(rows[0].1, f.run_id.as_str());
    assert_eq!(rows[0].2, 503);
    assert_eq!(rows[0].3, "retryable");
    assert!(rows[0].5.contains("diagnostic upstream overload"));
    assert!(rows[0].5.contains("upstream-trace"));
    assert!(rows[0].4.contains("diagnostic prompt"));
    assert!(!format!("{rows:?}").contains("PROVIDER_SECRET"));
    assert_eq!(rows[1].3, "success");
    let delay: Option<i64> =
        sqlx::query_scalar("SELECT retry_delay_ms FROM request_logs WHERE outcome='retryable'")
            .fetch_one(f.store.pool())
            .await
            .unwrap();
    assert!(
        delay.is_some(),
        "the actual recovery delay must be persisted"
    );
}

#[tokio::test]
async fn health_unknown_routes_and_method_errors_are_all_logged() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let app = router(AppState::new(
        f.store.clone(),
        f.provider.clone(),
        WebConfig::default(),
    ));
    for (method, path, status) in [
        ("GET", "/api/health", 200),
        ("GET", "/missing", 404),
        ("PUT", "/api/health", 405),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        to_bytes(response.into_body(), usize::MAX).await.unwrap();
    }
    let rows: Vec<i64> = sqlx::query_scalar("SELECT status FROM request_logs ORDER BY started_at")
        .fetch_all(f.store.pool())
        .await
        .unwrap();
    assert_eq!(rows, [200, 404, 405]);
    // A second store connection sees the committed facts, without journal logs.
    let reopened = uruk::store::Store::open(f.store.project_dir().join(".uruk/state.sqlite"))
        .await
        .unwrap();
    let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM request_logs WHERE finished_at IS NOT NULL AND duration_ms IS NOT NULL").fetch_one(reopened.pool()).await.unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
async fn a_cancelled_model_request_keeps_its_started_record_and_unknown_outcome() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut ctx = context(&f, cancel.clone());
    ctx.provider = Arc::new(HttpProvider::new(url, None, "m").unwrap());
    let call = tokio::spawn(async move {
        ctx.complete(ChatRequest::new("m", vec![Message::user("hi")]))
            .await
    });
    let (_socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let started: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM request_logs WHERE direction='provider' AND outcome='in_progress'",
    )
    .fetch_one(f.store.pool())
    .await
    .unwrap();
    assert_eq!(
        started, 1,
        "the audit start must commit before the request goes out"
    );
    cancel.cancel();
    assert!(matches!(call.await.unwrap(), Err(uruk::Error::Cancelled)));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let outcome: String =
                sqlx::query_scalar("SELECT outcome FROM request_logs WHERE direction='provider'")
                    .fetch_one(f.store.pool())
                    .await
                    .unwrap();
            if outcome == "cancelled" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled attempt must be finalized");
}

#[tokio::test]
async fn sse_is_streamed_immediately_and_disconnect_is_audited() {
    use futures_util::StreamExt;
    let token = "a".repeat(64);
    let f = fixture_owned(Mode::Campaign, MockProvider::new(), &token).await;
    let app = router(AppState::new(
        f.store.clone(),
        f.provider.clone(),
        WebConfig::default(),
    ));
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/runs/{}/events", f.run_id))
                .header("cookie", format!("{}={token}", uruk::web::COOKIE_NAME))
                .header("accept-encoding", "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers().get("content-encoding").is_none(),
        "SSE must not be buffered by gzip"
    );
    let mut stream = response.into_body().into_data_stream();
    let bytes = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("snapshot"));
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM request_logs")
        .fetch_one(f.store.pool())
        .await
        .unwrap();
    assert_eq!(outcome, "in_progress");
    drop(stream);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (outcome, response): (String, Option<String>) =
                sqlx::query_as("SELECT outcome,response FROM request_logs")
                    .fetch_one(f.store.pool())
                    .await
                    .unwrap();
            if outcome == "cancelled" {
                let detail = response.unwrap();
                assert!(detail.contains("snapshot"));
                assert!(!detail.contains(&token));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_model_request_is_not_dispatched_if_its_start_cannot_be_logged() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut ctx = context(&f, Default::default());
    ctx.provider = Arc::new(
        HttpProvider::new(
            format!("http://{}", listener.local_addr().unwrap()),
            None,
            "m",
        )
        .unwrap(),
    );
    sqlx::query("DROP TABLE request_logs")
        .execute(f.store.pool())
        .await
        .unwrap();
    assert!(matches!(
        ctx.complete(ChatRequest::new("m", vec![Message::user("hi")]))
            .await,
        Err(uruk::Error::Storage(_))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "unlogged request reached the provider"
    );
}
