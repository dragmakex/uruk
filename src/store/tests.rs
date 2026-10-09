//! Store-level invariant tests (SPEC §4.2, §4.3, §7, §9.2).

use super::*;
use crate::records::*;

pub(crate) fn test_run() -> (Run, Goal) {
    let run_id = RunId::new();
    let goal_id = GoalId::new();
    let t = now();
    let goal = Goal {
        id: goal_id.clone(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        revision: 0,
        question: "test question".into(),
        mode: Mode::Task,
        deliverables: vec!["report".into()],
        inputs: vec![],
        scope: None,
        exclusions: vec![],
        known_facts: vec![],
        assumptions: vec![],
        rubric: Rubric::default(),
        permissions: Permissions::default(),
        budget: Budget::default(),
        profile: None,
        revision_reason: None,
        created_at: t,
    };
    let run = Run {
        id: run_id,
        schema_version: SCHEMA_VERSION,
        project_id: ProjectId::from_raw("prj_test"),
        goal_id,
        plan_id: None,
        state: RunState::Running,
        stop_condition: None,
        iterations: 0,
        created_at: t,
        updated_at: t,
    };
    (run, goal)
}

pub(crate) fn test_item(run_id: &RunId, goal_id: &GoalId, title: &str) -> ResearchItem {
    let content = format!("content of {title}");
    ResearchItem {
        id: ItemId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        kind: ItemKind::Hypothesis,
        title: title.into(),
        content: content.clone(),
        hypothesis: Some(HypothesisFields {
            claim: format!("{title} claim"),
            ..Default::default()
        }),
        parent_ids: vec![],
        derivation: None,
        source_ids: vec![],
        author: Author::Agent {
            role: "generation".into(),
            strategy: "literature".into(),
        },
        produced_by: None,
        goal_id: goal_id.clone(),
        created_at: now(),
        content_hash: ContentHash::of_str(&content),
    }
}

pub(crate) fn test_task(run_id: &RunId, goal_id: &GoalId, plan_id: &PlanId, role: Role) -> Task {
    Task {
        id: TaskId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        role,
        strategy: "test".into(),
        goal_id: goal_id.clone(),
        plan_id: plan_id.clone(),
        input_refs: vec![],
        input_hash: ContentHash::of_str("input"),
        prompt_id: None,
        prompt_hash: None,
        model: None,
        provider: None,
        payload: None,
        feedback_id: None,
        depends_on: vec![],
        priority: 1.0,
        priority_rationale: "test".into(),
        permissions: Permissions::default(),
        reservation: CostReservation {
            model_calls: 1,
            tokens: 1000,
            tool_executions: 0,
        },
        state: TaskState::Pending,
        attempts: 0,
        max_attempts: 3,
        seed: 42,
        output_refs: vec![],
        actual_cost: CostActual::default(),
        error: None,
        created_at: now(),
        started_at: None,
        finished_at: None,
    }
}

pub(crate) fn test_plan(run_id: &RunId, goal_id: &GoalId) -> Plan {
    Plan {
        id: PlanId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        goal_id: goal_id.clone(),
        revision: 0,
        roles: vec!["reflection".into()],
        methods: vec![],
        priorities: vec![],
        outputs: vec![],
        stopping_conditions: vec![StopCondition::DeliverableSatisfied],
        rubric: Rubric::default(),
        rationale: "test plan".into(),
        created_at: now(),
    }
}

/// Insert a real review so an assessment has a basis that actually exists.
pub(crate) async fn stored_review(
    store: &Store,
    run_id: &RunId,
    item: &ResearchItem,
    proposed: Assessment,
) -> ReviewId {
    let review = Review {
        id: ReviewId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        item_id: item.id.clone(),
        item_hash: item.content_hash.clone(),
        strategy: ReviewStrategy::Full,
        assessment_text: "reviewed".into(),
        proposed_assessment: proposed,
        evidence: vec![],
        objections: vec![],
        unknowns: vec![],
        next_actions: vec![],
        execution_requests: vec![],
        observations: vec![],
        explanatory_label: None,
        score: None,
        author: Author::Agent {
            role: "reflection".into(),
            strategy: "full".into(),
        },
        produced_by: None,
        created_at: now(),
    };
    store.insert_review(&review).await.unwrap();
    review.id
}

async fn setup() -> (Store, Run, Goal) {
    let store = Store::open_in_memory().await.unwrap();
    let (run, goal) = test_run();
    store.ensure_project(&run.project_id, "test").await.unwrap();
    store.create_run(&run, &goal).await.unwrap();
    (store, run, goal)
}

#[tokio::test]
async fn items_are_immutable() {
    let (store, run, goal) = setup().await;
    let item = test_item(&run.id, &goal.id, "first");
    store.insert_item(&item).await.unwrap();

    // Re-inserting the same ID with different content must be refused.
    let mut mutated = item.clone();
    mutated.content = "rewritten".into();
    let err = store.insert_item(&mutated).await.unwrap_err();
    assert!(
        err.to_string().contains("immutable"),
        "expected immutability refusal, got: {err}"
    );
}

#[tokio::test]
async fn child_preserves_parent() {
    let (store, run, goal) = setup().await;
    let parent = test_item(&run.id, &goal.id, "parent");
    store.insert_item(&parent).await.unwrap();

    let mut child = test_item(&run.id, &goal.id, "child");
    child.parent_ids = vec![parent.id.clone()];
    child.derivation = Some("improved feasibility".into());
    store.insert_item(&child).await.unwrap();

    // Parent content is untouched.
    let reloaded = store.get_item(&parent.id).await.unwrap();
    assert_eq!(reloaded.content, parent.content);
    assert_eq!(reloaded.content_hash, parent.content_hash);

    let reloaded_child = store.get_item(&child.id).await.unwrap();
    assert_eq!(reloaded_child.parent_ids, vec![parent.id]);
    assert!(reloaded_child.is_child());
}

#[tokio::test]
async fn agent_critique_cannot_reach_supported() {
    let (store, run, goal) = setup().await;
    let item = test_item(&run.id, &goal.id, "hypothesis");
    store.insert_item(&item).await.unwrap();

    let critique = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run.id.clone(),
        kind: EvidenceKind::AgentCritique,
        content: "the panel agreed it is compelling".into(),
        method: "simulated debate".into(),
        inputs: vec![],
        source_ids: vec![],
        artifact_ids: vec![],
        limitations: None,
        produced_by: None,
        created_at: now(),
    };
    store.insert_evidence(&critique).await.unwrap();

    let review_id = stored_review(&store, &run.id, &item, Assessment::Plausible).await;
    let record = AssessmentRecord {
        item_id: item.id.clone(),
        assessment: Assessment::Supported,
        basis_review: Some(review_id),
        basis_decision: None,
        evidence: vec![EvidenceLink {
            evidence_id: critique.id.clone(),
            item_id: item.id.clone(),
            claim: "hypothesis claim".into(),
            relation: EvidenceRelation::Supports,
            justification: "the agents liked it".into(),
            limits: None,
        }],
        rationale: "consensus".into(),
        assessed_at: now(),
    };

    let err = store.record_assessment(&record).await.unwrap_err();
    assert!(
        err.to_string().contains("beyond agent opinion"),
        "expected §4.3 refusal, got: {err}"
    );

    // The item's assessment is unchanged.
    assert_eq!(
        store.current_assessment(&item.id).await.unwrap(),
        Assessment::Untested
    );
}

