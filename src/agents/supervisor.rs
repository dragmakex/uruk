//! Supervisor: plan, select the next useful work, and stop (SPEC §5, §6).
//!
//! Scheduling is Uruk policy; launching, waiting, retrying, and cancellation
//! belong to the runtime. Every selection and its rationale is persisted
//! before dispatch.

use super::outputs::RawExecRequest;
use crate::Result;
use crate::records::*;
use crate::store::{BudgetUsage, Store};
use crate::tools::ExecRequest;
use std::collections::{BTreeMap, BTreeSet};
use time::OffsetDateTime;

/// Weights for campaign work selection (SPEC §6).
///
/// "Numerical weights are Uruk configuration, not paper-prescribed
/// constants." All must be non-negative.
#[derive(Debug, Clone)]
pub struct Weights {
    /// An item with no review at all.
    pub unreviewed_item: f64,
    /// An item whose rating is stale after new evidence.
    pub stale_rating: f64,
    /// A candidate with few comparisons relative to its peers.
    pub tournament_coverage: f64,
    /// Clustering work, and a generation boost while directions are thin.
    pub underexplored_cluster: f64,
    /// A review that asked for a check that has not run.
    pub requested_check: f64,
    /// Baseline weight for generating more candidates.
    pub generation: f64,
    /// Share of capacity held for exploration, so low-ranked or new
    /// directions cannot starve (SPEC §6).
    pub exploration_reserve: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            unreviewed_item: 3.0,
            stale_rating: 2.5,
            tournament_coverage: 2.0,
            underexplored_cluster: 1.5,
            requested_check: 2.0,
            generation: 1.0,
            exploration_reserve: 0.2,
        }
    }
}

impl Weights {
    /// Reject a misconfiguration rather than silently scheduling nothing.
    pub fn validate(&self) -> Result<()> {
        let all = [
            ("unreviewed_item", self.unreviewed_item),
            ("stale_rating", self.stale_rating),
            ("tournament_coverage", self.tournament_coverage),
            ("underexplored_cluster", self.underexplored_cluster),
            ("requested_check", self.requested_check),
            ("generation", self.generation),
            ("exploration_reserve", self.exploration_reserve),
        ];
        for (name, value) in all {
            if value < 0.0 || !value.is_finite() {
                return Err(crate::Error::validation(format!(
                    "scheduling weight {name} must be non-negative and finite, got {value}"
                )));
            }
        }
        if self.exploration_reserve > 1.0 {
            return Err(crate::Error::validation(
                "exploration_reserve is a share and must not exceed 1.0",
            ));
        }
        Ok(())
    }
}

/// A unit of work the Supervisor proposes, before it becomes a Task.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub role: Role,
    pub strategy: String,
    /// Records this work consumes.
    pub input_refs: Vec<String>,
    pub priority: f64,
    pub rationale: String,
    /// Whether this work is part of the exploration reserve.
    pub exploratory: bool,
    /// Role-specific payload (a `Tool` task's execution request).
    pub payload: Option<serde_json::Value>,
}

impl WorkItem {
    fn new(
        role: Role,
        strategy: &str,
        input_refs: Vec<String>,
        priority: f64,
        rationale: String,
    ) -> Self {
        Self {
            role,
            strategy: strategy.to_string(),
            input_refs,
            priority,
            rationale,
            exploratory: false,
            payload: None,
        }
    }
}

/// Ceiling on the candidate pool, so a campaign converges rather than
/// generating indefinitely. A Uruk configuration choice, not a paper value.
pub const MAX_CANDIDATES: usize = 12;

/// Agent-generated candidates a bounded task may produce (SPEC §6: task mode
/// uses only the necessary roles).
pub const TASK_CANDIDATE_CAP: usize = 3;

/// How often the same work may fail before it is no longer re-proposed.
const MAX_FAILURES_PER_WORK: usize = 2;

