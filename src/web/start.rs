//! Starting a run over the API: validation, record creation, and dispatch.
//!
//! The creation path supports everything an anonymous browser can be
//! answerable for: mode, profile, deliverables, the
//! rubric, budgets, URL inputs, owner-bound uploads, network retrieval,
//! and opt-in literature search. Local subprocess execution and arbitrary
//! tool names remain unavailable because the web surface has no
//! exact-payload approve/deny step to bind them to, and
//! no request field here ever names a server filesystem path: files
//! reach a run only through the owner-bound upload store.

use super::AppState;
use crate::agents::{safety, supervisor};
use crate::records::*;
use crate::runtime::{Scheduler, SchedulerConfig};
use crate::store::{OwnerDigest, Store, Upload};
use crate::tools::{ingest_input, ingest_upload};
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
    /// Domain profile name recorded on the goal (SPEC §10).
    #[serde(default)]
    pub profile: Option<String>,
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
    /// Turn cap for multi-turn debates. Only meaningful with
    /// `ranking: "tournament"` (2–10); `simple` is single-turn by
    /// definition.
    #[serde(default)]
    pub max_debate_turns: Option<u32>,
    /// Open-access full-text acquisitions attempted per literature
    /// discovery.
    #[serde(default)]
    pub max_acquisitions: Option<u32>,
    /// `http(s)` URLs supplied as inputs. Never a filesystem path: files
    /// enter runs through `upload_ids` only.
    #[serde(default)]
    pub input_urls: Vec<String>,
    /// Ids of this browser's uploads (`POST /api/uploads`) to supply as
    /// inputs.
    #[serde(default)]
    pub upload_ids: Vec<String>,
    /// Permit network retrieval of the supplied URLs. This alone sends no
    /// goal-derived query anywhere; literature search needs `search`.
    #[serde(default)]
    pub allow_network: bool,
    /// Opt in to federated literature search (OpenAlex, Crossref, arXiv).
    /// Transmits goal-derived queries to those operators, so it requires
    /// `allow_network`.
    #[serde(default)]
    pub search: bool,
    /// Narrow the connector set after `search` (subset of `openalex`,
    /// `crossref`, `arxiv`). Requires `search`.
    #[serde(default)]
    pub search_connectors: Option<Vec<String>>,
    /// Validate, plan, and preview without creating or dispatching
    /// anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// A validated, normalized start request.
#[derive(Debug)]
pub struct ValidatedStart {
    pub question: String,
    pub mode: Mode,
    pub profile: Option<String>,
    pub debate_turns: u32,
    pub deliverables: Vec<String>,
    pub preferences: Vec<String>,
    pub attributes: Vec<String>,
    pub constraints: Vec<String>,
    pub max_model_calls: u32,
    pub max_seconds: u64,
    pub max_iterations: u32,
    pub max_acquisitions: u32,
    pub input_urls: Vec<String>,
    pub upload_ids: Vec<UploadId>,
    pub allow_network: bool,
    /// `search:*` allowlist entries; empty means search was not opted
    /// into.
    pub connector_tools: Vec<String>,
    pub dry_run: bool,
}

const MAX_GOAL_CHARS: usize = 8_000;
const MAX_LIST_ENTRIES: usize = 32;
const MAX_ENTRY_CHARS: usize = 500;
const MAX_PROFILE_CHARS: usize = 100;
const MAX_INPUT_URLS: usize = 16;
const MAX_URL_CHARS: usize = 2_000;
const MAX_UPLOAD_IDS: usize = 16;
const MAX_UPLOAD_ID_CHARS: usize = 128;

/// The one message every missing or foreign upload reference gets, so a
/// probing start request cannot tell those cases apart.
const NO_SUCH_UPLOAD: &str = "upload_ids: no such upload for this browser";

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

