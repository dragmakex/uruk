//! The architecture UI receives only owner-scoped, recorded run facts.
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;
use uruk::provider::MockProvider;
use uruk::records::{Mode, StopCondition};
use uruk::web::{AppState, WebConfig, router};

#[tokio::test]
async fn snapshot_exposes_the_brief_permissions_and_plan_stops_without_private_paths() {
    let f = common::fixture_owned(Mode::Campaign, MockProvider::new(), common::BROWSER_TOKEN).await;
    let mut goal = f.goal.clone();
    goal.rubric.preferences = vec!["Prefer replication".into()];
    goal.rubric.attributes = vec!["Testability".into()];
    goal.rubric.constraints = vec!["No animal work".into()];
    goal.rubric.acceptance = vec!["Name a measurable endpoint".into()];
    goal.permissions.network = true;
    goal.permissions.allowed_tools = vec!["literature.discover".into()];
    goal.permissions.read_paths = vec!["/private/architecture-fixture".into()];
    goal.permissions.write_paths = vec!["/private/architecture-output".into()];
    sqlx::query("UPDATE goals SET body = ? WHERE id = ?")
        .bind(serde_json::to_string(&goal).unwrap())
        .bind(goal.id.as_str())
        .execute(f.store.pool())
        .await
        .unwrap();
    let mut plan = f.plan.clone();
    plan.stopping_conditions = vec![
        StopCondition::RoundsCompleted,
        StopCondition::BudgetExhausted,
    ];
    sqlx::query("UPDATE plans SET body = ? WHERE id = ?")
        .bind(serde_json::to_string(&plan).unwrap())
        .bind(plan.id.as_str())
        .execute(f.store.pool())
        .await
        .unwrap();
    let app = router(AppState::new(
        f.store.clone(),
        f.provider.clone(),
        WebConfig::default(),
    ));
    let uri = format!("/api/runs/{}", f.run_id);
    let request = |token: &str| {
        Request::builder()
            .uri(&uri)
            .header(header::COOKIE, format!("uruk_browser={token}"))
            .body(Body::empty())
            .unwrap()
    };
    let response = app
        .clone()
        .oneshot(request(common::BROWSER_TOKEN))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let snapshot = &body["snapshot"];
    assert_eq!(
        snapshot["goal"]["rubric"],
        serde_json::to_value(&goal.rubric).unwrap()
    );
    assert_eq!(
        snapshot["goal"]["permissions"],
        serde_json::json!({"network":true, "execute":false, "allowed_tools":["literature.discover"]})
    );
    assert_eq!(
        snapshot["plan"]["stopping_conditions"],
        serde_json::json!(["rounds_completed", "budget_exhausted"])
    );
    assert!(!String::from_utf8_lossy(&bytes).contains("/private/architecture-"));
    assert_eq!(f.provider.call_count(), 0);
    let response = app
        .oneshot(request(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