/// Build the initial plan for a goal (SPEC §4.1).
///
/// The plan is deterministic policy, not a model call: a run must be able to
/// produce and export one without a provider (SPEC §9.1). Role selection is
/// inferred from the goal text and recorded as an explicit assumption in the
/// plan rationale.
pub fn build_plan(goal: &Goal, revision: u32) -> Plan {
    let mut roles = Vec::new();
    let mut methods = Vec::new();
    let mut priorities = Vec::new();
    let mut inferred: Vec<String> = Vec::new();

    match goal.mode {
        Mode::Task => {
            // Task mode uses only the necessary roles: no discovery loop is
            // inserted merely to exercise all of them (SPEC §6). Intent is
            // matched as a group of synonyms at word starts, honouring
            // negation and explicit exclusions.
            let critique = intent(goal, &["review", "critique", "assess"]);
            let hypotheses = intent(goal, &["hypothes", "explanation", "propose", "generate"]);

            if let Some(word) = &hypotheses {
                roles.push(Role::Generation.as_str().to_string());
                methods.push("grounded hypothesis generation".to_string());
                inferred.push(format!("'{word}' → Generation"));
            }
            if critique.is_some() || hypotheses.is_some() {
                roles.push(Role::Reflection.as_str().to_string());
                methods.push("source-grounded review".to_string());
                if let Some(word) = &critique {
                    inferred.push(format!("'{word}' → Reflection"));
                }
            }
            roles.push(Role::MetaReview.as_str().to_string());
            methods.push("evidence-grounded synthesis of the requested deliverable".to_string());

            priorities.push("complete the requested deliverable".to_string());
            priorities.push("report limitations and unavailable information".to_string());
        }
        Mode::Campaign => {
            for role in [
                Role::Generation,
                Role::Reflection,
                Role::Ranking,
                Role::Evolution,
                Role::Proximity,
                Role::MetaReview,
            ] {
                roles.push(role.as_str().to_string());
            }
            methods.extend([
                "grounded generation and simulated scientific debate".to_string(),
                "multi-strategy review".to_string(),
                "evidence-aware pairwise comparison and debate".to_string(),
                "immutable evolution with linked children".to_string(),
                "proximity clustering and duplicate detection".to_string(),
                "meta-review feedback and research overview into later generation".to_string(),
            ]);
            priorities.extend([
                "review unreviewed candidates before ranking them".to_string(),
                "refresh comparisons invalidated by new evidence".to_string(),
                "preserve exploration capacity for new directions".to_string(),
            ]);
        }
    }

    let mut stopping = vec![
        StopCondition::DeliverableSatisfied,
        StopCondition::BudgetExhausted,
        StopCondition::WorkExhausted,
        StopCondition::Cancelled,
        StopCondition::SafetyBoundary,
    ];
    if goal.mode == Mode::Campaign {
        stopping.push(StopCondition::ProgressStalled);
    }

    let outputs = if goal.deliverables.is_empty() {
        vec!["REPORT.md".to_string()]
    } else {
        goal.deliverables.clone()
    };

    let rationale = match goal.mode {
        Mode::Task if inferred.is_empty() => "task mode: no generation or review was asked for, \
             so only the requested deliverable is produced (assumption recorded, not asked)"
            .to_string(),
        Mode::Task => format!(
            "task mode: roles inferred from the goal text ({}); no discovery loop inserted \
             merely to exercise unused roles (assumption recorded, not asked)",
            inferred.join(", ")
        ),
        Mode::Campaign => "campaign mode: the full feedback system, selected from research \
             state each iteration"
            .to_string(),
    };

    Plan {
        id: PlanId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: goal.run_id.clone(),
        goal_id: goal.id.clone(),
        revision,
        roles,
        methods,
        priorities,
        outputs,
        stopping_conditions: stopping,
        rubric: goal.rubric.clone(),
        rationale,
        created_at: OffsetDateTime::now_utc(),
    }
}

/// Whether the goal asks for an activity, returning the word that triggered
/// it. An explicit exclusion or a negated mention of any synonym suppresses
/// the whole group, so "no new hypotheses" is not reintroduced by "propose".
fn intent(goal: &Goal, needles: &[&str]) -> Option<String> {
    let excluded = |needle: &str| {
        goal.exclusions
            .iter()
            .any(|e| !word_starts(&e.to_ascii_lowercase(), needle).is_empty())
    };
    if needles.iter().any(|n| excluded(n)) {
        return None;
    }

    let texts: Vec<String> = std::iter::once(goal.question.to_ascii_lowercase())
        .chain(goal.deliverables.iter().map(|d| d.to_ascii_lowercase()))
        .collect();

    let mut asked = None;
    for needle in needles {
        for text in &texts {
            for index in word_starts(text, needle) {
                if is_negated(text, index) {
                    return None;
                }
                asked.get_or_insert_with(|| (*needle).to_string());
            }
        }
    }
    asked
}

/// Byte offsets where `needle` occurs at the start of a word.
fn word_starts(text: &str, needle: &str) -> Vec<usize> {
    text.match_indices(needle)
        .filter(|(i, _)| {
            *i == 0
                || !text[..*i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric())
        })
        .map(|(i, _)| i)
        .collect()
}

/// Whether the mention at `index` falls under a negation.
///
/// Scans the clause before the match, since "do not generate new hypotheses"
/// and "without generating hypotheses" both exclude the work.
fn is_negated(text: &str, index: usize) -> bool {
    let clause_start = text[..index]
        .rfind(['.', ';', '\n'])
        .map(|i| i + 1)
        .unwrap_or(0);
    let before = &text[clause_start..index];

    const NEGATIONS: &[&str] = &[
        "do not",
        "don't",
        "without",
        "no new",
        "not to",
        "avoid",
        "never",
        "rather than",
        "instead of",
        "except",
        "excluding",
    ];
    NEGATIONS.iter().any(|n| !word_starts(before, n).is_empty())
}