#[tokio::test]
async fn computational_evidence_reaches_supported() {
    let (store, run, goal) = setup().await;
    let item = test_item(&run.id, &goal.id, "hypothesis");
    store.insert_item(&item).await.unwrap();

    let result = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run.id.clone(),
        kind: EvidenceKind::ComputationalResult,
        content: "measured effect 0.42 (95% CI 0.31-0.53)".into(),
        method: "bootstrap over supplied dataset".into(),
        inputs: vec!["data.csv@sha256:abc".into()],
        source_ids: vec![],
        artifact_ids: vec![],
        limitations: Some("single dataset".into()),
        produced_by: None,
        created_at: now(),
    };
    store.insert_evidence(&result).await.unwrap();

    let basis = stored_review(&store, &run.id, &item, Assessment::Supported).await;
    let record = AssessmentRecord {
        item_id: item.id.clone(),
        assessment: Assessment::Supported,
        basis_review: Some(basis),
        basis_decision: None,
        evidence: vec![EvidenceLink {
            evidence_id: result.id.clone(),
            item_id: item.id.clone(),
            claim: "hypothesis claim".into(),
            relation: EvidenceRelation::Supports,
            justification: "effect is in the predicted direction".into(),
            limits: Some("one dataset only".into()),
        }],
        rationale: "computational check passed".into(),
        assessed_at: now(),
    };
    store.record_assessment(&record).await.unwrap();

    assert_eq!(
        store.current_assessment(&item.id).await.unwrap(),
        Assessment::Supported
    );
}

#[tokio::test]
async fn new_contradicting_evidence_marks_rating_stale() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();

    let a = test_item(&run.id, &goal.id, "a");
    let b = test_item(&run.id, &goal.id, "b");
    store.insert_item(&a).await.unwrap();
    store.insert_item(&b).await.unwrap();
    store.ensure_rating(&a.id, &plan.id).await.unwrap();
    store.ensure_rating(&b.id, &plan.id).await.unwrap();

    let m = test_match(&run.id, &a.id, &b.id, &plan.id, MatchOutcome::WinA);
    assert!(store.commit_match(&m, "m1", DEFAULT_K).await.unwrap());
    assert!(
        !store
            .get_rating(&a.id, &plan.id)
            .await
            .unwrap()
            .unwrap()
            .stale
    );

    // Contradicting evidence arrives for the winner.
    let ev = Evidence {
        id: EvidenceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run.id.clone(),
        kind: EvidenceKind::ExternalExperiment,
        content: "replication failed".into(),
        method: "independent lab".into(),
        inputs: vec![],
        source_ids: vec![],
        artifact_ids: vec![],
        limitations: None,
        produced_by: None,
        created_at: now(),
    };
    store.insert_evidence(&ev).await.unwrap();
    store
        .link_evidence(&EvidenceLink {
            evidence_id: ev.id,
            item_id: a.id.clone(),
            claim: "a claim".into(),
            relation: EvidenceRelation::Contradicts,
            justification: "did not replicate".into(),
            limits: None,
        })
        .await
        .unwrap();

    assert!(
        store
            .get_rating(&a.id, &plan.id)
            .await
            .unwrap()
            .unwrap()
            .stale,
        "contradicting evidence must mark the ranking stale (SPEC §7)"
    );
}

#[tokio::test]
async fn leaderboard_breaks_rating_ties_by_item_id() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();

    // Insert in reverse id order. Every enrolled candidate starts at the
    // same initial Elo, so without an explicit tiebreak the exported
    // leaderboard order would be whatever SQLite scans first.
    for id in ["item_c", "item_b", "item_a"] {
        let mut item = test_item(&run.id, &goal.id, id);
        item.id = ItemId::from_raw(id);
        store.insert_item(&item).await.unwrap();
        store.ensure_rating(&item.id, &plan.id).await.unwrap();
    }

    let ids: Vec<String> = store
        .leaderboard(&run.id, &plan.id)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.item_id.as_str().to_string())
        .collect();
    assert_eq!(ids, ["item_a", "item_b", "item_c"]);
}

