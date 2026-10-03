//! Template loading and provenance (SPEC §5.1).

use super::ids;
use crate::records::ContentHash;
use crate::{Error, Result};
use std::collections::BTreeMap;

/// The published figure a template was adapted from (2025 Appendix A.2).
///
/// `None` marks a Uruk-authored template. SPEC §5.1: "do not label
/// Uruk-authored Supervisor, Proximity, or other missing templates as
/// published prompts."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFigure {
    pub figure: &'static str,
    pub page: u32,
}

/// One versioned prompt template.
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    pub id: &'static str,
    pub text: &'static str,
    /// Hash of the template text: the revision identity recorded on tasks.
    pub hash: ContentHash,
    /// Source figure when adapted from the preprint, else `None`.
    pub source: Option<SourceFigure>,
    /// Variables the template requires, parsed from its text.
    pub required_vars: Vec<String>,
}

impl PromptTemplate {
    /// Whether this template is an adaptation of a published example.
    pub fn is_published_adaptation(&self) -> bool {
        self.source.is_some()
    }

    /// Provenance string recorded on a task and shown in reports.
    pub fn provenance(&self) -> String {
        match self.source {
            Some(SourceFigure { figure, page }) => format!(
                "{}@{} (adapted from 2025 preprint {}, p. {})",
                self.id,
                self.hash.short(),
                figure,
                page
            ),
            None => format!("{}@{} (Uruk-authored)", self.id, self.hash.short()),
        }
    }
}

/// Templates are compiled in, so a run needs no external prompt directory.
macro_rules! templates {
    ($( $id:expr_2021 => ($file:expr_2021, $source:expr_2021) ),* $(,)?) => {
        &[$( ($id, include_str!(concat!("../../prompts/", $file)), $source) ),*]
    };
}

type TemplateDef = (&'static str, &'static str, Option<SourceFigure>);

const TEMPLATES: &[TemplateDef] = templates![
    ids::GENERATION_LITERATURE => ("generation.literature.md",
        Some(SourceFigure { figure: "Fig. A.1", page: 40 })),
    ids::GENERATION_DEBATE => ("generation.debate.md",
        Some(SourceFigure { figure: "Fig. A.2", page: 41 })),
    ids::REFLECTION_OBSERVATION => ("reflection.observation.md",
        Some(SourceFigure { figure: "Fig. A.3", page: 42 })),
    ids::RANKING_PAIRWISE => ("ranking.pairwise.md",
        Some(SourceFigure { figure: "Fig. A.4", page: 43 })),
    ids::RANKING_DEBATE => ("ranking.debate.md",
        Some(SourceFigure { figure: "Fig. A.5", page: 44 })),
    ids::EVOLUTION_FEASIBILITY => ("evolution.feasibility.md",
        Some(SourceFigure { figure: "Fig. A.6", page: 45 })),
    ids::EVOLUTION_ANALOGY => ("evolution.analogy.md",
        Some(SourceFigure { figure: "Fig. A.7", page: 45 })),
    ids::META_REVIEW => ("meta.review.md",
        Some(SourceFigure { figure: "Fig. A.8", page: 46 })),
    // Uruk-authored: the preprint publishes no example for these.
    ids::REFLECTION_INITIAL => ("reflection.initial.md", None),
    ids::REFLECTION_FULL => ("reflection.full.md", None),
    ids::REFLECTION_DEEP => ("reflection.deep.md", None),
    ids::REFLECTION_SIMULATION => ("reflection.simulation.md", None),
    ids::META_OVERVIEW => ("meta.overview.md", None),
    ids::PROXIMITY_SIMILARITY => ("proximity.similarity.md", None),
    ids::TASK_SYNTHESIS => ("task.synthesis.md", None),
    ids::SEARCH_PLAN_QUERIES => ("search.plan_queries.md", None),
];

/// All loaded templates, keyed by ID.
#[derive(Debug, Clone)]
pub struct PromptRegistry {
    templates: BTreeMap<&'static str, PromptTemplate>,
}

impl Default for PromptRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptRegistry {
    pub fn new() -> Self {
        let templates = TEMPLATES
            .iter()
            .map(|(id, text, source)| {
                (
                    *id,
                    PromptTemplate {
                        id,
                        text,
                        hash: ContentHash::of_str(text),
                        source: *source,
                        required_vars: super::render::extract_vars(text),
                    },
                )
            })
            .collect();
        Self { templates }
    }

    pub fn get(&self, id: &str) -> Result<&PromptTemplate> {
        self.templates
            .get(id)
            .ok_or_else(|| Error::not_found(format!("prompt template {id}")))
    }

    pub fn ids(&self) -> impl Iterator<Item = &&'static str> {
        self.templates.keys()
    }

    pub fn len(&self) -> usize {
        self.templates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// The eight published examples and their source figures (SPEC §13
    /// acceptance: "All eight published examples have source mappings").
    pub fn published_adaptations(&self) -> Vec<(&str, SourceFigure)> {
        self.templates
            .values()
            .filter_map(|t| t.source.map(|s| (t.id, s)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_eight_published_examples_have_source_mappings() {
        let reg = PromptRegistry::new();
        let published = reg.published_adaptations();
        assert_eq!(published.len(), 8, "got {published:?}");

        let figures: Vec<_> = published.iter().map(|(_, s)| s.figure).collect();
        for expected in [
            "Fig. A.1", "Fig. A.2", "Fig. A.3", "Fig. A.4", "Fig. A.5", "Fig. A.6", "Fig. A.7",
            "Fig. A.8",
        ] {
            assert!(figures.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn uruk_authored_templates_are_not_labelled_published() {
        let reg = PromptRegistry::new();
        for id in [
            ids::REFLECTION_INITIAL,
            ids::REFLECTION_FULL,
            ids::REFLECTION_DEEP,
            ids::REFLECTION_SIMULATION,
            ids::PROXIMITY_SIMILARITY,
            ids::META_OVERVIEW,
            ids::TASK_SYNTHESIS,
            ids::SEARCH_PLAN_QUERIES,
        ] {
            let t = reg.get(id).unwrap();
            assert!(
                !t.is_published_adaptation(),
                "{id} must not claim a source figure"
            );
            assert!(t.provenance().contains("Uruk-authored"));
        }
    }

    #[test]
    fn template_hash_is_the_revision_identity() {
        let reg = PromptRegistry::new();
        let a = reg.get(ids::RANKING_PAIRWISE).unwrap();
        assert_eq!(a.hash, ContentHash::of_str(a.text));
        assert_ne!(a.hash, reg.get(ids::RANKING_DEBATE).unwrap().hash);
    }

    #[test]
    fn every_template_declares_its_variables() {
        let reg = PromptRegistry::new();
        for id in reg.ids() {
            let t = reg.get(id).unwrap();
            assert!(!t.required_vars.is_empty(), "{id} binds no variables");
            assert!(
                t.required_vars.iter().any(|v| v == "goal"),
                "{id} must bind {{goal}}"
            );
        }
    }
}