/// Validate the supplied input URLs: `http(s)` only, bounded in count and
/// length. A filesystem path — absolute, relative, UNC, or `file://` —
/// is rejected outright; files reach a web run only as owner-bound
/// uploads.
fn clean_input_urls(values: Vec<String>) -> Result<Vec<String>> {
    let mut cleaned: Vec<String> = values
        .into_iter()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    let mut seen = std::collections::HashSet::new();
    cleaned.retain(|value| seen.insert(value.clone()));
    if cleaned.len() > MAX_INPUT_URLS {
        return Err(Error::validation(format!(
            "input_urls: at most {MAX_INPUT_URLS} entries, got {}",
            cleaned.len()
        )));
    }
    for url in &cleaned {
        if url.chars().count() > MAX_URL_CHARS {
            return Err(Error::validation(format!(
                "input_urls: entry exceeds {MAX_URL_CHARS} characters"
            )));
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            // Bounded echo: enough to recognize the mistake, never the
            // whole value.
            let short: String = url.chars().take(80).collect();
            return Err(Error::validation(format!(
                "input_urls accepts http(s) URLs only, got {short:?}; files are \
                 supplied through owner-bound uploads, never as server paths"
            )));
        }
    }
    Ok(cleaned)
}

/// Validate the referenced upload ids: shape only, ownership is checked
/// against the store when the run starts.
fn clean_upload_ids(values: Vec<String>) -> Result<Vec<UploadId>> {
    let mut cleaned: Vec<String> = values
        .into_iter()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    let mut seen = std::collections::HashSet::new();
    cleaned.retain(|value| seen.insert(value.clone()));
    if cleaned.len() > MAX_UPLOAD_IDS {
        return Err(Error::validation(format!(
            "upload_ids: at most {MAX_UPLOAD_IDS} entries, got {}",
            cleaned.len()
        )));
    }
    if cleaned
        .iter()
        .any(|v| v.chars().count() > MAX_UPLOAD_ID_CHARS)
    {
        return Err(Error::validation(NO_SUCH_UPLOAD));
    }
    Ok(cleaned.into_iter().map(UploadId::from_raw).collect())
}

/// Resolve the search opt-ins into `search:*` allowlist entries, with the
/// same disclosure semantics as the CLI: `search` needs the network
/// grant, and naming connectors needs `search`.
fn connector_tools(
    allow_network: bool,
    search: bool,
    search_connectors: Option<Vec<String>>,
) -> Result<Vec<String>> {
    if search && !allow_network {
        return Err(Error::validation(
            "search requires allow_network: literature search transmits \
             goal-derived queries to external operators",
        ));
    }
    if search_connectors.is_some() && !search {
        return Err(Error::validation(
            "search_connectors narrows the connector set and requires search",
        ));
    }
    if !search {
        return Ok(vec![]);
    }
    let Some(names) = search_connectors else {
        return Ok(crate::search::DEFAULT_CONNECTOR_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect());
    };
    let mut tools = Vec::new();
    for name in &names {
        let name = name.trim().to_ascii_lowercase();
        if !crate::search::CONNECTOR_NAMES.contains(&name.as_str()) {
            return Err(Error::validation(format!(
                "unknown search connector {name:?}; known: {}",
                crate::search::CONNECTOR_NAMES.join(", ")
            )));
        }
        let tool = format!("search:{name}");
        if !tools.contains(&tool) {
            tools.push(tool);
        }
    }
    if tools.is_empty() {
        return Err(Error::validation(format!(
            "search_connectors names no connector; known: {} \
             (to search them all, omit search_connectors)",
            crate::search::CONNECTOR_NAMES.join(", ")
        )));
    }
    Ok(tools)
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

    let profile = match req.profile.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(p) if p.chars().count() > MAX_PROFILE_CHARS => {
            return Err(Error::validation(format!(
                "profile is limited to {MAX_PROFILE_CHARS} characters"
            )));
        }
        Some(p) => Some(p.to_string()),
    };

    // Both options run the Elo tournament; the knob is the §7 debate turn
    // cap: single-turn comparisons versus multi-turn scientific debate.
    // An explicit cap only makes sense for the multi-turn method, and one
    // turn of "multi-turn debate" would make that label a lie.
    let ranking = req.ranking.as_deref();
    let debate_turns = match (ranking, req.max_debate_turns) {
        (None | Some("simple"), None | Some(1)) => 1,
        (None | Some("simple"), Some(n)) => {
            return Err(Error::validation(format!(
                "ranking \"simple\" is a single-turn comparison; \
                 max_debate_turns {n} needs ranking \"tournament\""
            )));
        }
        (Some("tournament"), None) => 5,
        (Some("tournament"), Some(n)) => bounded("max_debate_turns", n, 2, 10)?,
        (Some(other), _) => {
            return Err(Error::validation(format!(
                "ranking must be \"simple\" or \"tournament\", got {other:?}"
            )));
        }
    };

    Ok(ValidatedStart {
        question,
        mode,
        profile,
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
        max_acquisitions: bounded(
            "max_acquisitions",
            req.max_acquisitions.unwrap_or(8),
            1,
            100,
        )?,
        input_urls: clean_input_urls(req.input_urls)?,
        upload_ids: clean_upload_ids(req.upload_ids)?,
        allow_network: req.allow_network,
        connector_tools: connector_tools(req.allow_network, req.search, req.search_connectors)?,
        dry_run: req.dry_run,
    })
}