pub(crate) fn test_match(
    run_id: &RunId,
    a: &ItemId,
    b: &ItemId,
    rubric: &PlanId,
    outcome: MatchOutcome,
) -> Match {
    Match {
        id: MatchId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        item_a: a.clone(),
        item_b: b.clone(),
        rubric_id: rubric.clone(),
        evidence_snapshot: ContentHash::of_str("snapshot"),
        order_randomized: true,
        method: MatchMethod::Pairwise,
        judge_model: "mock".into(),
        outcome,
        rationale: "test".into(),
        rating_a_before: 0.0,
        rating_b_before: 0.0,
        rating_a_after: 0.0,
        rating_b_after: 0.0,
        produced_by: None,
        created_at: now(),
    }
}

#[tokio::test]
async fn duplicate_match_delivery_updates_elo_once() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();
    let a = test_item(&run.id, &goal.id, "a");
    let b = test_item(&run.id, &goal.id, "b");
    store.insert_item(&a).await.unwrap();
    store.insert_item(&b).await.unwrap();

    let m = test_match(&run.id, &a.id, &b.id, &plan.id, MatchOutcome::WinA);
    assert!(store.commit_match(&m, "same-key", DEFAULT_K).await.unwrap());
    let after_first = store.get_rating(&a.id, &plan.id).await.unwrap().unwrap();
    assert!(
        (after_first.rating - 1216.0).abs() < 1e-9,
        "{}",
        after_first.rating
    );

    // Replay the same result, as a crash-recovery duplicate would.
    assert!(!store.commit_match(&m, "same-key", DEFAULT_K).await.unwrap());
    let after_replay = store.get_rating(&a.id, &plan.id).await.unwrap().unwrap();
    assert_eq!(after_first.rating, after_replay.rating);
    assert_eq!(after_replay.matches_played, 1);
}

#[tokio::test]
async fn insufficient_basis_never_updates_ratings() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();
    let a = test_item(&run.id, &goal.id, "a");
    let b = test_item(&run.id, &goal.id, "b");
    store.insert_item(&a).await.unwrap();
    store.insert_item(&b).await.unwrap();
    store.ensure_rating(&a.id, &plan.id).await.unwrap();
    store.ensure_rating(&b.id, &plan.id).await.unwrap();

    let m = test_match(
        &run.id,
        &a.id,
        &b.id,
        &plan.id,
        MatchOutcome::InsufficientBasis,
    );
    store.commit_match(&m, "k", DEFAULT_K).await.unwrap();

    let ra = store.get_rating(&a.id, &plan.id).await.unwrap().unwrap();
    assert_eq!(ra.rating, INITIAL_ELO);
    assert_eq!(ra.matches_played, 0);
}

#[tokio::test]
async fn rubric_revision_starts_a_new_cohort() {
    let (store, run, goal) = setup().await;
    let plan_v0 = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan_v0).await.unwrap();
    let a = test_item(&run.id, &goal.id, "a");
    let b = test_item(&run.id, &goal.id, "b");
    store.insert_item(&a).await.unwrap();
    store.insert_item(&b).await.unwrap();

    let m = test_match(&run.id, &a.id, &b.id, &plan_v0.id, MatchOutcome::WinA);
    store.commit_match(&m, "m", DEFAULT_K).await.unwrap();
    assert!(
        store
            .get_rating(&a.id, &plan_v0.id)
            .await
            .unwrap()
            .unwrap()
            .rating
            > INITIAL_ELO
    );

    // A new rubric revision: the same item starts fresh.
    let mut plan_v1 = test_plan(&run.id, &goal.id);
    plan_v1.revision = 1;
    store.insert_plan(&plan_v1).await.unwrap();
    store.ensure_rating(&a.id, &plan_v1.id).await.unwrap();

    let new_cohort = store.get_rating(&a.id, &plan_v1.id).await.unwrap().unwrap();
    assert_eq!(new_cohort.rating, INITIAL_ELO);
    assert_eq!(new_cohort.matches_played, 0);
}

#[tokio::test]
async fn review_against_stale_content_is_refused() {
    let (store, run, goal) = setup().await;
    let item = test_item(&run.id, &goal.id, "target");
    store.insert_item(&item).await.unwrap();

    let review = Review {
        id: ReviewId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run.id.clone(),
        item_id: item.id.clone(),
        item_hash: ContentHash::of_str("some other content"),
        strategy: ReviewStrategy::Initial,
        assessment_text: "looks fine".into(),
        proposed_assessment: Assessment::Plausible,
        evidence: vec![],
        objections: vec![],
        unknowns: vec![],
        next_actions: vec![],
        execution_requests: vec![],
        observations: vec![],
        explanatory_label: None,
        score: None,
        author: Author::Agent {
            role: "reflection".into(),
            strategy: "initial".into(),
        },
        produced_by: None,
        created_at: now(),
    };
    let err = store.insert_review(&review).await.unwrap_err();
    assert!(err.to_string().contains("hash"), "got: {err}");
}

