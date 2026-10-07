//! Starting a run over the API: validation, record creation, and dispatch.
//!
//! The creation path mirrors `uruk run` for a web-scoped subset: no file
//! inputs, no network retrieval, no tool execution, no literature search.
//! Those grants stay CLI-only until the API has authentication to attribute
//! them to.

use super::AppState;
use crate::agents::{safety, supervisor};
use crate::records::*;
use crate::runtime::{Scheduler, SchedulerConfig};
use crate::store::OwnerDigest;
use crate::{Error, Result, report};
use time::OffsetDateTime;

/// Body of `POST /api/runs`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRunRequest {
    /// The research question: what would count as an answer.
    pub goal: String,
    /// `task` (default) or `campaign`.
    #[serde(default)]
    pub mode: Option<String>,
    /// `simple` (default) or `tournament`: how hard Ranking argues. Both
    /// keep Elo; `simple` caps comparison debates at a single turn,
    /// `tournament` allows multi-turn scientific debate.
    #[serde(default)]
    pub ranking: Option<String>,
    #[serde(default)]
    pub deliverables: Vec<String>,
    #[serde(default)]
    pub preferences: Vec<String>,
    #[serde(default)]
    pub attributes: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub max_model_calls: Option<u32>,
    #[serde(default)]
    pub max_seconds: Option<u64>,
    #[serde(default)]
    pub max_iterations: Option<u32>,
}

/// A validated, normalized start request.
#[derive(Debug)]
pub struct ValidatedStart {
    pub question: String,
    pub mode: Mode,
    pub debate_turns: u32,
    pub deliverables: Vec<String>,
    pub preferences: Vec<String>,
    pub attributes: Vec<String>,
    pub constraints: Vec<String>,
    pub max_model_calls: u32,
    pub max_seconds: u64,
    pub max_iterations: u32,
}

const MAX_GOAL_CHARS: usize = 8_000;
const MAX_LIST_ENTRIES: usize = 32;
const MAX_ENTRY_CHARS: usize = 500;

/// Trim entries, drop blanks, and enforce the size caps.
fn clean_list(name: &str, values: Vec<String>) -> Result<Vec<String>> {
    let cleaned: Vec<String> = values
        .into_iter()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    if cleaned.len() > MAX_LIST_ENTRIES {
        return Err(Error::validation(format!(
            "{name}: at most {MAX_LIST_ENTRIES} entries, got {}",
            cleaned.len()
        )));
    }
    if let Some(long) = cleaned.iter().find(|v| v.chars().count() > MAX_ENTRY_CHARS) {
        return Err(Error::validation(format!(
            "{name}: entry exceeds {MAX_ENTRY_CHARS} characters ({} chars)",
            long.chars().count()
        )));
    }
    Ok(cleaned)
}

fn bounded<T: PartialOrd + std::fmt::Display + Copy>(
    name: &str,
    value: T,
    min: T,
    max: T,
) -> Result<T> {
    if value < min || value > max {
        return Err(Error::validation(format!(
            "{name} must be between {min} and {max}, got {value}"
        )));
    }
    Ok(value)
}

/// Validate a start request into normalized values. Pure, so it is unit
/// tested without a store.
pub fn validate(req: StartRunRequest) -> Result<ValidatedStart> {
    let question = req.goal.trim().to_string();
    if question.is_empty() {
        return Err(Error::validation("goal must not be empty"));
    }
    if question.chars().count() > MAX_GOAL_CHARS {
        return Err(Error::validation(format!(
            "goal exceeds {MAX_GOAL_CHARS} characters"
        )));
    }

    let mode = match req.mode.as_deref() {
        None | Some("task") => Mode::Task,
        Some("campaign") => Mode::Campaign,
        Some(other) => {
            return Err(Error::validation(format!(
                "mode must be \"task\" or \"campaign\", got {other:?}"
            )));
        }
    };

    // Both options run the Elo tournament; the knob is the §7 debate turn
    // cap: single-turn comparisons versus multi-turn scientific debate.
    let debate_turns = match req.ranking.as_deref() {
        None | Some("simple") => 1,
        Some("tournament") => 5,
        Some(other) => {
            return Err(Error::validation(format!(
                "ranking must be \"simple\" or \"tournament\", got {other:?}"
            )));
        }
    };

    Ok(ValidatedStart {
        question,
        mode,
        debate_turns,
        deliverables: clean_list("deliverables", req.deliverables)?,
        preferences: clean_list("preferences", req.preferences)?,
        attributes: clean_list("attributes", req.attributes)?,
        constraints: clean_list("constraints", req.constraints)?,
        max_model_calls: bounded(
            "max_model_calls",
            req.max_model_calls.unwrap_or(200),
            1,
            100_000,
        )?,
        max_seconds: bounded("max_seconds", req.max_seconds.unwrap_or(3600), 1, 604_800)?,
        max_iterations: bounded("max_iterations", req.max_iterations.unwrap_or(10), 1, 1_000)?,
    })
}

