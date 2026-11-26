//! Typed replay instructions and the measured outcome of a verification.
use super::ids::{ContentHash, ExperimentId};
use serde::{Deserialize, Serialize};

/// Exact argv and limits. Display commands are never parsed for execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub program: String,
    pub args: Vec<String>,
    pub inputs: Vec<String>,
    pub timeout_secs: u64,
    pub max_output_bytes: usize,
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproductionVerdict {
    Reproduced,
    NotReproduced,
    Inconclusive,
}

impl ReproductionVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reproduced => "reproduced",
            Self::NotReproduced => "not_reproduced",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// A replay result, not an assessment of the scientific claim or independence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reproduction {
    pub original_experiment_id: ExperimentId,
    pub verdict: ReproductionVerdict,
    pub reason: String,
    pub original_stdout_hash: Option<ContentHash>,
    pub rerun_stdout_hash: Option<ContentHash>,
}
