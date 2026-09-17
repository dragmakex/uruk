//! Proximity: find related ideas, pairs, clusters, and duplicates (SPEC §5).
//!
//! "Start with structured overlap and a bounded semantic comparison; use
//! embeddings only when scale warrants them." Similarity alone never deletes
//! or invalidates a candidate.

use super::context::AgentContext;
use super::outputs::{self, ProximityOutput, ProximityRelation};
use super::reflection::render_item;
use crate::Result;
use crate::prompts::ids;
use crate::records::*;
use std::collections::BTreeSet;
use time::OffsetDateTime;

/// The assessed relation between two items.
#[derive(Debug, Clone)]
pub struct Proximity {
    pub a: ItemId,
    pub b: ItemId,
    pub relation: ProximityRelation,
    pub rationale: String,
    pub cluster_label: Option<String>,
    pub cost: CostActual,
    /// Differences the model judged material.
    pub material_differences: Vec<String>,
}

/// Cheap structural overlap, used to pick which pairs are worth a model call.
///
/// Jaccard similarity over content word sets. Deliberately crude: it only
/// decides what to look at more closely, never what is a duplicate.
pub fn lexical_overlap(a: &ResearchItem, b: &ResearchItem) -> f64 {
    let sa = word_set(&a.content);
    let sb = word_set(&b.content);
    if sa.is_empty() || sb.is_empty() {
        return 0.0;
    }
    let intersection = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    intersection / union
}

fn word_set(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 3)
        .map(|w| w.to_ascii_lowercase())
        .filter(|w| !is_stopword(w))
        .collect()
}

fn is_stopword(w: &str) -> bool {
    matches!(
        w,
        "this"
            | "that"
            | "with"
            | "from"
            | "have"
            | "been"
            | "will"
            | "would"
            | "could"
            | "which"
            | "these"
            | "those"
            | "than"
            | "then"
            | "when"
            | "what"
            | "were"
            | "into"
            | "such"
            | "more"
            | "most"
            | "some"
            | "hypothesis"
            | "mechanism"
            | "claim"
            | "scope"
            | "assumptions"
            | "predictions"
            | "falsifiers"
            | "validation"
            | "plan"
    )
}

/// Pairs worth comparing, most similar first.
///
/// Bounded by `limit` so a large candidate pool cannot fan out quadratically
/// into model calls.
pub fn candidate_pairs(items: &[ResearchItem], limit: usize) -> Vec<(usize, usize, f64)> {
    let mut pairs = Vec::new();
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            pairs.push((i, j, lexical_overlap(&items[i], &items[j])));
        }
    }
    pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    pairs.truncate(limit);
    pairs
}

/// Assess whether two items are genuinely the same proposal.
pub async fn assess(ctx: &AgentContext, a: &ResearchItem, b: &ResearchItem) -> Result<Proximity> {
    let bindings = ctx
        .base_bindings_for(ids::PROXIMITY_SIMILARITY)
        .set("hypothesis 1", render_item(a))
        .set("hypothesis 2", render_item(b));

    let completion = ctx.call(ids::PROXIMITY_SIMILARITY, bindings).await?;
    let output: ProximityOutput = outputs::parse(&completion.text)?;

    // A material difference in mechanism, scope, predictions, or tests keeps
    // the candidate alive, whatever the model concluded (SPEC §5).
    let relation = output.effective_relation();
    let material_differences = output
        .differences
        .iter()
        .filter(|d| d.material)
        .map(|d| format!("{}: {}", d.dimension, d.difference))
        .collect();

    Ok(Proximity {
        a: a.id.clone(),
        b: b.id.clone(),
        relation,
        rationale: output.rationale,
        cluster_label: output.cluster_label,
        cost: completion.cost,
        material_differences,
    })
}

/// Group items into clusters from assessed relations.
///
/// Union-find over `related` and `duplicate` edges. Singletons become their own
/// cluster so an unexplored direction stays visible to the Supervisor.
pub fn build_clusters(
    ctx: &AgentContext,
    items: &[ResearchItem],
    relations: &[Proximity],
) -> Vec<Cluster> {
    let mut parent: Vec<usize> = (0..items.len()).collect();

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    let index_of = |id: &ItemId| items.iter().position(|i| &i.id == id);

    for rel in relations {
        if rel.relation == ProximityRelation::Distinct {
            continue;
        }
        if let (Some(i), Some(j)) = (index_of(&rel.a), index_of(&rel.b)) {
            let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
            if ri != rj {
                parent[rj] = ri;
            }
        }
    }

    // Collect members per root, preserving item order.
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..items.len() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }

    groups
        .into_iter()
        .map(|(root, members)| {
            // Prefer a label the model suggested for a member of this group.
            let label = relations
                .iter()
                .find(|r| {
                    r.cluster_label.is_some()
                        && index_of(&r.a).map(|i| find(&mut parent.clone(), i)) == Some(root)
                })
                .and_then(|r| r.cluster_label.clone())
                .unwrap_or_else(|| items[members[0]].title.clone());

            Cluster {
                id: ClusterId::new(),
                schema_version: SCHEMA_VERSION,
                run_id: ctx.run_id.clone(),
                label,
                item_ids: members.iter().map(|&i| items[i].id.clone()).collect(),
                basis: "structural overlap and bounded semantic comparison".to_string(),
                created_at: OffsetDateTime::now_utc(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(content: &str) -> ResearchItem {
        ResearchItem {
            id: ItemId::new(),
            schema_version: SCHEMA_VERSION,
            run_id: RunId::from_raw("run_1"),
            kind: ItemKind::Hypothesis,
            title: "t".into(),
            content: content.into(),
            content_hash: ContentHash::of_str(content),
            hypothesis: None,
            parent_ids: vec![],
            derivation: None,
            source_ids: vec![],
            author: Author::Researcher,
            produced_by: None,
            goal_id: GoalId::from_raw("goal_1"),
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn overlap_detects_shared_vocabulary() {
        let a = item("mitochondrial dysfunction drives neuronal apoptosis in cortex");
        let b = item("mitochondrial dysfunction drives neuronal apoptosis in cortex");
        let c = item("galactic rotation curves imply unseen mass distribution");

        assert!((lexical_overlap(&a, &b) - 1.0).abs() < 1e-9);
        assert!(
            lexical_overlap(&a, &c) < 0.1,
            "unrelated items overlap little"
        );
    }

    #[test]
    fn candidate_pairs_are_bounded_and_ranked() {
        let items: Vec<_> = (0..5)
            .map(|i| item(&format!("mechanism alpha beta gamma variant {i}")))
            .collect();
        let pairs = candidate_pairs(&items, 3);
        assert_eq!(pairs.len(), 3, "fan-out must be bounded");
        assert!(pairs[0].2 >= pairs[1].2, "most similar first");
    }
}
