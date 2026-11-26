//! Honest UI view models projected from persisted records.
//!
//! Everything here is derived from SQLite facts: task states, the budget
//! ledger, ratings, and counts. Nothing invents activity prose or duration
//! estimates; the frontend renders real state or renders nothing.

use crate::Result;
use crate::records::*;
use crate::store::{BudgetUsage, ItemSummary, QueueState, Store};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// A complete, compact view of one run: the payload of `GET /api/runs/{id}`
/// and of every SSE `snapshot` event.
#[derive(Debug, Serialize)]
pub struct RunSnapshot {
    pub run: RunMeta,
    pub goal: GoalView,
    pub plan: Option<PlanView>,
    /// Supervisor first, then the plan's worker roles in plan order.
    pub agents: Vec<AgentPanel>,
    pub stats: Stats,
    pub usage: BudgetUsage,
    /// Ranking cards: best rating first, unrated last, ties by ID.
    pub items: Vec<ItemSummary>,
    pub ranking_baseline: Option<RankingBaseline>,
}

/// The previous completed round's board in the current rubric cohort.
#[derive(Debug, Serialize)]
pub struct RankingBaseline {
    pub iteration: u32,
    pub ratings: BTreeMap<String, f64>,
}

/// Run lifecycle facts.
#[derive(Debug, Serialize)]
pub struct RunMeta {
    pub id: String,
    pub state: String,
    pub stop_condition: Option<String>,
    pub iterations: u32,
    pub created_at: String,
    pub updated_at: String,
    /// When the run first executed; `null` while it is still queued.
    pub started_at: Option<String>,
    /// Place on the run queue while `state` is `queued`: `1` is next to
    /// start. `null` once the run executes.
    pub queue_position: Option<u32>,
    /// Seconds of settled provider waits.
    pub paused_secs: u64,
    /// The provider outage the run is currently waiting out, if any. A
    /// waiting run is `running`, not failed: its work resumes by itself
    /// and its research clock stands still meanwhile.
    pub provider_wait: Option<ProviderWaitView>,
    /// While this run waits on the queue (`queue_position` is set):
    /// whether the run being executed is waiting out a provider outage,
    /// so a queued researcher knows why nothing moves. One bit by
    /// design: never which run is waiting, whose it is, since when, or
    /// what the provider said. `false` once the provider serves again,
    /// when nothing is active, and for a run that is not on the queue.
    /// Read with `queue_position` in one store transaction.
    pub queue_provider_waiting: bool,
}

/// A run's open wait for a transiently unavailable model provider.
#[derive(Debug, Serialize)]
pub struct ProviderWaitView {
    /// When the upstream was last seen unavailable (RFC 3339).
    pub since: String,
    /// The transient failure last observed, as the adapter reported it.
    pub note: Option<String>,
}

/// The goal as specified, with the budget facts the header shows.
#[derive(Debug, Serialize)]
pub struct GoalView {
    pub question: String,
    pub mode: String,
    pub revision: u32,
    pub deliverables: Vec<String>,
    pub budget: BudgetView,
}

/// Real limits from the goal's budget; the UI shows these instead of
/// fabricated time estimates.
#[derive(Debug, Serialize)]
pub struct BudgetView {
    pub max_model_calls: u32,
    pub max_iterations: u32,
    pub max_debate_turns: u32,
}

#[derive(Debug, Serialize)]
pub struct PlanView {
    pub revision: u32,
    pub roles: Vec<String>,
    pub methods: Vec<String>,
    pub rationale: String,
}

/// Task counts for one role, straight from the persisted task set.
#[derive(Debug, Default, Clone, Serialize)]
pub struct TaskCounts {
    pub pending: u32,
    pub blocked: u32,
    pub running: u32,

    pub completed: u32,
    pub failed: u32,
    pub cancelled: u32,
    pub uncertain: u32,
}

impl TaskCounts {
    fn add(&mut self, state: TaskState) {
        match state {
            TaskState::Pending => self.pending += 1,
            TaskState::Blocked => self.blocked += 1,
            TaskState::Running => self.running += 1,

            TaskState::Completed => self.completed += 1,
            TaskState::Failed => self.failed += 1,
            TaskState::Cancelled => self.cancelled += 1,
            TaskState::Uncertain => self.uncertain += 1,
        }
    }

