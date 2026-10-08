//! The SPEC §13 acceptance table.
//!
//! Small deterministic fixtures, temporary SQLite stores, mock providers.

mod common;

use common::*;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uruk::agents::outputs::{self, ObservationReviewOutput};
use uruk::agents::{evolution, generation, ranking, reflection, safety, supervisor};
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::report;
use uruk::runtime::{Scheduler, SchedulerConfig};

/// Literature-only task: local papers, no repo or execution tools, produces a
/// cited comparison without a tournament or claiming unavailable text.
#[tokio::test]
async fn literature_only_task() {
    let provider = MockProvider::new()
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "limited sample", false));

    let f = fixture(Mode::Task, provider).await;
    seed_source(&f, "study-a", "Study A: 23% increase in 40 samples.").await;

    // An abstract-only source, to check the access limitation is surfaced.
    let partial = Source {
        id: SourceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        origin: Origin::LocalFile("study-b.pdf".into()),
        title: Some("Study B".into()),
        authors: Some("Author B".into()),
        date: Some("2025".into()),
        identifier: None,
        retrieved_at: OffsetDateTime::now_utc(),
        content_hash: ContentHash::of_str("b"),
        access: AccessLevel::AbstractOnly,
        access_limitations: Some("full text is paywalled".into()),
        text_artifact: None,
    };
    f.store.insert_source(&partial).await.unwrap();

    let scheduler = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    );
    let summary = scheduler.run(&f.run_id).await.unwrap();

    assert_eq!(summary.state, RunState::Completed);
    assert_eq!(summary.matches, 0, "no tournament for a literature task");

    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    let text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();

    assert!(
        text.contains("abstract_only"),
        "access level must be reported"
    );
    assert!(
        text.contains("paywalled"),
        "access limitation must be reported"
    );
    assert!(
        text.contains("No literature retrieval was permitted"),
        "coverage limits must be stated"
    );
    assert!(
        text.contains("Disagreements between sources"),
        "the deliverable's disagreements must reach the report"
    );
}

/// Critique-only task: review a supplied protocol without requiring Generation.
#[tokio::test]
async fn critique_only_task_needs_no_generation() {
    let goal_text = "review my study design for confounding; do not generate new hypotheses";
    let f = fixture(Mode::Task, MockProvider::new()).await;

    let mut goal = f.goal.clone();
    goal.question = goal_text.into();
    let plan = supervisor::build_plan(&goal, 1);

    assert!(
        !plan.roles.contains(&Role::Generation.as_str().to_string()),
        "critique-only must not require Generation: {:?}",
        plan.roles
    );
    assert!(plan.roles.contains(&Role::Reflection.as_str().to_string()));

    // Reviewer concerns are recorded as objections, distinct from evidence.
    let provider = MockProvider::new().default_reply(review_json(
        "plausible",
        "no randomisation is described",
        true,
    ));
    let f2 = fixture(Mode::Task, provider).await;
    let item = seed_item(&f2, "supplied protocol").await;
    let ctx = context(&f2, CancellationToken::new());

    let reviewed = reflection::review(&ctx, &item, ReviewStrategy::Initial, None)
        .await
        .unwrap();

    assert_eq!(reviewed.review.objections.len(), 1);
    assert!(reviewed.review.objections[0].fatal);
    assert!(
        reviewed.review.evidence.is_empty(),
        "a reviewer concern is not observed evidence"
    );
    assert_ne!(
        reviewed.review.proposed_assessment,
        Assessment::Refuted,
        "a review alone cannot refute"
    );
}

/// Prompt contracts: all eight published examples map to source figures,
/// adapted templates bind required fields and retain provenance.
#[tokio::test]
async fn prompt_contracts_hold() {
    let registry = uruk::prompts::PromptRegistry::new();

    let published = registry.published_adaptations();
    assert_eq!(published.len(), 8, "all eight examples must be mapped");

    for (id, figure) in &published {
        let t = registry.get(id).unwrap();
        assert!(t.provenance().contains(figure.figure));
        assert_eq!(
            t.hash,
            ContentHash::of_str(t.text),
            "hash pins the revision"
        );
    }

    // Missing inputs fail validation before any record is accepted.
    let template = registry.get(uruk::prompts::ids::RANKING_PAIRWISE).unwrap();
    let incomplete = uruk::prompts::Bindings::new().set("goal", "g");
    assert!(
        uruk::prompts::render(template, &incomplete).is_err(),
        "an unbound variable must fail before dispatch"
    );

    // Invalid structured output is refused.
    assert!(
        outputs::parse::<outputs::RankingOutput>("better idea: 1").is_err(),
        "a free-text marker is not a parser contract (SPEC §15.1)"
    );

    // Every anchor phrase the fixtures rely on names exactly one template.
    let anchors = [
        anchor::GENERATION_LITERATURE,
        anchor::GENERATION_DEBATE,
        anchor::REFLECTION_INITIAL,
        anchor::REFLECTION_FULL,
        anchor::REFLECTION_DEEP,
        anchor::REFLECTION_OBSERVATION,
        anchor::REFLECTION_SIMULATION,
        anchor::RANKING_PAIRWISE,
        anchor::RANKING_DEBATE,
        anchor::EVOLUTION_FEASIBILITY,
        anchor::EVOLUTION_ANALOGY,
        anchor::PROXIMITY,
        anchor::META_REVIEW,
        anchor::META_OVERVIEW,
        anchor::TASK_SYNTHESIS,
    ];
    for a in anchors {
        let hits = registry
            .ids()
            .filter(|id| registry.get(id).unwrap().text.contains(a))
            .count();
        assert_eq!(hits, 1, "anchor {a:?} must identify exactly one template");
    }
}