#[tokio::test]
async fn approval_binds_to_exact_payload() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();
    let task = test_task(&run.id, &goal.id, &plan.id, Role::Tool);
    store.enqueue_task(&task, None).await.unwrap();

    let payload = serde_json::json!({"action": "run", "script": "analyze.py"});
    let req = ApprovalRequest {
        id: RequestId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run.id.clone(),
        task_id: Some(task.id.clone()),
        action: "execute analyze.py".into(),
        payload: payload.clone(),
        payload_hash: ContentHash::of_json(&payload).unwrap(),
        state: ApprovalState::Pending,
        decided_by: None,
        decided_reason: None,
        created_at: now(),
        decided_at: None,
    };
    store.insert_approval(&req).await.unwrap();

    // Approving a *different* payload must be refused.
    let other =
        ContentHash::of_json(&serde_json::json!({"action": "run", "script": "evil.py"})).unwrap();
    let err = store
        .decide_approval(&req.id, true, Some(&other), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("changed request"), "got: {err}");

    // The exact payload is accepted, and the task is unblocked.
    let decided = store
        .decide_approval(&req.id, true, Some(&req.payload_hash), None)
        .await
        .unwrap();
    assert_eq!(decided.state, ApprovalState::Approved);
    assert_eq!(
        store.get_task(&task.id).await.unwrap().state,
        TaskState::Pending
    );
}

#[tokio::test]
async fn denial_cancels_the_waiting_task() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();
    let mut task = test_task(&run.id, &goal.id, &plan.id, Role::Tool);
    task.state = TaskState::WaitingForHuman;
    store.enqueue_task(&task, None).await.unwrap();

    let payload = serde_json::json!({"action": "delete everything"});
    let req = ApprovalRequest {
        id: RequestId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run.id.clone(),
        task_id: Some(task.id.clone()),
        action: "dangerous".into(),
        payload: payload.clone(),
        payload_hash: ContentHash::of_json(&payload).unwrap(),
        state: ApprovalState::Pending,
        decided_by: None,
        decided_reason: None,
        created_at: now(),
        decided_at: None,
    };
    store.insert_approval(&req).await.unwrap();
    store
        .decide_approval(&req.id, false, None, Some("no".into()))
        .await
        .unwrap();

    assert_eq!(
        store.get_task(&task.id).await.unwrap().state,
        TaskState::Cancelled
    );
}

#[tokio::test]
async fn goal_revision_invalidates_pending_approvals() {
    let (store, run, goal) = setup().await;
    let payload = serde_json::json!({"a": 1});
    let req = ApprovalRequest {
        id: RequestId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run.id.clone(),
        task_id: None,
        action: "something".into(),
        payload: payload.clone(),
        payload_hash: ContentHash::of_json(&payload).unwrap(),
        state: ApprovalState::Pending,
        decided_by: None,
        decided_reason: None,
        created_at: now(),
        decided_at: None,
    };
    store.insert_approval(&req).await.unwrap();

    let mut revised = goal.clone();
    revised.id = GoalId::new();
    revised.revision = 1;
    revised.question = "changed question".into();
    let n = store.revise_goal(&revised).await.unwrap();
    assert_eq!(n, 1, "pending approval should be invalidated");

    assert_eq!(
        store.get_approval(&req.id).await.unwrap().state,
        ApprovalState::Invalidated
    );
}

#[tokio::test]
async fn budget_reserves_output_allowance() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();

    let budget = Budget {
        max_model_calls: 10,
        reserve_calls_for_output: 4,
        ..Default::default()
    };

    // Ordinary work may use at most 10 - 4 = 6 calls.
    let mut task = test_task(&run.id, &goal.id, &plan.id, Role::Reflection);
    task.reservation.model_calls = 6;
    store.enqueue_task(&task, None).await.unwrap();
    store
        .reserve_budget(&run.id, &task.id, &task.reservation, &budget, false)
        .await
        .unwrap();

    let mut task2 = test_task(&run.id, &goal.id, &plan.id, Role::Reflection);
    task2.reservation.model_calls = 1;
    store.enqueue_task(&task2, None).await.unwrap();
    let err = store
        .reserve_budget(&run.id, &task2.id, &task2.reservation, &budget, false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("reserved for final output"),
        "got: {err}"
    );
}

#[tokio::test]
async fn settlement_is_exactly_once() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();
    let task = test_task(&run.id, &goal.id, &plan.id, Role::Reflection);
    store.enqueue_task(&task, None).await.unwrap();
    store
        .reserve_budget(
            &run.id,
            &task.id,
            &task.reservation,
            &Budget::default(),
            false,
        )
        .await
        .unwrap();
    store.claim_task(&task.id).await.unwrap();

    let actual = CostActual {
        model_calls: 1,
        prompt_tokens: 500,
        completion_tokens: 200,
        tool_executions: 0,
        cost_usd: Some(0.01),
    };
    store
        .complete_task(&task.id, TaskState::Completed, vec![], actual.clone(), None)
        .await
        .unwrap();

    let usage = store.budget_usage(&run.id).await.unwrap();
    assert_eq!(usage.model_calls, 1);
    assert_eq!(usage.tokens, 700);
    assert_eq!(usage.reserved_calls, 0, "reservation should be released");

    // Duplicate delivery must not double-charge.
    store
        .complete_task(&task.id, TaskState::Completed, vec![], actual, None)
        .await
        .unwrap();
    let usage2 = store.budget_usage(&run.id).await.unwrap();
    assert_eq!(usage2.model_calls, 1, "settlement must be exactly once");
    assert_eq!(usage2.tokens, 700);
}

