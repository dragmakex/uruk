//! Build `manifest.json`, `REPORT.md`, `research.jsonl`, and `sources.jsonl`.

use crate::Result;
use crate::agents::outputs::{ClaimBasis, SynthesisOutput};
use crate::records::*;
use crate::store::{Store, write_atomic};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use time::OffsetDateTime;

/// What was written.
#[derive(Debug, Clone, Serialize)]
pub struct Export {
    pub dir: PathBuf,
    pub files: Vec<String>,
    /// True when the run did not reach a normal completion.
    pub partial: bool,
}

/// Run identity, versions, permissions, state, and artifact hashes (SPEC §11).
#[derive(Debug, Serialize)]
struct Manifest {
    run_id: String,
    project_id: String,
    schema_version: u32,
    uruk_version: String,
    state: String,
    stop_condition: Option<String>,
    partial: bool,
    /// Why the final deliverable was not produced, when it was not.
    final_output_skipped: Option<String>,
    goal: GoalManifest,
    plan: Option<PlanManifest>,
    permissions: Permissions,
    budget: Budget,
    /// Provider adapters and models that served this run's tasks.
    providers: Vec<String>,
    models: Vec<String>,
    usage: UsageManifest,
    counts: Counts,
    artifacts: Vec<ArtifactManifest>,
    prompts: Vec<PromptManifest>,
    exported_at: String,
}

#[derive(Debug, Serialize)]
struct GoalManifest {
    id: String,
    revision: u32,
    question: String,
    mode: String,
    deliverables: Vec<String>,
    assumptions: Vec<String>,
    input_hashes: Vec<InputHash>,
}

#[derive(Debug, Serialize)]
struct InputHash {
    locator: String,
    content_hash: String,
    access: String,
}

