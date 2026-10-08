//! Stable record identity and content hashing (SPEC §4.2).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// Schema version stamped on every persisted record (SPEC §4.2).
pub const SCHEMA_VERSION: u32 = 1;

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident, $prefix:literal) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Mint a fresh random identifier.
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, uuid::Uuid::new_v4().simple()))
            }

            /// Wrap an existing string (e.g. read back from storage or API input).
            pub fn from_raw(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
    };
}

id_type!(/// Identifies a run (one invocation of a goal).
    RunId, "run");
id_type!(/// Identifies a project (a durable workspace across runs).
    ProjectId, "prj");
id_type!(/// Identifies a goal revision (SPEC §4.1).
    GoalId, "goal");
id_type!(/// Identifies a plan revision produced by the Supervisor.
    PlanId, "plan");
id_type!(/// Identifies a retrieved or supplied source (SPEC §4.2).
    SourceId, "src");
id_type!(/// Identifies an immutable research item version (SPEC §4.2).
    ItemId, "item");
id_type!(/// Identifies an evidence record.
    EvidenceId, "evd");
id_type!(/// Identifies a review record.
    ReviewId, "rev");
id_type!(/// Identifies an experiment record.
    ExperimentId, "exp");
id_type!(/// Identifies a tournament match.
    MatchId, "mtch");
id_type!(/// Identifies a decision record.
    DecisionId, "dec");
id_type!(/// Identifies a scheduled task.
    TaskId, "task");
id_type!(/// Identifies a stored artifact.
    ArtifactId, "art");
id_type!(/// Identifies a human approval request.
    RequestId, "req");
id_type!(/// Identifies a proximity cluster.
    ClusterId, "clus");
id_type!(/// Identifies a meta-review feedback version.
    FeedbackId, "fb");
id_type!(/// Identifies one executed literature search (connector × query).
    SearchId, "sch");

/// A SHA-256 content hash, rendered lowercase hex.
///
/// Used for artifact identity, prompt template revisions, and input versioning
/// so that a task records exactly which bytes it consumed (SPEC §4.2, §8).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(pub String);

impl ContentHash {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(hex::encode(hasher.finalize()))
    }

    pub fn of_str(s: &str) -> Self {
        Self::of_bytes(s.as_bytes())
    }

    /// Hash a value by its canonical JSON form.
    ///
    /// `serde_json` orders struct fields by declaration and (with the
    /// `preserve_order` feature off) maps by key, so this is stable for a
    /// fixed schema version.
    pub fn of_json<T: Serialize>(value: &T) -> Result<Self, serde_json::Error> {
        Ok(Self::of_bytes(serde_json::to_vec(value)?.as_slice()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// First 12 hex characters, for logs and human-facing reports.
    pub fn short(&self) -> &str {
        &self.0[..self.0.len().min(12)]
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_prefixed() {
        let a = ItemId::new();
        let b = ItemId::new();
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("item_"), "got {a}");
    }

    #[test]
    fn hash_is_stable_and_distinguishing() {
        assert_eq!(ContentHash::of_str("abc"), ContentHash::of_str("abc"));
        assert_ne!(ContentHash::of_str("abc"), ContentHash::of_str("abd"));
        // Known SHA-256 of "abc".
        assert_eq!(
            ContentHash::of_str("abc").as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(ContentHash::of_str("abc").short(), "ba7816bf8f01");
    }
}
