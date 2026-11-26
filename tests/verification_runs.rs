//! Reproduction is a measured, persisted result, not another model opinion.
mod common;

use common::*;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::runtime::execute_task;
use uruk::tools::ExecRequest;

fn tool_task(
    f: &Fixture,
    strategy: &str,
    inputs: Vec<String>,
    payload: Option<serde_json::Value>,
) -> Task {
    serde_json::from_value(serde_json::json!({
        "id": TaskId::new(), "schema_version": SCHEMA_VERSION, "run_id": f.run_id,
        "role": "tool", "strategy": strategy, "goal_id": f.goal.id, "plan_id": f.plan.id,
        "input_refs": inputs, "input_hash": ContentHash::of_str("verification fixture"),
        "prompt_id": null, "prompt_hash": null, "model": null, "payload": payload,
        "feedback_id": null, "depends_on": [], "priority": 1.0, "priority_rationale": "test",
        "permissions": f.goal.permissions, "reservation": {"model_calls": 0},
        "state": "running", "attempts": 1, "max_attempts": 1, "seed": 42,
        "output_refs": [], "actual_cost": {"model_calls":0,"prompt_tokens":0,"completion_tokens":0,"tool_executions":0,"cost_usd":null},
        "error": null, "created_at": "2026-01-01T00:00:00Z", "started_at": null, "finished_at": null
    })).unwrap()
}

async fn original(script: &str, supported: bool) -> (Fixture, ResearchItem, Experiment) {
    original_adjusted(script, supported, |_, _| {}).await
}

async fn original_adjusted(
    script: &str,
    supported: bool,
    adjust: impl FnOnce(&Fixture, &mut ExecRequest),
) -> (Fixture, ResearchItem, Experiment) {
    let f = fixture_with(Mode::Task, MockProvider::new(), |g| {
        g.permissions.execute = true;
        g.permissions.allowed_tools = vec!["sh".into()];
    })
    .await;
    let item = seed_item(&f, "computed claim").await;
    let mut request = ExecRequest::new("sh", vec!["-c".into(), script.into()]);
    request.seed = Some(42);
    adjust(&f, &mut request);
    let task = tool_task(
        &f,
        "exec",
        vec![item.id.to_string()],
        Some(serde_json::json!({
            "request": request, "target_item": item.id, "target_claim": "computed claim", "purpose": "check"
        })),
    );
    let outcome = execute_task(&context(&f, CancellationToken::new()), &task)
        .await
        .unwrap();
    assert_eq!(outcome.state, TaskState::Completed, "{:?}", outcome.error);
    for a in &outcome.batch.artifacts {
        f.store.insert_artifact(a).await.unwrap();
    }
    for e in &outcome.batch.evidence {
        f.store.insert_evidence(e).await.unwrap();
    }
    for l in &outcome.batch.links {
        f.store.link_evidence(l).await.unwrap();
    }
    let experiment = outcome.batch.experiments[0].clone();
    f.store.insert_experiment(&experiment).await.unwrap();
    if supported {
        let review = seed_review(&f, &item).await;
        f.store
            .record_assessment(&AssessmentRecord {
                item_id: item.id.clone(),
                assessment: Assessment::Supported,
                basis_review: Some(review.id),
                basis_decision: None,
                evidence: vec![EvidenceLink {
                    evidence_id: experiment.evidence_ids[0].clone(),
                    item_id: item.id.clone(),
                    claim: "computed claim".into(),
                    relation: EvidenceRelation::Supports,
                    justification: "the measured output supports this scoped claim".into(),
                    limits: None,
                }],
                rationale: "measured".into(),
                assessed_at: OffsetDateTime::now_utc(),
            })
            .await
            .unwrap();
    }
    (f, item, experiment)
}

