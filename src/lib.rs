//! Uruk — an autonomous research engine whose output is evidence-backed findings.
//!
//! See `docs/SPEC.md`. Module layout follows the ownership boundaries of SPEC §3:
//! research policy ([`agents`]) stays independent of runtime types
//! ([`runtime`]), records ([`records`]) are the shared contracts, and
//! [`store`] is the single controlled write path.

pub mod agents;
pub mod prompts;
pub mod provider;
pub mod records;
pub mod report;
pub mod runtime;
pub mod search;
pub mod store;
pub mod tools;
#[cfg(feature = "web")]
pub mod web;

/// Crate-wide error type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that cross module boundaries.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("storage: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization: {0}")]
    Json(#[from] serde_json::Error),
    #[error("assessment rejected: {0}")]
    Assessment(#[from] records::AssessmentError),
    #[error("validation: {0}")]
    Validation(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("permission denied: {0}")]
    Permission(String),
    #[error("budget exhausted: {0}")]
    Budget(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("provider: {0}")]
    Provider(String),
    #[error("search: {0}")]
    Search(String),
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    pub fn permission(msg: impl Into<String>) -> Self {
        Self::Permission(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// Stable error category, so a calling agent or API client can branch
    /// without parsing prose. The CLI `--json` output and the web API both
    /// report this exact string as `kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Storage(_) | Error::Migration(_) => "storage",
            Error::Io(_) => "io",
            Error::Json(_) => "serialization",
            Error::Assessment(_) => "assessment_rejected",
            Error::Validation(_) => "validation",
            Error::Conflict(_) => "conflict",
            Error::Permission(_) => "permission",
            Error::Budget(_) => "budget",
            Error::NotFound(_) => "not_found",
            Error::Provider(_) => "provider",
            Error::Search(_) => "search",
            Error::Cancelled => "cancelled",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn error_kinds_are_stable_strings() {
        assert_eq!(Error::validation("x").kind(), "validation");
        assert_eq!(Error::not_found("x").kind(), "not_found");
        assert_eq!(Error::permission("x").kind(), "permission");
        assert_eq!(Error::Budget("x".into()).kind(), "budget");
        assert_eq!(Error::Cancelled.kind(), "cancelled");
    }
}
