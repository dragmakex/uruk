//! Source ingestion, text extraction, and approved tool execution (SPEC §8, §12).
//!
//! Every tool declares its schemas, permissions, and limits. Its result is
//! structured evidence plus artifact references, never an unexamined prose
//! verdict.

pub mod exec;
pub mod extract;
pub mod ingest;

pub use exec::{Containment, ExecOutcome, ExecRequest, detect_containment, execute_analysis};
pub use extract::{Coverage, Extracted};
pub use ingest::{Ingested, MAX_RETRIEVAL_BYTES, ingest_input, ingest_upload};
