//! Deterministic export of a run (SPEC §11, §9.1).
//!
//! "Reports do not require a model call." Everything here is derived from
//! persisted records, so budget exhaustion or a crash still permits a clearly
//! labelled partial report.

mod export;

pub use export::{Export, export_run};