async fn verify(f: &Fixture, id: &ExperimentId) -> uruk::runtime::TaskOutcome {
    execute_task(
        &context(f, CancellationToken::new()),
        &tool_task(f, "verify", vec![id.to_string()], None),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn reruns_supported_experiment_with_exact_recipe_and_new_artifacts() {
    if !containment_available() {
        return;
    }
    let (f, item, original) = original("printf 'result=42\\n'", true).await;
    let outcome = verify(&f, &original.id).await;
    assert_eq!(outcome.state, TaskState::Completed, "{:?}", outcome.error);
    assert_eq!(outcome.cost.model_calls, 0);
    assert_eq!(outcome.cost.tool_executions, 1);
    assert!(
        outcome.batch.assessments.is_empty(),
        "a reproduction is not a claim assessment"
    );
    assert!(
        outcome
            .batch
            .links
            .iter()
            .all(|l| l.relation == EvidenceRelation::Context)
    );
    let rerun = &outcome.batch.experiments[0];
    let value = serde_json::to_value(rerun).unwrap();
    assert_eq!(value["reproduction"]["verdict"], "reproduced");
    assert_eq!(
        value["reproduction"]["original_experiment_id"],
        original.id.as_str()
    );
    assert_eq!(value["execution"]["request"]["seed"], 42);
    assert_ne!(rerun.id, original.id);
    assert_ne!(
        rerun.execution.as_ref().unwrap().stdout_artifact,
        original.execution.as_ref().unwrap().stdout_artifact
    );
    assert_eq!(
        f.store.current_assessment(&item.id).await.unwrap(),
        Assessment::Supported
    );
}

#[tokio::test]
async fn changed_output_is_not_reproduced() {
    if !containment_available() {
        return;
    }
    let (f, _, original) = original("pwd", true).await;
    let outcome = verify(&f, &original.id).await;
    assert_eq!(outcome.state, TaskState::Completed, "{:?}", outcome.error);
    let value = serde_json::to_value(&outcome.batch.experiments[0]).unwrap();
    assert_eq!(value["reproduction"]["verdict"], "not_reproduced");
    assert_ne!(
        value["reproduction"]["original_stdout_hash"],
        value["reproduction"]["rerun_stdout_hash"]
    );
}

#[tokio::test]
async fn successful_but_uninterpreted_execution_cannot_be_verified_as_supported() {
    if !containment_available() {
        return;
    }
    let (f, _, original) = original("printf 42", false).await;
    let outcome = verify(&f, &original.id).await;
    assert_eq!(outcome.state, TaskState::Failed);
    assert_eq!(outcome.cost.tool_executions, 0);
    assert!(outcome.batch.experiments.is_empty());
    assert!(outcome.error.unwrap().contains("supporting evidence"));
}

#[tokio::test]
async fn missing_recipe_is_inconclusive_without_parsing_shell_display_text() {
    if !containment_available() {
        return;
    }
    let (f, _, original) = original("printf 42", true).await;
    let mut value = serde_json::to_value(&original).unwrap();
    value["id"] = serde_json::json!(ExperimentId::new());
    value["execution"]
        .as_object_mut()
        .unwrap()
        .remove("request");
    let legacy: Experiment = serde_json::from_value(value).unwrap();
    f.store.insert_experiment(&legacy).await.unwrap();
    let outcome = verify(&f, &legacy.id).await;
    assert_eq!(outcome.state, TaskState::Completed, "{:?}", outcome.error);
    assert_eq!(outcome.cost.tool_executions, 0);
    let value = serde_json::to_value(&outcome.batch.experiments[0]).unwrap();
    assert_eq!(value["reproduction"]["verdict"], "inconclusive");
    assert!(
        value["reproduction"]["reason"]
            .as_str()
            .unwrap()
            .contains("recipe")
    );
    assert!(value["execution"].is_null());
}

#[tokio::test]
async fn baseline_artifact_drift_is_inconclusive_and_does_not_execute() {
    if !containment_available() {
        return;
    }
    let (f, _, original) = original("printf 42", true).await;
    let artifact = f
        .store
        .get_artifact(
            original
                .execution
                .as_ref()
                .unwrap()
                .stdout_artifact
                .as_ref()
                .unwrap(),
        )
        .await
        .unwrap();
    tokio::fs::write(f.store.resolve_path(&artifact.storage_path), "tampered")
        .await
        .unwrap();
    let outcome = verify(&f, &original.id).await;
    assert_eq!(outcome.state, TaskState::Completed, "{:?}", outcome.error);
    assert_eq!(outcome.cost.tool_executions, 0);
    let value = serde_json::to_value(&outcome.batch.experiments[0]).unwrap();
    assert_eq!(value["reproduction"]["verdict"], "inconclusive");
}

#[tokio::test]
async fn changed_inputs_are_inconclusive_even_when_replay_succeeds() {
    if !containment_available() {
        return;
    }
    let (f, _, original) = original_adjusted("cat data.txt", true, |f, request| {
        let path = f.dir.path().join("data.txt");
        std::fs::write(&path, "before").unwrap();
        request.inputs = vec![path.to_string_lossy().into_owned()];
    })
    .await;
    tokio::fs::write(f.dir.path().join("data.txt"), "after")
        .await
        .unwrap();
    let outcome = verify(&f, &original.id).await;
    assert_eq!(outcome.state, TaskState::Completed, "{:?}", outcome.error);
    assert_eq!(outcome.cost.tool_executions, 1);
    let rerun = &outcome.batch.experiments[0];
    assert!(rerun.execution.as_ref().unwrap().may_support_claims());
    assert_eq!(
        rerun.reproduction.as_ref().unwrap().verdict,
        ReproductionVerdict::Inconclusive
    );
}

#[tokio::test]
async fn failed_replay_is_inconclusive_and_cannot_create_supporting_evidence() {
    if !containment_available() {
        return;
    }
    let (f, item, original) = original_adjusted("", true, |f, request| {
        let sentinel = f.dir.path().join("sentinel");
        std::fs::write(&sentinel, "present").unwrap();
        request.args = vec![
            "-c".into(),
            format!("test -f '{}' && printf 42", sentinel.display()),
        ];
    })
    .await;
    tokio::fs::remove_file(f.dir.path().join("sentinel"))
        .await
        .unwrap();
    let outcome = verify(&f, &original.id).await;
    assert_eq!(outcome.state, TaskState::Completed, "{:?}", outcome.error);
    let rerun = &outcome.batch.experiments[0];
    assert!(!rerun.execution.as_ref().unwrap().may_support_claims());
    assert_eq!(
        rerun.reproduction.as_ref().unwrap().verdict,
        ReproductionVerdict::Inconclusive
    );
    assert!(outcome.batch.evidence.is_empty());
    assert!(outcome.batch.assessments.is_empty());
    assert_eq!(
        f.store.current_assessment(&item.id).await.unwrap(),
        Assessment::Supported
    );
}

#[tokio::test]
async fn recorded_reproduction_is_not_scheduled_again() {
    if !containment_available() {
        return;
    }
    let (f, _, original) = original("printf 42", true).await;
    let work = uruk::agents::supervisor::select_work(
        &f.store,
        &f.run_id,
        &f.goal,
        &f.plan,
        &Default::default(),
        100,
        0,
    )
    .await
    .unwrap();
    assert_eq!(work.iter().filter(|w| w.strategy == "verify").count(), 1);
    let outcome = verify(&f, &original.id).await;
    f.store
        .insert_experiment(&outcome.batch.experiments[0])
        .await
        .unwrap();
    let work = uruk::agents::supervisor::select_work(
        &f.store,
        &f.run_id,
        &f.goal,
        &f.plan,
        &Default::default(),
        100,
        0,
    )
    .await
    .unwrap();
    assert!(!work.iter().any(|w| w.strategy == "verify"));
}

#[tokio::test]
async fn verification_cannot_borrow_another_runs_experiment() {
    let f = fixture(Mode::Task, MockProvider::new()).await;
    let outcome = verify(&f, &ExperimentId::new()).await;
    assert_eq!(outcome.state, TaskState::Failed);
    assert_eq!(outcome.cost.tool_executions, 0);
    assert!(outcome.batch.experiments.is_empty());
}
