//! Shared fixtures for the acceptance checks (SPEC §9.3, §13).
//!
//! Every test runs against a temporary SQLite database and a deterministic
//! mock provider: no paid LLM, live network, or laboratory (SPEC §13).

#![allow(dead_code)]

use std::sync::Arc;
use time::OffsetDateTime;
use uruk::agents::supervisor;
use uruk::provider::MockProvider;
use uruk::records::*;
use uruk::store::Store;

pub struct Fixture {
    pub store: Store,
    pub run_id: RunId,
    pub goal: Goal,
    pub plan: Plan,
    pub provider: Arc<MockProvider>,
    pub dir: tempfile::TempDir,
}

/// Distinctive phrases in each template, for mock rules. Each appears in
/// exactly one template and in no fixture reply.
pub mod anchor {
    pub const GENERATION_LITERATURE: &str = "formulating a hypothesis that addresses";
    pub const GENERATION_DEBATE: &str = "collaborative discourse";
    pub const REFLECTION_INITIAL: &str = "fast screening review";
    pub const REFLECTION_FULL: &str = "source-grounded review of a research item";
    pub const REFLECTION_DEEP: &str = "deep verification review";
    pub const REFLECTION_OBSERVATION: &str = "expert in scientific hypothesis evaluation";
    pub const REFLECTION_SIMULATION: &str = "step by step to find where it";
    pub const RANKING_PAIRWISE: &str = "comparing two hypotheses against a shared goal";
    pub const RANKING_DEBATE: &str = "simulating a panel of domain experts";
    pub const EVOLUTION_FEASIBILITY: &str = "technological feasibility";
    pub const EVOLUTION_ANALOGY: &str = "generating one new hypothesis inspired by";
    pub const PROXIMITY: &str = "genuinely the same proposal";
    pub const META_REVIEW: &str = "qualitative synthesis";
    pub const META_OVERVIEW: &str = "periodic overview";
    pub const TASK_SYNTHESIS: &str = "Requested deliverable:";
    pub const SEARCH_PLAN_QUERIES: &str = "planning literature search queries";
}

/// A hypothesis reply matching the `generation.literature` contract.
pub fn hypothesis_json(title: &str, claim: &str) -> String {
    serde_json::json!({
        "title": title,
        "claim": claim,
        "mechanism": format!("proposed mechanism for {title}"),
        "assumptions": ["the measurement is valid"],
        "scope": "laboratory conditions",
        "predictions": [format!("{title} predicts a measurable shift")],
        "falsifiers": ["no shift under controlled replication"],
        "validation_plan": "replicate across three independent sites",
        "grounding": [],
        "limitations": "no direct evidence yet",
        "provisional": false
    })
    .to_string()
}

/// A concluding `generation.debate` turn.
pub fn debate_final_json(title: &str, claim: &str) -> String {
    let hypothesis: serde_json::Value =
        serde_json::from_str(&hypothesis_json(title, claim)).unwrap();
    serde_json::json!({
        "status": "final",
        "contribution": "the panel converged",
        "hypothesis": hypothesis
    })
    .to_string()
}

/// A review reply matching the review contract.
pub fn review_json(assessment: &str, objection: &str, fatal: bool) -> String {
    serde_json::json!({
        "assessment_text": format!("review finding: {objection}"),
        "proposed_assessment": assessment,
        "evidence": [],
        "objections": [{
            "description": objection,
            "fatal": fatal,
            "targets": "the central claim",
            "suggested_check": "run a controlled replication"
        }],
        "unknowns": ["effect size under field conditions"],
        "next_actions": ["replicate with a larger sample"]
    })
    .to_string()
}

/// A source-grounded review citing one real source.
pub fn full_review_json(source_id: &str, relation: &str) -> String {
    serde_json::json!({
        "assessment_text": "grounded finding: the cited passage bears on the claim",
        "proposed_assessment": "plausible",
        "evidence": [{
            "source_id": source_id,
            "locator": "p. 2",
            "reported": "the study reports the measured effect at p. 2",
            "claim": "the central claim",
            "relation": relation,
            "justification": "the reported effect is in the predicted direction",
            "limits": "one study"
        }],
        "objections": [],
        "unknowns": [],
        "next_actions": []
    })
    .to_string()
}

/// A review that asks for a contained computational check.
pub fn exec_request_review_json(program: &str, args: &[&str], input: &str) -> String {
    serde_json::json!({
        "assessment_text": "screening: the claim can be checked by computation",
        "proposed_assessment": "plausible",
        "evidence": [],
        "objections": [],
        "unknowns": [],
        "next_actions": [],
        "execution_requests": [{
            "program": program,
            "args": args,
            "inputs": [input],
            "purpose": "compute the mean of the supplied data",
            "target_claim": "mean is above zero"
        }]
    })
    .to_string()
}