/// What has already been scheduled, so the same work is not proposed twice
/// and failed work is re-proposed only a bounded number of times.
struct Ledger<'a> {
    tasks: &'a [Task],
}

impl Ledger<'_> {
    /// Input-less work (another generation, another synthesis) is a distinct
    /// stochastic trial each round, so a completed one does not block the
    /// next. Work over specific inputs runs once unless it failed.
    fn allows(&self, role: Role, strategy: &str, inputs: &[String]) -> bool {
        let same = |t: &&Task| t.role == role && t.strategy == strategy && t.input_refs == inputs;
        let repeatable = inputs.is_empty();
        let blocking = self.tasks.iter().filter(same).any(|t| {
            let open = !t.state.is_terminal();
            let done = t.state != TaskState::Failed;
            open || (!repeatable && done)
        });
        if blocking {
            return false;
        }
        let failures = self
            .tasks
            .iter()
            .filter(same)
            .filter(|t| t.state == TaskState::Failed)
            .count();
        failures < MAX_FAILURES_PER_WORK
    }

    fn deliverable_done(&self) -> bool {
        self.tasks.iter().any(|t| {
            t.role == Role::MetaReview
                && t.strategy == "deliverable"
                && matches!(
                    t.state,
                    TaskState::Completed | TaskState::Pending | TaskState::Running
                )
        })
    }

    fn has_unfinished_supporting_work(&self) -> bool {
        self.tasks
            .iter()
            .any(|t| t.role != Role::MetaReview && !t.state.is_terminal())
    }
}

/// The final deliverable (SPEC §6: final Meta-review runs only if permitted
/// and budgeted).
pub fn deliverable_work(goal: &Goal, weights: &Weights) -> WorkItem {
    WorkItem::new(
        Role::MetaReview,
        "deliverable",
        vec![],
        weights.unreviewed_item + 10.0,
        format!(
            "produce the requested deliverable: {}",
            goal.deliverables.join("; ")
        ),
    )
}

