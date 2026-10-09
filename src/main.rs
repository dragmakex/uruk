//! The `uruk` command-line interface (SPEC §11).
//!
//! Every command supports `--json` for machine consumption, so an outer agent
//! can drive Uruk as a tool rather than scraping human-facing text.

use clap::{Parser, Subcommand};
use time::OffsetDateTime;
use uruk::agents::supervisor;
use uruk::records::*;
use uruk::report;
use uruk::runtime::{Scheduler, SchedulerConfig};
use uruk::store::{ProjectLock, STATE_DB_RELATIVE, Store};
use uruk::tools::ingest_input;
use uruk::{Error, Result, provider};

#[derive(Parser, Debug)]
#[command(
    name = "uruk",
    version,
    about = "An autonomous research engine whose output is evidence-backed findings"
)]
struct Cli {
    /// Emit machine-readable JSON instead of human-facing text.
    #[arg(long, global = true)]
    json: bool,

    /// Project directory holding `.uruk/state.sqlite` and `runs/`.
    #[arg(long, global = true, default_value = ".")]
    project: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start a new run.
    Run {
        #[arg(long)]
        goal: String,
        #[arg(long, value_parser = ["task", "campaign"], default_value = "task")]
        mode: String,
        /// Input file or URL. Repeatable.
        #[arg(long = "input")]
        inputs: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
        /// Requested deliverable. Repeatable.
        #[arg(long = "deliverable")]
        deliverables: Vec<String>,
        /// Desired property of a good answer. Repeatable.
        #[arg(long = "preference")]
        preferences: Vec<String>,
        /// Comparison axis for ranking. Repeatable.
        #[arg(long = "attribute")]
        attributes: Vec<String>,
        /// Requirement or limit. Repeatable.
        #[arg(long = "constraint")]
        constraints: Vec<String>,
        /// Permit local, contained execution of approved tools. Each
        /// execution still needs `uruk approve`.
        #[arg(long)]
        allow_execute: bool,
        /// Permit network retrieval of supplied URLs. This alone sends no
        /// goal-derived query anywhere; literature search needs `--search`.
        #[arg(long)]
        allow_network: bool,
        /// Opt in to federated literature search (OpenAlex, Crossref,
        /// arXiv). Transmits goal-derived queries to those operators, so it
        /// requires `--allow-network`.
        #[arg(long)]
        search: bool,
        /// Narrow the connector set after `--search` (csv of openalex,
        /// crossref, arxiv). Requires `--search`.
        #[arg(long = "search-connectors")]
        search_connectors: Option<String>,
        /// Open-access full-text acquisitions attempted per discovery.
        #[arg(long, default_value_t = 8)]
        max_acquisitions: u32,
        /// Approved tool name. Repeatable.
        #[arg(long = "tool")]
        tools: Vec<String>,
        #[arg(long, default_value_t = 200)]
        max_model_calls: u32,
        #[arg(long, default_value_t = 3600)]
        max_seconds: u64,
        #[arg(long, default_value_t = 10)]
        max_iterations: u32,
        /// Turn cap for simulated debates (never more than 10).
        #[arg(long, default_value_t = 5)]
        max_debate_turns: u32,
        /// Plan and persist the run without dispatching any work.
        #[arg(long)]
        dry_run: bool,
    },
    /// Resume an existing run.
    Resume {
        #[arg(long = "run-id")]
        run_id: String,
    },
    /// Show a run's current state.
    Status {
        #[arg(long = "run-id")]
        run_id: Option<String>,
    },
    /// Supply researcher feedback, a seed idea, or a review of an item.
    Feedback {
        #[arg(long = "run-id")]
        run_id: String,
        #[arg(long)]
        file: String,
        /// Record as a seed research item rather than a comment.
        #[arg(long)]
        as_item: bool,
        /// Record as the researcher's review of this item.
        #[arg(long)]
        item: Option<String>,
    },
    /// Approve a pending request.
    Approve {
        #[arg(long = "run-id")]
        run_id: String,
        #[arg(long = "request-id")]
        request_id: String,
        /// The payload hash the approval binds to.
        #[arg(long)]
        payload_hash: Option<String>,
    },
    /// Deny a pending request.
    Deny {
        #[arg(long = "run-id")]
        run_id: String,
        #[arg(long = "request-id")]
        request_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Request that a run stop.
    Stop {
        #[arg(long = "run-id")]
        run_id: String,
    },
    /// Revise a run's goal, rubric, scope, or limits. A changed rubric starts
    /// a fresh tournament cohort; pending approvals are invalidated.
    Revise {
        #[arg(long = "run-id")]
        run_id: String,
        /// New research question.
        #[arg(long)]
        goal: Option<String>,
        #[arg(long = "deliverable")]
        deliverables: Vec<String>,
        #[arg(long = "preference")]
        preferences: Vec<String>,
        #[arg(long = "attribute")]
        attributes: Vec<String>,
        #[arg(long = "constraint")]
        constraints: Vec<String>,
        #[arg(long = "exclude")]
        exclusions: Vec<String>,
        #[arg(long = "assume")]
        assumptions: Vec<String>,
        #[arg(long)]
        max_model_calls: Option<u32>,
        #[arg(long)]
        max_iterations: Option<u32>,
        #[arg(long)]
        max_seconds: Option<u64>,
        /// Why the goal changed, recorded on the revision.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Export a run's report and records.
    Report {
        #[arg(long = "run-id")]
        run_id: String,
    },
    /// Query a run's local passage index (FTS5, offline).
    Passages {
        #[arg(long = "run-id")]
        run_id: String,
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// List the prompt templates and their provenance.
    Prompts,
    /// Serve the local web API (and the Next.js frontend's backend).
    ///
    /// SECURITY: there is no authentication. Keep the default loopback bind
    /// unless an authenticating reverse proxy fronts it (docs/WEB.md).
    #[cfg(feature = "web")]
    Serve {
        /// Address to bind, loopback by default.
        #[arg(long, default_value = "127.0.0.1:7913")]
        bind: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("URUK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("uruk=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let json = cli.json;
    load_dotenv(std::path::Path::new(&cli.project));

    match dispatch(cli).await {
        Ok(output) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output.data).unwrap_or_default()
                );
            } else {
                println!("{}", output.text);
            }
        }
        Err(e) => {
            if json {
                let body = serde_json::json!({
                    "ok": false,
                    "error": e.to_string(),
                    "kind": e.kind(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&body).unwrap_or_default()
                );
            } else {
                eprintln!("error: {e}");
            }
            std::process::exit(1);
        }
    }
}

/// A command's result in both forms.
struct Output {
    text: String,
    data: serde_json::Value,
}

async fn dispatch(cli: Cli) -> Result<Output> {
    let db_path = std::path::Path::new(&cli.project).join(STATE_DB_RELATIVE);

    match cli.command {
        Command::Prompts => {
            let registry = uruk::prompts::PromptRegistry::new();
            let mut rows = Vec::new();
            for id in registry.ids() {
                let t = registry.get(id)?;
                rows.push(serde_json::json!({
                    "id": t.id,
                    "hash": t.hash.0,
                    "published_adaptation": t.is_published_adaptation(),
                    "source_figure": t.source.map(|s| format!("{} p.{}", s.figure, s.page)),
                    "variables": t.required_vars,
                }));
            }
            let text = rows
                .iter()
                .map(|r| {
                    format!(
                        "{:<28} {} {}",
                        r["id"].as_str().unwrap_or(""),
                        &r["hash"].as_str().unwrap_or("")[..12],
                        r["source_figure"].as_str().unwrap_or("(Uruk-authored)")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Output {
                text,
                data: serde_json::json!({"ok": true, "prompts": rows}),
            })
        }

        Command::Run {
            goal,
            mode,
            inputs,
            profile,
            deliverables,
            preferences,
            attributes,
            constraints,
            allow_execute,
            allow_network,
            search,
            search_connectors,
            max_acquisitions,
            tools,
            max_model_calls,
            max_seconds,
            max_iterations,
            max_debate_turns,
            dry_run,
        } => {
            // Search is an explicit opt-in on top of the network permission:
            // --allow-network alone keeps today's URL-fetch-only behavior and
            // sends zero connector traffic.
            if search && !allow_network {
                return Err(Error::validation(
                    "--search requires --allow-network: literature search transmits \
                     goal-derived queries to external operators",
                ));
            }
            if search_connectors.is_some() && !search {
                return Err(Error::validation(
                    "--search-connectors narrows the connector set and requires --search",
                ));
            }

            // One scheduler owns a project at a time (SPEC §9.2).
            let _lock = ProjectLock::acquire(&cli.project)?;
            let store = Store::open(&db_path).await?;

            if allow_execute && uruk::tools::detect_containment().is_none() {
                return Err(Error::permission(
                    "--allow-execute needs OS-level containment (macOS sandbox-exec or Linux \
                     bubblewrap), which this host does not provide; execution is unavailable \
                     rather than uncontained (SPEC §9.3)",
                ));
            }

            let mode = if mode == "campaign" {
                Mode::Campaign
            } else {
                Mode::Task
            };
            let run_id = RunId::new();
            let goal_id = GoalId::new();
            // Identity shared with the web API, derived from the canonical
            // project directory rather than the raw --project spelling.
            let (project_id, project_name) = store.project_identity();

            // Reading a supplied input is implied by supplying it; nothing
            // else is granted without an explicit flag (SPEC §12).
            let read_paths: Vec<String> = inputs
                .iter()
                .filter(|i| !i.starts_with("http"))
                .filter_map(|i| {
                    std::path::Path::new(i)
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned())
                })
                .collect();

            // `--search` enables the three MVP connectors through the
            // allowlist; `--search-connectors` only narrows that set.
            let mut allowed_tools = tools;
            if search {
                let connector_tools = match &search_connectors {
                    Some(value) => {
                        uruk::search::parse_connector_flag(value).map_err(Error::validation)?
                    }
                    None => uruk::search::DEFAULT_CONNECTOR_TOOLS
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                };
                allowed_tools.extend(connector_tools);
            }

            let permissions = Permissions {
                read_paths,
                write_paths: vec![],
                network: allow_network,
                execute: allow_execute,
                allowed_tools,
                disclose_to_provider: true,
                require_approval_for: if allow_execute {
                    vec!["execute".to_string()]
                } else {
                    vec![]
                },
            };

            let goal_record = Goal {
                id: goal_id.clone(),
                schema_version: SCHEMA_VERSION,
                run_id: run_id.clone(),
                revision: 0,
                question: goal.clone(),
                mode,
                deliverables: if deliverables.is_empty() {
                    vec!["research report".to_string()]
                } else {
                    deliverables
                },
                inputs: inputs
                    .iter()
                    .map(|i| InputRef {
                        locator: i.clone(),
                        note: None,
                    })
                    .collect(),
                scope: None,
                exclusions: vec![],
                known_facts: vec![],
                assumptions: vec![],
                rubric: Rubric {
                    preferences,
                    attributes,
                    constraints,
                    acceptance: vec![],
                },
                permissions: permissions.clone(),
                budget: Budget {
                    wall_clock_secs: max_seconds,
                    max_model_calls,
                    max_iterations,
                    max_debate_turns: max_debate_turns.clamp(1, 10),
                    max_acquisitions,
                    ..Default::default()
                },
                profile,
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
                paused_at: None,
                paused_ms: 0,
                created_at: OffsetDateTime::now_utc(),
                updated_at: OffsetDateTime::now_utc(),
            };

            store.ensure_project(&project_id, &project_name).await?;
            store.create_run(&run, &goal_record).await?;

            // Check the goal itself before any work is scheduled (SPEC §12).
            let verdict = uruk::agents::safety::check(&goal_record.question);
            if let Some(decision) =
                uruk::agents::safety::safety_decision(&run_id, &verdict, "research goal", vec![])
            {
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

            // Ingest supplied sources.
            let mut ingested = Vec::new();
            let mut ingest_errors = Vec::new();
            for input in &goal_record.inputs {
                match ingest_input(&store, &run_id, input, &permissions).await {
                    Ok(result) => ingested.push(serde_json::json!({
                        "source_id": result.source.id.0,
                        "locator": input.locator,
                        "access": result.source.access.as_str(),
                        "content_hash": result.source.content_hash.0,
                    })),
                    Err(e) => ingest_errors.push(serde_json::json!({
                        "locator": input.locator,
                        "error": e.to_string(),
                    })),
                }
            }

            let plan = supervisor::build_plan(&goal_record, 0);
            store.insert_plan(&plan).await?;

            if dry_run {
                return Ok(Output {
                    text: format!(
                        "Planned run {run_id} ({} mode, roles: {}). \
                         {} source(s) ingested, {} failed. No work dispatched (--dry-run).",
                        mode.as_str(),
                        plan.roles.join(", "),
                        ingested.len(),
                        ingest_errors.len()
                    ),
                    data: serde_json::json!({
                        "ok": true,
                        "run_id": run_id.0,
                        "plan": {"id": plan.id.0, "roles": plan.roles, "rationale": plan.rationale},
                        "sources": ingested,
                        "ingest_errors": ingest_errors,
                        "dispatched": false,
                    }),
                });
            }

            let scheduler = build_scheduler(store.clone())?;
            install_signal_handler(scheduler.cancellation_token());

            let summary = scheduler.run(&run_id).await?;
            let export = report::export_run(&store, &run_id).await?;

            Ok(Output {
                text: format!(
                    "Run {} finished: {}{}.\n{} item(s), {} review(s), {} match(es).{}\n\
                     Report: {}",
                    summary.run_id,
                    summary.state.as_str(),
                    match &summary.stop_condition {
                        Some(c) => format!(" ({c})"),
                        None => String::new(),
                    },
                    summary.items,
                    summary.reviews,
                    summary.matches,
                    match &summary.final_output_skipped {
                        Some(r) => format!("\nFinal deliverable skipped: {r}."),
                        None => String::new(),
                    },
                    export.dir.join("REPORT.md").display()
                ),
                data: serde_json::json!({
                    "ok": true,
                    "run": summary,
                    "sources": ingested,
                    "ingest_errors": ingest_errors,
                    "export": export,
                }),
            })
        }

        Command::Resume { run_id } => {
            let _lock = ProjectLock::acquire(&cli.project)?;
            let store = Store::open(&db_path).await?;
            let run_id = RunId::from_raw(run_id);

            let scheduler = build_scheduler(store.clone())?;
            install_signal_handler(scheduler.cancellation_token());

            let summary = scheduler.run(&run_id).await?;
            let export = report::export_run(&store, &run_id).await?;

            Ok(Output {
                text: format!(
                    "Resumed run {} → {}. Report: {}",
                    summary.run_id,
                    summary.state.as_str(),
                    export.dir.join("REPORT.md").display()
                ),
                data: serde_json::json!({"ok": true, "run": summary, "export": export}),
            })
        }

        // Inspection does not take the scheduler lock, so `status` works while
        // a run is in progress (SPEC §9.2).
        Command::Status { run_id } => {
            let store = Store::open(&db_path).await?;

            let run_id = match run_id {
                Some(id) => RunId::from_raw(id),
                None => store
                    .list_runs()
                    .await?
                    .first()
                    .map(|(id, _, _)| id.clone())
                    .ok_or_else(|| Error::not_found("no runs in this project"))?,
            };

            let run = store.get_run(&run_id).await?;
            let goal = store.get_goal(&run.goal_id).await?;
            let plan = store.current_plan(&run_id).await?;
            let usage = store.budget_usage(&run_id).await?;
            let tasks = store.list_tasks(&run_id).await?;
            let pending = store
                .list_approvals(&run_id, Some(ApprovalState::Pending))
                .await?;
            let summaries = store
                .item_summaries(&run_id, plan.as_ref().map(|p| &p.id))
                .await?;

            let counts = |state: TaskState| tasks.iter().filter(|t| t.state == state).count();

            let searches = store.list_search_records(&run_id).await?;
            let works = store.list_works(&run_id).await?;
            let search_line = if searches.is_empty() {
                String::new()
            } else {
                use std::collections::BTreeSet;
                let queries: BTreeSet<&str> = searches.iter().map(|s| s.query.as_str()).collect();
                let connectors: BTreeSet<&str> = searches.iter().map(|s| s.tool.as_str()).collect();
                format!(
                    "Searches: {} queries, {} connectors, {} works, {} acquired\n",
                    queries.len(),
                    connectors.len(),
                    works.len(),
                    works.iter().filter(|w| w.source_id.is_some()).count(),
                )
            };

            let mut text = format!(
                "Run {} — {}{}\nGoal (rev {}): {}\n\n\
                 Tasks: {} completed, {} running, {} pending, {} waiting for you, {} failed, {} uncertain\n\
                 Usage: {} model call(s), {} token(s), cost {}\n{search_line}",
                run.id,
                run.state.as_str(),
                match &run.stop_condition {
                    Some(c) => format!(" ({})", c.as_str()),
                    None => String::new(),
                },
                goal.revision,
                goal.question,
                counts(TaskState::Completed),
                counts(TaskState::Running),
                counts(TaskState::Pending),
                counts(TaskState::WaitingForHuman),
                counts(TaskState::Failed),
                counts(TaskState::Uncertain),
                usage.model_calls,
                usage.tokens,
                match usage.cost_usd {
                    Some(c) => format!("${c:.4}"),
                    None => "unknown (not zero)".to_string(),
                },
            );

            if !pending.is_empty() {
                text.push_str("\nAwaiting your decision:\n");
                for req in &pending {
                    text.push_str(&format!(
                        "  {} — {} (payload {})\n",
                        req.id,
                        req.action,
                        req.payload_hash.short()
                    ));
                }
            }

            if !summaries.is_empty() {
                text.push_str("\nCandidates:\n");
                for s in summaries.iter().filter(|s| s.kind.is_rankable()).take(20) {
                    text.push_str(&format!(
                        "  {} {} [{}]{}\n",
                        s.id,
                        s.title,
                        s.assessment.as_str(),
                        match s.rating {
                            Some(r) => format!(
                                " Elo {r:.0}/{} match(es){}",
                                s.matches_played,
                                if s.stale_rating { " STALE" } else { "" }
                            ),
                            None => String::new(),
                        }
                    ));
                }
            }

            Ok(Output {
                text,
                data: serde_json::json!({
                    "ok": true,
                    "run_id": run.id.0,
                    "state": run.state.as_str(),
                    "stop_condition": run.stop_condition.as_ref().map(|s| s.as_str()),
                    "goal": {"revision": goal.revision, "question": goal.question,
                             "mode": goal.mode.as_str()},
                    "iterations": run.iterations,
                    "tasks": {
                        "completed": counts(TaskState::Completed),
                        "running": counts(TaskState::Running),
                        "pending": counts(TaskState::Pending),
                        "waiting_for_human": counts(TaskState::WaitingForHuman),
                        "failed": counts(TaskState::Failed),
                        "uncertain": counts(TaskState::Uncertain),
                    },
                    "usage": usage,
                    "searches": {
                        "records": searches.len(),
                        "works": works.len(),
                        "acquired": works.iter().filter(|w| w.source_id.is_some()).count(),
                    },
                    "pending_approvals": pending.iter().map(|r| serde_json::json!({
                        "request_id": r.id.0,
                        "action": r.action,
                        "payload_hash": r.payload_hash.0,
                        "payload": r.payload,
                    })).collect::<Vec<_>>(),
                    "items": summaries,
                }),
            })
        }

        Command::Feedback {
            run_id,
            file,
            as_item,
            item,
        } => {
            let store = Store::open(&db_path).await?;
            let run_id = RunId::from_raw(run_id);
            let content = tokio::fs::read_to_string(&file).await?;

            if content.trim().is_empty() {
                return Err(Error::validation("feedback file is empty"));
            }

            // Persist researcher input before acting on it (SPEC §6).
            let goal = store.current_goal(&run_id).await?;
            let mut refs = Vec::new();

            if as_item {
                let title = content
                    .lines()
                    .next()
                    .unwrap_or("researcher idea")
                    .to_string();
                // A free-text idea carries no structured hypothesis fields, so
                // it is a proposal (SPEC §4.2).
                let item = ResearchItem {
                    id: ItemId::new(),
                    schema_version: SCHEMA_VERSION,
                    run_id: run_id.clone(),
                    kind: ItemKind::Proposal,
                    title,
                    content_hash: ContentHash::of_str(&content),
                    content: content.clone(),
                    hypothesis: None,
                    parent_ids: vec![],
                    derivation: None,
                    source_ids: vec![],
                    author: Author::Researcher,
                    produced_by: None,
                    goal_id: goal.id.clone(),
                    created_at: OffsetDateTime::now_utc(),
                };
                store.insert_item(&item).await?;
                refs.push(item.id.0.clone());
            }

            if let Some(item_id) = &item {
                // A researcher review is accepted with attribution; authority
                // to steer does not make an empirical assertion true (SPEC §5).
                let target = store.get_item(&ItemId::from_raw(item_id.clone())).await?;
                let review = Review {
                    id: ReviewId::new(),
                    schema_version: SCHEMA_VERSION,
                    run_id: run_id.clone(),
                    item_id: target.id.clone(),
                    item_hash: target.content_hash.clone(),
                    strategy: ReviewStrategy::Researcher,
                    assessment_text: content.clone(),
                    proposed_assessment: Assessment::Untested,
                    evidence: vec![],
                    objections: vec![],
                    unknowns: vec![],
                    next_actions: vec![],
                    execution_requests: vec![],
                    observations: vec![],
                    explanatory_label: None,
                    score: None,
                    author: Author::Researcher,
                    produced_by: None,
                    created_at: OffsetDateTime::now_utc(),
                };
                store.insert_review(&review).await?;
                refs.push(review.id.0.clone());
            }

            let decision = Decision {
                id: DecisionId::new(),
                schema_version: SCHEMA_VERSION,
                run_id: run_id.clone(),
                kind: DecisionKind::HumanFeedback,
                actor: Actor::Researcher,
                reason: format!("researcher feedback from {file}"),
                referenced: refs.clone(),
                payload: serde_json::json!({"content": content, "as_item": as_item, "item": item}),
                created_at: OffsetDateTime::now_utc(),
            };
            store.insert_decision(&decision).await?;

            Ok(Output {
                text: format!(
                    "Recorded researcher feedback as {}{}. It is supplied to later tasks.",
                    decision.id,
                    if refs.is_empty() {
                        String::new()
                    } else {
                        format!(" with records {}", refs.join(", "))
                    }
                ),
                data: serde_json::json!({
                    "ok": true,
                    "decision_id": decision.id.0,
                    "record_ids": refs,
                }),
            })
        }

        Command::Approve {
            run_id,
            request_id,
            payload_hash,
        } => {
            let store = Store::open(&db_path).await?;
            let run_id = RunId::from_raw(run_id);
            let request_id = RequestId::from_raw(request_id);
            check_request_run(&store, &request_id, &run_id).await?;
            let expected = payload_hash.map(ContentHash);

            let decided = store
                .decide_approval(&request_id, true, expected.as_ref(), None)
                .await?;

            Ok(Output {
                text: format!(
                    "Approved {} ({}). Run `uruk resume --run-id {}` to continue.",
                    decided.id, decided.action, run_id
                ),
                data: serde_json::json!({
                    "ok": true,
                    "request_id": decided.id.0,
                    "state": decided.state.as_str(),
                }),
            })
        }

        Command::Deny {
            run_id,
            request_id,
            reason,
        } => {
            let store = Store::open(&db_path).await?;
            let run_id = RunId::from_raw(run_id);
            let request_id = RequestId::from_raw(request_id);
            check_request_run(&store, &request_id, &run_id).await?;

            let decided = store
                .decide_approval(&request_id, false, None, reason)
                .await?;

            Ok(Output {
                text: format!("Denied {} ({}).", decided.id, decided.action),
                data: serde_json::json!({
                    "ok": true,
                    "request_id": decided.id.0,
                    "state": decided.state.as_str(),
                }),
            })
        }

        Command::Stop { run_id } => {
            let store = Store::open(&db_path).await?;
            let run_id = RunId::from_raw(run_id);

            // A durable control request: the owning scheduler observes it,
            // cancels in-flight work, and a stopped run stays stopped across
            // restarts (SPEC §9.2). Shared with the web API's stop route.
            let outcome = uruk::runtime::request_stop(&store, &run_id).await?;

            Ok(Output {
                text: format!(
                    "Stop requested for {run_id}; {} pending task(s) cancelled. \
                     A running scheduler will observe this within a second.",
                    outcome.cancelled_tasks
                ),
                data: serde_json::json!({
                    "ok": true,
                    "run_id": run_id.0,
                    "cancelled_tasks": outcome.cancelled_tasks,
                }),
            })
        }

        Command::Revise {
            run_id,
            goal,
            deliverables,
            preferences,
            attributes,
            constraints,
            exclusions,
            assumptions,
            max_model_calls,
            max_iterations,
            max_seconds,
            reason,
        } => {
            let store = Store::open(&db_path).await?;
            let run_id = RunId::from_raw(run_id);
            let revised = supervisor::revise(
                &store,
                &run_id,
                supervisor::GoalChanges {
                    question: goal,
                    deliverables,
                    preferences,
                    attributes,
                    constraints,
                    exclusions,
                    assumptions,
                    max_model_calls,
                    max_iterations,
                    wall_clock_secs: max_seconds,
                    reason,
                },
            )
            .await?;

            Ok(Output {
                text: format!(
                    "Goal revised to revision {} (plan {}, revision {}).{}{} \
                     Run `uruk resume --run-id {run_id}` to continue under it.",
                    revised.goal.revision,
                    revised.plan.id,
                    revised.plan.revision,
                    if revised.rubric_changed {
                        " The rubric changed: comparisons start a fresh cohort."
                    } else {
                        ""
                    },
                    if revised.invalidated_approvals > 0 {
                        format!(
                            " {} pending approval(s) invalidated; changed requests need new approvals.",
                            revised.invalidated_approvals
                        )
                    } else {
                        String::new()
                    },
                ),
                data: serde_json::json!({
                    "ok": true,
                    "goal_id": revised.goal.id.0,
                    "goal_revision": revised.goal.revision,
                    "plan_id": revised.plan.id.0,
                    "plan_revision": revised.plan.revision,
                    "rubric_changed": revised.rubric_changed,
                    "invalidated_approvals": revised.invalidated_approvals,
                    "decision_id": revised.decision.id.0,
                }),
            })
        }

        Command::Passages {
            run_id,
            query,
            limit,
        } => {
            // Purely local FTS5 query; no network, no model call.
            let store = Store::open(&db_path).await?;
            let run_id = RunId::from_raw(run_id);
            let hits = store.search_passages(&run_id, &query, limit).await?;
            let sources = store.list_sources(&run_id).await?;

            let mut text = if hits.is_empty() {
                format!("No passages match {query:?} in run {run_id}.")
            } else {
                format!("{} passage(s) for {query:?}:\n", hits.len())
            };
            for (i, hit) in hits.iter().enumerate() {
                let (citation, access) = sources
                    .iter()
                    .find(|s| s.id == hit.source_id)
                    .map(|s| (s.short_citation(), s.access.as_str()))
                    .unwrap_or_else(|| (hit.source_id.as_str().to_string(), "unknown"));
                text.push_str(&format!(
                    "{rank:>3}. [{score:.3}] {citation} ({id}, access: {access}, chars {start}..{end})\n     {snippet}\n",
                    rank = i + 1,
                    score = hit.score,
                    id = hit.source_id,
                    start = hit.byte_start,
                    end = hit.byte_end,
                    snippet = hit.snippet.replace('\n', " "),
                ));
            }

            Ok(Output {
                text,
                data: serde_json::json!({
                    "ok": true,
                    "run_id": run_id.0,
                    "query": query,
                    "passages": hits,
                }),
            })
        }

        #[cfg(feature = "web")]
        Command::Serve { bind } => {
            let bind: std::net::SocketAddr = bind
                .parse()
                .map_err(|e| Error::validation(format!("bad --bind address {bind:?}: {e}")))?;
            // Strictly validated so a typo fails startup instead of
            // silently weakening the browser-identity cookie.
            let cookie_secure = uruk::web::cookie_secure_from_env(
                std::env::var("URUK_COOKIE_SECURE").ok().as_deref(),
            )?;
            uruk::web::serve(uruk::web::ServeOptions {
                bind,
                project: std::path::PathBuf::from(&cli.project),
                config: uruk::web::WebConfig {
                    cookie_secure,
                    ..uruk::web::WebConfig::default()
                },
            })
            .await?;
            Ok(Output {
                text: "web API stopped".into(),
                data: serde_json::json!({"ok": true}),
            })
        }

        Command::Report { run_id } => {
            let store = Store::open(&db_path).await?;
            let run_id = RunId::from_raw(run_id);
            let export = report::export_run(&store, &run_id).await?;

            Ok(Output {
                text: format!(
                    "Exported {} file(s) to {}{}",
                    export.files.len(),
                    export.dir.display(),
                    if export.partial {
                        " (partial result)"
                    } else {
                        ""
                    }
                ),
                data: serde_json::json!({"ok": true, "export": export}),
            })
        }
    }
}

/// An approval is scoped to its run; deciding one under another run's ID is
/// refused.
async fn check_request_run(store: &Store, request_id: &RequestId, run_id: &RunId) -> Result<()> {
    let req = store.get_approval(request_id).await?;
    if &req.run_id != run_id {
        return Err(Error::validation(format!(
            "approval {request_id} belongs to run {}, not {run_id}",
            req.run_id
        )));
    }
    Ok(())
}

/// Choose the model provider from the environment (SPEC §13: model selection
/// stays outside research policy).
///
/// Apply `<project>/.env` to the process environment. Variables already set
/// in the real environment win, so a shell override still works. A missing
/// file is not an error.
fn load_dotenv(project: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(project.join(".env")) else {
        return;
    };
    for (key, value) in parse_dotenv(&text) {
        if std::env::var_os(&key).is_none() {
            // SAFETY: called once at startup, before any other thread reads the
            // environment.
            unsafe { std::env::set_var(&key, &value) };
        }
    }
}

/// `KEY=value` lines; blanks, `#` comments, an `export ` prefix and one pair
/// of surrounding quotes are tolerated. Anything else is skipped.
fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let bad = key.is_empty() || key.starts_with('#') || key.contains(char::is_whitespace);
            if bad {
                return None;
            }
            let value = value.trim();
            let value = match value.as_bytes() {
                [q @ (b'"' | b'\''), .., last] if last == q => &value[1..value.len() - 1],
                _ => value,
            };
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

fn build_scheduler(store: Store) -> Result<Scheduler> {
    Ok(Scheduler::new(
        store,
        provider::from_env_or_offline()?,
        SchedulerConfig::default(),
    ))
}

/// Ctrl-C and SIGTERM request cancellation rather than killing the process,
/// so completed evidence is preserved and a partial report can still be
/// written (SPEC §6).
fn install_signal_handler(cancel: tokio_util::sync::CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    cancel.cancel();
                    return;
                }
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        tracing::warn!("cancellation requested; finishing in-flight work");
        cancel.cancel();
    });
}

#[cfg(test)]
mod tests {
    use super::parse_dotenv;

    #[test]
    fn dotenv_lines_parse_and_junk_is_skipped() {
        let text = "# comment\n\nexport URUK_MODEL=qwen3.5:9b\nURUK_PROVIDER_URL = \"http://localhost:11434/v1\"\nKEY='x=y'\nnot a pair\n=novalue\n";
        assert_eq!(
            parse_dotenv(text),
            vec![
                ("URUK_MODEL".into(), "qwen3.5:9b".into()),
                (
                    "URUK_PROVIDER_URL".into(),
                    "http://localhost:11434/v1".into()
                ),
                ("KEY".into(), "x=y".into()),
            ]
        );
    }
}
