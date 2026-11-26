mod common;

use common::*;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::web::view::run_snapshot;

#[tokio::test]
async fn overview_compares_to_the_previous_round_in_the_same_rubric() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let item = seed_item(&f, "Measured candidate").await;
    f.store.ensure_rating(&item.id, &f.plan.id).await.unwrap();
    let ratings = f.store.leaderboard(&f.run_id, &f.plan.id).await.unwrap();
    let mut checkpoint = serde_json::to_value(CampaignSnapshot {
        iteration: 1,
        ..Default::default()
    })
    .unwrap();
    checkpoint["ratings"] = serde_json::to_value(&ratings).unwrap();
    f.store
        .insert_snapshot(&f.run_id, &serde_json::from_value(checkpoint).unwrap())
        .await
        .unwrap();
    f.store.bump_iteration(&f.run_id).await.unwrap();
    f.store.bump_iteration(&f.run_id).await.unwrap();
    let view = serde_json::to_value(run_snapshot(&f.store, &f.run_id).await.unwrap()).unwrap();
    assert_eq!(view["ranking_baseline"]["iteration"], 1);
    assert_eq!(
        view["ranking_baseline"]["ratings"][item.id.as_str()],
        INITIAL_ELO
    );

    let mut revised = f.plan.clone();
    revised.id = PlanId::new();
    revised.revision += 1;
    f.store.insert_plan(&revised).await.unwrap();
    let view = serde_json::to_value(run_snapshot(&f.store, &f.run_id).await.unwrap()).unwrap();
    assert!(
        view["ranking_baseline"].is_null(),
        "rubric cohorts must not mix"
    );
}

#[tokio::test]
async fn legacy_checkpoints_do_not_invent_round_movement() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    f.store
        .insert_snapshot(
            &f.run_id,
            &CampaignSnapshot {
                iteration: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    f.store.bump_iteration(&f.run_id).await.unwrap();
    f.store.bump_iteration(&f.run_id).await.unwrap();
    let view = serde_json::to_value(run_snapshot(&f.store, &f.run_id).await.unwrap()).unwrap();
    assert!(view["ranking_baseline"].is_null());
}