/// The permissions a web-started run gets: exactly what the request
/// granted, plus provider disclosure, and never execution — the web
/// surface has no exact-payload approval step for an anonymous cookie to
/// answer, so grants with local side effects remain unavailable (SPEC §12).
fn permissions_for(v: &ValidatedStart, read_paths: Vec<String>) -> Permissions {
    Permissions {
        read_paths,
        write_paths: vec![],
        network: v.allow_network,
        execute: false,
        allowed_tools: v.connector_tools.clone(),
        disclose_to_provider: true,
        require_approval_for: vec![],
    }
}

/// The request's inputs resolved against the owner's upload store.
struct ResolvedInputs {
    /// Owner-verified uploads, in request order.
    uploads: Vec<Upload>,
    /// URL inputs first, then uploads under their display names.
    input_refs: Vec<InputRef>,
    /// Project-relative storage paths of the uploads: the complete read
    /// allowlist of a web run.
    read_paths: Vec<String>,
}

/// Resolve every referenced upload, owner-scoped. A foreign id and an
/// unknown id get one identical validation error, so a start request
/// cannot probe other browsers' uploads.
async fn resolve_inputs(
    store: &Store,
    v: &ValidatedStart,
    owner: &OwnerDigest,
) -> Result<ResolvedInputs> {
    let mut uploads = Vec::new();
    for id in &v.upload_ids {
        match store.get_upload_owned(id, owner).await {
            Ok(upload) => uploads.push(upload),
            Err(Error::NotFound(_)) => return Err(Error::validation(NO_SUCH_UPLOAD)),
            Err(e) => return Err(e),
        }
    }

    let mut input_refs: Vec<InputRef> = v
        .input_urls
        .iter()
        .map(|url| InputRef {
            locator: url.clone(),
            note: None,
        })
        .collect();
    input_refs.extend(uploads.iter().map(|u| InputRef {
        locator: u.file_name.clone(),
        note: Some(format!("web upload {}", u.id)),
    }));

    let read_paths = uploads.iter().map(|u| u.storage_path.clone()).collect();
    Ok(ResolvedInputs {
        uploads,
        input_refs,
        read_paths,
    })
}

/// Assemble the goal record for a validated start. Pure record building;
/// nothing is persisted here.
fn build_goal(
    v: &ValidatedStart,
    run_id: &RunId,
    goal_id: &GoalId,
    inputs: &ResolvedInputs,
) -> Goal {
    Goal {
        id: goal_id.clone(),
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        revision: 0,
        question: v.question.clone(),
        mode: v.mode,
        deliverables: if v.deliverables.is_empty() {
            vec!["research report".to_string()]
        } else {
            v.deliverables.clone()
        },
        inputs: inputs.input_refs.clone(),
        scope: None,
        exclusions: vec![],
        known_facts: vec![],
        assumptions: vec![],
        rubric: Rubric {
            preferences: v.preferences.clone(),
            attributes: v.attributes.clone(),
            constraints: v.constraints.clone(),
            acceptance: vec![],
        },
        permissions: permissions_for(v, inputs.read_paths.clone()),
        budget: Budget {
            wall_clock_secs: v.max_seconds,
            max_model_calls: v.max_model_calls,
            max_iterations: v.max_iterations,
            max_debate_turns: v.debate_turns,
            max_acquisitions: v.max_acquisitions,
            ..Default::default()
        },
        profile: v.profile.clone(),
        revision_reason: None,
        created_at: OffsetDateTime::now_utc(),
    }
}