/// Observation review: non-discriminating observations return neutral; a
/// missing-piece label alone cannot become Supported; proposed disproof
/// requires claim-relevant evidence.
#[tokio::test]
async fn observation_review_labels_are_bounded() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let item = seed_item(&f, "candidate").await;
    let source = seed_source(&f, "article", "Cells were present.").await;

    // Every observation would occur regardless of the hypothesis.
    let non_discriminating = serde_json::json!({
        "observations": [{
            "source_id": source.id.as_str(),
            "locator": {"kind": "page", "at": "3"},
            "observation": "cells were present in the sample",
            "cause_established": true,
            "consistent_with_hypothesis": true,
            "expected_regardless": true,
            "alternative_explanations": ["this is true of any sample"],
            "label": "missing_piece"
        }],
        "summary": "the hypothesis is consistent",
        "disproof": "none",
        "label": "missing_piece",
        "rationale": "consistent",
        "uncertainty": ""
    })
    .to_string();
    let f = Fixture {
        provider: std::sync::Arc::new(MockProvider::new().default_reply(non_discriminating)),
        ..f
    };
    let ctx = context(&f, CancellationToken::new());

    let reviewed = reflection::review_observations(&ctx, &item, &source, None)
        .await
        .unwrap();

    assert_eq!(
        reviewed.review.explanatory_label,
        Some(ExplanatoryLabel::Neutral),
        "observations expected regardless of the hypothesis are neutral"
    );
    assert_eq!(
        reviewed.review.proposed_assessment,
        Assessment::Untested,
        "a non-discriminating observation establishes nothing"
    );

    // A genuine missing_piece tops out at Plausible, never Supported.
    let genuine: ObservationReviewOutput = serde_json::from_value(serde_json::json!({
        "observations": [{
            "source_id": "src_x",
            "locator": {"kind": "figure", "at": "2"},
            "observation": "an unexplained secondary peak",
            "cause_established": false,
            "consistent_with_hypothesis": true,
            "expected_regardless": false,
            "alternative_explanations": [],
            "label": "missing_piece"
        }],
        "summary": "novel explanation for the peak",
        "disproof": "none",
        "label": "missing_piece",
        "rationale": "r",
        "uncertainty": ""
    }))
    .unwrap();
    assert_eq!(genuine.proposed_assessment(), Assessment::Plausible);

    // A proposed disproof is Contested pending claim-relevant evidence.
    let disproof: ObservationReviewOutput = serde_json::from_value(serde_json::json!({
        "observations": [{
            "source_id": "src_x",
            "locator": {"kind": "table", "at": "1"},
            "observation": "the predicted shift is absent",
            "cause_established": false,
            "consistent_with_hypothesis": false,
            "expected_regardless": false,
            "alternative_explanations": [],
            "label": "disproved"
        }],
        "summary": "",
        "disproof": "the predicted shift is absent",
        "label": "disproved",
        "rationale": "r",
        "uncertainty": ""
    }))
    .unwrap();
    assert_eq!(
        disproof.proposed_assessment(),
        Assessment::Contested,
        "a proposed disproof is not Refuted until evidence is checked"
    );

    // And the store refuses Refuted without empirical contradicting evidence.
    let critique = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        kind: EvidenceKind::AgentCritique,
        content: "the reviewer believes it is disproved".into(),
        method: "observation review".into(),
        inputs: vec![],
        source_ids: vec![],
        artifact_ids: vec![],
        limitations: None,
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_evidence(&critique).await.unwrap();

    let attempt = AssessmentRecord {
        item_id: item.id.clone(),
        assessment: Assessment::Refuted,
        basis_review: Some(reviewed.review.id.clone()),
        basis_decision: None,
        evidence: vec![EvidenceLink {
            evidence_id: critique.id,
            item_id: item.id.clone(),
            claim: "candidate claim".into(),
            relation: EvidenceRelation::Contradicts,
            justification: "the reviewer said so".into(),
            limits: None,
        }],
        rationale: "review concluded disproved".into(),
        assessed_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_review(&reviewed.review).await.unwrap();
    assert!(
        f.store.record_assessment(&attempt).await.is_err(),
        "an agent critique cannot refute a claim (SPEC §4.3)"
    );
}

