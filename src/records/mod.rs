//! Research records: the data contracts of SPEC §4.
//!
//! Item content is immutable; reviews, assessments, dispositions, and ratings
//! are separate records, so new evidence never rewrites what was proposed.

pub mod evidence;
pub mod goal;
pub mod ids;
pub mod item;
pub mod review;
pub mod run;
pub mod source;
pub mod task;
pub mod tournament;

pub use evidence::{
    Assessment, AssessmentError, AssessmentRecord, Disposition, Evidence, EvidenceKind,
    EvidenceLink, EvidenceRelation, validate_assessment,
};
pub use goal::{Budget, Goal, InputRef, Mode, Permissions, Plan, Rubric, StopCondition};
pub use ids::*;
pub use item::{Author, HypothesisFields, ItemKind, ItemState, ResearchItem};
pub use review::{
    ExplanatoryLabel, JudgeReviewView, Objection, ObservationNote, Review, ReviewStrategy,
};
pub use run::{CampaignSnapshot, Cluster, Critique, Feedback, Run, RunState, StrategyOutcome};
pub use source::{AccessLevel, Locator, Origin, SearchRecord, Source};
pub use task::{
    AccessClass, Actor, ApprovalRequest, ApprovalState, Artifact, CostActual, CostReservation,
    Decision, DecisionKind, ExecutionRecord, Experiment, Role, Task, TaskState,
};
pub use tournament::{
    DEFAULT_K, INITIAL_ELO, Match, MatchMethod, MatchOutcome, Rating, apply_elo, expected_score,
};