    /// Work not yet finished: what the supervisor still holds open.
    pub fn open(&self) -> u32 {
        self.pending + self.blocked + self.running
    }
}

/// The strategy facts of the task a panel is currently showing.
#[derive(Debug, Serialize)]
pub struct TaskFacts {
    pub id: String,
    pub reasoning_preview: Option<String>,
    pub reasoning_streaming: bool,
    pub strategy: String,
    pub state: String,
    pub attempts: u32,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error: Option<String>,
}

/// One agent box in the topology.
#[derive(Debug, Serialize)]
pub struct AgentPanel {
    pub role: String,
    /// `thinking` (a task is running), `waiting` (work is queued), or `idle`
    /// (nothing open).
    pub state: String,
    pub counts: TaskCounts,
    /// The oldest currently running task, when there is one.
    pub current: Option<TaskFacts>,
    /// The most recently finished task, for context when idle.
    pub last_finished: Option<TaskFacts>,
    /// Supervisor only: open work per worker role, the assignment table.
    pub assignments: Option<Vec<Assignment>>,
}

/// One row of the supervisor's assignment table.
#[derive(Debug, Serialize)]
pub struct Assignment {
    pub role: String,
    pub open: u32,
    pub running: u32,
    pub latest_strategy: Option<String>,
}

/// Aggregate facts across the run.
#[derive(Debug, Serialize)]
pub struct Stats {
    pub items: u32,
    pub reviews: u32,
    pub matches: u32,
    /// Decisions on the record, so the page can tell when the ledger it
    /// last read is out of date.
    pub decisions: u32,
    pub clusters: u32,
    pub sources: u32,
    pub works: u32,
    pub searches: u32,
    pub workers_busy: u32,
    pub workers_total: u32,
    pub leader: Option<Leader>,
}

/// The current top-rated candidate, when any candidate has a rating.
#[derive(Debug, Serialize)]
pub struct Leader {
    pub item_id: String,
    pub title: String,
    pub rating: f64,
    pub matches_played: u32,
}

/// Worker panel state from its task counts.
fn worker_state(counts: &TaskCounts) -> &'static str {
    if counts.running > 0 {
        "thinking"
    } else if counts.pending + counts.blocked > 0 {
        "waiting"
    } else {
        "idle"
    }
}

/// Supervisor state: it is "thinking" while the run is live and work is
/// open anywhere, "waiting" when live but quiescent, otherwise idle.
fn supervisor_state(run_state: RunState, open_tasks: u32) -> &'static str {
    if run_state.is_terminal() {
        "idle"
    } else if open_tasks > 0 {
        "thinking"
    } else {
        "waiting"
    }
}

/// Best rating first, unrated last, ties by ID so the order is stable.
fn sort_items(items: &mut [ItemSummary]) {
    items.sort_by(|a, b| match (a.rating, b.rating) {
        (Some(x), Some(y)) => y.total_cmp(&x).then_with(|| a.id.cmp(&b.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.id.cmp(&b.id),
    });
}

fn rfc3339(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| t.unix_timestamp().to_string())
}

fn task_facts(
    task: &Task,
    reasoning_previews: &HashMap<String, crate::store::ReasoningPreview>,
) -> TaskFacts {
    let reasoning = reasoning_previews.get(task.id.as_str());
    TaskFacts {
        id: task.id.as_str().to_string(),
        reasoning_preview: reasoning.map(|preview| preview.content.clone()),
        reasoning_streaming: reasoning.is_some_and(|preview| preview.streaming),
        strategy: task.strategy.clone(),
        state: task.state.as_str().to_string(),
        attempts: task.attempts,
        started_at: task.started_at.map(rfc3339),
        finished_at: task.finished_at.map(rfc3339),
        error: task.error.clone(),
    }
}

/// Group tasks by role name, preserving creation order within each group.
fn tasks_by_role(tasks: &[Task]) -> BTreeMap<&'static str, Vec<&Task>> {
    let mut by_role: BTreeMap<&'static str, Vec<&Task>> = BTreeMap::new();
    for task in tasks {
        by_role.entry(task.role.as_str()).or_default().push(task);
    }
    by_role
}

