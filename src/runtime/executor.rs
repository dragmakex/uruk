//! Dispatch one task to its role and collect its proposed records (SPEC §9.2).
//!
//! "On completion, validate outputs and commit the task outcome, referenced
//! research records, budget settlement, and any applicable rating update
//! atomically." Nothing here writes to the store: every record a role
//! proposes goes into a [`RecordBatch`] that the scheduler commits together
//! with the task outcome. A crash between execution and commit therefore
//! leaves no record behind, and a re-run cannot duplicate one.

use crate::agents::outputs::ProximityRelation;
use crate::agents::{
    AgentContext, evolution, generation, meta_review, proximity, ranking, reflection, safety,
};
use crate::records::*;
use crate::store::{RecordBatch, write_atomic};
use crate::tools::{ExecRequest, execute_analysis};
use crate::{Error, Result};
use time::OffsetDateTime;

/// What a task produced.
#[derive(Debug, Clone)]
pub struct TaskOutcome {
    pub state: TaskState,
    /// Records to commit with the outcome.
    pub batch: RecordBatch,
    /// Everything the attempt consumed, whatever its outcome (SPEC §6:
    /// "Failed work still costs what it consumed").
    pub cost: CostActual,
    pub error: Option<String>,
    /// True when the failure was an infrastructure fault worth a bounded
    /// retry, as opposed to a scientific or validation failure.
    pub retryable: bool,
}

/// Execute one task and collect its records.
///
/// Provider faults become retryable failures, validation failures become
/// permanent ones, and cancellation retains labelled partial artifacts. Only
/// storage errors propagate, since nothing can be recorded through a broken
/// store.
pub async fn execute_task(ctx: &AgentContext, task: &Task) -> Result<TaskOutcome> {
    let mut batch = RecordBatch::default();
    let result = match task.role {
        Role::Generation => run_generation(ctx, task, &mut batch).await,
        Role::Reflection => run_reflection(ctx, task, &mut batch).await,
        Role::Ranking => run_ranking(ctx, task, &mut batch).await,
        Role::Evolution => run_evolution(ctx, task, &mut batch).await,
        Role::Proximity => run_proximity(ctx, task, &mut batch).await,
        Role::MetaReview => run_meta_review(ctx, task, &mut batch).await,
        Role::Tool => run_tool(ctx, task, &mut batch).await,
        Role::Supervisor => Err(Error::validation(
            "the supervisor role is not dispatched through execute_task",
        )),
    };
    let cost = ctx.spent();

    let (state, error, retryable) = match result {
        Ok(()) => {
            return Ok(TaskOutcome {
                state: TaskState::Completed,
                batch,
                cost,
                error: None,
                retryable: false,
            });
        }
        Err(Error::Cancelled) => (
            TaskState::Cancelled,
            "cancelled; partial outputs retained with this context".to_string(),
            false,
        ),
        Err(e @ Error::Storage(_)) | Err(e @ Error::Migration(_)) => return Err(e),
        Err(Error::Provider(msg)) => (TaskState::Failed, msg, true),
        Err(e) => (TaskState::Failed, e.to_string(), false),
    };

    // Half-built scientific records are not committed; artifacts and
    // decisions are, labelled with how the task ended.
    batch.retain_partial();
    for artifact in &mut batch.artifacts {
        let label = artifact.label.take().unwrap_or_default();
        artifact.label = Some(format!("{label} [partial: task {}]", state.as_str()));
    }

    Ok(TaskOutcome {
        state,
        batch,
        cost,
        error: Some(error),
        retryable,
    })
}

/// The turn cap for a debate, bounded by the published maximum and by what
/// the run budget can still afford.
fn debate_turns(ctx: &AgentContext) -> u32 {
    ctx.goal
        .budget
        .max_debate_turns
        .clamp(1, generation::HARD_TURN_CAP)
}