/// A comparison reply matching the `ranking.pairwise` contract.
pub fn ranking_json(outcome: &str) -> String {
    let mut body = serde_json::json!({
        "outcome": outcome,
        "rationale": "one candidate is better grounded under the shared rubric",
        "tradeoffs": ["the loser is simpler to test"],
        "evidence_cited": ["both reviews"],
        "missing_to_decide": []
    });
    if outcome == "insufficient_basis" {
        body["missing_to_decide"] =
            serde_json::json!(["a direct measurement distinguishing the two mechanisms"]);
    }
    body.to_string()
}

/// A concluding `ranking.debate` turn.
pub fn ranking_debate_final_json(outcome: &str) -> String {
    serde_json::json!({
        "status": "final",
        "contribution": "the panel weighed both candidates",
        "outcome": outcome,
        "rationale": "the winner explains more of the recorded evidence",
        "tradeoffs": [],
        "missing_to_decide": []
    })
    .to_string()
}

/// An `evolution.feasibility` reply.
pub fn evolution_json() -> String {
    serde_json::json!({
        "title": "Calibrated variant",
        "claim": "drift explains it, testable with an in-line reference",
        "mechanism": "a reference channel cancels drift",
        "assumptions": ["the reference is stable"],
        "scope": "bench",
        "predictions": ["residual disagreement falls below 2%"],
        "falsifiers": ["disagreement persists with the reference"],
        "validation_plan": "re-measure with the reference channel",
        "grounding": [],
        "limitations": "reference stability unverified",
        "provisional": false,
        "changes": "added an in-line reference channel",
        "assumptions_retained": ["drift is the cause"],
        "assumptions_rejected": ["that recalibration between runs suffices"],
        "feasibility_basis": "the reference channel is already installed",
        "discriminating_test": "compare with and without the reference",
        "remaining_limitations": "reference stability"
    })
    .to_string()
}

/// An `evolution.analogy` reply.
pub fn analogy_json() -> String {
    serde_json::json!({
        "title": "Feedback-locked measurement",
        "claim": "a control loop rather than calibration removes the disagreement",
        "mechanism": "closed-loop nulling of the detector offset",
        "assumptions": ["the loop bandwidth exceeds the drift rate"],
        "scope": "bench",
        "predictions": ["disagreement independent of run time"],
        "falsifiers": ["disagreement persists under closed-loop control"],
        "validation_plan": "add a nulling loop and re-measure",
        "transferable_principle": "null the error rather than correct it afterwards",
        "analogy_source": "lock-in amplification",
        "analogy_failure_modes": ["loop instability"],
        "discriminating_prediction": "no dependence on warm-up time",
        "borrowed_vs_new_evidence": "lock-in evidence does not transfer to this detector"
    })
    .to_string()
}

/// A `proximity.similarity` reply keeping both items distinct.
pub fn proximity_json() -> String {
    serde_json::json!({
        "relation": "related",
        "shared_mechanism": "both concern measurement error",
        "differences": [{
            "dimension": "mechanism",
            "difference": "drift versus sampling bias",
            "material": true
        }],
        "rationale": "different mechanisms and different tests",
        "cluster_label": "measurement error"
    })
    .to_string()
}

/// A `meta.review` reply with one recurring critique.
pub const CRITIQUE_MARKER: &str = "candidates rarely state a falsifier";

pub fn meta_review_json() -> String {
    serde_json::json!({
        "critiques": [{
            "issue": CRITIQUE_MARKER,
            "occurrences": 2,
            "supporting_records": ["rev_1", "rev_2"],
            "exceptions": [],
            "recommended_check": "require an explicit falsifier",
            "target_roles": ["generation"]
        }],
        "methodological_gaps": ["no independent measurement"],
        "coverage_gaps": ["sampling bias unexplored"],
        "caution": "not every proposal needs a falsifier to be useful"
    })
    .to_string()
}

/// A `meta.overview` reply naming one underexplored direction.
pub const UNDEREXPLORED_MARKER: &str = "detector housing thermal history";