/// Select the next useful work from current research state (SPEC §6).
///
/// Considers missing reviews, new evidence, uncertain assumptions, tournament
/// coverage, underexplored clusters, and requested checks — not only the
/// highest Elo. `iteration` lets alternating strategies (literature/debate,
/// same-cluster/cross-cluster pairs) vary deterministically.
pub async fn select_work(
    store: &Store,
    run_id: &RunId,
    goal: &Goal,
    plan: &Plan,
    weights: &Weights,
    limit: usize,
    iteration: u32,
) -> Result<Vec<WorkItem>> {
    weights.validate()?;

    // Only roles the plan selected may be scheduled. A task-mode plan that
    // asked for review and synthesis must not acquire a discovery loop
    // merely because the machinery exists (SPEC §6).
    let enabled = |role: Role| plan.roles.iter().any(|r| r == role.as_str());

    let items = store.list_items(run_id).await?;
    let tasks = store.list_tasks(run_id).await?;
    let ledger = Ledger { tasks: &tasks };
    let all_reviews = store.list_reviews(run_id).await?;
    let sources = store.list_sources(run_id).await?;
    let clusters = store.list_clusters(run_id).await?;

    let mut candidates: Vec<&ResearchItem> = Vec::new();
    for item in &items {
        if item.kind.is_rankable()
            && matches!(store.get_disposition(&item.id).await?, Disposition::Active)
        {
            candidates.push(item);
        }
    }
    let reviews_of =
        |id: &ItemId| -> Vec<&Review> { all_reviews.iter().filter(|r| &r.item_id == id).collect() };
    let full_text_source = sources.iter().find(|s| s.access == AccessLevel::FullText);

    let mut work: Vec<WorkItem> = Vec::new();

    // --- Search: one federated discovery per goal revision ---
    // Gated on the network permission and the per-connector allowlist; the
    // acquisition tasks are enqueued by the discover task itself, dependent
    // on it and priority-ordered by fused rank.
    if crate::search::any_connector_enabled(&goal.permissions)
        && [Role::Generation, Role::Reflection, Role::MetaReview]
            .iter()
            .any(|r| enabled(*r))
    {
        let inputs = vec![format!("goal_rev:{}", goal.revision)];
        if ledger.allows(Role::Search, "discover", &inputs) {
            work.push(WorkItem::new(
                Role::Search,
                "discover",
                inputs,
                weights.unreviewed_item + 5.0,
                format!(
                    "federated literature discovery for goal revision {}",
                    goal.revision
                ),
            ));
        }
    }

    // --- Reflection: every candidate earns reviews, strategy by strategy ---
    if enabled(Role::Reflection) {
        for item in &candidates {
            let reviews = reviews_of(&item.id);
            let has = |s: ReviewStrategy| reviews.iter().any(|r| r.strategy == s);
            let mut propose = |strategy: ReviewStrategy,
                               inputs: Vec<String>,
                               priority: f64,
                               rationale: String| {
                if ledger.allows(Role::Reflection, strategy.as_str(), &inputs) {
                    work.push(WorkItem::new(
                        Role::Reflection,
                        strategy.as_str(),
                        inputs,
                        priority,
                        rationale,
                    ));
                }
            };
            let me = vec![item.id.0.clone()];

            if reviews.is_empty() {
                propose(
                    ReviewStrategy::Initial,
                    me,
                    weights.unreviewed_item,
                    format!("item {} has no review yet", item.id),
                );
                continue;
            }

            if !has(ReviewStrategy::Full) && !sources.is_empty() {
                propose(
                    ReviewStrategy::Full,
                    me.clone(),
                    weights.unreviewed_item - 0.5,
                    format!("item {} awaits a source-grounded review", item.id),
                );
            } else if !has(ReviewStrategy::DeepVerification)
                && reviews
                    .iter()
                    .any(|r| !r.next_actions.is_empty() || r.objections.iter().any(|o| o.fatal))
            {
                propose(
                    ReviewStrategy::DeepVerification,
                    me.clone(),
                    weights.requested_check,
                    format!("reviews of {} raised checks or fatal objections", item.id),
                );
            }

            if !has(ReviewStrategy::Observation)
                && has(ReviewStrategy::Full)
                && let Some(src) = full_text_source
            {
                {
                    propose(
                        ReviewStrategy::Observation,
                        vec![item.id.0.clone(), src.id.0.clone()],
                        weights.requested_check - 0.5,
                        format!("check {} against the observations in {}", item.id, src.id),
                    );
                }
            }

            let has_plan = item
                .hypothesis
                .as_ref()
                .is_some_and(|h| !h.validation_plan.trim().is_empty());
            if !has(ReviewStrategy::Simulation)
                && has_plan
                && (has(ReviewStrategy::Full) || has(ReviewStrategy::DeepVerification))
            {
                propose(
                    ReviewStrategy::Simulation,
                    me.clone(),
                    weights.requested_check - 0.5,
                    format!(
                        "walk through the mechanism and validation plan of {}",
                        item.id
                    ),
                );
            }

            // Evidence linked to the item that no review has yet interpreted
            // must be consumed by a review rather than assumed (SPEC §5
            // Reflection: "It consumes the returned evidence").
            let cited: BTreeSet<&str> = reviews
                .iter()
                .flat_map(|r| r.evidence.iter().map(|l| l.evidence_id.as_str()))
                .collect();
            let uncited = store
                .list_evidence_links(&item.id)
                .await?
                .into_iter()
                .filter(|l| !cited.contains(l.evidence_id.as_str()))
                .count();
            if uncited > 0 {
                propose(
                    ReviewStrategy::Recurrent,
                    me.clone(),
                    weights.stale_rating + 0.5,
                    format!(
                        "{uncited} evidence record(s) for {} await a review",
                        item.id
                    ),
                );
            }
        }
    }

    // --- Tool: computational checks reviewers asked for, within permissions ---
    if goal.permissions.execute {
        for item in &candidates {
            for review in reviews_of(&item.id) {
                for (n, raw) in review.execution_requests.iter().enumerate() {
                    let Ok(req) = serde_json::from_value::<RawExecRequest>(raw.clone()) else {
                        continue;
                    };
                    if !goal.permissions.allows_tool(&req.program) {
                        tracing::info!(program = %req.program, review = %review.id,
                            "requested tool is not approved; not scheduled");
                        continue;
                    }
                    let inputs = vec![item.id.0.clone(), review.id.0.clone(), n.to_string()];
                    if !ledger.allows(Role::Tool, "exec", &inputs) {
                        continue;
                    }
                    let mut exec = ExecRequest::new(req.program.clone(), req.args.clone());
                    exec.inputs = req.inputs.clone();
                    let mut w = WorkItem::new(
                        Role::Tool,
                        "exec",
                        inputs,
                        weights.requested_check + 1.0,
                        format!("review {} requested a check: {}", review.id, req.purpose),
                    );
                    w.payload = Some(serde_json::json!({
                        "request": exec,
                        "target_item": item.id.0,
                        "target_claim": req.target_claim,
                        "purpose": req.purpose,
                    }));
                    work.push(w);
                }
            }
        }
    }

    // --- Ranking: stale ratings first, then coverage, cross-cluster on odd rounds ---
    if enabled(Role::Ranking) && candidates.len() >= 2 {
        let mut pool: Vec<(&ResearchItem, Option<Rating>)> = Vec::new();
        for c in &candidates {
            if !reviews_of(&c.id).is_empty() {
                pool.push((c, store.get_rating(&c.id, &plan.id).await?));
            }
        }
        if pool.len() >= 2 {
            let rating_of = |r: &Option<Rating>| r.as_ref().map_or(INITIAL_ELO, |r| r.rating);
            let played_of = |r: &Option<Rating>| r.as_ref().map_or(0, |r| r.matches_played);
            let cluster_of: BTreeMap<&str, &str> = clusters
                .iter()
                .flat_map(|c| c.item_ids.iter().map(move |i| (i.as_str(), c.id.as_str())))
                .collect();

            let mut chosen: Option<(usize, usize, &str, f64, String)> = None;

            if let Some(si) = pool
                .iter()
                .position(|(_, r)| r.as_ref().is_some_and(|r| r.stale))
            {
                let (stale, sr) = &pool[si];
                let peer = pool
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != si)
                    .min_by(|(_, a), (_, b)| {
                        (rating_of(&a.1) - rating_of(sr))
                            .abs()
                            .total_cmp(&(rating_of(&b.1) - rating_of(sr)).abs())
                    })
                    .map(|(j, _)| j);
                if let Some(j) = peer {
                    chosen = Some((
                        si,
                        j,
                        "refresh",
                        weights.stale_rating,
                        format!(
                            "rating for {} is stale after new claim-relevant evidence",
                            stale.id
                        ),
                    ));
                }
            }

            if chosen.is_none() {
                let mut best: Option<(usize, usize, f64, u32)> = None;
                for i in 0..pool.len() {
                    for j in (i + 1)..pool.len() {
                        let played = played_of(&pool[i].1) + played_of(&pool[j].1);
                        let cross = match (
                            cluster_of.get(pool[i].0.id.as_str()),
                            cluster_of.get(pool[j].0.id.as_str()),
                        ) {
                            (Some(a), Some(b)) => a != b,
                            _ => false,
                        };
                        let bonus = if cross && iteration % 2 == 1 {
                            0.5
                        } else {
                            0.0
                        };
                        let score = played as f64 - bonus;
                        if best.is_none_or(|b| score < b.2) {
                            best = Some((i, j, score, played));
                        }
                    }
                }
                if let Some((i, j, _, played)) = best {
                    let cross = cluster_of.get(pool[i].0.id.as_str())
                        != cluster_of.get(pool[j].0.id.as_str());
                    chosen = Some((
                        i,
                        j,
                        "pairwise",
                        weights.tournament_coverage + 1.0 / (1.0 + played as f64),
                        format!(
                            "least-compared reviewed candidates ({played} matches so far){}",
                            if cross && !clusters.is_empty() {
                                "; cross-cluster comparison"
                            } else {
                                ""
                            }
                        ),
                    ));
                }
            }

            if let Some((i, j, mut strategy, priority, rationale)) = chosen {
                // Multi-turn debate for consequential comparisons: both
                // candidates already rated and among the top two (SPEC §7).
                let mut ranked: Vec<&(&ResearchItem, Option<Rating>)> = pool.iter().collect();
                ranked.sort_by(|a, b| rating_of(&b.1).total_cmp(&rating_of(&a.1)));
                let top: BTreeSet<&str> = ranked
                    .iter()
                    .take(2)
                    .map(|(it, _)| it.id.as_str())
                    .collect();
                let both_top =
                    top.contains(pool[i].0.id.as_str()) && top.contains(pool[j].0.id.as_str());
                if strategy == "pairwise"
                    && both_top
                    && played_of(&pool[i].1) >= 1
                    && played_of(&pool[j].1) >= 1
                {
                    strategy = "debate";
                }
                let mut inputs = vec![pool[i].0.id.0.clone(), pool[j].0.id.0.clone()];
                inputs.sort();
                if ledger.allows(Role::Ranking, strategy, &inputs) {
                    work.push(WorkItem::new(
                        Role::Ranking,
                        strategy,
                        inputs,
                        priority,
                        rationale,
                    ));
                }
            }
        }
    }

    // --- Proximity: re-cluster when the population changed ---
    if enabled(Role::Proximity) && candidates.len() >= 2 {
        let ids: BTreeSet<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let covered: BTreeSet<&str> = clusters
            .iter()
            .flat_map(|c| c.item_ids.iter().map(|i| i.as_str()))
            .collect();
        if !ids.is_subset(&covered) {
            let inputs: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
            if ledger.allows(Role::Proximity, "cluster", &inputs) {
                work.push(WorkItem::new(
                    Role::Proximity,
                    "cluster",
                    inputs,
                    weights.underexplored_cluster,
                    format!(
                        "{} candidate(s) are not yet clustered",
                        ids.len() - covered.len().min(ids.len())
                    ),
                ));
            }
        }
    }

    // --- Generation: exploration reserve, alternating strategies ---
    if enabled(Role::Generation) {
        let agent_generated = items
            .iter()
            .filter(|i| matches!(&i.author, Author::Agent { role, .. } if role == Role::Generation.as_str()))
            .count();
        let cap = match goal.mode {
            Mode::Task => TASK_CANDIDATE_CAP,
            Mode::Campaign => MAX_CANDIDATES,
        };
        if agent_generated < cap && items.len() < MAX_CANDIDATES {
            let strategy = if all_reviews.len() >= 2 && iteration % 2 == 1 {
                "debate"
            } else {
                "literature"
            };
            let singleton_share = if clusters.is_empty() {
                0.0
            } else {
                clusters.iter().filter(|c| c.item_ids.len() == 1).count() as f64
                    / clusters.len() as f64
            };
            if ledger.allows(Role::Generation, strategy, &[]) {
                let mut w = WorkItem::new(
                    Role::Generation,
                    strategy,
                    vec![],
                    weights.generation * (1.0 + weights.exploration_reserve)
                        + weights.underexplored_cluster * singleton_share,
                    "exploration reserve: keep new directions reachable".to_string(),
                );
                w.exploratory = true;
                work.push(w);
            }
        }
    }

    // --- Evolution: feasibility children first, then one analogy over the field ---
    if enabled(Role::Evolution) {
        let reviewed: Vec<&ResearchItem> = candidates
            .iter()
            .copied()
            .filter(|c| !reviews_of(&c.id).is_empty())
            .collect();
        let without_child = reviewed
            .iter()
            .find(|c| !items.iter().any(|i| i.parent_ids.contains(&c.id)));
        if let Some(parent) = without_child {
            let inputs = vec![parent.id.0.clone()];
            if ledger.allows(Role::Evolution, "feasibility", &inputs) {
                work.push(WorkItem::new(
                    Role::Evolution,
                    "feasibility",
                    inputs,
                    weights.generation,
                    format!("{} is reviewed but has produced no child yet", parent.id),
                ));
            }
        } else if reviewed.len() >= 2 {
            let mut inputs: Vec<String> = reviewed.iter().take(3).map(|i| i.id.0.clone()).collect();
            inputs.sort();
            if ledger.allows(Role::Evolution, "analogy", &inputs) {
                work.push(WorkItem::new(
                    Role::Evolution,
                    "analogy",
                    inputs,
                    weights.generation * 0.9,
                    "every reviewed candidate has a child; seek one genuinely different \
                     mechanism by analogy"
                        .to_string(),
                ));
            }
        }
    }

    // --- Meta-review: feedback and overview as reviews accumulate (campaign) ---
    if enabled(Role::MetaReview) && goal.mode == Mode::Campaign {
        let feedback_revision = store.next_feedback_revision(run_id).await?;
        if all_reviews.len() >= 2
            && (feedback_revision as usize) <= all_reviews.len() / 2
            && ledger.allows(Role::MetaReview, "synthesize", &[])
        {
            work.push(WorkItem::new(
                Role::MetaReview,
                "synthesize",
                vec![],
                weights.unreviewed_item,
                format!(
                    "{} review(s) accumulated since feedback revision {feedback_revision}",
                    all_reviews.len()
                ),
            ));
        }
        if let Some(fb) = store.latest_feedback(run_id).await? {
            let inputs = vec![fb.id.0.clone()];
            if ledger.allows(Role::MetaReview, "overview", &inputs) {
                work.push(WorkItem::new(
                    Role::MetaReview,
                    "overview",
                    inputs,
                    weights.unreviewed_item - 1.0,
                    format!("research overview after feedback revision {}", fb.revision),
                ));
            }
        }
    }

    // --- Task mode: the deliverable, once nothing else remains ---
    if goal.mode == Mode::Task
        && enabled(Role::MetaReview)
        && !ledger.deliverable_done()
        && work.is_empty()
        && !ledger.has_unfinished_supporting_work()
    {
        work.push(deliverable_work(goal, weights));
    }

    work.sort_by(|a, b| b.priority.total_cmp(&a.priority));
    work.truncate(limit);
    Ok(work)
}

