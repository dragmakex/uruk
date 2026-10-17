use super::*;
use crate::{provider::MockProvider, store::Store, web::WebConfig};
use axum::{Router, body::to_bytes, http::Request, routing::get};
use tower::ServiceExt;

#[tokio::test]
async fn infrastructure_errors_keep_private_source_detail_in_sqlite_not_on_the_wire() {
    let store = Store::open_in_memory().await.unwrap();
    let state = AppState::new(
        store.clone(),
        Arc::new(MockProvider::new()),
        WebConfig::default(),
    );
    let app = Router::new()
        .route(
            "/fail",
            get(|| async {
                ApiError(crate::Error::Provider(
                    "full private upstream failure: /private/provider.sock".into(),
                ))
            }),
        )
        .layer(axum::middleware::from_fn_with_state(state, record));
    let response = app
        .oneshot(Request::builder().uri("/fail").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("provider.sock"));
    let error: String = sqlx::query_scalar("SELECT error FROM request_logs")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert!(error.contains("full private upstream failure: /private/provider.sock"));
}

#[tokio::test]
async fn positive_timeouts_can_cancel_writes_without_blocking_the_audit_writer() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE cancelled_writes(value TEXT)")
        .execute(store.pool())
        .await
        .unwrap();
    let state = AppState::new(
        store.clone(),
        Arc::new(MockProvider::new()),
        WebConfig::default(),
    );
    for _ in 0..24 {
        let handler_store = store.clone();
        let app = Router::new()
            .route(
                "/cancel-write",
                get(move || {
                    let store = handler_store.clone();
                    async move {
                        let mut tx = store.pool().begin_with("BEGIN IMMEDIATE").await.unwrap();
                        sqlx::query("INSERT INTO cancelled_writes VALUES('must roll back')")
                            .execute(&mut *tx)
                            .await
                            .unwrap();
                        std::future::pending::<()>().await;
                        axum::http::StatusCode::OK
                    }
                }),
            )
            .layer(tower_http::timeout::TimeoutLayer::with_status_code(
                axum::http::StatusCode::REQUEST_TIMEOUT,
                std::time::Duration::from_millis(1),
            ))
            .layer(axum::middleware::from_fn_with_state(state.clone(), record));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/cancel-write")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 408);
            to_bytes(response.into_body(), usize::MAX).await.unwrap();
        })
        .await
        .expect("a cancelled transaction must not stall diagnostic writes");
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM request_logs WHERE status=408 AND outcome='http_error'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(count, 24);
    let writes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cancelled_writes")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(writes, 0);
}