/// Intermediate safety: a permitted goal cannot admit a blocked candidate.
#[tokio::test]
async fn intermediate_safety_blocks_generated_candidates() {
    let unsafe_hypothesis = serde_json::json!({
        "title": "Passage study",
        "claim": "serial passage will enhance transmissibility of a human pathogen",
        "mechanism": "repeated passage selects for receptor affinity",
        "assumptions": [],
        "scope": "laboratory",
        "predictions": [],
        "falsifiers": [],
        "validation_plan": "passage in ferrets",
        "grounding": [],
        "limitations": "",
        "provisional": false
    })
    .to_string();

    // The goal itself is benign.
    assert_eq!(
        safety::check("understand respiratory virus host range"),
        safety::SafetyVerdict::Clear
    );

    let f = fixture(
        Mode::Campaign,
        MockProvider::new().default_reply(unsafe_hypothesis),
    )
    .await;
    let ctx = context(&f, CancellationToken::new());

    let generated = generation::from_literature(&ctx, None, None).await.unwrap();
    let verdict = safety::check_item(&generated.item);
    assert!(
        verdict.is_blocked(),
        "the generated candidate must be checked on its own content"
    );

    // A blocked candidate does not enter the tournament.
    f.store.insert_item(&generated.item).await.unwrap();
    f.store
        .set_disposition(&generated.item.id, &Disposition::Blocked, verdict.concern())
        .await
        .unwrap();

    let other = seed_item(&f, "benign alternative").await;
    let work = supervisor::select_work(
        &f.store,
        &f.run_id,
        &f.goal,
        &f.plan,
        &supervisor::Weights::default(),
        10,
        0,
    )
    .await
    .unwrap();

    assert!(
        !work
            .iter()
            .any(|w| w.input_refs.contains(&generated.item.id.0)),
        "a blocked candidate must not be scheduled: {work:?}"
    );
    assert!(
        work.iter().any(|w| w.input_refs.contains(&other.id.0)),
        "permitted work must still proceed"
    );
}