/// Whether the run should stop, and why (SPEC §6).
pub async fn should_stop(
    store: &Store,
    run_id: &RunId,
    goal: &Goal,
    usage: &BudgetUsage,
    iterations: u32,
    deadline_passed: bool,
) -> Result<Option<StopCondition>> {
    if deadline_passed {
        return Ok(Some(StopCondition::BudgetExhausted));
    }

    // The output reserve is not available to ordinary work, so exhaustion is
    // measured against the working ceiling; the final deliverable then draws
    // on the reserve.
    let working_ceiling = goal
        .budget
        .max_model_calls
        .saturating_sub(goal.budget.reserve_calls_for_output);

    if usage.committed_calls() >= working_ceiling
        || usage.committed_tokens() >= goal.budget.max_tokens
    {
        return Ok(Some(StopCondition::BudgetExhausted));
    }

    if goal.mode == Mode::Campaign && iterations >= goal.budget.max_iterations {
        return Ok(Some(StopCondition::BudgetExhausted));
    }

    // Progress stall: an iteration that produced no new item, review, match,
    // evidence, or cluster.
    if goal.mode == Mode::Campaign && iterations >= 2 {
        let snapshots = store.list_snapshots(run_id).await?;
        if snapshots.len() >= 2 {
            let last = &snapshots[snapshots.len() - 1];
            let prev = &snapshots[snapshots.len() - 2];
            let progressed = last.items_generated > prev.items_generated
                || last.reviews_completed > prev.reviews_completed
                || last.matches_completed > prev.matches_completed
                || last.external_evidence_items > prev.external_evidence_items
                || last.clusters > prev.clusters;
            if !progressed {
                return Ok(Some(StopCondition::ProgressStalled));
            }
        }
    }

    Ok(None)
}