pub fn overview_json() -> String {
    serde_json::json!({
        "directions": [{
            "label": "instrument effects",
            "importance": "explains the disagreement without new physics",
            "illustrative_items": [],
            "status": "untested proposals only",
            "open_questions": ["is drift monotonic"]
        }],
        "discriminating_experiments": [{
            "question": "does the disagreement track run time",
            "approach": "log temperature during a repeat",
            "distinguishes": ["drift", "sampling bias"]
        }],
        "underexplored": [UNDEREXPLORED_MARKER],
        "suggested_reviewers": ["detector metrology"],
        "limitations": "two sources only"
    })
    .to_string()
}

/// An observation review that finds nothing discriminating.
pub fn observation_neutral_json(source_id: &str) -> String {
    serde_json::json!({
        "observations": [{
            "source_id": source_id,
            "locator": {"kind": "page", "at": "1"},
            "observation": "a measurement was reported",
            "cause_established": true,
            "consistent_with_hypothesis": true,
            "expected_regardless": true,
            "alternative_explanations": ["any hypothesis predicts a measurement"],
            "label": "neutral"
        }],
        "summary": "nothing discriminating",
        "disproof": "none",
        "label": "neutral",
        "rationale": "the article's observations do not bear on the mechanism",
        "uncertainty": ""
    })
    .to_string()
}

/// A synthesis reply matching the `task.synthesis` contract.
pub fn synthesis_json() -> String {
    serde_json::json!({
        "title": "Comparison of the supplied studies",
        "body": "The two studies disagree on effect size.",
        "claims": [],
        "disagreements": ["Study A reports an increase; Study B reports no change"],
        "limitations": "two sources only",
        "next_actions": ["obtain the underlying data"]
    })
    .to_string()
}

/// A provider scripted for every template a campaign can reach. `source_id`
/// is cited by the full and observation reviews.
pub fn campaign_provider(source_id: &str) -> MockProvider {
    MockProvider::new()
        .rule(
            anchor::GENERATION_LITERATURE,
            hypothesis_json(
                "Thermal drift",
                "instrument drift explains the disagreement",
            ),
        )
        .rule(
            anchor::GENERATION_DEBATE,
            debate_final_json(
                "Sampling window",
                "the sampling window differs between studies",
            ),
        )
        .rule(
            anchor::REFLECTION_INITIAL,
            review_json("plausible", "no falsifier is stated", false),
        )
        .rule(
            anchor::REFLECTION_FULL,
            full_review_json(source_id, "context"),
        )
        .rule(
            anchor::REFLECTION_DEEP,
            review_json(
                "plausible",
                "assumption of stable housing is unchecked",
                false,
            ),
        )
        .rule(
            anchor::REFLECTION_OBSERVATION,
            observation_neutral_json(source_id),
        )
        .rule(
            anchor::REFLECTION_SIMULATION,
            review_json("plausible", "step three needs a measurement", false),
        )
        .rule(anchor::RANKING_PAIRWISE, ranking_json("win_1"))
        .rule(anchor::RANKING_DEBATE, ranking_debate_final_json("win_2"))
        .rule(anchor::EVOLUTION_FEASIBILITY, evolution_json())
        .rule(anchor::EVOLUTION_ANALOGY, analogy_json())
        .rule(anchor::PROXIMITY, proximity_json())
        .rule(anchor::META_REVIEW, meta_review_json())
        .rule(anchor::META_OVERVIEW, overview_json())
        .rule(anchor::TASK_SYNTHESIS, synthesis_json())
}

/// Build a fixture with a real on-disk store, so restart can be exercised.
pub async fn fixture(mode: Mode, provider: MockProvider) -> Fixture {
    fixture_with(mode, provider, |_| {}).await
}

/// Build a fixture, adjusting the goal before it is persisted.
pub async fn fixture_with(
    mode: Mode,
    provider: MockProvider,
    adjust: impl FnOnce(&mut Goal),
) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join(".uruk/state.sqlite"))
        .await
        .expect("open store");
    let (run_id, goal, plan) = seed(&store, mode, dir.path(), adjust).await;

    Fixture {
        store,
        run_id,
        goal,
        plan,
        provider: Arc::new(provider),
        dir,
    }
}

