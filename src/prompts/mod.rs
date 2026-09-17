//! Versioned prompt templates (SPEC §5.1, §15).
//!
//! Templates are text assets loaded by Rust, not a scripting runtime. Each
//! rendered task records the template ID, its content hash, and the rendered
//! input, so a completed output is never reinterpreted under a changed prompt.

mod registry;
mod render;

pub use registry::{PromptRegistry, PromptTemplate, SourceFigure};
pub use render::{Bindings, RenderError, Rendered, render};

/// Template IDs. These are the contract between roles and prompt assets.
pub mod ids {
    pub const GENERATION_LITERATURE: &str = "generation.literature";
    pub const GENERATION_DEBATE: &str = "generation.debate";
    pub const REFLECTION_INITIAL: &str = "reflection.initial";
    pub const REFLECTION_FULL: &str = "reflection.full";
    pub const REFLECTION_DEEP: &str = "reflection.deep";
    pub const REFLECTION_OBSERVATION: &str = "reflection.observation";
    pub const REFLECTION_SIMULATION: &str = "reflection.simulation";
    pub const RANKING_PAIRWISE: &str = "ranking.pairwise";
    pub const RANKING_DEBATE: &str = "ranking.debate";
    pub const EVOLUTION_FEASIBILITY: &str = "evolution.feasibility";
    pub const EVOLUTION_ANALOGY: &str = "evolution.analogy";
    pub const META_REVIEW: &str = "meta.review";
    pub const META_OVERVIEW: &str = "meta.overview";
    pub const PROXIMITY_SIMILARITY: &str = "proximity.similarity";
    pub const TASK_SYNTHESIS: &str = "task.synthesis";
}
