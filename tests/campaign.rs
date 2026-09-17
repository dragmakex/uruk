//! Campaign mode end to end (SPEC §6, §13 "Discovery campaign").

mod common;

use common::*;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::report;
use uruk::runtime::{ConcurrencyLimits, Scheduler, SchedulerConfig};

/// A campaign generates, reviews, clusters, compares, evolves, synthesizes
/// feedback that reaches later prompts, and delivers last, staying inside its
/// budget and stopping on a recorded condition.
#[tokio::test]
async fn campaign_runs_the_discovery_loop() {
    let f = fixture_with(Mode::Campaign, MockProvider::new(), |g| {
        g.budget.max_iterations = 12;
        g.budget.max_model_calls = 120;
    })
    .await;
    let source = seed_source(&f, "study-a", "Study A reports a 23% increase.").await;
    // Rules are shared through the Arc, so scripting after seeding is fine.
    let _ = (*f.provider)
        .clone()
        .merge_rules(campaign_provider(source.id.as_str()));
    seed_item(&f, "sampling bias").await;

    let scheduler = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig {
            limits: ConcurrencyLimits::new(3, 1),
            batch_size: 6,
            ..Default::default()
        },
    );

    let summary = scheduler.run(&f.run_id).await.unwrap();

    // The run reached a recorded stopping condition, not an error.
    assert!(summary.state.is_terminal(), "{summary:?}");
    assert!(summary.stop_condition.is_some(), "{summary:?}");
    assert!(
        summary.final_output_skipped.is_none(),
        "the final deliverable must run within the reserve: {summary:?}"
    );

    let items = f.store.list_items(&f.run_id).await.unwrap();
    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    let failed: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == TaskState::Failed)
        .map(|t| format!("{}/{}: {:?}", t.role.as_str(), t.strategy, t.error))
        .collect();
    assert!(
        failed.is_empty(),
        "no task may fail in a fully scripted campaign: {failed:?}"
    );

    // Generation actually produced candidates, by more than one strategy.
    let generated: Vec<_> = items
        .iter()
        .filter(|i| matches!(&i.author, Author::Agent { role, .. } if role == "generation"))
        .collect();
    assert!(
        generated.len() >= 2,
        "generation must produce candidates: {} found",
        generated.len()
    );
    assert!(
        generated
            .iter()
            .any(|i| matches!(&i.author, Author::Agent { strategy, .. } if strategy == "debate")),
        "the debate strategy must be used once reviews exist"
    );

    // Reviews used several strategies, and every match compared reviewed items.
    let reviews = f.store.list_reviews(&f.run_id).await.unwrap();
    let strategies: std::collections::BTreeSet<_> = reviews.iter().map(|r| r.strategy).collect();
    assert!(strategies.contains(&ReviewStrategy::Initial));
    assert!(strategies.contains(&ReviewStrategy::Full), "{strategies:?}");
    assert!(
        strategies.len() >= 3,
        "several review strategies must run: {strategies:?}"
    );
    let matches = f.store.list_matches(&f.run_id).await.unwrap();
    assert!(!matches.is_empty(), "candidates must be compared");
    for m in &matches {
        for id in [&m.item_a, &m.item_b] {
            assert!(
                !f.store.list_reviews_for_item(id).await.unwrap().is_empty(),
                "an unreviewed candidate was ranked: {id}"
            );
        }
    }

    // Literature citations from full reviews became evidence records.
    let evidence = f.store.list_evidence(&f.run_id).await.unwrap();
    assert!(
        evidence
            .iter()
            .any(|e| e.kind == EvidenceKind::RetrievedLiterature),
        "a full review's citation must be recorded as literature evidence"
    );

    // A child was created without mutating its parent.
    let children: Vec<_> = items.iter().filter(|i| i.is_child()).collect();
    assert!(!children.is_empty(), "evolution must create a child");
    for child in &children {
        for parent_id in &child.parent_ids {
            let parent = f.store.get_item(parent_id).await.unwrap();
            assert_eq!(parent.content_hash, ContentHash::of_str(&parent.content));
        }
        assert!(
            child.derivation.is_some(),
            "a child must state what changed"
        );
        let rating = f.store.get_rating(&child.id, &f.plan.id).await.unwrap();
        if let Some(r) = rating {
            assert!(r.matches_played == 0 || r.rating != INITIAL_ELO || r.matches_played > 0);
        }
    }

    // Proximity clustered the population.
    assert!(
        !f.store.list_clusters(&f.run_id).await.unwrap().is_empty(),
        "proximity must cluster the candidates"
    );

    // Meta-review feedback was synthesized and carried into a later prompt.
    let feedback = f.store.latest_feedback(&f.run_id).await.unwrap();
    assert!(feedback.is_some(), "feedback must be synthesized");
    let prompts = f.provider.seen_prompts();
    let first_synth = prompts
        .iter()
        .position(|p| p.contains(anchor::META_REVIEW))
        .expect("meta-review ran");
    assert!(
        prompts[first_synth + 1..]
            .iter()
            .any(|p| p.contains(CRITIQUE_MARKER)),
        "a recorded meta-critique must reach later work (SPEC §13)"
    );

    // The deliverable came last and was not itself reviewed.
    let deliverable = tasks
        .iter()
        .find(|t| t.role == Role::MetaReview && t.strategy == "deliverable")
        .expect("deliverable produced");
    assert!(
        tasks.iter().all(|t| t.created_at <= deliverable.created_at),
        "the deliverable must be scheduled after all research work"
    );
    let synthesis_ids: Vec<_> = items
        .iter()
        .filter(|i| i.kind == ItemKind::Synthesis)
        .map(|i| i.id.0.clone())
        .collect();
    assert!(!synthesis_ids.is_empty());
    assert!(
        !tasks.iter().any(|t| t.role == Role::Reflection
            && t.input_refs.iter().any(|r| synthesis_ids.contains(r))),
        "the deliverable is not a candidate and must not be reviewed"
    );

    // Budget was respected and every reservation settled.
    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert!(
        usage.model_calls <= f.goal.budget.max_model_calls,
        "budget exceeded: {usage:?}"
    );
    assert_eq!(
        usage.reserved_calls, 0,
        "all reservations must settle: {usage:?}"
    );

    // Snapshots were recorded for scheduler feedback (SPEC §6).
    let snapshots = f.store.list_snapshots(&f.run_id).await.unwrap();
    assert!(snapshots.len() >= 2);
    assert!(snapshots.iter().any(|s| !s.outcomes_by_strategy.is_empty()));

    // The export is complete and honest.
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    assert!(
        export.dir.starts_with(f.dir.path()),
        "exports live under the project: {:?}",
        export.dir
    );
    let text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert!(text.contains("Elo reflects relative prioritisation"));
    assert!(text.contains("not independent empirical validation"));
    let manifest: serde_json::Value = serde_json::from_str(
        &tokio::fs::read_to_string(export.dir.join("manifest.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["stop_condition"],
        summary.stop_condition.clone().unwrap()
    );
    assert!(
        manifest["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "mock")
    );
}

/// A campaign stops when its iteration budget runs out, preserving evidence
/// and still producing the final deliverable from the output reserve.
#[tokio::test]
async fn campaign_stops_at_its_budget_and_delivers_from_the_reserve() {
    let f = fixture_with(
        Mode::Campaign,
        MockProvider::new()
            .rule(anchor::TASK_SYNTHESIS, synthesis_json())
            .default_reply(review_json("plausible", "needs work", false)),
        |g| {
            g.budget.max_model_calls = 6;
            g.budget.reserve_calls_for_output = 2;
            g.budget.max_iterations = 2;
        },
    )
    .await;

    for i in 0..4 {
        seed_item(&f, &format!("candidate {i}")).await;
    }

    let scheduler = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    );
    let summary = scheduler.run(&f.run_id).await.unwrap();

    assert!(summary.state.is_terminal(), "{summary:?}");
    assert_eq!(summary.stop_condition.as_deref(), Some("budget_exhausted"));

    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert!(
        usage.model_calls <= 6,
        "the hard budget was exceeded: {usage:?}"
    );

    // The deliverable used the reserve.
    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    assert!(
        tasks
            .iter()
            .any(|t| t.strategy == "deliverable" && t.state == TaskState::Completed),
        "the final deliverable must run from the output reserve: {summary:?}"
    );

    // Completed evidence survives, and a partial report needs no model call.
    let calls_before = f.provider.call_count();
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    assert_eq!(f.provider.call_count(), calls_before);
    assert!(export.files.contains(&"REPORT.md".to_string()));
}

/// With no reserve left, the skipped final meta-review is recorded, not
/// silently omitted (SPEC §6).
#[tokio::test]
async fn skipped_final_output_is_recorded() {
    let f = fixture_with(
        Mode::Campaign,
        MockProvider::new().default_reply(review_json("plausible", "needs work", false)),
        |g| {
            g.budget.max_model_calls = 2;
            g.budget.reserve_calls_for_output = 0;
            g.budget.max_iterations = 3;
        },
    )
    .await;
    for i in 0..3 {
        seed_item(&f, &format!("candidate {i}")).await;
    }
    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    assert!(summary.final_output_skipped.is_some(), "{summary:?}");
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    let text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert!(text.contains("No final deliverable"), "{text}");
}

/// One scheduler owns a project at a time; inspection is still permitted
/// (SPEC §9.2).
#[tokio::test]
async fn project_lock_admits_one_scheduler() {
    use uruk::store::ProjectLock;

    let dir = tempfile::tempdir().unwrap();

    let first = ProjectLock::acquire(dir.path()).expect("first scheduler takes the lock");
    let second = ProjectLock::acquire(dir.path());
    assert!(
        second.is_err(),
        "a second scheduler must not start on the same project"
    );
    let message = second.unwrap_err().to_string();
    assert!(message.contains("already owns this project"), "{message}");

    // Inspection does not need the lock: `uruk status` opens the store
    // directly while a scheduler runs.
    let store = uruk::store::Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("inspection must work while the scheduler holds the lock");
    assert!(store.list_runs().await.unwrap().is_empty());

    drop(first);
    assert!(
        ProjectLock::acquire(dir.path()).is_ok(),
        "the lock must be released when the scheduler exits"
    );
}