/// Changes the researcher requests to a goal (SPEC §4.1, §6). Lists replace
/// the current list when non-empty; `None` leaves a field unchanged.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GoalChanges {
    pub question: Option<String>,
    pub deliverables: Vec<String>,
    pub preferences: Vec<String>,
    pub attributes: Vec<String>,
    pub constraints: Vec<String>,
    pub exclusions: Vec<String>,
    pub assumptions: Vec<String>,
    pub max_model_calls: Option<u32>,
    pub max_iterations: Option<u32>,
    pub wall_clock_secs: Option<u64>,
    pub reason: Option<String>,
}

impl GoalChanges {
    pub fn is_empty(&self) -> bool {
        self.question.is_none()
            && self.deliverables.is_empty()
            && self.preferences.is_empty()
            && self.attributes.is_empty()
            && self.constraints.is_empty()
            && self.exclusions.is_empty()
            && self.assumptions.is_empty()
            && self.max_model_calls.is_none()
            && self.max_iterations.is_none()
            && self.wall_clock_secs.is_none()
    }
}

/// What a revision produced.
#[derive(Debug, Clone)]
pub struct Revised {
    pub goal: Goal,
    pub plan: Plan,
    /// Whether the rubric changed, which starts a new tournament cohort.
    pub rubric_changed: bool,
    /// Pending approvals invalidated by the revision.
    pub invalidated_approvals: usize,
    pub decision: Decision,
}