async fn run_generation(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    let (item, transcript, provisional) = match task.strategy.as_str() {
        "debate" => {
            match generation::through_debate(ctx, debate_turns(ctx), 3, Some(task.id.clone()))
                .await?
            {
                generation::DebateResult::Converged(g) => (g.item, g.transcript, g.provisional),
                generation::DebateResult::Incomplete {
                    transcript,
                    unresolved,
                    turns_used,
                    ..
                } => {
                    // An unconverged debate produces no item, only a record of
                    // what was discussed (SPEC §6).
                    let artifact = store_transcript(ctx, task, &transcript).await?;
                    batch.decisions.push(Decision {
                        id: DecisionId::new(),
                        schema_version: SCHEMA_VERSION,
                        run_id: ctx.run_id.clone(),
                        kind: DecisionKind::Scheduling,
                        actor: Actor::Agent("generation".into()),
                        reason: format!(
                            "debate did not converge within {turns_used} turn(s); \
                             no hypothesis was fabricated to end it"
                        ),
                        referenced: vec![task.id.0.clone(), artifact.id.0.clone()],
                        payload: serde_json::json!({
                            "unresolved": unresolved,
                            "turns_used": turns_used,
                        }),
                        created_at: OffsetDateTime::now_utc(),
                    });
                    batch.artifacts.push(artifact);
                    return Ok(());
                }
            }
        }
        _ => {
            let source = match task.input_refs.first() {
                Some(id) => Some(ctx.store.get_item(&ItemId::from_raw(id.clone())).await?),
                None => None,
            };
            let g =
                generation::from_literature(ctx, source.as_ref(), Some(task.id.clone())).await?;
            (g.item, g.transcript, g.provisional)
        }
    };

    if provisional {
        tracing::warn!(item = %item.id, "candidate is provisional: source grounding was insufficient");
    }
    if let Some(text) = transcript {
        batch
            .artifacts
            .push(store_transcript(ctx, task, &text).await?);
    }
    admit_candidate(ctx, item, "candidate", batch);
    Ok(())
}

/// Record a new candidate with its own safety check (SPEC §12): a permitted
/// goal is not blanket approval, and a blocked candidate never enters the
/// tournament.
fn admit_candidate(ctx: &AgentContext, item: ResearchItem, what: &str, batch: &mut RecordBatch) {
    let verdict = safety::check_item(&item);
    if let Some(decision) = safety::safety_decision(
        &ctx.run_id,
        &verdict,
        &format!("{what} {}", item.id),
        vec![item.id.0.clone()],
    ) {
        batch.decisions.push(decision);
        if verdict.is_blocked() {
            batch.dispositions.push((
                item.id.clone(),
                Disposition::Blocked,
                verdict.concern().map(str::to_string),
            ));
        }
    }
    // A candidate starts at the initial rating in this cohort: it inherits
    // neither a parent's rating nor its support (SPEC §5).
    if item.kind.is_rankable() && !verdict.is_blocked() {
        batch.ratings.push((item.id.clone(), ctx.plan.id.clone()));
    }
    batch.items.push(item);
}