#[tokio::test]
async fn unknown_cost_is_not_recorded_as_zero() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();
    let task = test_task(&run.id, &goal.id, &plan.id, Role::Reflection);
    store.enqueue_task(&task, None).await.unwrap();
    store.claim_task(&task.id).await.unwrap();
    store
        .complete_task(
            &task.id,
            TaskState::Completed,
            vec![],
            CostActual {
                model_calls: 1,
                cost_usd: None,
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

    let usage = store.budget_usage(&run.id).await.unwrap();
    assert_eq!(usage.cost_usd, None, "unknown cost must not become 0.0");
    assert!(usage.cost_partially_unknown);
}

#[tokio::test]
async fn dependencies_gate_readiness() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();

    let first = test_task(&run.id, &goal.id, &plan.id, Role::Generation);
    store.enqueue_task(&first, None).await.unwrap();

    let mut second = test_task(&run.id, &goal.id, &plan.id, Role::Reflection);
    second.depends_on = vec![first.id.clone()];
    store.enqueue_task(&second, None).await.unwrap();

    let ready = store.ready_tasks(&run.id, 10).await.unwrap();
    assert_eq!(ready.len(), 1, "dependent task must not be ready");
    assert_eq!(ready[0].id, first.id);

    store.claim_task(&first.id).await.unwrap();
    store
        .complete_task(
            &first.id,
            TaskState::Completed,
            vec![],
            CostActual::default(),
            None,
        )
        .await
        .unwrap();

    let ready = store.ready_tasks(&run.id, 10).await.unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, second.id, "dependency met, now ready");
}

#[tokio::test]
async fn a_task_is_claimed_once() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();
    let task = test_task(&run.id, &goal.id, &plan.id, Role::Generation);
    store.enqueue_task(&task, None).await.unwrap();

    assert!(store.claim_task(&task.id).await.unwrap());
    assert!(
        !store.claim_task(&task.id).await.unwrap(),
        "no double claim"
    );
}

#[tokio::test]
async fn interrupted_tool_task_becomes_uncertain() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();

    let model_task = test_task(&run.id, &goal.id, &plan.id, Role::Generation);
    let tool_task = test_task(&run.id, &goal.id, &plan.id, Role::Tool);
    store.enqueue_task(&model_task, None).await.unwrap();
    store.enqueue_task(&tool_task, None).await.unwrap();
    store.claim_task(&model_task.id).await.unwrap();
    store.claim_task(&tool_task.id).await.unwrap();

    // Simulate a crash and restart.
    let uncertain = store.reconcile_interrupted(&run.id).await.unwrap();
    assert_eq!(uncertain, vec![tool_task.id.clone()]);
    assert_eq!(
        store.get_task(&model_task.id).await.unwrap().state,
        TaskState::Pending,
        "a pure model call is safe to retry"
    );
    assert_eq!(
        store.get_task(&tool_task.id).await.unwrap().state,
        TaskState::Uncertain,
        "an external effect of unknown outcome must not be blindly retried"
    );
}

#[tokio::test]
async fn scheduler_choices_replay_rather_than_redraw() {
    let (store, run, _goal) = setup().await;
    let first = store
        .record_or_replay_choice(&run.id, 0, "pair", "a-vs-b")
        .await
        .unwrap();
    assert_eq!(first, "a-vs-b");

    // On resume a fresh draw is offered, but the persisted choice wins.
    let replayed = store
        .record_or_replay_choice(&run.id, 0, "pair", "c-vs-d")
        .await
        .unwrap();
    assert_eq!(replayed, "a-vs-b", "resume must replay the recorded choice");
}

#[tokio::test]
async fn completed_task_is_reused_by_dedupe_key() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();

    let task = test_task(&run.id, &goal.id, &plan.id, Role::Reflection);
    let id1 = store.enqueue_task(&task, Some("dk")).await.unwrap();
    store.claim_task(&id1).await.unwrap();
    store
        .complete_task(
            &id1,
            TaskState::Completed,
            vec![],
            CostActual::default(),
            None,
        )
        .await
        .unwrap();

    let other = test_task(&run.id, &goal.id, &plan.id, Role::Reflection);
    let id2 = store.enqueue_task(&other, Some("dk")).await.unwrap();
    assert_eq!(id1, id2, "identical work must reuse the completed task");
    assert_eq!(store.list_tasks(&run.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn retries_are_bounded() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();
    let mut task = test_task(&run.id, &goal.id, &plan.id, Role::Generation);
    task.max_attempts = 2;
    store.enqueue_task(&task, None).await.unwrap();

    store.claim_task(&task.id).await.unwrap();
    let spent = CostActual {
        model_calls: 1,
        ..Default::default()
    };
    assert!(
        store
            .requeue_task(&task.id, "transient", &spent)
            .await
            .unwrap()
    );
    store.claim_task(&task.id).await.unwrap();
    assert!(
        !store
            .requeue_task(&task.id, "transient again", &spent)
            .await
            .unwrap(),
        "second attempt exhausts max_attempts"
    );
    assert_eq!(
        store.get_task(&task.id).await.unwrap().state,
        TaskState::Failed
    );
    // Both attempts were charged (SPEC §6: retries are bounded and charged).
    assert_eq!(store.budget_usage(&run.id).await.unwrap().model_calls, 2);
}

#[tokio::test]
async fn duplicate_disposition_keeps_provenance() {
    let (store, run, goal) = setup().await;
    let rep = test_item(&run.id, &goal.id, "representative");
    let dup = test_item(&run.id, &goal.id, "duplicate");
    store.insert_item(&rep).await.unwrap();
    store.insert_item(&dup).await.unwrap();

    store
        .set_disposition(
            &dup.id,
            &Disposition::Duplicate(rep.id.clone()),
            Some("same mechanism and predictions"),
        )
        .await
        .unwrap();

    assert_eq!(
        store.get_disposition(&dup.id).await.unwrap(),
        Disposition::Duplicate(rep.id)
    );
    // The duplicate's own content survives.
    assert_eq!(store.get_item(&dup.id).await.unwrap().title, "duplicate");
}

#[tokio::test]
async fn state_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");

    let (run, goal) = test_run();
    {
        let store = Store::open(&path).await.unwrap();
        store.ensure_project(&run.project_id, "p").await.unwrap();
        store.create_run(&run, &goal).await.unwrap();
        let item = test_item(&run.id, &goal.id, "persisted");
        store.insert_item(&item).await.unwrap();
        store.close().await;
    }

    let store = Store::open(&path).await.unwrap();
    let items = store.list_items(&run.id).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "persisted");
    assert_eq!(
        store.get_run(&run.id).await.unwrap().state,
        RunState::Running
    );
}