/// Preview a start without persisting anything: resolve the inputs,
/// apply the same safety gate a real start applies, and return the plan
/// the Supervisor would run. `uruk run --dry-run` persists its planned
/// run for a later `uruk resume`; the web cannot, because the serve
/// process auto-resumes every persisted `running` run, so the web dry
/// run is a pure preview instead.
pub async fn preview(
    state: &AppState,
    v: ValidatedStart,
    owner: &OwnerDigest,
) -> Result<serde_json::Value> {
    let inputs = resolve_inputs(state.store(), &v, owner).await?;
    let goal = build_goal(&v, &RunId::new(), &GoalId::new(), &inputs);

    let verdict = safety::check(&goal.question);
    if verdict.is_blocked() {
        return Err(Error::permission(
            verdict
                .concern()
                .unwrap_or("the goal is blocked")
                .to_string(),
        ));
    }

    let plan = supervisor::build_plan(&goal, 0);
    Ok(serde_json::json!({
        "ok": true,
        "dry_run": true,
        "plan": {
            "roles": plan.roles,
            "methods": plan.methods,
            "rationale": plan.rationale,
        },
        "inputs": goal.inputs.iter().map(|i| i.locator.clone()).collect::<Vec<_>>(),
        "permissions": {
            "network": goal.permissions.network,
            "execute": goal.permissions.execute,
            "allowed_tools": goal.permissions.allowed_tools,
        },
        "budget": {
            "max_model_calls": goal.budget.max_model_calls,
            "max_seconds": goal.budget.wall_clock_secs,
            "max_iterations": goal.budget.max_iterations,
            "max_debate_turns": goal.budget.max_debate_turns,
            "max_acquisitions": goal.budget.max_acquisitions,
        },
    }))
}