async fn run_reflection(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    let item_id = task
        .input_refs
        .first()
        .ok_or_else(|| Error::validation("reflection task has no target item"))?;
    let item = ctx
        .store
        .get_item(&ItemId::from_raw(item_id.clone()))
        .await?;
    if !item.kind.is_rankable() {
        return Err(Error::validation(format!(
            "{} is a {} and is not reviewed as a candidate",
            item.id,
            item.kind.as_str()
        )));
    }

    let strategy = ReviewStrategy::parse(&task.strategy)
        .ok_or_else(|| Error::validation(format!("unknown review strategy {:?}", task.strategy)))?;

    let reviewed = if strategy == ReviewStrategy::Observation {
        let source_id = task
            .input_refs
            .get(1)
            .ok_or_else(|| Error::validation("observation review has no target source"))?;
        let source = ctx
            .store
            .get_source(&SourceId::from_raw(source_id.clone()))
            .await?;
        reflection::review_observations(ctx, &item, &source, Some(task.id.clone())).await?
    } else {
        reflection::review(ctx, &item, strategy, Some(task.id.clone())).await?
    };

    let review = reviewed.review;
    let links = review.evidence.clone();

    // Kinds of every linked evidence item, whether new in this batch or
    // already stored, so the assessment is derived on the same basis the
    // store's gate will check.
    let mut kinds = Vec::with_capacity(links.len());
    for link in &links {
        let kind = match reviewed
            .new_evidence
            .iter()
            .find(|e| e.id == link.evidence_id)
        {
            Some(e) => e.kind,
            None => ctx.store.get_evidence(&link.evidence_id).await?.kind,
        };
        kinds.push(kind);
    }
    let assessment = derive_assessment(review.proposed_assessment, &links, &kinds);

    batch.evidence.extend(reviewed.new_evidence);
    batch.links.extend(links.clone());
    batch.assessments.push(AssessmentRecord {
        item_id: item.id.clone(),
        assessment,
        basis_review: Some(review.id.clone()),
        basis_decision: None,
        evidence: links,
        rationale: review.assessment_text.clone(),
        assessed_at: OffsetDateTime::now_utc(),
    });

    if let Some(concern) = reviewed.safety_concern {
        let verdict = safety::check(&format!("{} {}", item.content, concern));
        if let Some(decision) = safety::safety_decision(
            &ctx.run_id,
            &verdict,
            &format!("candidate {} flagged during review", item.id),
            vec![item.id.0.clone(), review.id.0.clone()],
        ) {
            batch.decisions.push(decision);
            if verdict.is_blocked() {
                batch.dispositions.push((
                    item.id.clone(),
                    Disposition::Blocked,
                    verdict.concern().map(str::to_string),
                ));
            }
        }
    }

    batch.reviews.push(review);
    Ok(())
}

/// The assessment the evidence actually warrants (SPEC §4.3).
///
/// Empirical evidence in both directions is `Contested`; in one direction
/// `Supported` or `Refuted`; otherwise the review's own proposal stands,
/// which validation already capped below `Supported`. The store's gate
/// re-checks this on commit.
pub fn derive_assessment(
    proposed: Assessment,
    links: &[EvidenceLink],
    kinds: &[EvidenceKind],
) -> Assessment {
    let empirical = |relation: EvidenceRelation| {
        links
            .iter()
            .zip(kinds)
            .any(|(l, k)| l.relation == relation && k.is_empirical())
    };
    match (
        empirical(EvidenceRelation::Supports),
        empirical(EvidenceRelation::Contradicts),
    ) {
        (true, true) => Assessment::Contested,
        (true, false) => Assessment::Supported,
        (false, true) => Assessment::Refuted,
        (false, false) => proposed,
    }
}