#[tokio::test]
async fn assessment_citing_a_nonexistent_review_is_refused() {
    let (store, run, goal) = setup().await;
    let item = test_item(&run.id, &goal.id, "hypothesis");
    store.insert_item(&item).await.unwrap();

    let record = AssessmentRecord {
        item_id: item.id.clone(),
        assessment: Assessment::Plausible,
        basis_review: Some(ReviewId::new()), // never stored
        basis_decision: None,
        evidence: vec![],
        rationale: "trust me".into(),
        assessed_at: now(),
    };
    let err = store.record_assessment(&record).await.unwrap_err();
    assert!(err.to_string().contains("does not exist"), "got: {err}");
    assert_eq!(
        store.current_assessment(&item.id).await.unwrap(),
        Assessment::Untested
    );
}

#[tokio::test]
async fn invalidated_approval_cannot_be_decided() {
    let (store, run, goal) = setup().await;
    let payload = serde_json::json!({"action": "x"});
    let req = ApprovalRequest {
        id: RequestId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run.id.clone(),
        task_id: None,
        action: "x".into(),
        payload: payload.clone(),
        payload_hash: ContentHash::of_json(&payload).unwrap(),
        state: ApprovalState::Pending,
        decided_by: None,
        decided_reason: None,
        created_at: now(),
        decided_at: None,
    };
    store.insert_approval(&req).await.unwrap();

    let mut revised = goal.clone();
    revised.id = GoalId::new();
    revised.revision = 1;
    store.revise_goal(&revised).await.unwrap();

    // A stale approval must not be usable after the goal changed.
    let err = store
        .decide_approval(&req.id, true, Some(&req.payload_hash), None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("already invalidated"),
        "got: {err}"
    );
}

#[tokio::test]
async fn revision_counters_start_at_zero() {
    let (store, run, _goal) = setup().await;
    // `MAX(revision)` over an empty table is NULL; the counter must start at
    // 0 rather than wrapping through `-1 as u32`.
    assert_eq!(store.next_feedback_revision(&run.id).await.unwrap(), 0);
    assert_eq!(store.next_plan_revision(&run.id).await.unwrap(), 0);
}

#[tokio::test]
async fn concurrent_writers_do_not_collide() {
    // SQLite allows one writer; colliding transactions fail with
    // SQLITE_BUSY_SNAPSHOT rather than waiting, so writes are serialized.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state.sqlite")).await.unwrap();
    let (run, goal) = test_run();
    store.ensure_project(&run.project_id, "p").await.unwrap();
    store.create_run(&run, &goal).await.unwrap();

    let mut handles = Vec::new();
    for i in 0..16 {
        let store = store.clone();
        let run_id = run.id.clone();
        let goal_id = goal.id.clone();
        handles.push(tokio::spawn(async move {
            let item = test_item(&run_id, &goal_id, &format!("item {i}"));
            store.insert_item(&item).await
        }));
    }

    for handle in handles {
        handle
            .await
            .unwrap()
            .expect("concurrent write must not fail");
    }
    assert_eq!(store.list_items(&run.id).await.unwrap().len(), 16);
}

/// Two `Store` handles on one file behave like two processes: a scheduler
/// and another writer (e.g. a stop request) writing at once must queue,
/// not fail (SPEC §9.2).
#[tokio::test]
async fn concurrent_writers_across_stores_do_not_collide() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".uruk/state.sqlite");
    let store_a = Store::open(&path).await.unwrap();
    let store_b = Store::open(&path).await.unwrap();
    let (run, goal) = test_run();
    store_a.ensure_project(&run.project_id, "p").await.unwrap();
    store_a.create_run(&run, &goal).await.unwrap();
    let plan = test_plan(&run.id, &goal.id);
    store_a.insert_plan(&plan).await.unwrap();

    let mut ids = Vec::new();
    for _ in 0..40 {
        let task = test_task(&run.id, &goal.id, &plan.id, Role::Reflection);
        store_a.enqueue_task(&task, None).await.unwrap();
        ids.push(task.id);
    }

    // claim_task reads then writes inside one transaction: the pattern that
    // fails with SQLITE_BUSY_SNAPSHOT under deferred transactions.
    let mut handles = Vec::new();
    for (i, id) in ids.into_iter().enumerate() {
        let store = if i % 2 == 0 {
            store_a.clone()
        } else {
            store_b.clone()
        };
        handles.push(tokio::spawn(async move { store.claim_task(&id).await }));
    }
    for h in handles {
        assert!(h.await.unwrap().unwrap(), "every claim must succeed");
    }
    assert_eq!(
        store_a
            .count_tasks_in_state(&run.id, TaskState::Running)
            .await
            .unwrap(),
        40
    );
}