/// Revise a run's goal (SPEC §4.1: "Goal or rubric changes create a
/// revision. Subsequent tasks reference that revision; in-flight results
/// retain their original context").
///
/// A changed rubric produces a new plan revision, which is a fresh Elo cohort
/// (SPEC §7); pending approvals are invalidated (SPEC §6); a finished run is
/// reopened so it can be resumed under the new revision.
pub async fn revise(store: &Store, run_id: &RunId, changes: GoalChanges) -> Result<Revised> {
    if changes.is_empty() {
        return Err(crate::Error::validation(
            "nothing to revise: supply a new goal, rubric entry, scope, or limit",
        ));
    }
    let current = store.current_goal(run_id).await?;
    let mut goal = current.clone();
    goal.id = GoalId::new();
    goal.revision = store.next_goal_revision(run_id).await?;
    goal.revision_reason = Some(
        changes
            .reason
            .clone()
            .unwrap_or_else(|| "researcher revision".to_string()),
    );
    goal.created_at = OffsetDateTime::now_utc();

    if let Some(q) = &changes.question {
        // The revised goal is checked before anything is persisted (SPEC §12).
        let verdict = super::safety::check(q);
        if verdict.is_blocked() {
            return Err(crate::Error::permission(
                verdict
                    .concern()
                    .unwrap_or("the revised goal is blocked")
                    .to_string(),
            ));
        }
        goal.question = q.clone();
    }
    let replace = |target: &mut Vec<String>, with: &Vec<String>| {
        if !with.is_empty() {
            *target = with.clone();
        }
    };
    replace(&mut goal.deliverables, &changes.deliverables);
    replace(&mut goal.rubric.preferences, &changes.preferences);
    replace(&mut goal.rubric.attributes, &changes.attributes);
    replace(&mut goal.rubric.constraints, &changes.constraints);
    replace(&mut goal.exclusions, &changes.exclusions);
    replace(&mut goal.assumptions, &changes.assumptions);
    if let Some(n) = changes.max_model_calls {
        goal.budget.max_model_calls = n;
    }
    if let Some(n) = changes.max_iterations {
        goal.budget.max_iterations = n;
    }
    if let Some(n) = changes.wall_clock_secs {
        goal.budget.wall_clock_secs = n;
    }

    let rubric_changed =
        serde_json::to_value(&current.rubric)? != serde_json::to_value(&goal.rubric)?;
    let invalidated_approvals = store.revise_goal(&goal).await?;

    // A new plan serves the new revision. Its ID keys the rating cohort, so a
    // plan is only replaced when the rubric or the selected roles change.
    let candidate = build_plan(&goal, store.next_plan_revision(run_id).await?);
    let plan = match store.current_plan(run_id).await? {
        Some(existing) if !rubric_changed && existing.roles == candidate.roles => existing,
        _ => {
            store.insert_plan(&candidate).await?;
            candidate
        }
    };

    let decision = Decision {
        id: DecisionId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        kind: DecisionKind::PlanChange,
        actor: Actor::Researcher,
        reason: format!(
            "goal revised to revision {}: {}",
            goal.revision,
            goal.revision_reason.as_deref().unwrap_or("")
        ),
        referenced: vec![goal.id.0.clone(), plan.id.0.clone()],
        payload: serde_json::json!({
            "goal_revision": goal.revision,
            "plan_revision": plan.revision,
            "rubric_changed": rubric_changed,
            "invalidated_approvals": invalidated_approvals,
            "changes": changes,
        }),
        created_at: OffsetDateTime::now_utc(),
    };
    store.insert_decision(&decision).await?;

    if store.get_run(run_id).await?.state.is_terminal() {
        store.reopen_run(run_id).await?;
    }

    Ok(Revised {
        goal,
        plan,
        rubric_changed,
        invalidated_approvals,
        decision,
    })
}

