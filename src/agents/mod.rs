//! Research policy: the Supervisor and six specialized roles (SPEC §5).
//!
//! Policy code stays independent of runtime types (SPEC §3): roles receive a
//! provider, a store handle, and versioned records, and return proposed
//! records for the controlled write path to commit.

pub mod context;
pub mod evolution;
pub mod generation;
pub mod meta_review;
pub mod outputs;
pub mod proximity;
pub mod ranking;
pub mod reflection;
pub mod safety;
pub mod supervisor;

pub use context::{AgentContext, Completion};