/// A task's records, outcome, and settlement commit together, or not at all,
/// and a replayed commit changes nothing (SPEC §9.2).
#[tokio::test]
async fn commit_task_is_atomic_and_idempotent() {
    let (store, run, goal) = setup().await;
    let plan = test_plan(&run.id, &goal.id);
    store.insert_plan(&plan).await.unwrap();

    // A batch whose assessment fails the §4.3 gate commits nothing.
    let bad_task = test_task(&run.id, &goal.id, &plan.id, Role::Reflection);
    store.enqueue_task(&bad_task, None).await.unwrap();
    store.claim_task(&bad_task.id).await.unwrap();
    let item = test_item(&run.id, &goal.id, "gated");
    let mut batch = RecordBatch::default();
    batch.items.push(item.clone());
    batch.assessments.push(AssessmentRecord {
        item_id: item.id.clone(),
        assessment: Assessment::Supported,
        basis_review: None,
        basis_decision: None,
        evidence: vec![],
        rationale: "no basis".into(),
        assessed_at: now(),
    });
    assert!(
        store
            .commit_task(
                &bad_task.id,
                TaskState::Completed,
                &batch,
                CostActual::default(),
                None
            )
            .await
            .is_err()
    );
    assert!(
        store.get_item(&item.id).await.is_err(),
        "a rejected batch must leave no item behind"
    );
    assert_eq!(
        store.get_task(&bad_task.id).await.unwrap().state,
        TaskState::Running,
        "the task outcome must not commit without its records"
    );

    // A good batch commits once; replaying it is a no-op.
    let task = test_task(&run.id, &goal.id, &plan.id, Role::Generation);
    store.enqueue_task(&task, None).await.unwrap();
    store
        .reserve_budget(
            &run.id,
            &task.id,
            &task.reservation,
            &Budget::default(),
            false,
        )
        .await
        .unwrap();
    store.claim_task(&task.id).await.unwrap();
    let item = test_item(&run.id, &goal.id, "committed");
    let mut batch = RecordBatch::default();
    batch.items.push(item.clone());
    batch.ratings.push((item.id.clone(), plan.id.clone()));
    let spent = CostActual {
        model_calls: 1,
        prompt_tokens: 10,
        completion_tokens: 5,
        ..Default::default()
    };
    let refs = store
        .commit_task(&task.id, TaskState::Completed, &batch, spent.clone(), None)
        .await
        .unwrap();
    assert_eq!(refs, vec![item.id.0.clone()]);
    let again = store
        .commit_task(&task.id, TaskState::Completed, &batch, spent, None)
        .await
        .unwrap();
    assert!(again.is_empty(), "duplicate delivery must be ignored");
    assert_eq!(store.list_items(&run.id).await.unwrap().len(), 1);
    let usage = store.budget_usage(&run.id).await.unwrap();
    assert_eq!(usage.model_calls, 1);
    assert_eq!(usage.reserved_calls, 0);
    assert_eq!(
        store.get_task(&task.id).await.unwrap().output_refs,
        vec![item.id.0]
    );
}

// --- Literature search: passages, FTS5, works (migration 0002) ---

fn test_source(run_id: &RunId, text: &str) -> Source {
    Source {
        id: SourceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        origin: Origin::Supplied,
        title: Some("test source".into()),
        authors: Some("Test Author".into()),
        date: Some("2025".into()),
        identifier: None,
        retrieved_at: now(),
        content_hash: ContentHash::of_str(text),
        access: AccessLevel::FullText,
        access_limitations: None,
        text_artifact: Some(ArtifactId::new()),
    }
}

/// Migration smoke test: a file-backed store opens with migration 0002
/// applied, and an indexed passage is visible through FTS5 — proving the
/// bundled SQLite has FTS5 and the triggers keep the index in sync.
#[tokio::test]
async fn migration_applies_and_fts5_search_works() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .unwrap();
    let (run, goal) = test_run();
    store.ensure_project(&run.project_id, "test").await.unwrap();
    store.create_run(&run, &goal).await.unwrap();
    let _ = goal;

    let text = "Catalyst degradation accelerates under thermal cycling.\n\n\
                A control paragraph about unrelated sampling procedures.";
    let source = test_source(&run.id, text);
    store.insert_source(&source).await.unwrap();
    let indexed = store.index_source_passages(&source, text).await.unwrap();
    assert!(indexed >= 1);
    assert_eq!(store.count_passages(&run.id).await.unwrap(), indexed as u64);

    let hits = store
        .search_passages(&run.id, "catalyst degradation", 10)
        .await
        .unwrap();
    assert!(!hits.is_empty(), "FTS5 must find the indexed passage");
    let hit = &hits[0];
    assert_eq!(hit.source_id, source.id);
    assert_eq!(
        &text[hit.byte_start..hit.byte_end],
        hit.text,
        "byte span must slice the canonical text exactly"
    );
    assert!(hit.score.is_finite());
    assert_eq!(
        store
            .passage_artifact_hash(&source.id, hit.seq)
            .await
            .unwrap(),
        ContentHash::of_str(text)
    );
}

