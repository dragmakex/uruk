//! Closed-loop re-execution with a conservative byte-exact verdict.
use crate::agents::{AgentContext, verification::supported_targets};
use crate::records::*;
use crate::store::{RecordBatch, Store};
use crate::tools::execute_analysis;
use crate::{Error, Result};
use time::OffsetDateTime;

pub(super) async fn run(ctx: &AgentContext, task: &Task, batch: &mut RecordBatch) -> Result<()> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let id = task
        .input_refs
        .first()
        .ok_or_else(|| Error::validation("verification names no experiment"))?;
    let original = ctx
        .store
        .list_experiments(&ctx.run_id)
        .await?
        .into_iter()
        .find(|e| e.id.as_str() == id)
        .ok_or_else(|| Error::not_found("verification experiment"))?;
    let targets = supported_targets(&ctx.store, &ctx.run_id, &original).await?;
    if targets.is_empty() {
        return Err(Error::validation(
            "experiment has no current supporting evidence for an active Supported claim",
        ));
    }
    let baseline = original
        .execution
        .as_ref()
        .expect("eligibility requires execution");
    let mut reproduction = Reproduction {
        original_experiment_id: original.id.clone(),
        verdict: ReproductionVerdict::Inconclusive,
        reason: String::new(),
        original_stdout_hash: None,
        rerun_stdout_hash: None,
    };
    let mut execution = None;
    let mut evidence_ids = vec![];
    let preflight = prepare_baseline(&ctx.store, &ctx.run_id, baseline).await;
    match preflight {
        Err(reason) => reproduction.reason = reason,
        Ok((request, stdout_hash)) => {
            reproduction.original_stdout_hash = Some(stdout_hash.clone());
            // Both the goal and the narrower task context must allow replay.
            if !ctx.goal.permissions.execute || !ctx.goal.permissions.allows_tool(&request.program)
            {
                reproduction.reason = "original program is not permitted by the run".into();
            } else {
                ctx.charge_tool_execution();
                match execute_analysis(
                    &ctx.store,
                    &ctx.run_id,
                    &task.id,
                    &request,
                    &ctx.permissions,
                    &ctx.cancel,
                )
                .await
                {
                    Ok(outcome) => {
                        let rerun_hash = outcome
                            .artifacts
                            .iter()
                            .find(|a| Some(&a.id) == outcome.record.stdout_artifact.as_ref())
                            .map(|a| a.content_hash.clone());
                        reproduction.rerun_stdout_hash = rerun_hash.clone();
                        let (verdict, reason) =
                            compare(baseline, &outcome.record, &stdout_hash, rerun_hash.as_ref());
                        reproduction.verdict = verdict;
                        reproduction.reason = reason.into();
                        execution = Some(outcome.record);
                        batch.artifacts.extend(outcome.artifacts);
                        if let Some(evidence) = outcome.evidence {
                            evidence_ids.push(evidence.id.clone());
                            for item_id in &targets {
                                batch.links.push(EvidenceLink {
                                    evidence_id: evidence.id.clone(), item_id: item_id.clone(),
                                    claim: "re-execution of the original computation".into(),
                                    relation: EvidenceRelation::Context,
                                    justification: "reproduction result requires claim-relevant interpretation; no automatic promotion or refutation".into(),
                                    limits: Some("same method and data, not independent corroboration".into()),
                                });
                            }
                            batch.evidence.push(evidence);
                        }
                    }
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(e @ Error::Storage(_)) | Err(e @ Error::Migration(_)) => return Err(e),
                    Err(_) => {
                        reproduction.reason =
                            "contained replay was unavailable; no result was obtained".into()
                    }
                }
            }
        }
    }
    batch.experiments.push(Experiment {
        id: ExperimentId::new(), schema_version: SCHEMA_VERSION, run_id: ctx.run_id.clone(),
        external: false, protocol: format!("Replay of {}", original.id), target_claims: targets,
        inputs: original.inputs.clone(), controls: original.controls.clone(),
        expected_outcomes: vec!["byte-identical complete stdout under the recorded recipe, input hashes, platform and top-level program hash".into()],
        analysis_method: "byte-exact-stdout-v1 (not scientific equivalence or independent replication)".into(),
        required_permissions: original.required_permissions.clone(), execution,
        reproduction: Some(reproduction), evidence_ids, contributor: None,
        collection_context: Some("fresh contained workspace; original records preserved".into()),
        created_at: OffsetDateTime::now_utc(),
    });
    Ok(())
}

/// Validate the actual baseline bytes, not just a stored hash or display command.
async fn prepare_baseline(
    store: &Store,
    run_id: &RunId,
    baseline: &ExecutionRecord,
) -> std::result::Result<(ExecutionRequest, ContentHash), String> {
    let request = baseline
        .request
        .clone()
        .ok_or("original execution has no typed replay recipe")?;
    if baseline.program_hash.is_none() || baseline.code_hash.is_none() {
        return Err("original execution has incomplete code or program fingerprints".into());
    }
    let id = baseline
        .stdout_artifact
        .as_ref()
        .ok_or("original stdout artifact is missing")?;
    let artifact = store
        .get_artifact(id)
        .await
        .map_err(|_| "original stdout artifact is unavailable")?;
    if artifact.run_id != *run_id {
        return Err("original stdout artifact belongs to another run".into());
    }
    let bytes = tokio::fs::read(store.resolve_path(&artifact.storage_path))
        .await
        .map_err(|_| "original stdout bytes are unavailable")?;
    if ContentHash::of_bytes(&bytes) != artifact.content_hash
        || bytes.len() as u64 != artifact.size_bytes
    {
        return Err("original stdout artifact failed integrity validation".into());
    }
    Ok((request, artifact.content_hash))
}

fn compare(
    original: &ExecutionRecord,
    rerun: &ExecutionRecord,
    original_hash: &ContentHash,
    rerun_hash: Option<&ContentHash>,
) -> (ReproductionVerdict, &'static str) {
    if !rerun.may_support_claims() {
        return (
            ReproductionVerdict::Inconclusive,
            "replay failed or returned incomplete output; no scientific refutation follows",
        );
    }
    let environment = |record: &ExecutionRecord| {
        record
            .environment
            .iter()
            .filter(|entry| !entry.starts_with("workspace="))
            .cloned()
            .collect::<Vec<_>>()
    };
    if original.request != rerun.request
        || original.input_hashes != rerun.input_hashes
        || original.code_hash != rerun.code_hash
        || original.program_hash != rerun.program_hash
        || environment(original) != environment(rerun)
    {
        return (
            ReproductionVerdict::Inconclusive,
            "recorded recipe, inputs, platform or top-level program changed; outputs are not comparable",
        );
    }
    match rerun_hash {
        None => (
            ReproductionVerdict::Inconclusive,
            "replay stdout artifact is missing",
        ),
        Some(hash) if hash == original_hash => (
            ReproductionVerdict::Reproduced,
            "complete stdout is byte-identical with matching recorded recipe and fingerprints; this is not independent corroboration",
        ),
        Some(_) => (
            ReproductionVerdict::NotReproduced,
            "complete stdout differs with matching recorded recipe and fingerprints; the scientific claim is not automatically refuted",
        ),
    }
}