#[derive(Debug, Serialize)]
struct PlanManifest {
    id: String,
    revision: u32,
    roles: Vec<String>,
    rationale: String,
    stopping_conditions: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UsageManifest {
    model_calls: u32,
    tokens: u64,
    tool_executions: u32,
    /// `null` means unknown, which is not the same as zero (SPEC §6).
    cost_usd: Option<f64>,
    cost_note: String,
}

#[derive(Debug, Serialize)]
struct Counts {
    items: u32,
    sources: u32,
    reviews: u32,
    evidence: u32,
    matches: u32,
    decisions: u32,
    experiments: u32,
    clusters: u32,
    tasks_completed: u32,
    tasks_failed: u32,
    tasks_uncertain: u32,
}

#[derive(Debug, Serialize)]
struct ArtifactManifest {
    id: String,
    label: Option<String>,
    media_type: String,
    content_hash: String,
    size_bytes: u64,
    path: String,
}

#[derive(Debug, Serialize)]
struct PromptManifest {
    id: String,
    hash: String,
    provenance: String,
}

/// A line in `research.jsonl`.
#[derive(Debug, Serialize)]
#[serde(tag = "record")]
enum ResearchLine {
    Item {
        #[serde(flatten)]
        item: ResearchItem,
        assessment: String,
        disposition: String,
        rating: Option<f64>,
        matches_played: u32,
        stale_rating: bool,
    },
    Review(Review),
    Evidence {
        #[serde(flatten)]
        evidence: Evidence,
        links: Vec<EvidenceLink>,
    },
    Match(Match),
    Decision(Decision),
    Experiment(Experiment),
    Cluster(Cluster),
    Feedback(Feedback),
}

/// Export a run's applicable artifacts under `<project>/runs/<run-id>/`.
///
/// Only applicable files are written: empty tournament files are not required
/// for standalone work (SPEC §11).
pub async fn export_run(store: &Store, run_id: &RunId) -> Result<Export> {
    let run = store.get_run(run_id).await?;
    let goal = store.get_goal(&run.goal_id).await?;
    let plan = store.current_plan(run_id).await?;
    let dir = store.run_dir(run_id);
    tokio::fs::create_dir_all(&dir).await?;

    let partial = !matches!(run.state, RunState::Completed);
    let mut files = Vec::new();

    // --- sources.jsonl ---
    let sources = store.list_sources(run_id).await?;
    if !sources.is_empty() {
        let mut out = String::new();
        for s in &sources {
            out.push_str(&serde_json::to_string(s)?);
            out.push('\n');
        }
        for search in store.list_search_records(run_id).await? {
            out.push_str(&serde_json::to_string(&serde_json::json!({
                "record": "search", "search": search
            }))?);
            out.push('\n');
        }
        write_atomic(&dir.join("sources.jsonl"), out.as_bytes()).await?;
        files.push("sources.jsonl".into());
    }

    // --- research.jsonl ---
    let items = store.list_items(run_id).await?;
    let reviews = store.list_reviews(run_id).await?;
    let evidence = store.list_evidence(run_id).await?;
    let matches = store.list_matches(run_id).await?;
    let decisions = store.list_decisions(run_id).await?;
    let experiments = store.list_experiments(run_id).await?;
    let clusters = store.list_clusters(run_id).await?;

    let mut lines: Vec<ResearchLine> = Vec::new();
    for item in &items {
        let rating = match &plan {
            Some(p) => store.get_rating(&item.id, &p.id).await?,
            None => None,
        };
        lines.push(ResearchLine::Item {
            assessment: store
                .current_assessment(&item.id)
                .await?
                .as_str()
                .to_string(),
            disposition: store.get_disposition(&item.id).await?.as_str().to_string(),
            rating: rating.as_ref().map(|r| r.rating),
            matches_played: rating.as_ref().map(|r| r.matches_played).unwrap_or(0),
            stale_rating: rating.as_ref().map(|r| r.stale).unwrap_or(false),
            item: item.clone(),
        });
    }
    for r in &reviews {
        lines.push(ResearchLine::Review(r.clone()));
    }
    for e in &evidence {
        lines.push(ResearchLine::Evidence {
            links: store.links_for_evidence(&e.id).await?,
            evidence: e.clone(),
        });
    }
    for m in &matches {
        lines.push(ResearchLine::Match(m.clone()));
    }
    for d in &decisions {
        lines.push(ResearchLine::Decision(d.clone()));
    }
    for e in &experiments {
        lines.push(ResearchLine::Experiment(e.clone()));
    }
    for c in &clusters {
        lines.push(ResearchLine::Cluster(c.clone()));
    }
    if let Some(f) = store.latest_feedback(run_id).await? {
        lines.push(ResearchLine::Feedback(f));
    }

    if !lines.is_empty() {
        let mut out = String::new();
        for line in &lines {
            out.push_str(&serde_json::to_string(line)?);
            out.push('\n');
        }
        write_atomic(&dir.join("research.jsonl"), out.as_bytes()).await?;
        files.push("research.jsonl".into());
    }

    // --- tournament.jsonl (campaigns with matches only) ---
    if !matches.is_empty() {
        let mut out = String::new();
        for m in &matches {
            out.push_str(&serde_json::to_string(m)?);
            out.push('\n');
        }
        if let Some(p) = &plan {
            for rating in store.leaderboard(run_id, &p.id).await? {
                out.push_str(&serde_json::to_string(&serde_json::json!({
                    "record": "rating", "rating": rating
                }))?);
                out.push('\n');
            }
        }
        write_atomic(&dir.join("tournament.jsonl"), out.as_bytes()).await?;
        files.push("tournament.jsonl".into());
    }

    // --- manifest.json ---
    let usage = store.budget_usage(run_id).await?;
    let tasks = store.list_tasks(run_id).await?;
    let artifacts = store.list_artifacts(run_id).await?;
    let registry = crate::prompts::PromptRegistry::new();

    let final_output_skipped = decisions.iter().rev().find_map(|d| {
        d.payload["final_output_skipped"]
            .as_str()
            .map(str::to_string)
    });

    let providers: Vec<String> = tasks
        .iter()
        .filter_map(|t| t.provider.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let models: Vec<String> = tasks
        .iter()
        .filter_map(|t| t.model.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let manifest = Manifest {
        run_id: run.id.0.clone(),
        project_id: run.project_id.0.clone(),
        schema_version: SCHEMA_VERSION,
        uruk_version: env!("CARGO_PKG_VERSION").to_string(),
        state: run.state.as_str().to_string(),
        stop_condition: run.stop_condition.as_ref().map(|s| s.as_str().to_string()),
        partial,
        final_output_skipped,
        goal: GoalManifest {
            id: goal.id.0.clone(),
            revision: goal.revision,
            question: goal.question.clone(),
            mode: goal.mode.as_str().to_string(),
            deliverables: goal.deliverables.clone(),
            assumptions: goal.assumptions.clone(),
            input_hashes: sources
                .iter()
                .map(|s| InputHash {
                    locator: match &s.origin {
                        Origin::LocalFile(p) => p.clone(),
                        Origin::Url(u) => u.clone(),
                        Origin::Dataset(d) => d.clone(),
                        Origin::Code(c) => c.clone(),
                        Origin::Supplied => "(supplied inline)".into(),
                    },
                    content_hash: s.content_hash.0.clone(),
                    access: s.access.as_str().to_string(),
                })
                .collect(),
        },
        plan: plan.as_ref().map(|p| PlanManifest {
            id: p.id.0.clone(),
            revision: p.revision,
            roles: p.roles.clone(),
            rationale: p.rationale.clone(),
            stopping_conditions: p
                .stopping_conditions
                .iter()
                .map(|s| s.as_str().to_string())
                .collect(),
        }),
        permissions: goal.permissions.clone(),
        budget: goal.budget.clone(),
        providers,
        models,
        usage: UsageManifest {
            model_calls: usage.model_calls,
            tokens: usage.tokens,
            tool_executions: usage.tool_executions,
            cost_usd: usage.cost_usd,
            cost_note: if usage.cost_usd.is_none() {
                "the provider did not report cost; this is unknown, not zero".into()
            } else if usage.cost_partially_unknown {
                "partial: some calls reported no cost".into()
            } else {
                "reported by the provider".into()
            },
        },
        counts: Counts {
            items: items.len() as u32,
            sources: sources.len() as u32,
            reviews: reviews.len() as u32,
            evidence: evidence.len() as u32,
            matches: matches.len() as u32,
            decisions: decisions.len() as u32,
            experiments: experiments.len() as u32,
            clusters: clusters.len() as u32,
            tasks_completed: tasks
                .iter()
                .filter(|t| t.state == TaskState::Completed)
                .count() as u32,
            tasks_failed: tasks
                .iter()
                .filter(|t| t.state == TaskState::Failed)
                .count() as u32,
            tasks_uncertain: tasks
                .iter()
                .filter(|t| t.state == TaskState::Uncertain)
                .count() as u32,
        },
        artifacts: artifacts
            .iter()
            .map(|a| ArtifactManifest {
                id: a.id.0.clone(),
                label: a.label.clone(),
                media_type: a.media_type.clone(),
                content_hash: a.content_hash.0.clone(),
                size_bytes: a.size_bytes,
                path: a.storage_path.clone(),
            })
            .collect(),
        prompts: registry
            .ids()
            .filter_map(|id| registry.get(id).ok())
            .map(|t| PromptManifest {
                id: t.id.to_string(),
                hash: t.hash.0.clone(),
                provenance: t.provenance(),
            })
            .collect(),
        exported_at: OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    };

    write_atomic(
        &dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
    )
    .await?;
    files.push("manifest.json".into());

    // --- REPORT.md ---
    let report = build_report(store, run_id, &run, &goal, &plan, &manifest, &artifacts).await?;
    write_atomic(&dir.join("REPORT.md"), report.as_bytes()).await?;
    files.push("REPORT.md".into());

    Ok(Export {
        dir,
        files,
        partial,
    })
}

/// The structured deliverable, if the run produced one.
async fn load_deliverable(store: &Store, artifacts: &[Artifact]) -> Option<SynthesisOutput> {
    let artifact = artifacts
        .iter()
        .rev()
        .find(|a| a.label.as_deref() == Some("deliverable with claim provenance"))?;
    let bytes = tokio::fs::read(store.resolve_path(&artifact.storage_path))
        .await
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Build the human-facing report (SPEC §11's five required distinctions).
async fn build_report(
    store: &Store,
    run_id: &RunId,
    run: &Run,
    goal: &Goal,
    plan: &Option<Plan>,
    manifest: &Manifest,
    artifacts: &[Artifact],
) -> Result<String> {
    let tasks = store.list_tasks(run_id).await?;
    let tasks = tasks.as_slice();
    let mut s = String::new();
    let partial = manifest.partial;

    s.push_str(&format!("# {}\n\n", goal.question));

    if partial {
        s.push_str(&format!(
            "> **Partial result.** This run ended in state `{}`{}. \
             The findings below cover only the work that actually completed.\n\n",
            run.state.as_str(),
            match &run.stop_condition {
                Some(c) => format!(" ({})", c.as_str()),
                None => String::new(),
            }
        ));
    }
    if let Some(reason) = &manifest.final_output_skipped {
        s.push_str(&format!(
            "> **No final deliverable.** The final meta-review was skipped: {reason}.\n\n"
        ));
    }

    // 1. The question, scope, constraints, and what work actually ran.
    s.push_str("## 1. Question, scope, and work performed\n\n");
    s.push_str(&format!("- **Mode:** {}\n", goal.mode.as_str()));
    s.push_str(&format!("- **Goal revision:** {}\n", goal.revision));
    if let Some(scope) = &goal.scope {
        s.push_str(&format!("- **Scope:** {scope}\n"));
    }
    if !goal.exclusions.is_empty() {
        s.push_str(&format!("- **Excluded:** {}\n", goal.exclusions.join("; ")));
    }
    if !goal.deliverables.is_empty() {
        s.push_str(&format!(
            "- **Requested:** {}\n",
            goal.deliverables.join("; ")
        ));
    }
    if !goal.assumptions.is_empty() {
        s.push_str("- **Assumptions recorded rather than asked about:**\n");
        for a in &goal.assumptions {
            s.push_str(&format!("  - {a}\n"));
        }
    }
    if let Some(p) = plan {
        // What ran, not what was planned (SPEC §11): a role counts as used
        // only when at least one of its tasks completed.
        let (used, unused) = completed_roles(p, tasks);
        s.push_str(&format!(
            "- **Roles used:** {}\n",
            if used.is_empty() {
                "none (no task completed)".to_string()
            } else {
                used.join(", ")
            }
        ));
        if !unused.is_empty() {
            s.push_str(&format!(
                "- **Roles planned but without completed work:** {}\n",
                unused.join(", ")
            ));
        }
        s.push_str(&format!("- **Plan rationale:** {}\n", p.rationale));
    }
    s.push_str(&format!(
        "- **Work completed:** {} task(s); {} failed; {} with unknown external outcome\n",
        manifest.counts.tasks_completed,
        manifest.counts.tasks_failed,
        manifest.counts.tasks_uncertain
    ));
    s.push_str(&format!(
        "- **Model provider:** {}\n",
        if manifest.providers.is_empty() {
            "none (no task ran)".to_string()
        } else {
            format!(
                "{} (models: {})",
                manifest.providers.join(", "),
                manifest.models.join(", ")
            )
        }
    ));

    // Permissions actually in force, so an absent capability is visible.
    s.push_str(&format!(
        "- **Capabilities:** network {}, execution {}, tools: {}\n\n",
        yes_no(goal.permissions.network),
        yes_no(goal.permissions.execute),
        if goal.permissions.allowed_tools.is_empty() {
            "none".to_string()
        } else {
            goal.permissions.allowed_tools.join(", ")
        }
    ));

    // 2. Established results, Uruk's interpretations, and untested proposals.
    s.push_str("## 2. Findings\n\n");

    let items = store.list_items(run_id).await?;
    let syntheses: Vec<_> = items
        .iter()
        .filter(|i| i.kind == ItemKind::Synthesis || i.kind == ItemKind::Manuscript)
        .collect();

    for item in &syntheses {
        s.push_str(&format!("{}\n\n", item.content));
    }

    if let Some(deliverable) = load_deliverable(store, artifacts).await {
        if !deliverable.claims.is_empty() {
            s.push_str("### Claim provenance\n\n");
            for basis in [
                ClaimBasis::SourceReported,
                ClaimBasis::UrukInference,
                ClaimBasis::UntestedProposal,
            ] {
                let claims: Vec<_> = deliverable
                    .claims
                    .iter()
                    .filter(|c| c.basis == basis)
                    .collect();
                if claims.is_empty() {
                    continue;
                }
                s.push_str(&format!("**{}:**\n", capitalize(basis.label())));
                for c in claims {
                    let cites = c
                        .citations
                        .iter()
                        .map(|g| format!("`{}` {}", g.source_id, g.locator))
                        .collect::<Vec<_>>()
                        .join("; ");
                    s.push_str(&format!(
                        "- {}{}\n",
                        c.claim,
                        if cites.is_empty() {
                            String::new()
                        } else {
                            format!(" [{cites}]")
                        }
                    ));
                }
                s.push('\n');
            }
        }
        if !deliverable.disagreements.is_empty() {
            s.push_str("**Disagreements between sources:**\n");
            for d in &deliverable.disagreements {
                s.push_str(&format!("- {d}\n"));
            }
            s.push('\n');
        }
    }

    let candidates: Vec<_> = items.iter().filter(|i| i.kind.is_rankable()).collect();
    if !candidates.is_empty() {
        s.push_str("### Candidates\n\n");
        for item in &candidates {
            let assessment = store.current_assessment(&item.id).await?;
            let disposition = store.get_disposition(&item.id).await?;
            let rating = match plan {
                Some(p) => store.get_rating(&item.id, &p.id).await?,
                None => None,
            };

            s.push_str(&format!("#### {} `{}`\n\n", item.title, item.id));
            s.push_str(&format!(
                "- **Epistemic status:** {} — {}\n",
                assessment.as_str(),
                describe_assessment(assessment)
            ));
            if !matches!(disposition, Disposition::Active) {
                s.push_str(&format!(
                    "- **Workflow disposition:** {} (separate from scientific status)\n",
                    disposition.as_str()
                ));
            }
            if let Some(r) = &rating {
                s.push_str(&format!(
                    "- **Elo:** {:.0} over {} match(es){} — relative prioritisation within \
                     this rubric cohort, not a probability or confidence\n",
                    r.rating,
                    r.matches_played,
                    if r.stale {
                        " **(STALE: new evidence has not yet been reflected)**"
                    } else {
                        ""
                    }
                ));
            }
            if item.is_child() {
                s.push_str(&format!(
                    "- **Derived from:** {}\n",
                    item.parent_ids
                        .iter()
                        .map(|p| format!("`{p}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                if let Some(d) = &item.derivation {
                    s.push_str(&format!("- **What changed:** {d}\n"));
                }
            }
            if let Some(h) = &item.hypothesis
                && !h.falsifiers.is_empty()
            {
                s.push_str(&format!("- **Falsifiers:** {}\n", h.falsifiers.join("; ")));
            }
            s.push('\n');
        }
    }

    if syntheses.is_empty() && candidates.is_empty() {
        s.push_str(
            "No findings were produced. This is a recorded outcome, not an execution \
             error: no discovery is a valid result.\n\n",
        );
    }

    // 3. Supporting and contradicting evidence.
    s.push_str("## 3. Evidence\n\n");
    let evidence = store.list_evidence(run_id).await?;
    if evidence.is_empty() {
        s.push_str("No evidence beyond agent reasoning was obtained.\n\n");
    } else {
        for e in &evidence {
            s.push_str(&format!(
                "- `{}` ({}): {}\n",
                e.id,
                e.kind.as_str(),
                first_line(&e.content, 200)
            ));
            s.push_str(&format!("  - Method: {}\n", e.method));
            if let Some(l) = &e.limitations {
                s.push_str(&format!("  - Limitations: {l}\n"));
            }
            for link in store.links_for_evidence(&e.id).await? {
                s.push_str(&format!(
                    "  - **{}** claim on `{}`: {}\n",
                    link.relation.as_str(),
                    link.item_id,
                    link.justification
                ));
            }
        }
        s.push('\n');
    }

    let sources = store.list_sources(run_id).await?;
    if !sources.is_empty() {
        s.push_str("### Sources\n\n");
        for src in &sources {
            s.push_str(&format!(
                "- `{}` {} — access: {}{}\n",
                src.id,
                src.short_citation(),
                src.access.as_str(),
                match &src.access_limitations {
                    Some(l) => format!(" ({l})"),
                    None => String::new(),
                }
            ));
        }
        s.push('\n');
    }

    // 4. Methods, uncertainty, unavailable information, incomplete validation.
    s.push_str("## 4. Methods, uncertainty, and limitations\n\n");

    s.push_str("**Methods used** (from completed tasks):\n");
    let completed = completed_by_strategy(tasks);
    if completed.is_empty() {
        s.push_str("- none: no task completed\n");
    }
    for ((role, strategy), n) in &completed {
        s.push_str(&format!("- {role}: {strategy} ×{n}\n"));
    }
    s.push('\n');
    if let Some(p) = plan {
        let (_, unused) = completed_roles(p, tasks);
        // `build_plan` pushes one method per role, in the same order.
        let not_run: Vec<&String> = p
            .roles
            .iter()
            .zip(&p.methods)
            .filter(|(role, _)| unused.contains(role))
            .map(|(_, method)| method)
            .collect();
        if !not_run.is_empty() {
            s.push_str("**Planned but not run** (no task of that role completed):\n");
            for m in not_run {
                s.push_str(&format!("- {m}\n"));
            }
            s.push('\n');
        }
    }

    s.push_str("**Resource use:**\n");
    s.push_str(&format!(
        "- {} model call(s), {} token(s), {} tool execution(s)\n",
        manifest.usage.model_calls, manifest.usage.tokens, manifest.usage.tool_executions
    ));
    s.push_str(&format!(
        "- Cost: {}\n\n",
        match manifest.usage.cost_usd {
            Some(c) => format!("${c:.4} ({})", manifest.usage.cost_note),
            None => format!("unknown — {}", manifest.usage.cost_note),
        }
    ));

    let unknowns: Vec<_> = store
        .list_reviews(run_id)
        .await?
        .iter()
        .flat_map(|r| r.unknowns.clone())
        .collect();
    if !unknowns.is_empty() {
        s.push_str("**Unresolved by review:**\n");
        for u in unknowns.iter().take(20) {
            s.push_str(&format!("- {u}\n"));
        }
        s.push('\n');
    }

    // Safety decisions and blocked work are always reported.
    let decisions = store.list_decisions(run_id).await?;
    let safety: Vec<_> = decisions
        .iter()
        .filter(|d| d.kind == DecisionKind::SafetyBlock)
        .collect();
    if !safety.is_empty() {
        s.push_str("**Safety and oversight:**\n");
        for d in &safety {
            s.push_str(&format!(
                "- {} {}\n",
                if d.payload["blocked"] == true {
                    "**BLOCKED:**"
                } else {
                    "Flagged:"
                },
                d.reason
            ));
        }
        s.push('\n');
    }

    s.push_str("**Standing limitations:**\n");
    s.push_str(
        "- Agent critiques and simulated expert debate are reasoning, not independent \
         empirical validation. Several agents agreeing is not corroboration.\n",
    );
    s.push_str(
        "- Elo reflects relative prioritisation under one rubric revision. It is not a \
         probability, a confidence level, or evidence of discovery.\n",
    );
    if manifest.providers.iter().any(|p| p.starts_with("mock")) {
        s.push_str(
            "- No model provider was configured: replies came from an offline stand-in, so \
             no source content was analysed by a model.\n",
        );
    }
    if !goal.permissions.network {
        s.push_str(
            "- No literature retrieval was permitted. Novelty and coverage claims hold \
             only within the supplied material.\n",
        );
    }
    if !goal.permissions.execute {
        s.push_str("- No computation was executed, so no claim here rests on a run result.\n");
    }
    if manifest.counts.tasks_uncertain > 0 {
        s.push_str(&format!(
            "- {} task(s) were interrupted with external effects of unknown outcome and \
             require review before retry.\n",
            manifest.counts.tasks_uncertain
        ));
    }
    s.push('\n');

    // 5. Reproduction instructions and next actions requiring human judgement.
    s.push_str("## 5. Reproduction and next steps\n\n");

    let experiments = store.list_experiments(run_id).await?;
    let executed: Vec<_> = experiments
        .iter()
        .filter(|e| e.execution.is_some())
        .collect();
    if !executed.is_empty() {
        s.push_str("**Reproduction:**\n");
        for e in &executed {
            if let Some(exec) = &e.execution {
                match &exec.rerun {
                    Ok(cmd) => s.push_str(&format!(
                        "- `{}` → `{cmd}` (exit {}, outputs {})\n",
                        e.id,
                        exec.exit_status,
                        if exec.outputs_valid {
                            "valid"
                        } else {
                            "invalid"
                        }
                    )),
                    Err(why) => s.push_str(&format!("- `{}`: not reproducible — {why}\n", e.id)),
                }
            }
        }
        s.push('\n');
    }

    let next: Vec<_> = store
        .list_reviews(run_id)
        .await?
        .iter()
        .flat_map(|r| r.next_actions.clone())
        .collect();
    if next.is_empty() {
        s.push_str("No further actions were identified by the work that ran.\n");
    } else {
        s.push_str("**Requiring human judgement:**\n");
        for a in next.iter().take(20) {
            s.push_str(&format!("- {a}\n"));
        }
    }

    s.push_str(&format!(
        "\n---\n\nGenerated by Uruk {} from persisted records, without a model call. \
         Run `{}`, exported {}.\n",
        env!("CARGO_PKG_VERSION"),
        run.id,
        manifest.exported_at
    ));

    Ok(s)
}

/// The plan's roles split into those with at least one completed task and
/// those without.
fn completed_roles(plan: &Plan, tasks: &[Task]) -> (Vec<String>, Vec<String>) {
    let done: BTreeSet<&str> = tasks
        .iter()
        .filter(|t| t.state == TaskState::Completed)
        .map(|t| t.role.as_str())
        .collect();
    plan.roles
        .iter()
        .cloned()
        .partition(|r| done.contains(r.as_str()))
}

/// Completed tasks counted by role and strategy, in a stable order.
fn completed_by_strategy(tasks: &[Task]) -> BTreeMap<(String, String), usize> {
    let mut counts = BTreeMap::new();
    for t in tasks.iter().filter(|t| t.state == TaskState::Completed) {
        *counts
            .entry((t.role.as_str().to_string(), t.strategy.clone()))
            .or_insert(0) += 1;
    }
    counts
}

fn describe_assessment(a: Assessment) -> &'static str {
    match a {
        Assessment::Untested => "proposed, with no relevant validation yet",
        Assessment::Plausible => "survived review; not experimentally established",
        Assessment::Supported => "relevant evidence supports the scoped claim, with limits stated",
        Assessment::Contested => "supporting and contradicting evidence remain unresolved",
        Assessment::Refuted => "evidence contradicts the scoped claim or a necessary assumption",
        Assessment::Inconclusive => "available checks cannot resolve the claim",
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "permitted" } else { "not permitted" }
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.len() <= max {
        line.to_string()
    } else {
        let end = line
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= max)
            .last()
            .unwrap_or(0);
        format!("{}…", &line[..end])
    }
}
