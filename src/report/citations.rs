//! Honest structured citation inspection (docs/WEB.md, SPEC §4.3, §11).
//!
//! Collects the citations a run has persisted — the synthesis
//! deliverable's claim citations and the Reflection reviews' observation
//! locators — and resolves each one against the run's recorded sources.
//! A resolution is exactly one of three honest shapes:
//!
//! - **span**: the exact bytes a `Locator::Span` denotes in the canonical
//!   extracted-text artifact, served only after the artifact bytes still
//!   hash to the recorded [`crate::records::ContentHash`] and both offsets
//!   fall on UTF-8 character boundaries;
//! - **source_locator**: the citation names a recorded source at a coarse
//!   locator (page, section, …) that cannot be mapped to exact bytes;
//! - **unresolved**: the citation cannot be verified, with the reason.
//!
//! Nothing here fabricates a resolution: a drifted artifact, an unknown
//! source id, or a span off a character boundary is reported as
//! unresolved, never repaired or approximated. All byte-offset slicing
//! happens here in Rust against the canonical UTF-8 text — clients render
//! the resolved text and never slice offsets themselves.

use super::export::load_deliverable;
use crate::Result;
use crate::agents::outputs::ClaimBasis;
use crate::records::{ContentHash, Locator, ReviewId, RunId, Source};
use crate::store::Store;
use serde::Serialize;
use std::collections::BTreeMap;

/// Longest span excerpt served inline, in bytes of whole characters. A
/// longer span is truncated at a character boundary and flagged, so the
/// served text is still an exact prefix of the cited bytes.
const MAX_EXCERPT_BYTES: usize = 2_048;

/// Where a persisted citation comes from.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CitationOrigin {
    /// A citation on one claim of the synthesis deliverable.
    DeliverableClaim { basis: ClaimBasis },
    /// An observation recorded by a Reflection review (SPEC §15.4).
    ReviewObservation { review_id: ReviewId },
}

/// What a citation honestly resolves to. See the module doc for the
/// three-way contract.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CitationResolution {
    /// The exact bytes of the span, verified against the recorded hash.
    Span {
        start: usize,
        end: usize,
        /// The resolved text; an exact prefix of the span when `truncated`.
        text: String,
        truncated: bool,
    },
    /// Resolves to the recorded source at a coarse locator, not to bytes.
    SourceLocator,
    /// Cannot be verified; `reason` states why, honestly.
    Unresolved { reason: String },
}

/// One persisted citation with its honest resolution.
#[derive(Debug, Clone, Serialize)]
pub struct CitationRecord {
    pub origin: CitationOrigin,
    /// The claim or observation text the citation grounds.
    pub claim: String,
    /// The cited source id as recorded — kept verbatim even when unknown.
    pub source_id: String,
    /// The recorded source's title, when the source is known and titled.
    pub source_title: Option<String>,
    /// Human-facing locator text, when the citation carries one.
    pub locator: Option<String>,
    pub resolution: CitationResolution,
}

/// Collect and resolve every persisted citation of `run_id`: deliverable
/// claim citations first (claim order), then review observations (review
/// order). Only this run's records are read; resolution failures are data
/// (`unresolved`), not errors.
///
/// # Errors
///
/// Fails only on storage errors while reading the run's records.
pub async fn collect_citations(store: &Store, run_id: &RunId) -> Result<Vec<CitationRecord>> {
    let sources = store.list_sources(run_id).await?;
    let by_id: BTreeMap<&str, &Source> = sources.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut out = Vec::new();

    let artifacts = store.list_artifacts(run_id).await?;
    if let Some(deliverable) = load_deliverable(store, &artifacts).await {
        for claim in &deliverable.claims {
            for grounding in &claim.citations {
                let locator = grounding.locator.trim();
                out.push(
                    resolve_citation(
                        store,
                        &by_id,
                        CitationOrigin::DeliverableClaim { basis: claim.basis },
                        claim.claim.clone(),
                        &grounding.source_id,
                        (!locator.is_empty()).then(|| locator.to_string()),
                        parse_span_locator(locator),
                    )
                    .await,
                );
            }
        }
    }

    for review in store.list_reviews(run_id).await? {
        for obs in &review.observations {
            let span = match obs.locator {
                Locator::Span { start, end } => Some((start, end)),
                _ => None,
            };
            out.push(
                resolve_citation(
                    store,
                    &by_id,
                    CitationOrigin::ReviewObservation {
                        review_id: review.id.clone(),
                    },
                    obs.observation.clone(),
                    obs.source_id.as_str(),
                    Some(obs.locator.render()),
                    span,
                )
                .await,
            );
        }
    }

    Ok(out)
}

/// Resolve one citation against the run's recorded sources.
async fn resolve_citation(
    store: &Store,
    sources_by_id: &BTreeMap<&str, &Source>,
    origin: CitationOrigin,
    claim: String,
    source_id: &str,
    locator: Option<String>,
    span: Option<(usize, usize)>,
) -> CitationRecord {
    let source = sources_by_id.get(source_id).copied();
    let resolution = match (source, span) {
        (None, _) => CitationResolution::Unresolved {
            reason: format!("the cited source {source_id} is not recorded by this run"),
        },
        (Some(_), None) => CitationResolution::SourceLocator,
        (Some(source), Some((start, end))) => resolve_span(store, source, start, end).await,
    };
    CitationRecord {
        origin,
        claim,
        source_id: source_id.to_string(),
        source_title: source.and_then(|s| s.title.clone()),
        locator,
        resolution,
    }
}

