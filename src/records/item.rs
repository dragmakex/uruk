//! Research items: immutable content with parent lineage (SPEC §4.2).

use super::evidence::Disposition;
use super::ids::*;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// What kind of research item this is (SPEC §4.2 `ResearchItem`).
///
/// SPEC §4.2 warns against forcing hypothesis fields onto a bibliography or
/// manuscript draft, so the hypothesis-specific payload is separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Question,
    Hypothesis,
    Proposal,
    Analysis,
    Synthesis,
    Manuscript,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Hypothesis => "hypothesis",
            Self::Proposal => "proposal",
            Self::Analysis => "analysis",
            Self::Synthesis => "synthesis",
            Self::Manuscript => "manuscript",
        }
    }

    /// Whether items of this kind compete in a tournament (SPEC §5 Ranking).
    ///
    /// "A standalone analysis or report need not acquire an Elo rating."
    pub fn is_rankable(self) -> bool {
        matches!(self, Self::Hypothesis | Self::Proposal)
    }
}

/// Who authored an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "by", content = "detail")]
pub enum Author {
    /// Produced by a Uruk role using a named strategy.
    Agent { role: String, strategy: String },
    /// Supplied by the researcher.
    Researcher,
    /// Imported from a source without modification.
    Imported(SourceId),
}

/// The hypothesis-specific payload (SPEC §4.2).
///
/// "A hypothesis additionally states its claim, explanatory mechanism,
/// assumptions, scope, testable predictions, potential falsifiers, and
/// proposed validation."
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HypothesisFields {
    pub claim: String,
    pub mechanism: String,
    pub assumptions: Vec<String>,
    pub scope: String,
    pub predictions: Vec<String>,
    pub falsifiers: Vec<String>,
    pub validation_plan: String,
}

/// An immutable research item version (SPEC §4.2).
///
/// Content never changes. Revision or evolution creates a new item with parent
/// links; reviews, assessments, and ratings are separate records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchItem {
    pub id: ItemId,
    pub schema_version: u32,
    pub run_id: RunId,
    pub kind: ItemKind,
    /// Short title for listings and reports.
    pub title: String,
    /// Full content. For a hypothesis this is the prose exposition.
    pub content: String,
    /// Present only for hypotheses and proposals.
    pub hypothesis: Option<HypothesisFields>,
    /// Items this was derived from (SPEC §5 Evolution: never edit a parent).
    pub parent_ids: Vec<ItemId>,
    /// What changed relative to the parents, and why.
    pub derivation: Option<String>,
    pub source_ids: Vec<SourceId>,
    pub author: Author,
    /// The task that produced it.
    pub produced_by: Option<TaskId>,
    /// Goal revision this item was created under.
    pub goal_id: GoalId,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Content hash, so a reviewer can confirm which version they judged.
    pub content_hash: ContentHash,
}

impl ResearchItem {
    /// Whether this item was derived from another.
    pub fn is_child(&self) -> bool {
        !self.parent_ids.is_empty()
    }
}

/// Mutable workflow state for an item, kept separate from its content.
///
/// SPEC §4.3: workflow disposition is separate from scientific assessment, and
/// item content is immutable, so this lives in its own row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemState {
    pub item_id: ItemId,
    pub disposition: Disposition,
    /// Why the item is in this disposition.
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}