fn panel_for_role(
    role: &str,
    tasks: &[&Task],
    reasoning_previews: &HashMap<String, crate::store::ReasoningPreview>,
) -> AgentPanel {
    let mut counts = TaskCounts::default();
    for task in tasks {
        counts.add(task.state);
    }
    let current = tasks
        .iter()
        .filter(|t| t.state == TaskState::Running)
        .min_by_key(|t| t.started_at)
        .map(|t| task_facts(t, reasoning_previews));
    let last_finished = tasks
        .iter()
        .filter(|t| t.finished_at.is_some())
        .max_by_key(|t| t.finished_at)
        .map(|t| task_facts(t, reasoning_previews));
    AgentPanel {
        role: role.to_string(),
        state: worker_state(&counts).to_string(),
        counts,
        current,
        last_finished,
        assignments: None,
    }
}

/// One side of a recorded match: the candidate as the judge saw it, with
/// the rating movement the verdict caused.
#[derive(Debug, Serialize)]
pub struct FeedSide {
    pub item_id: String,
    pub title: String,
    pub rating_before: f64,
    pub rating_after: f64,
    /// `rating_after - rating_before`, signed. Zero when the verdict
    /// changed nothing (an `insufficient_basis` match).
    pub delta: f64,
}

/// One recorded match as a feed entry: both candidates, the verdict, the
/// judge that gave it, and the rating movement it produced.
#[derive(Debug, Serialize)]
pub struct FeedMatch {
    pub id: String,
    pub method: String,
    pub judge_model: String,
    pub outcome: String,
    pub rationale: String,
    /// The candidate presented first (`order_randomized` says whether that
    /// order was shuffled).
    pub first: FeedSide,
    /// The candidate presented second.
    pub second: FeedSide,
    pub order_randomized: bool,
    pub created_at: String,
    /// The task that produced this match, when one is recorded.
    pub produced_by: Option<String>,
}

/// Every match the engine recorded for one run, in recording order.
#[derive(Debug, Serialize)]
pub struct TournamentFeed {
    pub run_id: String,
    pub matches: Vec<FeedMatch>,
}

/// Build the tournament feed from persisted matches. Titles come from the
/// item records; a match whose candidate has since been removed keeps its
/// ID as the label rather than inventing one.
pub async fn tournament_feed(store: &Store, run_id: &RunId) -> Result<TournamentFeed> {
    let matches = store.list_matches(run_id).await?;
    let titles: HashMap<String, String> = store
        .item_summaries(run_id, None)
        .await?
        .into_iter()
        .map(|item| (item.id.as_str().to_string(), item.title))
        .collect();

    let side = |item_id: &ItemId, before: f64, after: f64| FeedSide {
        item_id: item_id.as_str().to_string(),
        title: titles
            .get(item_id.as_str())
            .cloned()
            .unwrap_or_else(|| item_id.as_str().to_string()),
        rating_before: before,
        rating_after: after,
        delta: after - before,
    };

    let entries = matches
        .iter()
        .map(|m| FeedMatch {
            id: m.id.as_str().to_string(),
            method: m.method.as_str().to_string(),
            judge_model: m.judge_model.clone(),
            outcome: m.outcome.as_str().to_string(),
            rationale: m.rationale.clone(),
            first: side(&m.item_a, m.rating_a_before, m.rating_a_after),
            second: side(&m.item_b, m.rating_b_before, m.rating_b_after),
            order_randomized: m.order_randomized,
            created_at: rfc3339(m.created_at),
            produced_by: m.produced_by.as_ref().map(|task| task.as_str().to_string()),
        })
        .collect();

    Ok(TournamentFeed {
        run_id: run_id.as_str().to_string(),
        matches: entries,
    })
}

/// One recorded decision: who took it, what kind it was, why, and which
/// records it refers to. The structured `payload` is deliberately left out:
/// it is shaped per kind, and a ledger that renders it as prose would be
/// inventing a reading of it.
#[derive(Debug, Serialize)]
pub struct DecisionView {
    pub id: String,
    pub kind: String,
    /// `researcher`, `supervisor`, `system`, or the agent's own name.
    pub actor: String,
    /// The actor's stated reason, in its own words.
    pub reason: String,
    pub referenced: Vec<String>,
    pub created_at: String,
}

/// Every decision on the run's record, in the order it was taken.
#[derive(Debug, Serialize)]
pub struct DecisionLedger {
    pub run_id: String,
    pub decisions: Vec<DecisionView>,
}

