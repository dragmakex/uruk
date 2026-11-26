//! Explainable, deterministic metadata relevance after candidate fusion.
//! This is a lexical heuristic, not a semantic model or scientific verdict.
use super::{FusedWork, SearchQuery, dedup::normalize_title, types::RetrievalRanking};
use std::collections::BTreeSet;

fn terms(text: &str) -> BTreeSet<String> {
    normalize_title(text)
        .split_whitespace()
        .filter(|w| w.len() > 1)
        .map(str::to_owned)
        .collect()
}

/// Reorder by 75% best-query title/abstract overlap and 25% normalized RRF.
/// Titles count twice as much as abstracts; popularity, age and OA status
/// do not imply relevance or evidence. All inputs and components are saved.
pub fn rerank(works: &mut [FusedWork], queries: &[SearchQuery]) {
    let max_rrf = works.iter().map(|w| w.rrf_score).fold(0.0_f64, f64::max);
    let query_terms: Vec<_> = queries.iter().map(|q| terms(&q.text)).collect();
    for work in works.iter_mut() {
        let title = terms(&work.record.title);
        let abstract_terms = terms(work.record.abstract_text.as_deref().unwrap_or_default());
        let mut lexical = 0.0_f64;
        let mut matched = BTreeSet::new();
        let mut questions = BTreeSet::new();
        for (query, tokens) in queries.iter().zip(&query_terms) {
            if tokens.is_empty() {
                continue;
            }
            let title_matches = tokens.intersection(&title).count();
            let abstract_matches = tokens.intersection(&abstract_terms).count();
            lexical = lexical
                .max((2 * title_matches + abstract_matches) as f64 / (3 * tokens.len()) as f64);
            matched.extend(
                tokens
                    .iter()
                    .filter(|t| title.contains(*t) || abstract_terms.contains(*t))
                    .cloned(),
            );
            if (title_matches + abstract_matches) > 0
                && let Some(question) = &query.subquestion
            {
                questions.insert(question.clone());
            }
        }
        let score = 0.75 * lexical
            + if max_rrf > 0.0 {
                0.25 * work.rrf_score / max_rrf
            } else {
                0.0
            };
        let mut why = vec![if matched.is_empty() {
            "Connector-ranked candidate; no exact title/abstract term overlap. Inspect before using as evidence.".into()
        } else {
            format!(
                "Title/abstract match search terms: {}",
                matched.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        }];
        let connectors: BTreeSet<_> = work
            .record
            .hits
            .iter()
            .map(|h| h.connector.as_str())
            .collect();
        why.push(format!(
            "Found by: {}; raw RRF {:.6}; lexical relevance {:.3}",
            connectors.into_iter().collect::<Vec<_>>().join(", "),
            work.rrf_score,
            lexical
        ));
        why.push("Retrieval relevance only; this score does not validate a claim.".into());
        work.record.retrieval = Some(RetrievalRanking {
            method: "lexical_rrf_v1".into(),
            score,
            lexical_score: lexical,
            matched_terms: matched.into_iter().collect(),
            subquestions: questions.into_iter().collect(),
            why_this_source: why,
        });
    }
    works.sort_by(|a, b| {
        b.record
            .retrieval
            .as_ref()
            .unwrap()
            .score
            .total_cmp(&a.record.retrieval.as_ref().unwrap().score)
            .then_with(|| a.record.work_key.cmp(&b.record.work_key))
    });
}
