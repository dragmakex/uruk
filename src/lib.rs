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
pub mod store;
pub mod tools;

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
    #[error("permission denied: {0}")]
    Permission(String),
    #[error("budget exhausted: {0}")]
    Budget(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("provider: {0}")]
    Provider(String),
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    pub fn permission(msg: impl Into<String>) -> Self {
        Self::Permission(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
}