/// Resolve a byte span against a source's canonical text artifact,
/// verifying integrity before serving any bytes.
async fn resolve_span(
    store: &Store,
    source: &Source,
    start: usize,
    end: usize,
) -> CitationResolution {
    let Some(artifact_id) = &source.text_artifact else {
        return unresolved("the source has no extracted-text artifact to resolve the span against");
    };
    let Ok(artifact) = store.get_artifact(artifact_id).await else {
        return unresolved("the extracted-text artifact record is missing");
    };
    if artifact.run_id != source.run_id {
        // A source may only cite its own run's artifact; anything else is
        // a corrupt record, not a resolvable citation.
        return unresolved("the extracted-text artifact belongs to a different run");
    }
    let Ok(bytes) = tokio::fs::read(store.resolve_path(&artifact.storage_path)).await else {
        return unresolved("the extracted-text artifact file is unavailable");
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return unresolved("the extracted-text artifact is not valid UTF-8");
    };
    if ContentHash::of_str(&text) != artifact.content_hash {
        return unresolved(
            "the artifact bytes no longer match the recorded content hash, \
             so the cited span cannot be trusted",
        );
    }
    slice_span(&text, start, end)
}

fn unresolved(reason: &str) -> CitationResolution {
    CitationResolution::Unresolved {
        reason: reason.to_string(),
    }
}

/// Slice a verified artifact text at exact byte offsets. Pure; the only
/// place span bytes are turned into served text.
fn slice_span(text: &str, start: usize, end: usize) -> CitationResolution {
    if start > end || end > text.len() {
        return CitationResolution::Unresolved {
            reason: format!(
                "span {start}..{end} lies outside the {}-byte artifact text",
                text.len()
            ),
        };
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return CitationResolution::Unresolved {
            reason: format!(
                "span {start}..{end} does not start and end on a UTF-8 character boundary"
            ),
        };
    }
    let span = &text[start..end];
    let (excerpt, truncated) = if span.len() > MAX_EXCERPT_BYTES {
        (&span[..last_char_boundary(span, MAX_EXCERPT_BYTES)], true)
    } else {
        (span, false)
    };
    CitationResolution::Span {
        start,
        end,
        text: excerpt.to_string(),
        truncated,
    }
}

/// The largest char boundary `<= at` within `text`.
fn last_char_boundary(text: &str, at: usize) -> usize {
    let mut i = at.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Strictly parse the rendered form of [`Locator::Span`] (`chars N..M`),
/// the only locator string that may be byte-resolved. Anything else —
/// extra text, signs, spaces — is not a span claim and resolves as a
/// coarse locator instead.
fn parse_span_locator(raw: &str) -> Option<(usize, usize)> {
    let rest = raw.strip_prefix("chars ")?;
    let (start, end) = rest.split_once("..")?;
    let strictly_decimal = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<usize>().ok())
            .flatten()
    };
    Some((strictly_decimal(start)?, strictly_decimal(end)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod parse_span_locator {
        use super::*;

        #[test]
        fn inverts_the_span_render_exactly() {
            let rendered = Locator::Span { start: 4, end: 24 }.render();
            assert_eq!(parse_span_locator(&rendered), Some((4, 24)));
        }

        #[test]
        fn rejects_anything_but_the_strict_form() {
            for raw in [
                "p. 3",
                "chars 4..24 extra",
                "chars 4..",
                "chars ..24",
                "chars -4..24",
                "chars 4..24.5",
                "chars  4..24",
                "CHARS 4..24",
                "",
            ] {
                assert_eq!(parse_span_locator(raw), None, "{raw:?}");
            }
        }
    }

    mod slice_span {
        use super::*;

        const TEXT: &str = "Ηρώ: θερμική ολίσθηση";

        #[test]
        fn serves_the_exact_bytes_of_a_boundary_aligned_span() {
            let start = TEXT.find("θερμική").expect("present");
            let end = start + "θερμική".len();
            assert_eq!(
                slice_span(TEXT, start, end),
                CitationResolution::Span {
                    start,
                    end,
                    text: "θερμική".into(),
                    truncated: false,
                }
            );
        }

        #[test]
        fn a_mid_character_offset_is_unresolved() {
            assert!(!TEXT.is_char_boundary(1), "byte 1 splits the Eta");
            let resolution = slice_span(TEXT, 1, 5);
            assert!(
                matches!(
                    &resolution,
                    CitationResolution::Unresolved { reason }
                        if reason.contains("character boundary")
                ),
                "{resolution:?}"
            );
        }

        #[test]
        fn an_out_of_bounds_span_is_unresolved() {
            let resolution = slice_span(TEXT, 0, TEXT.len() + 1);
            assert!(
                matches!(
                    &resolution,
                    CitationResolution::Unresolved { reason } if reason.contains("outside")
                ),
                "{resolution:?}"
            );
        }

        #[test]
        fn an_inverted_span_is_unresolved() {
            assert!(matches!(
                slice_span(TEXT, 5, 0),
                CitationResolution::Unresolved { .. }
            ));
        }

        #[test]
        fn an_oversized_span_is_truncated_on_a_character_boundary() {
            let text = "é".repeat(2_000); // 4 000 bytes of 2-byte chars
            let resolution = slice_span(&text, 0, text.len());
            let CitationResolution::Span {
                text: excerpt,
                truncated,
                ..
            } = resolution
            else {
                panic!("expected a span, got {resolution:?}");
            };
            assert!(truncated);
            assert!(excerpt.len() <= MAX_EXCERPT_BYTES);
            assert!(text.starts_with(&excerpt), "an exact prefix is served");
        }
    }
}
