//! Request-time pagination over real persisted records, without model calls.
mod common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use uruk::{
    provider::MockProvider,
    records::*,
    web::{AppState, WebConfig, router},
};

async fn fixture() -> (Router, common::Fixture) {
    let f = common::fixture_owned(Mode::Campaign, MockProvider::new(), common::BROWSER_TOKEN).await;
    let mut items = Vec::new();
    for n in 0..7 {
        let item = common::seed_item(&f, &format!("Candidate {n}")).await;
        f.store.ensure_rating(&item.id, &f.plan.id).await.unwrap();
        sqlx::query("UPDATE ratings SET rating = ? WHERE item_id = ?")
            .bind(1500.0 - n as f64)
            .bind(item.id.as_str())
            .execute(f.store.pool())
            .await
            .unwrap();
        items.push(item);
    }
    for n in 0..7 {
        let m = Match {
            id: MatchId::from_raw(format!("match_page_{n:02}")),
            schema_version: SCHEMA_VERSION,
            run_id: f.run_id.clone(),
            item_a: items[0].id.clone(),
            item_b: items[1].id.clone(),
            rubric_id: f.plan.id.clone(),
            evidence_snapshot: ContentHash::of_str("fixture"),
            order_randomized: false,
            method: MatchMethod::Pairwise,
            judge_model: "fixture".into(),
            outcome: MatchOutcome::InsufficientBasis,
            rationale: format!("Match reason {n}"),
            rating_a_before: 0.0,
            rating_b_before: 0.0,
            rating_a_after: 0.0,
            rating_b_after: 0.0,
            produced_by: None,
            created_at: time::macros::datetime!(2026-01-01 00:00 UTC),
        };
        f.store
            .commit_match(&m, &format!("page-{n}"), DEFAULT_K)
            .await
            .unwrap();
        let d = Decision {
            id: DecisionId::from_raw(format!("decision_page_{n:02}")),
            schema_version: SCHEMA_VERSION,
            run_id: f.run_id.clone(),
            kind: DecisionKind::Scheduling,
            actor: Actor::Supervisor,
            reason: format!("Decision reason {n}"),
            referenced: vec![],
            payload: json!({}),
            created_at: time::macros::datetime!(2026-01-01 00:00 UTC),
        };
        sqlx::query(
            "INSERT INTO decisions (id, run_id, kind, body, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(d.id.as_str())
        .bind(f.run_id.as_str())
        .bind(d.kind.as_str())
        .bind(serde_json::to_string(&d).unwrap())
        .bind("2026-01-01T00:00:00Z")
        .execute(f.store.pool())
        .await
        .unwrap();
    }
    let app = router(AppState::new(
        f.store.clone(),
        f.provider.clone(),
        WebConfig::default(),
    ));
    (app, f)
}

async fn read(app: &Router, uri: &str, owner: &str) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, format!("uruk_browser={owner}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn history_routes_return_only_the_requested_slice_with_stable_ties() {
    let (app, f) = fixture().await;
    for (route, envelope, field, first, sixth) in [
        (
            "tournament",
            "tournament",
            "matches",
            "match_page_06",
            "match_page_01",
        ),
        (
            "decisions",
            "ledger",
            "decisions",
            "decision_page_00",
            "decision_page_05",
        ),
    ] {
        let base = format!("/api/runs/{}/{route}", f.run_id);
        let (status, body) = read(&app, &base, common::BROWSER_TOKEN).await;
        assert_eq!(status, StatusCode::OK);
        let page = &body[envelope];
        assert_eq!(page["total"], 7);
        assert_eq!(page["limit"], 5);
        assert_eq!(page["offset"], 0);
        assert_eq!(page[field].as_array().unwrap().len(), 5);
        assert_eq!(page[field][0]["id"], first);
        let through = page["through"]
            .as_i64()
            .expect("append-only history watermark");
        let (status, body) = read(
            &app,
            &format!("{base}?offset=5&limit=5&through={through}"),
            common::BROWSER_TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[envelope][field].as_array().unwrap().len(), 2);
        assert_eq!(body[envelope][field][0]["id"], sixth);
        assert_eq!(body[envelope]["offset"], 5);
    }
}

#[tokio::test]
async fn candidate_pages_are_ranked_in_sql_and_include_review_facts() {
    let (app, f) = fixture().await;
    let items = f.store.list_items(&f.run_id).await.unwrap();
    common::seed_review(
        &f,
        items
            .iter()
            .find(|item| item.title == "Candidate 6")
            .unwrap(),
    )
    .await;
    let base = format!("/api/runs/{}/candidates", f.run_id);
    let (status, body) = read(
        &app,
        &format!("{base}?offset=5&limit=5"),
        common::BROWSER_TOKEN,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page = &body["candidates"];
    assert_eq!(page["total"], 7);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["items"][0]["title"], "Candidate 5");
    assert_eq!(page["items"][1]["title"], "Candidate 6");
    assert_eq!(page["items"][1]["review_count"], 1);
    assert_eq!(page["items"][0]["assessment"], "untested");
    assert_eq!(page["items"][0]["assessment_basis"], Value::Null);
}

#[tokio::test]
async fn snapshot_keeps_only_the_top_five_without_loading_history_bodies() {
    let (app, f) = fixture().await;
    sqlx::query("UPDATE matches SET body = 'not-json' WHERE run_id = ?")
        .bind(f.run_id.as_str())
        .execute(f.store.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE decisions SET body = 'not-json' WHERE run_id = ?")
        .bind(f.run_id.as_str())
        .execute(f.store.pool())
        .await
        .unwrap();
    let (status, body) = read(
        &app,
        &format!("/api/runs/{}", f.run_id),
        common::BROWSER_TOKEN,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "snapshots count histories, never deserialize them: {body}"
    );
    let snapshot = &body["snapshot"];
    assert_eq!(snapshot["stats"]["items"], 7);
    assert_eq!(snapshot["stats"]["matches"], 7);
    assert_eq!(snapshot["stats"]["decisions"], 7);
    assert_eq!(snapshot["items"].as_array().unwrap().len(), 5);
    assert_eq!(snapshot["items"][0]["title"], "Candidate 0");
    assert_eq!(snapshot["items"][4]["title"], "Candidate 4");
}

#[tokio::test]
async fn page_queries_do_not_deserialize_off_page_records() {
    let (app, f) = fixture().await;
    // These records are outside page one for each ordering.
    sqlx::query("UPDATE matches SET body = 'not-json' WHERE id = 'match_page_00'")
        .execute(f.store.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE decisions SET body = 'not-json' WHERE id = 'decision_page_06'")
        .execute(f.store.pool())
        .await
        .unwrap();
    let last = f
        .store
        .list_items(&f.run_id)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.title == "Candidate 6")
        .unwrap();
    sqlx::query("UPDATE items SET body = 'not-json' WHERE id = ?")
        .bind(last.id.as_str())
        .execute(f.store.pool())
        .await
        .unwrap();
    for route in ["tournament", "decisions", "candidates"] {
        let (status, body) = read(
            &app,
            &format!("/api/runs/{}/{route}?limit=5", f.run_id),
            common::BROWSER_TOKEN,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{route} loaded an off-page body: {body}"
        );
    }
}

#[tokio::test]
async fn all_page_routes_reject_bad_queries_and_hide_foreign_runs() {
    let (app, f) = fixture().await;
    for route in ["tournament", "decisions", "candidates"] {
        let base = format!("/api/runs/{}/{route}", f.run_id);
        for query in [
            "limit=0",
            "limit=101",
            "offset=-1",
            "offset=no",
            "limit=2.5",
            "other=1",
            "through=-1",
        ] {
            let (status, body) =
                read(&app, &format!("{base}?{query}"), common::BROWSER_TOKEN).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{route}?{query}: {body}");
            assert_eq!(body["kind"], "validation");
        }
        let (status, body) = read(&app, &base, &"b".repeat(64)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["kind"], "not_found");
    }
}

#[tokio::test]
async fn history_watermark_excludes_later_inserts_even_if_they_are_backdated() {
    let (app, f) = fixture().await;
    let base = format!("/api/runs/{}/tournament", f.run_id);
    let (_, first) = read(&app, &base, common::BROWSER_TOKEN).await;
    let through = first["tournament"]["through"].as_i64().expect("watermark");
    let mut m = f
        .store
        .list_matches(&f.run_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    m.id = MatchId::from_raw("match_inserted_later");
    m.created_at = time::macros::datetime!(2025-01-01 00:00 UTC);
    f.store
        .commit_match(&m, "later-backdated", DEFAULT_K)
        .await
        .unwrap();
    let (_, older) = read(
        &app,
        &format!("{base}?offset=5&limit=5&through={through}"),
        common::BROWSER_TOKEN,
    )
    .await;
    assert_eq!(older["tournament"]["total"], 7);
    assert_eq!(older["tournament"]["matches"].as_array().unwrap().len(), 2);
    let (_, refreshed) = read(&app, &base, common::BROWSER_TOKEN).await;
    assert_eq!(refreshed["tournament"]["total"], 8);
}

#[tokio::test]
async fn history_pages_do_not_sort_the_full_history_before_applying_limit() {
    let (_, f) = fixture().await;
    for (table, order) in [
        ("matches", "created_at DESC, id DESC"),
        ("decisions", "created_at, id"),
    ] {
        let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(&format!(
            "EXPLAIN QUERY PLAN SELECT body FROM {table} INDEXED BY idx_{table}_run_page WHERE run_id = ? AND rowid <= ? ORDER BY {order} LIMIT ? OFFSET ?"
        ))
        .bind(f.run_id.as_str()).bind(100_i64).bind(5).bind(0)
        .fetch_all(f.store.pool()).await.unwrap();
        assert!(
            !plan.iter().any(|row| row.3.contains("TEMP B-TREE")),
            "{table}: {plan:?}"
        );
    }
}

#[tokio::test]
async fn candidate_ties_unrated_rows_and_rubric_changes_preserve_the_recorded_order() {
    let (app, f) = fixture().await;
    sqlx::query("UPDATE ratings SET rating = 1200 WHERE rubric_id = ?")
        .bind(f.plan.id.as_str())
        .execute(f.store.pool())
        .await
        .unwrap();
    let items = f.store.list_items(&f.run_id).await.unwrap();
    let unrated = &items[0];
    sqlx::query("DELETE FROM ratings WHERE item_id = ?")
        .bind(unrated.id.as_str())
        .execute(f.store.pool())
        .await
        .unwrap();
    let base = format!("/api/runs/{}/candidates?limit=100", f.run_id);
    let (status, body) = read(&app, &base, common::BROWSER_TOKEN).await;
    assert_eq!(status, StatusCode::OK);
    let actual: Vec<_> = body["candidates"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    let mut expected: Vec<_> = items
        .iter()
        .filter(|i| i.id != unrated.id)
        .map(|i| i.id.as_str())
        .collect();
    expected.sort();
    expected.push(unrated.id.as_str());
    assert_eq!(actual, expected);
    assert!(body["candidates"]["items"][6]["rating"].is_null());
    let mut plan = f.plan.clone();
    plan.id = PlanId::new();
    plan.revision += 1;
    f.store.insert_plan(&plan).await.unwrap();
    let (_, body) = read(&app, &base, common::BROWSER_TOKEN).await;
    assert!(
        body["candidates"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["rating"].is_null())
    );
}

#[tokio::test]
async fn stale_offsets_clamp_and_maximum_limits_stay_consistent() {
    let (app, f) = fixture().await;
    for (route, envelope, field) in [
        ("tournament", "tournament", "matches"),
        ("decisions", "ledger", "decisions"),
        ("candidates", "candidates", "items"),
    ] {
        let base = format!("/api/runs/{}/{route}", f.run_id);
        let (status, body) = read(
            &app,
            &format!("{base}?offset=4294967295&limit=5"),
            common::BROWSER_TOKEN,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body[envelope]["offset"], 5);
        assert_eq!(body[envelope][field].as_array().unwrap().len(), 2);
        let (_, body) = read(&app, &format!("{base}?limit=100"), common::BROWSER_TOKEN).await;
        assert_eq!(body[envelope]["limit"], 100);
        assert_eq!(body[envelope][field].as_array().unwrap().len(), 7);
        for query in ["offset=5&limit=5", "limit=0"] {
            let (status, _) = read(&app, &format!("{base}?{query}"), &"b".repeat(64)).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
    }
    let (status, _) = read(
        &app,
        &format!("/api/runs/{}/candidates?through=7", f.run_id),
        common::BROWSER_TOKEN,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn overview_filters_before_limit_and_baseline_only_contains_the_visible_candidates() {
    let (app, f) = fixture().await;
    let items = f.store.list_items(&f.run_id).await.unwrap();
    let first = items.iter().find(|i| i.title == "Candidate 0").unwrap();
    sqlx::query("UPDATE item_states SET disposition = 'archived' WHERE item_id = ?")
        .bind(first.id.as_str())
        .execute(f.store.pool())
        .await
        .unwrap();
    let checkpoint = CampaignSnapshot {
        iteration: 0,
        ratings: Some(f.store.leaderboard(&f.run_id, &f.plan.id).await.unwrap()),
        ..Default::default()
    };
    f.store
        .insert_snapshot(&f.run_id, &checkpoint)
        .await
        .unwrap();
    f.store.bump_iteration(&f.run_id).await.unwrap();
    let (_, body) = read(
        &app,
        &format!("/api/runs/{}", f.run_id),
        common::BROWSER_TOKEN,
    )
    .await;
    let snapshot = &body["snapshot"];
    assert_eq!(snapshot["stats"]["items"], 7);
    assert_eq!(snapshot["items"].as_array().unwrap().len(), 5);
    assert_eq!(snapshot["items"][0]["title"], "Candidate 1");
    let baseline = snapshot["ranking_baseline"]["ratings"].as_object().unwrap();
    assert_eq!(baseline.len(), 5);
    assert!(!baseline.contains_key(first.id.as_str()));
    let (_, body) = read(
        &app,
        &format!("/api/runs/{}/candidates", f.run_id),
        common::BROWSER_TOKEN,
    )
    .await;
    assert_eq!(body["candidates"]["items"][0]["title"], "Candidate 0");
}