/// Discovery campaign: seed ideas, review and compare them, create a child
/// without mutating its parent, and carry a meta-critique into later work.
#[tokio::test]
async fn discovery_campaign_preserves_parents() {
    let f = fixture(
        Mode::Campaign,
        MockProvider::new().default_reply(evolution_json()),
    )
    .await;
    let parent = seed_item(&f, "original candidate").await;
    let parent_hash = parent.content_hash.clone();

    // Give the parent a rating, to confirm the child does not inherit it.
    f.store.ensure_rating(&parent.id, &f.plan.id).await.unwrap();
    let other = seed_item(&f, "rival").await;
    let m = Match {
        id: MatchId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        item_a: parent.id.clone(),
        item_b: other.id.clone(),
        rubric_id: f.plan.id.clone(),
        evidence_snapshot: ContentHash::of_str("s"),
        order_randomized: true,
        method: MatchMethod::Pairwise,
        judge_model: "mock".into(),
        outcome: MatchOutcome::WinA,
        rationale: "parent wins".into(),
        rating_a_before: 0.0,
        rating_b_before: 0.0,
        rating_a_after: 0.0,
        rating_b_after: 0.0,
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.commit_match(&m, "m1", DEFAULT_K).await.unwrap();
    let parent_rating = f
        .store
        .get_rating(&parent.id, &f.plan.id)
        .await
        .unwrap()
        .unwrap();
    assert!(parent_rating.rating > INITIAL_ELO);

    let ctx = context(&f, CancellationToken::new());
    let evolved = evolution::improve_feasibility(&ctx, &parent, None)
        .await
        .unwrap();
    f.store.insert_item(&evolved.child).await.unwrap();
    f.store
        .ensure_rating(&evolved.child.id, &f.plan.id)
        .await
        .unwrap();

    // The parent is untouched.
    let reloaded = f.store.get_item(&parent.id).await.unwrap();
    assert_eq!(
        reloaded.content_hash, parent_hash,
        "parent content must not change"
    );

    // The child links back and states what changed.
    assert_eq!(evolved.child.parent_ids, vec![parent.id.clone()]);
    assert!(
        evolved
            .child
            .derivation
            .as_ref()
            .unwrap()
            .contains("in-line reference")
    );
    assert!(!evolved.discriminating_test.is_empty());

    // The child inherits neither rating nor support.
    let child_rating = f
        .store
        .get_rating(&evolved.child.id, &f.plan.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        child_rating.rating, INITIAL_ELO,
        "a child must not inherit its parent's rating"
    );
    assert_eq!(
        f.store.current_assessment(&evolved.child.id).await.unwrap(),
        Assessment::Untested,
        "a child must not inherit its parent's support"
    );
}

/// Evidence integrity: new contradictory evidence marks rankings stale,
/// duplicate citations and agent votes cannot become independent
/// corroboration, and evidence for one claim cannot support another.
#[tokio::test]
async fn evidence_integrity_holds() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let a = seed_item(&f, "alpha").await;
    let b = seed_item(&f, "beta").await;

    f.store.ensure_rating(&a.id, &f.plan.id).await.unwrap();
    f.store.ensure_rating(&b.id, &f.plan.id).await.unwrap();
    let m = Match {
        id: MatchId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        item_a: a.id.clone(),
        item_b: b.id.clone(),
        rubric_id: f.plan.id.clone(),
        evidence_snapshot: ContentHash::of_str("s"),
        order_randomized: true,
        method: MatchMethod::Pairwise,
        judge_model: "mock".into(),
        outcome: MatchOutcome::WinA,
        rationale: "alpha wins".into(),
        rating_a_before: 0.0,
        rating_b_before: 0.0,
        rating_a_after: 0.0,
        rating_b_after: 0.0,
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.commit_match(&m, "m", DEFAULT_K).await.unwrap();
    assert!(
        !f.store
            .get_rating(&a.id, &f.plan.id)
            .await
            .unwrap()
            .unwrap()
            .stale
    );

    // Contradicting evidence arrives.
    let contradiction = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        kind: EvidenceKind::ExternalExperiment,
        content: "independent replication found no effect".into(),
        method: "third-party lab".into(),
        inputs: vec![],
        source_ids: vec![],
        artifact_ids: vec![],
        limitations: None,
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_evidence(&contradiction).await.unwrap();
    f.store
        .link_evidence(&EvidenceLink {
            evidence_id: contradiction.id.clone(),
            item_id: a.id.clone(),
            claim: "alpha claim".into(),
            relation: EvidenceRelation::Contradicts,
            justification: "no effect observed".into(),
            limits: None,
        })
        .await
        .unwrap();

    assert!(
        f.store
            .get_rating(&a.id, &f.plan.id)
            .await
            .unwrap()
            .unwrap()
            .stale,
        "new contradicting evidence must mark the ranking stale"
    );

    // The report shows the stale marker.
    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    let text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert!(
        text.contains("STALE"),
        "a stale ranking must be visibly marked"
    );

    // Several agents repeating the same source is not corroboration: three
    // agent critiques still cannot reach Supported.
    let mut critique_ids = Vec::new();
    for i in 0..3 {
        let e = Evidence {
            id: EvidenceId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: f.run_id.clone(),
            kind: EvidenceKind::AgentCritique,
            content: format!("agent {i} agrees with the same source"),
            method: "review".into(),
            inputs: vec!["src_same".into()],
            source_ids: vec![],
            artifact_ids: vec![],
            limitations: None,
            produced_by: None,
            created_at: OffsetDateTime::now_utc(),
        };
        f.store.insert_evidence(&e).await.unwrap();
        critique_ids.push(e.id);
    }
    let review = seed_review(&f, &b).await;
    let consensus = AssessmentRecord {
        item_id: b.id.clone(),
        assessment: Assessment::Supported,
        basis_review: Some(review.id.clone()),
        basis_decision: None,
        evidence: critique_ids
            .into_iter()
            .map(|id| EvidenceLink {
                evidence_id: id,
                item_id: b.id.clone(),
                claim: "beta claim".into(),
                relation: EvidenceRelation::Supports,
                justification: "agent agreement".into(),
                limits: None,
            })
            .collect(),
        rationale: "unanimous agent agreement".into(),
        assessed_at: OffsetDateTime::now_utc(),
    };
    assert!(
        f.store.record_assessment(&consensus).await.is_err(),
        "agent consensus is not independent corroboration (SPEC §4.3, §12)"
    );

    // Empirical evidence for alpha's claim cannot be borrowed to support beta.
    let borrowed = AssessmentRecord {
        item_id: b.id.clone(),
        assessment: Assessment::Supported,
        basis_review: Some(review.id),
        basis_decision: None,
        evidence: vec![EvidenceLink {
            evidence_id: contradiction.id,
            item_id: a.id.clone(),
            claim: "alpha claim".into(),
            relation: EvidenceRelation::Supports,
            justification: "borrowed".into(),
            limits: None,
        }],
        rationale: "evidence for another item".into(),
        assessed_at: OffsetDateTime::now_utc(),
    };
    let err = f.store.record_assessment(&borrowed).await.unwrap_err();
    assert!(err.to_string().contains("different item"), "{err}");
}

/// Tournament integrity: one update per match, draws behave, insufficient
/// evidence causes no update, rubric revisions do not share a cohort.
#[tokio::test]
async fn tournament_integrity_holds() {
    let f = fixture(Mode::Campaign, MockProvider::new()).await;
    let a = seed_item(&f, "alpha").await;
    let b = seed_item(&f, "beta").await;
    f.store.ensure_rating(&a.id, &f.plan.id).await.unwrap();
    f.store.ensure_rating(&b.id, &f.plan.id).await.unwrap();

    let mk = |outcome: MatchOutcome| Match {
        id: MatchId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        item_a: a.id.clone(),
        item_b: b.id.clone(),
        rubric_id: f.plan.id.clone(),
        evidence_snapshot: ContentHash::of_str("s"),
        order_randomized: true,
        method: MatchMethod::Pairwise,
        judge_model: "mock".into(),
        outcome,
        rationale: "r".into(),
        rating_a_before: 0.0,
        rating_b_before: 0.0,
        rating_a_after: 0.0,
        rating_b_after: 0.0,
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };

    // Insufficient basis: no update at all.
    f.store
        .commit_match(&mk(MatchOutcome::InsufficientBasis), "k0", DEFAULT_K)
        .await
        .unwrap();
    let r = f
        .store
        .get_rating(&a.id, &f.plan.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.rating, INITIAL_ELO);
    assert_eq!(r.matches_played, 0);

    // A draw between equals leaves ratings unchanged but counts the match.
    f.store
        .commit_match(&mk(MatchOutcome::Draw), "k1", DEFAULT_K)
        .await
        .unwrap();
    let r = f
        .store
        .get_rating(&a.id, &f.plan.id)
        .await
        .unwrap()
        .unwrap();
    assert!((r.rating - INITIAL_ELO).abs() < 1e-9);
    assert_eq!(r.matches_played, 1);

    // A win updates once, and a replay does not update again.
    f.store
        .commit_match(&mk(MatchOutcome::WinA), "k2", DEFAULT_K)
        .await
        .unwrap();
    let after = f
        .store
        .get_rating(&a.id, &f.plan.id)
        .await
        .unwrap()
        .unwrap();
    assert!((after.rating - 1216.0).abs() < 1e-9, "{}", after.rating);

    let applied = f
        .store
        .commit_match(&mk(MatchOutcome::WinA), "k2", DEFAULT_K)
        .await
        .unwrap();
    assert!(!applied);
    assert_eq!(
        f.store
            .get_rating(&a.id, &f.plan.id)
            .await
            .unwrap()
            .unwrap()
            .rating,
        after.rating
    );

    // A rubric revision starts a new cohort at the initial rating.
    let mut revised = supervisor::build_plan(&f.goal, 1);
    revised.rubric.attributes = vec!["reproducible".into()];
    f.store.insert_plan(&revised).await.unwrap();
    f.store.ensure_rating(&a.id, &revised.id).await.unwrap();

    let new_cohort = f
        .store
        .get_rating(&a.id, &revised.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new_cohort.rating, INITIAL_ELO);
    assert_eq!(new_cohort.matches_played, 0);
    // The old cohort is retained with its history.
    assert_eq!(
        f.store
            .get_rating(&a.id, &f.plan.id)
            .await
            .unwrap()
            .unwrap()
            .rating,
        after.rating
    );
}

/// Candidate presentation order can be reversed, and maps back to stable IDs.
#[tokio::test]
async fn presentation_order_is_randomized_and_mapped_back() {
    let f = fixture(
        Mode::Campaign,
        MockProvider::new().default_reply(ranking_json("win_1")),
    )
    .await;
    let a = seed_item(&f, "alpha").await;
    let b = seed_item(&f, "beta").await;

    // Different seeds present different orders; each is recorded honestly.
    let mut orders = std::collections::BTreeSet::new();
    for seed in [1_u64, 2, 3, 4, 5, 6] {
        let mut ctx = context(&f, CancellationToken::new());
        ctx.seed = seed;
        let judged = ranking::compare(&ctx, &a, &b, MatchMethod::Pairwise, 3, None)
            .await
            .unwrap();
        orders.insert(judged.r#match.item_a.0.clone());

        // Whichever way round, the stored match names real item IDs, and the
        // shuffle flag says whether the argument order was reversed.
        assert!([a.id.0.clone(), b.id.0.clone()].contains(&judged.r#match.item_a.0));
        assert_eq!(
            judged.r#match.order_randomized,
            judged.r#match.item_a == b.id
        );
        // The judge picked position 1, which is whatever was presented first.
        assert_eq!(judged.r#match.outcome, MatchOutcome::WinA);
    }
    assert_eq!(
        orders.len(),
        2,
        "both presentation orders must occur: {orders:?}"
    );
}

/// Researcher intervention: human ideas and reviews survive restart and reach
/// later prompts; approval/denial applies only to the exact request.
#[tokio::test]
async fn researcher_input_survives_restart_and_reaches_prompts() {
    let f = fixture(
        Mode::Campaign,
        MockProvider::new().default_reply(review_json("plausible", "x", false)),
    )
    .await;
    let item = seed_item(&f, "researcher idea").await;

    // Researcher feedback, recorded as the CLI records it.
    let decision = Decision {
        id: DecisionId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        kind: DecisionKind::HumanFeedback,
        actor: Actor::Researcher,
        reason: "researcher feedback".into(),
        referenced: vec![],
        payload: serde_json::json!({"content": "RESEARCHER_STEERING_MARKER: focus on drift", "as_item": false}),
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_decision(&decision).await.unwrap();

    // --- restart ---
    let path = f.dir.path().join(".uruk/state.sqlite");
    f.store.close().await;
    let store = uruk::store::Store::open(&path).await.unwrap();
    let reloaded = store.get_item(&item.id).await.unwrap();
    assert_eq!(
        reloaded.author,
        Author::Researcher,
        "attribution must survive"
    );

    // The feedback reaches the next review's prompt, attributed.
    let summary = Scheduler::new(
        store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    assert!(summary.reviews >= 1, "{summary:?}");
    let prompts = f.provider.seen_prompts();
    assert!(
        prompts
            .iter()
            .any(|p| p.contains("RESEARCHER_STEERING_MARKER") && p.contains("Researcher feedback")),
        "researcher feedback must be supplied to later tasks with attribution"
    );
}

/// Domain independence: a literature task and a computational fixture use the
/// same core records, and a missing tool is reported as unavailable.
#[tokio::test]
async fn domain_independence_and_missing_tools() {
    // A literature run needs no repository or execution capability.
    let lit = fixture(Mode::Task, MockProvider::new()).await;
    seed_source(&lit, "paper", "text").await;
    assert!(!lit.goal.permissions.execute);
    let export = report::export_run(&lit.store, &lit.run_id).await.unwrap();
    assert!(export.files.contains(&"sources.jsonl".to_string()));

    // A computational run uses the same records.
    let comp = fixture(Mode::Task, MockProvider::new()).await;
    let item = seed_item(&comp, "computational claim").await;
    let evidence = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: comp.run_id.clone(),
        kind: EvidenceKind::ComputationalResult,
        content: "mean = 0.42".into(),
        method: "python analyze.py".into(),
        inputs: vec!["data.csv".into()],
        source_ids: vec![],
        artifact_ids: vec![],
        limitations: None,
        produced_by: None,
        created_at: OffsetDateTime::now_utc(),
    };
    comp.store.insert_evidence(&evidence).await.unwrap();
    comp.store
        .link_evidence(&EvidenceLink {
            evidence_id: evidence.id,
            item_id: item.id.clone(),
            claim: "computational claim".into(),
            relation: EvidenceRelation::Supports,
            justification: "computed".into(),
            limits: None,
        })
        .await
        .unwrap();

    // A denied tool cannot execute, and this is a capability report, not a
    // scientific finding.
    use uruk::tools::{ExecRequest, execute_analysis};
    let denied = execute_analysis(
        &comp.store,
        &comp.run_id,
        &TaskId::new(),
        &ExecRequest::new("python", vec!["analyze.py".into()]),
        &comp.goal.permissions,
        &CancellationToken::new(),
    )
    .await;

    match denied {
        Err(uruk::Error::Permission(msg)) => {
            assert!(msg.contains("does not permit local execution"), "{msg}");
        }
        other => panic!("a denied tool must not execute: {other:?}"),
    }
    assert_eq!(
        comp.store.current_assessment(&item.id).await.unwrap(),
        Assessment::Untested,
        "an unavailable tool says nothing about whether the claim is true"
    );
}

/// Computational task: execute a permitted analysis, record hashes and real
/// outputs, provide a rerun; a failed execution cannot become support.
#[tokio::test]
#[cfg(unix)]
async fn computational_task_records_reproducibility() {
    use uruk::tools::{ExecRequest, execute_analysis};
    if !containment_available() {
        return;
    }

    let f = fixture(Mode::Task, MockProvider::new()).await;
    let item = seed_item(&f, "mean is above zero").await;

    // A tiny deterministic dataset.
    let data_path = f.dir.path().join("data.csv");
    tokio::fs::write(&data_path, "value\n1\n2\n3\n")
        .await
        .unwrap();

    let permissions = Permissions {
        execute: true,
        allowed_tools: vec!["sh".into()],
        read_paths: vec![f.dir.path().to_string_lossy().into_owned()],
        ..Default::default()
    };

    let request = ExecRequest {
        program: "sh".into(),
        args: vec![
            "-c".into(),
            "awk 'NR>1 {s+=$1; n++} END {print s/n}' data.csv".into(),
        ],
        inputs: vec![data_path.to_string_lossy().into_owned()],
        timeout_secs: 30,
        max_output_bytes: 4096,
        seed: Some(42),
    };

    let task_id = TaskId::new();
    let outcome = execute_analysis(
        &f.store,
        &f.run_id,
        &task_id,
        &request,
        &permissions,
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    assert!(outcome.usable_as_evidence(), "{:?}", outcome.record);
    assert_eq!(outcome.record.exit_status, 0);
    assert_eq!(outcome.record.input_hashes.len(), 1, "input must be hashed");
    assert!(
        outcome.record.code_hash.is_some(),
        "the code that ran must be hashed"
    );
    assert_eq!(outcome.record.seed, Some(42));
    assert!(
        outcome.record.rerun.is_ok(),
        "a rerun command must be recorded"
    );
    assert!(
        outcome
            .record
            .environment
            .iter()
            .any(|e| e.starts_with("containment=")),
        "the containment used must be recorded: {:?}",
        outcome.record.environment
    );

    let evidence = outcome
        .evidence
        .clone()
        .expect("successful execution yields evidence");
    assert_eq!(evidence.kind, EvidenceKind::ComputationalResult);
    assert!(
        evidence.content.trim() == "2",
        "actual output: {:?}",
        evidence.content
    );

    // The records are proposals; commit them through the controlled path.
    for a in &outcome.artifacts {
        f.store.insert_artifact(a).await.unwrap();
    }
    f.store.insert_evidence(&evidence).await.unwrap();

    // It can support the claim, because it is claim-relevant empirical evidence.
    let review = seed_review(&f, &item).await;
    f.store
        .record_assessment(&AssessmentRecord {
            item_id: item.id.clone(),
            assessment: Assessment::Supported,
            basis_review: Some(review.id),
            basis_decision: None,
            evidence: vec![EvidenceLink {
                evidence_id: evidence.id.clone(),
                item_id: item.id.clone(),
                claim: "mean is above zero".into(),
                relation: EvidenceRelation::Supports,
                justification: "computed mean is 2".into(),
                limits: Some("one small dataset".into()),
            }],
            rationale: "execution confirms".into(),
            assessed_at: OffsetDateTime::now_utc(),
        })
        .await
        .unwrap();
    assert_eq!(
        f.store.current_assessment(&item.id).await.unwrap(),
        Assessment::Supported
    );

    // A failing execution produces no evidence at all.
    let failing = ExecRequest {
        program: "sh".into(),
        args: vec!["-c".into(), "exit 3".into()],
        inputs: vec![],
        timeout_secs: 30,
        max_output_bytes: 4096,
        seed: None,
    };
    let failed = execute_analysis(
        &f.store,
        &f.run_id,
        &TaskId::new(),
        &failing,
        &permissions,
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    assert!(!failed.record.may_support_claims());
    assert!(
        failed.evidence.is_none(),
        "a failed execution must not become support (SPEC §8)"
    );
    assert!(failed.record.validation_note.is_some());
}

/// Containment is enforced, not advisory: a contained program cannot write
/// outside its workspace or read files the run may not read (SPEC §9.3, §12).
#[tokio::test]
#[cfg(unix)]
async fn containment_denies_escapes() {
    use uruk::tools::{ExecRequest, execute_analysis};
    if !containment_available() {
        return;
    }

    let f = fixture(Mode::Task, MockProvider::new()).await;
    let outside = f.dir.path().join("escape.txt");
    let permissions = Permissions {
        execute: true,
        allowed_tools: vec!["sh".into()],
        read_paths: vec![f.dir.path().join("inputs").to_string_lossy().into_owned()],
        ..Default::default()
    };

    let escape = ExecRequest {
        program: "sh".into(),
        args: vec!["-c".into(), format!("echo x > {}", outside.display())],
        inputs: vec![],
        timeout_secs: 30,
        max_output_bytes: 4096,
        seed: None,
    };
    let outcome = execute_analysis(
        &f.store,
        &f.run_id,
        &TaskId::new(),
        &escape,
        &permissions,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_ne!(
        outcome.record.exit_status, 0,
        "a write outside the workspace must fail"
    );
    assert!(
        !outside.exists(),
        "nothing may be written outside the workspace"
    );
    assert!(outcome.evidence.is_none());

    // An input outside the permitted read paths is refused before anything runs.
    let secret = f.dir.path().join("secret.txt");
    tokio::fs::write(&secret, "s").await.unwrap();
    let exfiltrate = ExecRequest {
        program: "sh".into(),
        args: vec!["-c".into(), "cat secret.txt".into()],
        inputs: vec![secret.to_string_lossy().into_owned()],
        timeout_secs: 30,
        max_output_bytes: 4096,
        seed: None,
    };
    let err = execute_analysis(
        &f.store,
        &f.run_id,
        &TaskId::new(),
        &exfiltrate,
        &permissions,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, uruk::Error::Permission(_)), "{err}");
}

/// Runtime safety: a denied tool cannot execute even when the run permits
/// execution generally.
#[tokio::test]
async fn tool_allowlist_is_enforced() {
    use uruk::tools::{ExecRequest, execute_analysis};

    let f = fixture(Mode::Task, MockProvider::new()).await;
    let permissions = Permissions {
        execute: true,
        allowed_tools: vec!["python".into()],
        ..Default::default()
    };

    let result = execute_analysis(
        &f.store,
        &f.run_id,
        &TaskId::new(),
        &ExecRequest::new("sh", vec!["-c".into(), "echo pwned".into()]),
        &permissions,
        &CancellationToken::new(),
    )
    .await;

    match result {
        Err(uruk::Error::Permission(msg)) => {
            assert!(
                msg.contains("not in this task's approved tool list"),
                "{msg}"
            );
        }
        other => panic!("an unapproved tool must not run: {other:?}"),
    }
}

/// The full evidence loop: a review requests a computational check, the run
/// parks on a persisted approval, the researcher approves, the contained
/// execution produces evidence, a recurrent review consumes it, and the claim
/// reaches `Supported` through the store's gate (SPEC §5, §8, §9.2, §13).
#[tokio::test]
#[cfg(unix)]
async fn approved_execution_feeds_a_review_that_supports_the_claim() {
    if !containment_available() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let data_path = dir.path().join("data.csv");
    tokio::fs::write(&data_path, "value\n1\n2\n3\n")
        .await
        .unwrap();
    let data = data_path.to_string_lossy().into_owned();

    let provider = MockProvider::new()
        .rule(
            anchor::REFLECTION_INITIAL,
            exec_request_review_json(
                "sh",
                &["-c", "awk 'NR>1 {s+=$1; n++} END {print s/n}' data.csv"],
                &data,
            ),
        )
        // The recurrent review cites the evidence record it was shown.
        .rule_with(anchor::REFLECTION_FULL, |prompt| {
            let start = prompt
                .find("[evd_")
                .expect("the evidence record is in the prompt")
                + 1;
            let end = start + prompt[start..].find(']').unwrap();
            let evidence_id = &prompt[start..end];
            serde_json::json!({
                "assessment_text": "the computed mean is 2, which bears on the claim",
                "proposed_assessment": "plausible",
                "evidence": [{
                    "source_id": "",
                    "evidence_id": evidence_id,
                    "locator": "stdout",
                    "reported": "mean = 2",
                    "claim": "mean is above zero",
                    "relation": "supports",
                    "justification": "2 > 0 on the supplied data",
                    "limits": "three data points"
                }],
                "objections": [], "unknowns": [], "next_actions": []
            })
            .to_string()
        })
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "x", false));

    let read_dir = dir.path().to_string_lossy().into_owned();
    let f = fixture_with(Mode::Task, provider, |g| {
        g.question = "assess this candidate claim about the mean".into();
        g.permissions.execute = true;
        g.permissions.allowed_tools = vec!["sh".into()];
        g.permissions.require_approval_for = vec!["execute".into()];
        g.permissions.read_paths.push(read_dir);
    })
    .await;
    let item = seed_item(&f, "mean is above zero").await;

    // Pass 1: the review requests a check; the run parks on approval.
    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    assert_eq!(summary.state, RunState::WaitingForHuman, "{summary:?}");
    let pending = f
        .store
        .list_approvals(&f.run_id, Some(ApprovalState::Pending))
        .await
        .unwrap();
    assert_eq!(pending.len(), 1, "one approval request must be pending");
    assert!(pending[0].action.contains("sh -c"), "{}", pending[0].action);
    assert!(
        f.store.list_evidence(&f.run_id).await.unwrap().is_empty(),
        "nothing may execute before approval"
    );

    // The researcher approves the exact payload (the future approval UI's
    // write path).
    f.store
        .decide_approval(&pending[0].id, true, Some(&pending[0].payload_hash), None)
        .await
        .unwrap();

    // Pass 2 (resume): the contained execution runs, its result is reviewed,
    // and the claim is supported by empirical evidence.
    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    assert_eq!(summary.state, RunState::Completed, "{summary:?}");
    assert_eq!(summary.tasks_failed, 0, "{summary:?}");

    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    let tool = tasks
        .iter()
        .find(|t| t.role == Role::Tool)
        .expect("tool task");
    assert_eq!(tool.state, TaskState::Completed, "{:?}", tool.error);

    let evidence = f.store.list_evidence(&f.run_id).await.unwrap();
    let result = evidence
        .iter()
        .find(|e| e.kind == EvidenceKind::ComputationalResult)
        .expect("the execution produced evidence");
    assert_eq!(result.content.trim(), "2");

    let experiments = f.store.list_experiments(&f.run_id).await.unwrap();
    assert_eq!(experiments.len(), 1);
    let exec = experiments[0].execution.as_ref().unwrap();
    assert!(exec.rerun.is_ok());
    assert!(exec.code_hash.is_some());

    assert!(
        tasks
            .iter()
            .any(|t| t.strategy == "recurrent" && t.state == TaskState::Completed),
        "a recurrent review must consume the execution result"
    );
    assert_eq!(
        f.store.current_assessment(&item.id).await.unwrap(),
        Assessment::Supported,
        "claim-relevant empirical evidence, interpreted by a review, reaches Supported"
    );

    let usage = f.store.budget_usage(&f.run_id).await.unwrap();
    assert_eq!(usage.tool_executions, 1);

    let export = report::export_run(&f.store, &f.run_id).await.unwrap();
    let text = tokio::fs::read_to_string(export.dir.join("REPORT.md"))
        .await
        .unwrap();
    assert!(text.contains("**Reproduction:**"), "{text}");
    assert!(text.contains("supported"), "{text}");
}

/// A denied request never executes, and the run still completes honestly.
#[tokio::test]
#[cfg(unix)]
async fn denied_execution_never_runs() {
    if !containment_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let data_path = dir.path().join("data.csv");
    tokio::fs::write(&data_path, "value\n1\n").await.unwrap();
    let data = data_path.to_string_lossy().into_owned();

    let provider = MockProvider::new()
        .rule(
            anchor::REFLECTION_INITIAL,
            exec_request_review_json("sh", &["-c", "cat data.csv"], &data),
        )
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
        .default_reply(review_json("plausible", "x", false));
    let read_dir = dir.path().to_string_lossy().into_owned();
    let f = fixture_with(Mode::Task, provider, |g| {
        g.question = "assess this candidate claim".into();
        g.permissions.execute = true;
        g.permissions.allowed_tools = vec!["sh".into()];
        g.permissions.require_approval_for = vec!["execute".into()];
        g.permissions.read_paths.push(read_dir);
    })
    .await;
    seed_item(&f, "a claim").await;

    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    assert_eq!(summary.state, RunState::WaitingForHuman);
    let pending = f
        .store
        .list_approvals(&f.run_id, Some(ApprovalState::Pending))
        .await
        .unwrap();
    f.store
        .decide_approval(&pending[0].id, false, None, Some("not this".into()))
        .await
        .unwrap();

    let summary = Scheduler::new(
        f.store.clone(),
        f.provider.clone(),
        SchedulerConfig::default(),
    )
    .run(&f.run_id)
    .await
    .unwrap();
    assert_eq!(summary.state, RunState::Completed, "{summary:?}");
    let tasks = f.store.list_tasks(&f.run_id).await.unwrap();
    let tool = tasks.iter().find(|t| t.role == Role::Tool).unwrap();
    assert_eq!(tool.state, TaskState::Cancelled);
    assert!(f.store.list_evidence(&f.run_id).await.unwrap().is_empty());
    assert!(
        f.store
            .list_experiments(&f.run_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.store
            .budget_usage(&f.run_id)
            .await
            .unwrap()
            .tool_executions,
        0
    );
}