/// Record a scheduling decision before dispatch (SPEC §5 Supervisor).
pub fn scheduling_decision(run_id: &RunId, work: &WorkItem, task_id: &TaskId) -> Decision {
    Decision {
        id: DecisionId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        kind: DecisionKind::Scheduling,
        actor: Actor::Supervisor,
        reason: work.rationale.clone(),
        referenced: {
            let mut refs = work.input_refs.clone();
            refs.push(task_id.0.clone());
            refs
        },
        payload: serde_json::json!({
            "role": work.role.as_str(),
            "strategy": work.strategy,
            "priority": work.priority,
            "exploratory": work.exploratory,
            "payload": work.payload,
        }),
        created_at: OffsetDateTime::now_utc(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(mode: Mode, question: &str, deliverables: &[&str]) -> Goal {
        Goal {
            id: GoalId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: RunId::from_raw("run_1"),
            revision: 0,
            question: question.into(),
            mode,
            deliverables: deliverables.iter().map(|s| s.to_string()).collect(),
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
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn critique_only_task_does_not_schedule_generation() {
        let plan = build_plan(
            &goal(
                Mode::Task,
                "review my study design for confounding",
                &["critique"],
            ),
            0,
        );
        assert!(
            !plan.roles.contains(&Role::Generation.as_str().to_string()),
            "critique-only task must not require Generation (SPEC §13): {:?}",
            plan.roles
        );
        assert!(plan.roles.contains(&Role::Reflection.as_str().to_string()));
        assert!(plan.rationale.contains("'review' → Reflection"));
    }

    #[test]
    fn literature_task_needs_no_tournament() {
        let plan = build_plan(
            &goal(
                Mode::Task,
                "compare these five papers and explain where they disagree",
                &["cited comparison"],
            ),
            0,
        );
        assert!(
            !plan.roles.contains(&Role::Ranking.as_str().to_string()),
            "a literature task must not generate a tournament: {:?}",
            plan.roles
        );
    }

    #[test]
    fn matches_only_at_word_starts() {
        // "peer-reviewed" mentions review; "previewed" does not.
        let plan = build_plan(&goal(Mode::Task, "summarise what we previewed", &[]), 0);
        assert!(!plan.roles.contains(&Role::Reflection.as_str().to_string()));
        let plan = build_plan(
            &goal(Mode::Task, "assess the peer-reviewed evidence", &[]),
            0,
        );
        assert!(plan.roles.contains(&Role::Reflection.as_str().to_string()));
    }

    #[test]
    fn campaign_uses_the_full_role_set() {
        let plan = build_plan(&goal(Mode::Campaign, "find explanations for X", &[]), 0);
        for role in [
            Role::Generation,
            Role::Reflection,
            Role::Ranking,
            Role::Evolution,
            Role::Proximity,
        ] {
            assert!(plan.roles.contains(&role.as_str().to_string()));
        }
        assert!(
            plan.stopping_conditions
                .contains(&StopCondition::ProgressStalled)
        );
    }

    #[test]
    fn negation_suppresses_a_role() {
        let g = goal(
            Mode::Task,
            "review my study design for confounding; do not generate new hypotheses",
            &["critique"],
        );
        let plan = build_plan(&g, 0);
        assert!(!plan.roles.contains(&Role::Generation.as_str().to_string()));

        // A genuine request in a later clause still counts.
        let g2 = goal(
            Mode::Task,
            "do not review the protocol. propose hypotheses for the anomaly",
            &[],
        );
        let plan2 = build_plan(&g2, 0);
        assert!(
            plan2.roles.contains(&Role::Generation.as_str().to_string()),
            "a negation in an earlier clause must not suppress a later request: {:?}",
            plan2.roles
        );
    }

    #[test]
    fn exclusions_override_the_question() {
        let mut g = goal(Mode::Task, "propose hypotheses for this anomaly", &[]);
        g.exclusions = vec!["no new hypotheses; critique only".into()];
        let plan = build_plan(&g, 0);
        assert!(
            !plan.roles.contains(&Role::Generation.as_str().to_string()),
            "an explicit exclusion must override the question: {:?}",
            plan.roles
        );
    }

    #[test]
    fn negative_weights_are_rejected() {
        let w = Weights {
            unreviewed_item: -1.0,
            ..Default::default()
        };
        assert!(w.validate().is_err());
        assert!(Weights::default().validate().is_ok());
    }
}
