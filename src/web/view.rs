//! Honest UI view models projected from persisted records.
//!
//! Everything here is derived from SQLite facts: task states, the budget
//! ledger, ratings, and counts. Nothing invents activity prose or duration
//! estimates; the frontend renders real state or renders nothing.

use crate::Result;
use crate::records::*;
use crate::store::{BudgetUsage, ItemSummary, Store};
use serde::Serialize;
use std::collections::BTreeMap;

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
    pub wall_clock_secs: u64,
    pub max_debate_turns: u32,
    pub max_tokens: u64,
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
    pub waiting_for_human: u32,
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
            TaskState::WaitingForHuman => self.waiting_for_human += 1,
            TaskState::Completed => self.completed += 1,
            TaskState::Failed => self.failed += 1,
            TaskState::Cancelled => self.cancelled += 1,
            TaskState::Uncertain => self.uncertain += 1,
        }
    }

    /// Work not yet finished: what the supervisor still holds open.
    pub fn open(&self) -> u32 {
        self.pending + self.blocked + self.running + self.waiting_for_human
    }
}

/// The strategy facts of the task a panel is currently showing.
#[derive(Debug, Serialize)]
pub struct TaskFacts {
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
    /// `thinking` (a task is running), `waiting` (work queued or parked on a
    /// human), or `idle` (nothing open).
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
    pub clusters: u32,
    pub sources: u32,
    pub works: u32,
    pub searches: u32,
    pub pending_approvals: u32,
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
    } else if counts.pending + counts.blocked + counts.waiting_for_human > 0 {
        "waiting"
    } else {
        "idle"
    }
}

/// Supervisor state: it is "thinking" while the run is live and work is
/// open anywhere, "waiting" when live but quiescent, otherwise idle.
fn supervisor_state(run_state: RunState, open_tasks: u32) -> &'static str {
    if run_state.is_terminal() || run_state == RunState::Blocked {
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

fn task_facts(task: &Task) -> TaskFacts {
    TaskFacts {
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

fn panel_for_role(role: &str, tasks: &[&Task]) -> AgentPanel {
    let mut counts = TaskCounts::default();
    for task in tasks {
        counts.add(task.state);
    }
    let current = tasks
        .iter()
        .filter(|t| t.state == TaskState::Running)
        .min_by_key(|t| t.started_at)
        .map(|t| task_facts(t));
    let last_finished = tasks
        .iter()
        .filter(|t| t.finished_at.is_some())
        .max_by_key(|t| t.finished_at)
        .map(|t| task_facts(t));
    AgentPanel {
        role: role.to_string(),
        state: worker_state(&counts).to_string(),
        counts,
        current,
        last_finished,
        assignments: None,
    }
}

/// Build the complete snapshot for one run.
pub async fn run_snapshot(store: &Store, run_id: &RunId) -> Result<RunSnapshot> {
    let run = store.get_run(run_id).await?;
    let goal = store.get_goal(&run.goal_id).await?;
    let plan = store.current_plan(run_id).await?;
    let tasks = store.list_tasks(run_id).await?;
    let usage = store.budget_usage(run_id).await?;
    let mut items = store
        .item_summaries(run_id, plan.as_ref().map(|p| &p.id))
        .await?;
    sort_items(&mut items);

    let matches = store.list_matches(run_id).await?.len() as u32;
    let clusters = store.list_clusters(run_id).await?.len() as u32;
    let sources = store.list_sources(run_id).await?.len() as u32;
    let works = store.list_works(run_id).await?.len() as u32;
    let searches = store.list_search_records(run_id).await?.len() as u32;
    let pending_approvals = store
        .list_approvals(run_id, Some(ApprovalState::Pending))
        .await?
        .len() as u32;

    // Worker roles come from the plan; roles that already have tasks but
    // fell out of a revised plan still get a panel so no work is hidden.
    let by_role = tasks_by_role(&tasks);
    let mut roles: Vec<String> = plan.as_ref().map(|p| p.roles.clone()).unwrap_or_default();
    for role in by_role.keys() {
        if *role != "supervisor" && !roles.iter().any(|r| r == role) {
            roles.push((*role).to_string());
        }
    }

    let empty: Vec<&Task> = Vec::new();
    let mut agents = Vec::with_capacity(roles.len() + 1);
    let mut workers_busy = 0u32;
    let mut assignments = Vec::with_capacity(roles.len());
    let mut open_tasks = 0u32;

    let mut worker_panels = Vec::with_capacity(roles.len());
    for role in &roles {
        let role_tasks = by_role.get(role.as_str()).unwrap_or(&empty);
        let panel = panel_for_role(role, role_tasks);
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
    let mut supervisor = panel_for_role("supervisor", supervisor_tasks);
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
        },
        goal: GoalView {
            question: goal.question,
            mode: goal.mode.as_str().to_string(),
            revision: goal.revision,
            deliverables: goal.deliverables,
            budget: BudgetView {
                max_model_calls: goal.budget.max_model_calls,
                max_iterations: goal.budget.max_iterations,
                wall_clock_secs: goal.budget.wall_clock_secs,
                max_debate_turns: goal.budget.max_debate_turns,
                max_tokens: goal.budget.max_tokens,
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
            clusters,
            sources,
            works,
            searches,
            pending_approvals,
            workers_busy,
            workers_total: roles.len() as u32,
            leader,
        },
        usage,
        items,
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
    fn a_parked_human_gate_is_waiting() {
        let c = TaskCounts {
            waiting_for_human: 1,
            ..TaskCounts::default()
        };
        assert_eq!(worker_state(&c), "waiting");
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
        assert_eq!(supervisor_state(RunState::Blocked, 1), "idle");
    }

    fn summary(id: &str, rating: Option<f64>) -> ItemSummary {
        ItemSummary {
            id: ItemId::from_raw(id),
            kind: ItemKind::Hypothesis,
            title: id.to_string(),
            disposition: "active".into(),
            assessment: Assessment::Untested,
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