fn actor_label(actor: &Actor) -> String {
    match actor {
        Actor::Researcher => "researcher".to_string(),
        Actor::Supervisor => "supervisor".to_string(),
        Actor::Agent(name) => name.clone(),
        Actor::System => "system".to_string(),
    }
}

/// The run's decisions, oldest first: a ledger reads as the sequence of
/// choices the run actually made.
pub async fn decision_ledger(store: &Store, run_id: &RunId) -> Result<DecisionLedger> {
    let decisions = store.list_decisions(run_id).await?;
    Ok(DecisionLedger {
        run_id: run_id.as_str().to_string(),
        decisions: decisions
            .iter()
            .map(|d| DecisionView {
                id: d.id.as_str().to_string(),
                kind: d.kind.as_str().to_string(),
                actor: actor_label(&d.actor),
                reason: d.reason.clone(),
                referenced: d.referenced.clone(),
                created_at: rfc3339(d.created_at),
            })
            .collect(),
    })
}

/// One match as a single candidate's history reads it: the opponent, what
/// the verdict meant from this candidate's side, and the rating it moved.
#[derive(Debug, Serialize)]
pub struct HistoryEntry {
    pub id: String,
    pub opponent_id: String,
    pub opponent_title: String,
    /// `won`, `lost`, `drew` or `unresolved`, stated from this candidate's
    /// side so a reader never has to work out which one was `item_a`.
    pub result: String,
    pub method: String,
    pub judge_model: String,
    pub rationale: String,
    pub rating_before: f64,
    pub rating_after: f64,
    /// `rating_after - rating_before`, signed. Zero for `unresolved`.
    pub delta: f64,
    pub created_at: String,
    pub produced_by: Option<String>,
}

/// Everything the record says about one candidate's matches, newest first.
#[derive(Debug, Serialize)]
pub struct CandidateHistory {
    pub run_id: String,
    pub item_id: String,
    /// The candidate's title, or its ID when the item record is gone.
    pub title: String,
    pub matches: Vec<HistoryEntry>,
}

/// Build one candidate's match history. `None` when the run has no such
/// item: a route that answers `null` for a candidate it cannot find is
/// clearer than an empty history that reads like "never fought".
pub async fn candidate_history(
    store: &Store,
    run_id: &RunId,
    item_id: &ItemId,
) -> Result<Option<CandidateHistory>> {
    let titles: HashMap<String, String> = store
        .item_summaries(run_id, None)
        .await?
        .into_iter()
        .map(|item| (item.id.as_str().to_string(), item.title))
        .collect();
    let title = match titles.get(item_id.as_str()) {
        Some(title) => title.clone(),
        None => return Ok(None),
    };

    let mut entries: Vec<HistoryEntry> = store
        .list_matches(run_id)
        .await?
        .iter()
        .filter_map(|m| {
            let own_first = m.item_a == *item_id;
            let (opponent, before, after) = if own_first {
                (&m.item_b, m.rating_a_before, m.rating_a_after)
            } else if m.item_b == *item_id {
                (&m.item_a, m.rating_b_before, m.rating_b_after)
            } else {
                return None;
            };
            let result = match (m.outcome, own_first) {
                (MatchOutcome::WinA, true) | (MatchOutcome::WinB, false) => "won",
                (MatchOutcome::WinA, false) | (MatchOutcome::WinB, true) => "lost",
                (MatchOutcome::Draw, _) => "drew",
                (MatchOutcome::InsufficientBasis, _) => "unresolved",
            };
            Some(HistoryEntry {
                id: m.id.as_str().to_string(),
                opponent_id: opponent.as_str().to_string(),
                opponent_title: titles
                    .get(opponent.as_str())
                    .cloned()
                    .unwrap_or_else(|| opponent.as_str().to_string()),
                result: result.to_string(),
                method: m.method.as_str().to_string(),
                judge_model: m.judge_model.clone(),
                rationale: m.rationale.clone(),
                rating_before: before,
                rating_after: after,
                delta: after - before,
                created_at: rfc3339(m.created_at),
                produced_by: m.produced_by.as_ref().map(|task| task.as_str().to_string()),
            })
        })
        .collect();
    // Newest first: a history is read from the most recent result back.
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(Some(CandidateHistory {
        run_id: run_id.as_str().to_string(),
        item_id: item_id.as_str().to_string(),
        title,
        matches: entries,
    }))
}

