//! Durable task execution (SPEC §6, §9).
//!
//! Tokio supplies concurrency, not durability: the persistence and recovery
//! guarantees are implemented here and in [`crate::store`]. This is the minimum
//! application-specific runtime required by SPEC §6, not a workflow engine.

mod control;
mod executor;
mod limits;
pub mod scheduler;

pub use control::{StopOutcome, request_stop};
pub use executor::{TaskOutcome, execute_task};
pub use limits::ConcurrencyLimits;
pub use scheduler::FINAL_OUTPUT_CALLS;
pub use scheduler::{RunSummary, Scheduler, SchedulerConfig};