#[tokio::test]
async fn reindexing_a_source_is_idempotent() {
    let (store, run, _goal) = setup().await;
    let text = "One paragraph about electrodes.\n\nAnother paragraph about fouling.";
    let source = test_source(&run.id, text);
    store.insert_source(&source).await.unwrap();

    let first = store.index_source_passages(&source, text).await.unwrap();
    let second = store.index_source_passages(&source, text).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(store.count_passages(&run.id).await.unwrap(), first as u64);

    // The FTS index must not have doubled either.
    let hits = store.search_passages(&run.id, "fouling", 10).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[tokio::test]
async fn bm25_ordering_is_deterministic_and_relevance_ranked() {
    let (store, run, _goal) = setup().await;
    let relevant = "Catalyst catalyst catalyst degradation in catalyst beds.";
    let marginal = "A passing mention of a catalyst among many other topics \
                    like sampling, electrodes, and instrumentation drift.";
    for text in [relevant, marginal] {
        let source = test_source(&run.id, text);
        store.insert_source(&source).await.unwrap();
        store.index_source_passages(&source, text).await.unwrap();
    }

    let a = store
        .search_passages(&run.id, "catalyst", 10)
        .await
        .unwrap();
    let b = store
        .search_passages(&run.id, "catalyst", 10)
        .await
        .unwrap();
    assert_eq!(a.len(), 2);
    let order = |hits: &[PassageHit]| hits.iter().map(|h| h.text.clone()).collect::<Vec<_>>();
    assert_eq!(order(&a), order(&b), "same query, same order");
    assert!(
        a[0].text.contains("beds"),
        "denser match ranks first: {a:?}"
    );
    assert!(a[0].score >= a[1].score);
}

#[tokio::test]
async fn works_round_trip_with_hits_in_the_body() {
    use crate::search::types::{WorkHit, WorkKey, WorkRecord};

    let (store, run, _goal) = setup().await;
    let search_id = SearchId::new();
    let mut work = WorkRecord {
        title: "A discovered work".into(),
        doi: Some("10.1/x".into()),
        year: Some(2024),
        // Ranks and raw hashes live only in the body (§ revised 6/13).
        hits: vec![WorkHit {
            connector: "openalex".into(),
            search_id: search_id.clone(),
            rank: 1,
            raw_hash: ContentHash::of_str("raw"),
        }],
        ..Default::default()
    };
    work.work_key = WorkKey("doi:10.1/x".into());

    store.insert_work(&run.id, &work, 0.032).await.unwrap();
    // Replay is harmless.
    store.insert_work(&run.id, &work, 0.032).await.unwrap();

    let record = SearchRecord {
        id: search_id.clone(),
        run_id: run.id.clone(),
        query: "catalyst degradation".into(),
        tool: "openalex".into(),
        executed_at: now(),
        filters: None,
        results_found: 1,
        results_retrieved: vec![],
        unavailable: vec![],
        connector_errors: vec![],
    };
    store.insert_search_record(&record).await.unwrap();

    let works = store.list_works(&run.id).await.unwrap();
    assert_eq!(works.len(), 1, "idempotent insert");
    assert_eq!(works[0].work.title, "A discovered work");
    assert_eq!(works[0].rrf_score, Some(0.032));
    assert!(works[0].source_id.is_none());
    assert_eq!(works[0].work.hits.len(), 1, "hits round-trip via body");
    assert_eq!(works[0].work.hits[0].search_id, search_id);
    assert_eq!(works[0].work.hits[0].rank, 1);

    let source = test_source(&run.id, "abstract text");
    store.insert_source(&source).await.unwrap();
    store
        .set_work_source(&run.id, &work.work_key, &source.id)
        .await
        .unwrap();
    assert_eq!(
        store
            .get_work(&run.id, "doi:10.1/x")
            .await
            .unwrap()
            .source_id,
        Some(source.id.clone())
    );

    // Search records are addressed by the id inside the JSON body.
    let updated = store
        .update_search_record(&run.id, &search_id, |r| {
            r.results_retrieved.push(source.id.clone());
        })
        .await
        .unwrap();
    assert!(updated);
    assert!(
        !store
            .update_search_record(&run.id, &SearchId::new(), |_| {})
            .await
            .unwrap(),
        "an unknown id updates nothing"
    );
    let records = store.list_search_records(&run.id).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, search_id);
    assert_eq!(records[0].results_retrieved, vec![source.id]);
}

#[tokio::test]
async fn marking_a_run_running_is_refused_once_it_is_cancelled() {
    let (store, run, _goal) = setup().await;

    assert!(
        store.mark_run_running(&run.id).await.unwrap(),
        "a live run may (re)enter running"
    );

    store
        .set_run_state(&run.id, RunState::Cancelled, Some(StopCondition::Cancelled))
        .await
        .unwrap();
    assert!(
        !store.mark_run_running(&run.id).await.unwrap(),
        "a durable stop request must not be overwritten by a starting scheduler"
    );
    assert_eq!(
        store.get_run(&run.id).await.unwrap().state,
        RunState::Cancelled
    );
}

#[tokio::test]
async fn cancelling_a_run_is_refused_once_it_is_terminal() {
    let (store, run, _goal) = setup().await;

    assert!(
        store.try_cancel_run(&run.id).await.unwrap(),
        "a live run can be cancelled"
    );
    assert_eq!(
        store.get_run(&run.id).await.unwrap().state,
        RunState::Cancelled
    );

    let (run2, goal2) = test_run();
    store.create_run(&run2, &goal2).await.unwrap();
    store
        .set_run_state(
            &run2.id,
            RunState::Completed,
            Some(StopCondition::DeliverableSatisfied),
        )
        .await
        .unwrap();
    assert!(
        !store.try_cancel_run(&run2.id).await.unwrap(),
        "a finished run must not be flipped to cancelled by a racing stop"
    );
    assert_eq!(
        store.get_run(&run2.id).await.unwrap().state,
        RunState::Completed
    );
}

#[tokio::test]
async fn project_identity_is_stable_and_derived_from_the_project_dir() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .unwrap();

    let (id, name) = store.project_identity();
    let (id_again, name_again) = store.project_identity();
    assert_eq!(id, id_again, "identity is deterministic");
    assert_eq!(name, name_again);
    assert_eq!(name, store.project_dir().to_string_lossy());
    assert!(id.as_str().starts_with("prj_"), "id: {}", id.as_str());
}