/// Build the complete snapshot for one run.
pub async fn run_snapshot(store: &Store, run_id: &RunId) -> Result<RunSnapshot> {
    let run = store.get_run(run_id).await?;
    let goal = store.get_goal(&run.goal_id).await?;
    let plan = store.current_plan(run_id).await?;
    let timing = store.run_timing(run_id).await?;
    let queue = store.queue_context(run_id).await?;
    let tasks = store.list_tasks(run_id).await?;
    let usage = store.budget_usage(run_id).await?;
    let mut items = store
        .item_summaries(run_id, plan.as_ref().map(|p| &p.id))
        .await?;
    sort_items(&mut items);

    let ranking_baseline = store
        .list_snapshots(run_id)
        .await?
        .into_iter()
        .find(|snapshot| snapshot.iteration.checked_add(1) == Some(run.iterations))
        .and_then(|snapshot| {
            let plan = plan.as_ref()?;
            let ratings = snapshot.ratings?;
            if !ratings.iter().any(|rating| rating.rubric_id == plan.id) {
                return None;
            }
            Some(RankingBaseline {
                iteration: snapshot.iteration,
                ratings: ratings
                    .into_iter()
                    .filter(|rating| rating.rubric_id == plan.id)
                    .map(|rating| (rating.item_id.to_string(), rating.rating))
                    .collect(),
            })
        });

    let matches = store.list_matches(run_id).await?.len() as u32;
    let decisions = store.count_decisions(run_id).await?;
    let clusters = store.list_clusters(run_id).await?.len() as u32;
    let sources = store.list_sources(run_id).await?.len() as u32;
    let works = store.list_works(run_id).await?.len() as u32;
    let searches = store.list_search_records(run_id).await?.len() as u32;
    // Worker roles come from the plan; roles that already have tasks but
    // fell out of a revised plan still get a panel so no work is hidden.
    let by_role = tasks_by_role(&tasks);
    let mut roles: Vec<String> = plan.as_ref().map(|p| p.roles.clone()).unwrap_or_default();
    for role in by_role.keys() {
        if *role != "supervisor" && !roles.iter().any(|r| r == role) {
            roles.push((*role).to_string());
        }
    }

    let mut preview_task_ids = Vec::with_capacity(by_role.len() * 2);
    for role_tasks in by_role.values() {
        if let Some(task) = role_tasks
            .iter()
            .filter(|task| task.state == TaskState::Running)
            .min_by_key(|task| task.started_at)
        {
            preview_task_ids.push(task.id.clone());
        }
        if let Some(task) = role_tasks
            .iter()
            .filter(|task| task.finished_at.is_some())
            .max_by_key(|task| task.finished_at)
        {
            preview_task_ids.push(task.id.clone());
        }
    }
    let reasoning_previews = store
        .task_reasoning_previews(run_id, &preview_task_ids)
        .await?;

    let empty: Vec<&Task> = Vec::new();
    let mut agents = Vec::with_capacity(roles.len() + 1);
    let mut workers_busy = 0u32;
    let mut assignments = Vec::with_capacity(roles.len());
    let mut open_tasks = 0u32;

    let mut worker_panels = Vec::with_capacity(roles.len());
    for role in &roles {
        let role_tasks = by_role.get(role.as_str()).unwrap_or(&empty);
        let panel = panel_for_role(role, role_tasks, &reasoning_previews);
        open_tasks += panel.counts.open();
        if panel.counts.running > 0 {
            workers_busy += 1;
        }
        assignments.push(Assignment {
            role: role.clone(),
            open: panel.counts.open(),
            running: panel.counts.running,
            latest_strategy: panel
                .current
                .as_ref()
                .or(panel.last_finished.as_ref())
                .map(|t| t.strategy.clone()),
        });
        worker_panels.push(panel);
    }

    let supervisor_tasks = by_role.get("supervisor").unwrap_or(&empty);
    let mut supervisor = panel_for_role("supervisor", supervisor_tasks, &reasoning_previews);
    open_tasks += supervisor.counts.open();
    supervisor.state = supervisor_state(run.state, open_tasks).to_string();
    supervisor.assignments = Some(assignments);
    agents.push(supervisor);
    agents.extend(worker_panels);

    let reviews = items.iter().map(|i| i.review_count).sum();
    let leader = items.iter().find(|i| i.rating.is_some()).map(|i| Leader {
        item_id: i.id.as_str().to_string(),
        title: i.title.clone(),
        rating: i.rating.unwrap_or(INITIAL_ELO),
        matches_played: i.matches_played,
    });

    Ok(RunSnapshot {
        run: RunMeta {
            id: run.id.as_str().to_string(),
            state: run.state.as_str().to_string(),
            stop_condition: run.stop_condition.as_ref().map(|c| c.as_str().to_string()),
            iterations: run.iterations,
            created_at: rfc3339(run.created_at),
            updated_at: rfc3339(run.updated_at),
            started_at: timing.started_at.map(rfc3339),
            queue_position: queue
                .status
                .filter(|q| matches!(q.state, QueueState::Preparing | QueueState::Queued))
                .map(|q| q.ahead.saturating_add(1)),
            paused_secs: timing.paused_secs,
            provider_wait: timing.paused_since.map(|since| ProviderWaitView {
                since: rfc3339(since),
                note: timing.pause_note.clone(),
            }),
            queue_provider_waiting: queue.active_provider_waiting,
        },
        goal: GoalView {
            question: goal.question,
            mode: goal.mode.as_str().to_string(),
            revision: goal.revision,
            deliverables: goal.deliverables,
            budget: BudgetView {
                max_model_calls: goal.budget.max_model_calls,
                max_iterations: goal.budget.max_iterations,
                max_debate_turns: goal.budget.max_debate_turns,
            },
        },
        plan: plan.map(|p| PlanView {
            revision: p.revision,
            roles: p.roles,
            methods: p.methods,
            rationale: p.rationale,
        }),
        agents,
        stats: Stats {
            items: items.len() as u32,
            reviews,
            matches,
            decisions,
            clusters,
            sources,
            works,
            searches,
            workers_busy,
            workers_total: roles.len() as u32,
            leader,
        },
        usage,
        items,
        ranking_baseline,
    })
}