/// Persist the run, gate it on safety, plan it, and dispatch the scheduler
/// in the background. Mirrors the CLI `run` path (SPEC §11, §12), with one
/// addition: the run is bound to the requesting browser's [`OwnerDigest`]
/// in the same transaction that creates it, so a crash cannot leave an
/// unowned web run behind.
pub async fn start_run(state: &AppState, v: ValidatedStart, owner: &OwnerDigest) -> Result<RunId> {
    let store = state.store();
    let run_id = RunId::new();
    let goal_id = GoalId::new();
    // The same derivation the CLI uses, so web and CLI runs share one
    // project row.
    let (project_id, project_name) = store.project_identity();

    // Web runs grant nothing beyond provider disclosure: no paths, no
    // network, no execution, no tools (SPEC §12).
    let permissions = Permissions {
        disclose_to_provider: true,
        ..Default::default()
    };

    let goal = Goal {
        id: goal_id.clone(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        revision: 0,
        question: v.question,
        mode: v.mode,
        deliverables: if v.deliverables.is_empty() {
            vec!["research report".to_string()]
        } else {
            v.deliverables
        },
        inputs: vec![],
        scope: None,
        exclusions: vec![],
        known_facts: vec![],
        assumptions: vec![],
        rubric: Rubric {
            preferences: v.preferences,
            attributes: v.attributes,
            constraints: v.constraints,
            acceptance: vec![],
        },
        permissions,
        budget: Budget {
            wall_clock_secs: v.max_seconds,
            max_model_calls: v.max_model_calls,
            max_iterations: v.max_iterations,
            max_debate_turns: v.debate_turns,
            ..Default::default()
        },
        profile: None,
        revision_reason: None,
        created_at: OffsetDateTime::now_utc(),
    };

    let run = Run {
        id: run_id.clone(),
        schema_version: SCHEMA_VERSION,
        project_id: project_id.clone(),
        goal_id,
        plan_id: None,
        state: RunState::Running,
        stop_condition: None,
        iterations: 0,
        created_at: OffsetDateTime::now_utc(),
        updated_at: OffsetDateTime::now_utc(),
    };

    store.ensure_project(&project_id, &project_name).await?;
    store.create_run_owned(&run, &goal, owner).await?;

    // Check the goal itself before any work is scheduled (SPEC §12).
    let verdict = safety::check(&goal.question);
    if let Some(decision) = safety::safety_decision(&run_id, &verdict, "research goal", vec![]) {
        store.insert_decision(&decision).await?;
        if verdict.is_blocked() {
            store
                .set_run_state(
                    &run_id,
                    RunState::Blocked,
                    Some(StopCondition::SafetyBoundary),
                )
                .await?;
            return Err(Error::permission(
                verdict
                    .concern()
                    .unwrap_or("the goal is blocked")
                    .to_string(),
            ));
        }
    }

    let plan = supervisor::build_plan(&goal, 0);
    store.insert_plan(&plan).await?;

    dispatch(state.clone(), run_id.clone());
    Ok(run_id)
}

/// Run the scheduler for this run in the background, then export the
/// report. Stop requests reach it through the persisted run state
/// (SPEC §9.2), so no handle needs to be kept. The run stays claimed in
/// [`AppState`] for the scheduler's lifetime so the reconcile sweep never
/// starts a second scheduler for it; returns whether this call took the
/// claim.
fn dispatch(state: AppState, run_id: RunId) -> bool {
    if !state.claim_run(&run_id) {
        return false;
    }
    tokio::spawn(async move {
        let scheduler = Scheduler::new(
            state.store().clone(),
            state.provider().clone(),
            SchedulerConfig::default(),
        );
        match scheduler.run(&run_id).await {
            Ok(summary) => tracing::info!(
                run = %run_id,
                state = summary.state.as_str(),
                "run finished"
            ),
            Err(e) => tracing::error!(run = %run_id, error = %e, "run failed"),
        }
        // Export whatever state the run reached; a partial export is marked
        // as partial rather than hidden (SPEC §6).
        if let Err(e) = report::export_run(state.store(), &run_id).await {
            tracing::error!(run = %run_id, error = %e, "report export failed");
        }
        state.release_run(&run_id);
    });
    true
}

/// Dispatch schedulers for runs this process should be driving but is not.
///
/// Two cases, both consequences of `uruk serve` owning the project
/// scheduler lock (which locks `uruk resume` out):
///
/// - a run left `running` by an interrupted process would otherwise sit
///   that way forever;
/// - a run parked `waiting-for-human` whose approvals have all been
///   decided (`uruk approve` / `uruk deny` work from another terminal)
///   needs a scheduler to pick the released work back up.
///
/// Runs already claimed by an in-process scheduler are skipped, as are
/// `blocked` and terminal runs. Returns the runs dispatched by this sweep.
pub async fn resume_orphaned_runs(state: &AppState) -> Result<Vec<RunId>> {
    let mut resumed = Vec::new();
    for run in state.store().list_run_overviews().await? {
        let eligible = match run.state {
            RunState::Running => true,
            RunState::WaitingForHuman => state
                .store()
                .list_approvals(&run.id, Some(ApprovalState::Pending))
                .await?
                .is_empty(),
            _ => false,
        };
        if !eligible {
            continue;
        }
        if dispatch(state.clone(), run.id.clone()) {
            tracing::info!(run = %run.id, state = run.state.as_str(), "resuming orphaned run");
            resumed.push(run.id);
        }
    }
    Ok(resumed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(goal: &str) -> StartRunRequest {
        StartRunRequest {
            goal: goal.into(),
            mode: None,
            ranking: None,
            deliverables: vec![],
            preferences: vec![],
            attributes: vec![],
            constraints: vec![],
            max_model_calls: None,
            max_seconds: None,
            max_iterations: None,
        }
    }

    #[test]
    fn a_blank_goal_is_rejected() {
        assert!(validate(request("   \n\t ")).is_err());
    }

    #[test]
    fn defaults_mirror_the_cli() {
        let v = validate(request("why do the measurements disagree")).expect("valid");
        assert_eq!(v.mode, Mode::Task);
        assert_eq!(v.max_model_calls, 200);
        assert_eq!(v.max_seconds, 3600);
        assert_eq!(v.max_iterations, 10);
    }

    #[test]
    fn simple_ranking_caps_debates_at_one_turn_and_tournament_at_five() {
        let simple = validate(StartRunRequest {
            ranking: Some("simple".into()),
            ..request("q")
        })
        .expect("valid");
        assert_eq!(simple.debate_turns, 1);

        let tournament = validate(StartRunRequest {
            ranking: Some("tournament".into()),
            ..request("q")
        })
        .expect("valid");
        assert_eq!(tournament.debate_turns, 5);
    }

    #[test]
    fn unknown_mode_and_ranking_are_validation_errors() {
        assert!(matches!(
            validate(StartRunRequest {
                mode: Some("swarm".into()),
                ..request("q")
            }),
            Err(Error::Validation(_))
        ));
        assert!(matches!(
            validate(StartRunRequest {
                ranking: Some("elaborate".into()),
                ..request("q")
            }),
            Err(Error::Validation(_))
        ));
    }

    #[test]
    fn lists_are_trimmed_capped_and_blank_entries_dropped() {
        let v = validate(StartRunRequest {
            preferences: vec!["  grounded  ".into(), "   ".into()],
            ..request("q")
        })
        .expect("valid");
        assert_eq!(v.preferences, vec!["grounded".to_string()]);

        let too_many = validate(StartRunRequest {
            attributes: (0..40).map(|i| format!("a{i}")).collect(),
            ..request("q")
        });
        assert!(too_many.is_err());
    }

    #[test]
    fn budget_bounds_are_enforced() {
        assert!(
            validate(StartRunRequest {
                max_model_calls: Some(0),
                ..request("q")
            })
            .is_err()
        );
        assert!(
            validate(StartRunRequest {
                max_seconds: Some(10_000_000),
                ..request("q")
            })
            .is_err()
        );
    }
}