/// Persist the run, gate it on safety, ingest its inputs, plan it, and
/// dispatch the scheduler in the background (SPEC §11, §12). The run is
/// bound to the requesting
/// browser's [`OwnerDigest`] in the same transaction that creates it, so
/// a crash cannot leave an unowned web run behind.
pub async fn start_run(state: &AppState, v: ValidatedStart, owner: &OwnerDigest) -> Result<RunId> {
    if v.dry_run {
        return Err(Error::validation(
            "a dry run must use preview; it cannot be dispatched",
        ));
    }
    let store = state.store();
    let inputs = resolve_inputs(store, &v, owner).await?;

    let run_id = RunId::new();
    let goal_id = GoalId::new();
    // Derived from the canonical project directory, so every run in this
    // project shares one project row.
    let (project_id, project_name) = store.project_identity();

    let goal = build_goal(&v, &run_id, &goal_id, &inputs);
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

    // Ingest supplied sources before dispatch, exactly as the CLI does: a
    // URL without the network grant is recorded as unavailable rather
    // than silently skipped, and an upload that cannot be read or
    // extracted is logged without sinking the whole start.
    for input in goal.inputs.iter().take(v.input_urls.len()) {
        if let Err(e) = ingest_input(store, &run_id, input, &goal.permissions).await {
            tracing::warn!(run = %run_id, locator = %input.locator, error = %e, "URL ingest failed");
        }
    }
    for upload in &inputs.uploads {
        let path = store.resolve_path(&upload.storage_path);
        let result = match tokio::fs::read(&path).await {
            Ok(bytes) => ingest_upload(store, &run_id, &upload.file_name, bytes).await,
            Err(e) => Err(Error::Io(e)),
        };
        if let Err(e) = result {
            tracing::warn!(run = %run_id, upload = %upload.id, error = %e, "upload ingest failed");
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
/// Two cases, both consequences of `uruk serve` being the project's only
/// scheduler owner:
///
/// - a run left `running` by an interrupted process would otherwise sit
///   that way forever;
/// - a run parked `waiting-for-human` with no approvals left pending
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
            profile: None,
            deliverables: vec![],
            preferences: vec![],
            attributes: vec![],
            constraints: vec![],
            max_model_calls: None,
            max_seconds: None,
            max_iterations: None,
            max_debate_turns: None,
            max_acquisitions: None,
            input_urls: vec![],
            upload_ids: vec![],
            allow_network: false,
            search: false,
            search_connectors: None,
            dry_run: false,
        }
    }

    #[test]
    fn a_blank_goal_is_rejected() {
        assert!(validate(request("   \n\t ")).is_err());
    }

    /// The route layer diverts `dry_run` to [`preview`], so this guard is
    /// defense in depth: any future caller of [`start_run`] must get a
    /// refusal, never a dispatched run, from a dry-run request.
    #[tokio::test]
    async fn start_run_refuses_a_dry_run_and_persists_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join(".uruk/state.sqlite"))
            .await
            .expect("open store");
        let state = AppState::new(
            store,
            std::sync::Arc::new(crate::provider::MockProvider::new()),
            crate::web::WebConfig::default(),
        );
        let v = validate(StartRunRequest {
            dry_run: true,
            ..request("q")
        })
        .expect("valid");
        let owner = OwnerDigest::from_token("browser-token");

        let err = start_run(&state, v, &owner).await.expect_err("refused");

        assert!(matches!(err, Error::Validation(_)), "got {err:?}");
        assert!(
            state
                .store()
                .list_run_overviews()
                .await
                .expect("list runs")
                .is_empty(),
            "a refused dry run must not leave a run behind"
        );
    }

    #[test]
    fn omitted_fields_get_the_documented_defaults() {
        let v = validate(request("why do the measurements disagree")).expect("valid");
        assert_eq!(v.mode, Mode::Task);
        assert_eq!(v.max_model_calls, 200);
        assert_eq!(v.max_seconds, 3600);
        assert_eq!(v.max_iterations, 10);
        assert_eq!(v.max_acquisitions, 8);
        assert_eq!(v.profile, None);
        assert!(!v.allow_network);
        assert!(v.connector_tools.is_empty());
        assert!(!v.dry_run);
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
    fn explicit_debate_turns_must_fit_the_ranking_method() {
        // Direct comparison is single-turn by definition; stating 1 is
        // redundant but truthful, anything more contradicts the method.
        assert_eq!(
            validate(StartRunRequest {
                ranking: Some("simple".into()),
                max_debate_turns: Some(1),
                ..request("q")
            })
            .expect("valid")
            .debate_turns,
            1
        );
        assert!(
            validate(StartRunRequest {
                ranking: Some("simple".into()),
                max_debate_turns: Some(3),
                ..request("q")
            })
            .is_err()
        );
        assert!(
            validate(StartRunRequest {
                max_debate_turns: Some(3),
                ..request("q")
            })
            .is_err(),
            "the default ranking is simple, so a turn cap needs tournament"
        );

        // Multi-turn debate: 2 through the published hard cap of 10.
        for (turns, ok) in [(1, false), (2, true), (10, true), (11, false)] {
            let result = validate(StartRunRequest {
                ranking: Some("tournament".into()),
                max_debate_turns: Some(turns),
                ..request("q")
            });
            assert_eq!(result.is_ok(), ok, "tournament with {turns} turns");
        }
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
        assert!(
            validate(StartRunRequest {
                max_acquisitions: Some(0),
                ..request("q")
            })
            .is_err()
        );
        assert!(
            validate(StartRunRequest {
                max_acquisitions: Some(101),
                ..request("q")
            })
            .is_err()
        );
    }

    #[test]
    fn the_profile_is_trimmed_and_bounded() {
        let v = validate(StartRunRequest {
            profile: Some("  materials-science  ".into()),
            ..request("q")
        })
        .expect("valid");
        assert_eq!(v.profile.as_deref(), Some("materials-science"));

        let blank = validate(StartRunRequest {
            profile: Some("   ".into()),
            ..request("q")
        })
        .expect("valid");
        assert_eq!(blank.profile, None);

        assert!(
            validate(StartRunRequest {
                profile: Some("x".repeat(MAX_PROFILE_CHARS + 1)),
                ..request("q")
            })
            .is_err()
        );
    }

    #[test]
    fn input_urls_accept_http_only_never_paths() {
        let ok = validate(StartRunRequest {
            input_urls: vec![
                "  https://example.org/paper.pdf ".into(),
                "https://example.org/paper.pdf".into(),
                "http://example.org/data.csv".into(),
                "".into(),
            ],
            ..request("q")
        })
        .expect("valid");
        assert_eq!(
            ok.input_urls,
            vec![
                "https://example.org/paper.pdf".to_string(),
                "http://example.org/data.csv".to_string(),
            ]
        );

        for bad in [
            "/etc/passwd",
            "file:///etc/passwd",
            "C:\\data\\x.txt",
            "../up.pdf",
            "ftp://host/file",
            "httpss://typo.example",
        ] {
            assert!(
                validate(StartRunRequest {
                    input_urls: vec![bad.into()],
                    ..request("q")
                })
                .is_err(),
                "must reject {bad:?}"
            );
        }
    }

    #[test]
    fn upload_id_lists_are_bounded() {
        assert!(
            validate(StartRunRequest {
                upload_ids: (0..20).map(|i| format!("upl_{i}")).collect(),
                ..request("q")
            })
            .is_err()
        );
        let v = validate(StartRunRequest {
            upload_ids: vec!["  upl_a ".into(), "upl_a".into(), "".into()],
            ..request("q")
        })
        .expect("valid");
        assert_eq!(v.upload_ids, vec![UploadId::from_raw("upl_a")]);
    }

    #[test]
    fn search_needs_network_and_connectors_need_search() {
        assert!(matches!(
            validate(StartRunRequest {
                search: true,
                ..request("q")
            }),
            Err(Error::Validation(msg)) if msg.contains("allow_network")
        ));
        assert!(matches!(
            validate(StartRunRequest {
                allow_network: true,
                search_connectors: Some(vec!["arxiv".into()]),
                ..request("q")
            }),
            Err(Error::Validation(msg)) if msg.contains("search")
        ));
    }

    #[test]
    fn search_enables_the_default_connectors_and_selection_narrows_them() {
        let all = validate(StartRunRequest {
            allow_network: true,
            search: true,
            ..request("q")
        })
        .expect("valid");
        assert_eq!(
            all.connector_tools,
            vec!["search:openalex", "search:crossref", "search:arxiv"]
        );

        let narrowed = validate(StartRunRequest {
            allow_network: true,
            search: true,
            search_connectors: Some(vec!["ArXiv".into(), "arxiv".into()]),
            ..request("q")
        })
        .expect("valid");
        assert_eq!(
            narrowed.connector_tools,
            vec!["search:arxiv"],
            "names are case-insensitive and deduplicated"
        );

        for bad in [vec!["scopus".to_string()], vec![], vec!["  ".to_string()]] {
            assert!(
                validate(StartRunRequest {
                    allow_network: true,
                    search: true,
                    search_connectors: Some(bad.clone()),
                    ..request("q")
                })
                .is_err(),
                "must reject {bad:?}"
            );
        }
    }

    #[test]
    fn web_permissions_never_grant_execution_or_approval_lanes() {
        let v = validate(StartRunRequest {
            allow_network: true,
            search: true,
            ..request("q")
        })
        .expect("valid");
        let p = permissions_for(&v, vec![".uruk/uploads/d/upl_1".into()]);
        assert!(!p.execute);
        assert!(p.write_paths.is_empty());
        assert!(p.require_approval_for.is_empty());
        assert!(p.disclose_to_provider);
        assert_eq!(p.allowed_tools, v.connector_tools);
        assert_eq!(p.read_paths, vec![".uruk/uploads/d/upl_1".to_string()]);
    }
}