async fn run_ranking(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    if task.input_refs.len() < 2 {
        return Err(Error::validation(
            "a comparison needs two candidates; a single item cannot be ranked",
        ));
    }
    let a = ctx
        .store
        .get_item(&ItemId::from_raw(task.input_refs[0].clone()))
        .await?;
    let b = ctx
        .store
        .get_item(&ItemId::from_raw(task.input_refs[1].clone()))
        .await?;

    // A blocked candidate never enters the tournament (SPEC §12).
    for item in [&a, &b] {
        if matches!(
            ctx.store.get_disposition(&item.id).await?,
            Disposition::Blocked
        ) {
            return Err(Error::permission(format!(
                "candidate {} is blocked and cannot be compared",
                item.id
            )));
        }
    }

    let method = if task.strategy == "debate" {
        MatchMethod::Debate
    } else {
        MatchMethod::Pairwise
    };

    let judged = ranking::compare(
        ctx,
        &a,
        &b,
        method,
        debate_turns(ctx),
        Some(task.id.clone()),
    )
    .await?;

    batch.ratings.push((a.id.clone(), ctx.plan.id.clone()));
    batch.ratings.push((b.id.clone(), ctx.plan.id.clone()));
    if let Some(text) = judged.transcript {
        batch
            .artifacts
            .push(store_transcript(ctx, task, &text).await?);
    }
    batch.matches.push((judged.r#match, judged.dedupe_key));
    Ok(())
}

async fn run_evolution(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    let parents: Vec<ResearchItem> = {
        let mut out = Vec::new();
        for id in &task.input_refs {
            out.push(ctx.store.get_item(&ItemId::from_raw(id.clone())).await?);
        }
        out
    };
    if parents.is_empty() {
        return Err(Error::validation("evolution task has no parent item"));
    }

    let evolved = if task.strategy == "analogy" {
        evolution::through_analogy(ctx, &parents, Some(task.id.clone())).await?
    } else {
        evolution::improve_feasibility(ctx, &parents[0], Some(task.id.clone())).await?
    };

    // The child is checked independently of its parent (SPEC §12).
    admit_candidate(ctx, evolved.child, "evolved candidate", batch);
    Ok(())
}

async fn run_proximity(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    let rankable: Vec<ResearchItem> = if task.input_refs.is_empty() {
        ctx.store
            .list_items(&ctx.run_id)
            .await?
            .into_iter()
            .filter(|i| i.kind.is_rankable())
            .collect()
    } else {
        let mut out = Vec::new();
        for id in &task.input_refs {
            out.push(ctx.store.get_item(&ItemId::from_raw(id.clone())).await?);
        }
        out
    };

    if rankable.len() < 2 {
        return Ok(());
    }

    let pairs = proximity::candidate_pairs(&rankable, 6);
    let mut relations = Vec::new();

    for (i, j, overlap) in pairs {
        // Only spend a model call where structural overlap suggests it.
        if overlap < 0.15 {
            continue;
        }
        let assessed = proximity::assess(ctx, &rankable[i], &rankable[j]).await?;

        if assessed.relation == ProximityRelation::Duplicate {
            // Marking a duplicate preserves the item and its provenance.
            batch.dispositions.push((
                rankable[j].id.clone(),
                Disposition::Duplicate(rankable[i].id.clone()),
                Some(assessed.rationale.clone()),
            ));
        }
        relations.push(assessed);
    }

    batch
        .clusters
        .extend(proximity::build_clusters(ctx, &rankable, &relations));
    Ok(())
}

async fn run_meta_review(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    match task.strategy.as_str() {
        "overview" => {
            let overview = meta_review::research_overview(ctx).await?;
            let json = serde_json::to_string_pretty(&overview.output)?;
            let artifact = store_artifact(
                ctx,
                task,
                "research-overview.json",
                "application/json",
                json.as_bytes(),
                Some("research overview"),
            )
            .await?;

            // Emerging directions are checked separately (SPEC §12): a safe
            // goal is not blanket approval for where the research is heading.
            for direction in &overview.output.directions {
                let text = format!(
                    "{} {}",
                    direction["label"].as_str().unwrap_or(""),
                    direction["importance"].as_str().unwrap_or("")
                );
                let verdict = safety::check(&text);
                if let Some(decision) = safety::safety_decision(
                    &ctx.run_id,
                    &verdict,
                    &format!(
                        "research direction {:?}",
                        direction["label"].as_str().unwrap_or("")
                    ),
                    vec![artifact.id.0.clone()],
                ) {
                    batch.decisions.push(decision);
                }
            }

            batch.decisions.push(Decision {
                id: DecisionId::new(),
                schema_version: SCHEMA_VERSION,
                run_id: ctx.run_id.clone(),
                kind: DecisionKind::MetaFeedback,
                actor: Actor::Agent("meta_review".into()),
                reason: format!(
                    "research overview: {} direction(s), {} underexplored",
                    overview.output.directions.len(),
                    overview.output.underexplored.len()
                ),
                referenced: vec![artifact.id.0.clone()],
                payload: serde_json::json!({
                    "overview": overview.output,
                    "underexplored": overview.output.underexplored,
                }),
                created_at: OffsetDateTime::now_utc(),
            });
            batch.artifacts.push(artifact);
            Ok(())
        }
        "deliverable" => {
            let requested = ctx
                .goal
                .deliverables
                .first()
                .cloned()
                .unwrap_or_else(|| "research report".to_string());
            let produced = meta_review::produce_deliverable(ctx, &requested).await?;

            let item = ResearchItem {
                id: ItemId::new(),
                schema_version: SCHEMA_VERSION,
                run_id: ctx.run_id.clone(),
                kind: ItemKind::Synthesis,
                title: produced.output.title.clone(),
                content_hash: ContentHash::of_str(&produced.output.body),
                content: produced.output.body.clone(),
                hypothesis: None,
                parent_ids: vec![],
                derivation: None,
                source_ids: vec![],
                author: Author::Agent {
                    role: Role::MetaReview.as_str().to_string(),
                    strategy: "deliverable".to_string(),
                },
                produced_by: Some(task.id.clone()),
                goal_id: ctx.goal.id.clone(),
                created_at: OffsetDateTime::now_utc(),
            };

            let json = serde_json::to_string_pretty(&produced.output)?;
            let artifact = store_artifact(
                ctx,
                task,
                "deliverable.json",
                "application/json",
                json.as_bytes(),
                Some("deliverable with claim provenance"),
            )
            .await?;

            batch.items.push(item);
            batch.artifacts.push(artifact);
            Ok(())
        }
        _ => {
            let revision = ctx.store.next_feedback_revision(&ctx.run_id).await?;
            let synthesized = meta_review::synthesize(ctx, revision, Some(task.id.clone())).await?;

            batch.decisions.push(Decision {
                id: DecisionId::new(),
                schema_version: SCHEMA_VERSION,
                run_id: ctx.run_id.clone(),
                kind: DecisionKind::MetaFeedback,
                actor: Actor::Agent("meta_review".into()),
                reason: format!(
                    "meta-review revision {revision}: {} recurring critique(s)",
                    synthesized.feedback.critiques.len()
                ),
                referenced: vec![synthesized.feedback.id.0.clone()],
                payload: serde_json::json!({
                    "caution": synthesized.caution,
                    "target_roles": synthesized.feedback.target_roles,
                }),
                created_at: OffsetDateTime::now_utc(),
            });
            batch.feedback.push(synthesized.feedback);
            Ok(())
        }
    }
}

/// An approved, contained execution (SPEC §8). The result is recorded as an
/// experiment plus evidence linked as `context` to the target claim: its
/// bearing on the claim is for a later review to judge, never assumed.
async fn run_tool(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    let payload = task
        .payload
        .as_ref()
        .ok_or_else(|| Error::validation("tool task has no execution payload"))?;
    let request: ExecRequest = serde_json::from_value(payload["request"].clone()).map_err(|e| {
        Error::validation(format!(
            "tool task payload is not an execution request: {e}"
        ))
    })?;
    let target_item = payload["target_item"]
        .as_str()
        .map(ItemId::from_raw)
        .ok_or_else(|| Error::validation("tool task names no target item"))?;
    let target_claim = payload["target_claim"].as_str().unwrap_or("").to_string();
    let purpose = payload["purpose"].as_str().unwrap_or("").to_string();

    ctx.charge_tool_execution();
    let outcome = execute_analysis(
        &ctx.store,
        &ctx.run_id,
        &task.id,
        &request,
        &ctx.permissions,
        &ctx.cancel,
    )
    .await?;

    let usable = outcome.record.may_support_claims();
    let evidence_ids: Vec<EvidenceId> = outcome.evidence.iter().map(|e| e.id.clone()).collect();

    batch.experiments.push(Experiment {
        id: ExperimentId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: ctx.run_id.clone(),
        external: false,
        protocol: if purpose.is_empty() {
            request.command_line()
        } else {
            purpose.clone()
        },
        target_claims: vec![target_item.clone()],
        inputs: request.inputs.clone(),
        controls: vec![],
        expected_outcomes: vec![],
        analysis_method: request.command_line(),
        required_permissions: vec!["execute".into(), format!("tool:{}", request.program)],
        execution: Some(outcome.record.clone()),
        evidence_ids,
        contributor: None,
        collection_context: Some(outcome.containment.clone()),
        created_at: OffsetDateTime::now_utc(),
    });
    batch.artifacts.extend(outcome.artifacts);

    match outcome.evidence {
        Some(evidence) => {
            batch.links.push(EvidenceLink {
                evidence_id: evidence.id.clone(),
                item_id: target_item,
                claim: if target_claim.is_empty() {
                    purpose
                } else {
                    target_claim
                },
                relation: EvidenceRelation::Context,
                justification: "execution result recorded; its bearing on the claim is for a \
                                review to judge after method and applicability checks"
                    .into(),
                limits: Some("not yet interpreted".into()),
            });
            batch.evidence.push(evidence);
        }
        None => {
            // A failed check is a recorded negative outcome, not support and
            // not an error (SPEC §8, §12).
            batch.decisions.push(Decision {
                id: DecisionId::new(),
                schema_version: SCHEMA_VERSION,
                run_id: ctx.run_id.clone(),
                kind: DecisionKind::Scheduling,
                actor: Actor::System,
                reason: format!(
                    "execution produced no usable evidence: {}",
                    outcome
                        .record
                        .validation_note
                        .as_deref()
                        .unwrap_or("no detail")
                ),
                referenced: vec![task.id.0.clone(), target_item.0.clone()],
                payload: serde_json::json!({ "usable": usable, "exit_status": outcome.record.exit_status }),
                created_at: OffsetDateTime::now_utc(),
            });
        }
    }
    Ok(())
}

/// Persist a debate transcript as an artifact.
///
/// Debate contributions are artifacts, not Evidence (SPEC §15.1).
async fn store_transcript(ctx: &AgentContext, task: &Task, text: &str) -> Result<Artifact> {
    store_artifact(
        ctx,
        task,
        "transcript.md",
        "text/markdown",
        text.as_bytes(),
        Some("debate transcript (reasoning, not evidence)"),
    )
    .await
}

/// Write an artifact to disk under the task's own directory and describe it;
/// the reference is committed with the task (SPEC §9.2).
///
/// "Write and durably finalize immutable artifacts before committing
/// references to them." Each task owns a directory, so two tasks producing
/// the same file name never overwrite each other.
pub(crate) async fn store_artifact(
    ctx: &AgentContext,
    task: &Task,
    filename: &str,
    media_type: &str,
    bytes: &[u8],
    label: Option<&str>,
) -> Result<Artifact> {
    let path = ctx
        .store
        .artifact_dir(&ctx.run_id)
        .join(task.id.as_str())
        .join(filename);
    write_atomic(&path, bytes).await?;

    Ok(Artifact {
        id: ArtifactId::new(),
        schema_version: SCHEMA_VERSION,
        run_id: ctx.run_id.clone(),
        media_type: media_type.to_string(),
        content_hash: ContentHash::of_bytes(bytes),
        size_bytes: bytes.len() as u64,
        storage_path: ctx.store.storable_path(&path),
        produced_by: Some(task.id.clone()),
        access: AccessClass::Open,
        label: label.map(|s| s.to_string()),
        supersedes: None,
        superseded_reason: None,
        created_at: OffsetDateTime::now_utc(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(relation: EvidenceRelation) -> EvidenceLink {
        EvidenceLink {
            evidence_id: EvidenceId::new(),
            item_id: ItemId::from_raw("item_x"),
            claim: "c".into(),
            relation,
            justification: "j".into(),
            limits: None,
        }
    }

    #[test]
    fn assessment_follows_empirical_evidence_only() {
        use EvidenceKind::*;
        use EvidenceRelation::*;
        assert_eq!(
            derive_assessment(Assessment::Plausible, &[link(Supports)], &[AgentCritique]),
            Assessment::Plausible,
            "agent critique cannot promote"
        );
        assert_eq!(
            derive_assessment(
                Assessment::Plausible,
                &[link(Supports)],
                &[ComputationalResult]
            ),
            Assessment::Supported
        );
        assert_eq!(
            derive_assessment(
                Assessment::Plausible,
                &[link(Contradicts)],
                &[ExternalExperiment]
            ),
            Assessment::Refuted
        );
        assert_eq!(
            derive_assessment(
                Assessment::Plausible,
                &[link(Supports), link(Contradicts)],
                &[RetrievedLiterature, ExternalExperiment]
            ),
            Assessment::Contested
        );
        assert_eq!(
            derive_assessment(
                Assessment::Inconclusive,
                &[link(Context)],
                &[ComputationalResult]
            ),
            Assessment::Inconclusive,
            "context links do not move the assessment"
        );
    }
}