/// Whether a serialized snapshot describes a finished run.
pub fn is_terminal(snapshot: &RunSnapshot) -> bool {
    RunState::parse(&snapshot.run.state).is_some_and(RunState::is_terminal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(running: u32, pending: u32, completed: u32) -> TaskCounts {
        TaskCounts {
            running,
            pending,
            completed,
            ..TaskCounts::default()
        }
    }

    #[test]
    fn a_running_task_makes_the_worker_thinking() {
        assert_eq!(worker_state(&counts(1, 3, 2)), "thinking");
    }

    #[test]
    fn queued_work_without_a_running_task_is_waiting() {
        assert_eq!(worker_state(&counts(0, 2, 1)), "waiting");
    }

    #[test]
    fn no_open_work_is_idle() {
        assert_eq!(worker_state(&counts(0, 0, 5)), "idle");
        assert_eq!(worker_state(&TaskCounts::default()), "idle");
    }

    #[test]
    fn supervisor_thinks_while_work_is_open_and_idles_when_terminal() {
        assert_eq!(supervisor_state(RunState::Running, 3), "thinking");
        assert_eq!(supervisor_state(RunState::Running, 0), "waiting");
        assert_eq!(supervisor_state(RunState::Completed, 3), "idle");
        assert_eq!(supervisor_state(RunState::Cancelled, 0), "idle");
    }

    fn summary(id: &str, rating: Option<f64>) -> ItemSummary {
        ItemSummary {
            id: ItemId::from_raw(id),
            kind: ItemKind::Hypothesis,
            title: id.to_string(),
            disposition: "active".into(),
            assessment: Assessment::Untested,
            assessment_basis: None,
            rating,
            matches_played: 0,
            stale_rating: false,
            review_count: 0,
        }
    }

    #[test]
    fn items_sort_by_rating_desc_with_unrated_last_and_stable_ties() {
        let mut items = vec![
            summary("item_c", None),
            summary("item_b", Some(1210.0)),
            summary("item_a", Some(1190.0)),
            summary("item_d", Some(1210.0)),
            summary("item_e", None),
        ];
        sort_items(&mut items);
        let order: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(order, ["item_b", "item_d", "item_a", "item_c", "item_e"]);
    }
}