async fn seed(
    store: &Store,
    mode: Mode,
    dir: &std::path::Path,
    adjust: impl FnOnce(&mut Goal),
) -> (RunId, Goal, Plan) {
    let run_id = RunId::new();
    let project_id = ProjectId::new();
    let goal_id = GoalId::new();
    let now = OffsetDateTime::now_utc();

    let mut goal = Goal {
        id: goal_id.clone(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        revision: 0,
        question: "why do these measurements disagree".into(),
        mode,
        deliverables: vec!["cited comparison".into()],
        inputs: vec![],
        scope: None,
        exclusions: vec![],
        known_facts: vec![],
        assumptions: vec![],
        rubric: Rubric {
            preferences: vec!["grounded in the supplied sources".into()],
            attributes: vec!["testable".into()],
            constraints: vec![],
            acceptance: vec![],
        },
        permissions: Permissions {
            read_paths: vec![dir.to_string_lossy().into_owned()],
            disclose_to_provider: true,
            ..Default::default()
        },
        budget: Budget {
            max_model_calls: 40,
            max_iterations: 4,
            wall_clock_secs: 60,
            ..Default::default()
        },
        profile: None,
        revision_reason: None,
        created_at: now,
    };
    adjust(&mut goal);

    let run = Run {
        id: run_id.clone(),
        schema_version: SCHEMA_VERSION,
        project_id: project_id.clone(),
        goal_id,
        plan_id: None,
        state: RunState::Running,
        stop_condition: None,
        iterations: 0,
        created_at: now,
        updated_at: now,
    };

    store.ensure_project(&project_id, "test").await.unwrap();
    store.create_run(&run, &goal).await.unwrap();

    let plan = supervisor::build_plan(&goal, 0);
    store.insert_plan(&plan).await.unwrap();

    (run_id, goal, plan)
}

/// Build an agent context over a fixture.
pub fn context(
    f: &Fixture,
    cancel: tokio_util::sync::CancellationToken,
) -> uruk::agents::AgentContext {
    uruk::agents::AgentContext::new(
        f.store.clone(),
        f.provider.clone(),
        Arc::new(uruk::prompts::PromptRegistry::new()),
        f.run_id.clone(),
        f.goal.clone(),
        f.plan.clone(),
        f.goal.permissions.clone(),
        7,
        cancel,
    )
}

/// Insert an item directly, for tests that need a starting candidate.
pub async fn seed_item(f: &Fixture, title: &str) -> ResearchItem {
    let content = format!("{title}: a candidate explanation with a stated mechanism");
    let item = ResearchItem {
        id: ItemId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        kind: ItemKind::Hypothesis,
        title: title.into(),
        content_hash: ContentHash::of_str(&content),
        content,
        hypothesis: Some(HypothesisFields {
            claim: format!("{title} claim"),
            mechanism: "stated mechanism".into(),
            ..Default::default()
        }),
        parent_ids: vec![],
        derivation: None,
        source_ids: vec![],
        author: Author::Researcher,
        produced_by: None,
        goal_id: f.goal.id.clone(),
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_item(&item).await.unwrap();
    item
}

/// Insert a plain review of an item, so it counts as reviewed.
pub async fn seed_review(f: &Fixture, item: &ResearchItem) -> Review {
    let review = Review {
        id: ReviewId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        item_id: item.id.clone(),
        item_hash: item.content_hash.clone(),
        strategy: ReviewStrategy::Initial,
        assessment_text: "reviewed".into(),
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
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_review(&review).await.unwrap();
    review
}

/// Insert a source with extracted text, for grounded-review tests.
pub async fn seed_source(f: &Fixture, name: &str, text: &str) -> Source {
    let artifact_path = f.dir.path().join(format!("{name}.txt"));
    tokio::fs::write(&artifact_path, text).await.unwrap();

    let artifact = Artifact {
        id: ArtifactId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        media_type: "text/plain".into(),
        content_hash: ContentHash::of_str(text),
        size_bytes: text.len() as u64,
        storage_path: artifact_path.to_string_lossy().into_owned(),
        produced_by: None,
        access: AccessClass::Open,
        label: Some(name.into()),
        supersedes: None,
        superseded_reason: None,
        created_at: OffsetDateTime::now_utc(),
    };
    f.store.insert_artifact(&artifact).await.unwrap();

    let source = Source {
        id: SourceId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: f.run_id.clone(),
        origin: Origin::LocalFile(format!("{name}.md")),
        title: Some(name.into()),
        authors: Some("Test Author".into()),
        date: Some("2025".into()),
        identifier: None,
        retrieved_at: OffsetDateTime::now_utc(),
        content_hash: ContentHash::of_str(text),
        access: AccessLevel::FullText,
        access_limitations: None,
        text_artifact: Some(artifact.id),
    };
    f.store.insert_source(&source).await.unwrap();
    source
}

/// Whether a test that needs OS containment can run on this host.
pub fn containment_available() -> bool {
    if uruk::tools::detect_containment().is_none() {
        eprintln!("skipping: no OS-level containment (sandbox-exec / bwrap) on this host");
        return false;
    }
    true
}
