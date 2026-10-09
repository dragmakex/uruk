//! Deterministic export of a run (SPEC §11, §9.1).
//!
//! "Reports do not require a model call." Everything here is derived from
//! persisted records, so budget exhaustion or a crash still permits a clearly
//! labelled partial report.

mod citations;
mod export;
mod pdf;

pub use citations::{CitationOrigin, CitationRecord, CitationResolution, collect_citations};
pub use export::{Export, REPORT_SECTIONS, REPORT_TEMPLATE_VERSION, export_run};
pub use pdf::render_report_pdf;
