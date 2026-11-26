//! Shared eligibility policy for scheduling and dispatching reproduction.
use crate::Result;
use crate::records::*;
use crate::store::Store;

/// Only a successful original experiment whose result is cited as support by
/// a current Supported assessment is eligible. A context link is insufficient.
pub async fn supported_targets(
    store: &Store,
    run_id: &RunId,
    experiment: &Experiment,
) -> Result<Vec<ItemId>> {
    if experiment.run_id != *run_id
        || experiment.external
        || experiment.reproduction.is_some()
        || !experiment
            .execution
            .as_ref()
            .is_some_and(ExecutionRecord::may_support_claims)
    {
        return Ok(vec![]);
    }
    let mut targets = Vec::new();
    for id in &experiment.target_claims {
        let item = store.get_item(id).await?;
        if item.run_id != *run_id
            || !matches!(store.get_disposition(id).await?, Disposition::Active)
        {
            continue;
        }
        let Some(record) = store.current_assessment_record(id).await? else {
            continue;
        };
        if record.assessment != Assessment::Supported {
            continue;
        }
        for link in &record.evidence {
            if link.item_id == *id
                && link.relation == EvidenceRelation::Supports
                && experiment.evidence_ids.contains(&link.evidence_id)
            {
                let evidence = store.get_evidence(&link.evidence_id).await?;
                if evidence.run_id == *run_id && evidence.kind == EvidenceKind::ComputationalResult
                {
                    targets.push(id.clone());
                    break;
                }
            }
        }
    }
    Ok(targets)
}
