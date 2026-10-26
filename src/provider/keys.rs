//! Reloadable API-key pool for one provider endpoint and model.
//!
//! Key files are deliberately boring: one `*.key` file per account, one key
//! per file. The directory is re-read for every logical completion cycle, so
//! operators can add a key without changing configuration or restarting Uruk.

use crate::{Error, Result};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ApiKey {
    id: String,
    secret: String,
}

impl ApiKey {
    pub(super) fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn secret(&self) -> &str {
        &self.secret
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKey")
            .field("id", &self.id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

pub(super) struct ApiKeyPool {
    directory: Option<PathBuf>,
    fallback: Vec<String>,
    cursor: AtomicUsize,
}

impl fmt::Debug for ApiKeyPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyPool")
            .field("directory", &self.directory)
            .field("fallback_keys", &self.fallback.len())
            .field("cursor", &self.cursor.load(Ordering::Relaxed))
            .finish()
    }
}

impl ApiKeyPool {
    pub(super) fn new(directory: Option<PathBuf>, fallback: Option<String>) -> Result<Self> {
        Self::with_fallback(directory, fallback.into_iter().collect())
    }

    pub(super) fn with_fallback(directory: Option<PathBuf>, fallback: Vec<String>) -> Result<Self> {
        let pool = Self {
            directory,
            fallback,
            cursor: AtomicUsize::new(0),
        };
        pool.load()?;
        Ok(pool)
    }

    /// A full key cycle starting at the next round-robin position.
    ///
    /// The cursor advances once whenever a cycle starts. A new logical call
    /// therefore starts on the next account, and if the provider gate retries
    /// after a fully failed cycle, that probe also resumes at the next account.
    /// Every cycle still gives each currently configured account one chance.
    pub(super) fn next_keys(&self) -> Result<Vec<ApiKey>> {
        let mut keys = self.load()?;
        if keys.len() > 1 {
            let start = self.cursor.fetch_add(1, Ordering::SeqCst) % keys.len();
            keys.rotate_left(start);
        }
        Ok(keys)
    }

    #[cfg(test)]
    pub(super) fn next_cycle(&self) -> Result<Vec<String>> {
        Ok(self
            .next_keys()?
            .into_iter()
            .map(|key| key.secret)
            .collect())
    }

    fn load(&self) -> Result<Vec<ApiKey>> {
        let mut keys = match &self.directory {
            Some(directory) => load_directory(directory)?,
            None => Vec::new(),
        };
        for (index, secret) in self.fallback.iter().enumerate() {
            validate_secret(secret, "configured API key")?;
            if !keys.iter().any(|key| key.secret == *secret) {
                keys.push(ApiKey {
                    id: format!("configured-{index}"),
                    secret: secret.clone(),
                });
            }
        }
        Ok(keys)
    }
}

fn load_directory(directory: &Path) -> Result<Vec<ApiKey>> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        Error::validation(format!(
            "cannot read API key directory {}: {error}",
            directory.display()
        ))
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::validation(format!(
                "cannot enumerate API key directory {}: {error}",
                directory.display()
            ))
        })?;
        let path = entry.path();
        let is_key = path.extension().and_then(|value| value.to_str()) == Some("key");
        let is_regular = entry.file_type().map_err(|error| {
            Error::validation(format!(
                "cannot inspect API key directory entry {}: {error}",
                path.display()
            ))
        })?;
        if is_key && is_regular.is_file() {
            paths.push(path);
        }
    }
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("<non-UTF-8 key file>")
                .to_string();
            let secret = std::fs::read_to_string(&path).map_err(|error| {
                Error::validation(format!("cannot read API key file {label}: {error}"))
            })?;
            let secret = secret.trim_end_matches(['\r', '\n']).to_string();
            validate_secret(&secret, &label)?;
            Ok(ApiKey { id: label, secret })
        })
        .collect()
}

fn validate_secret(secret: &str, source: &str) -> Result<()> {
    if secret.is_empty() || secret.chars().any(char::is_whitespace) {
        return Err(Error::validation(format!(
            "API key in {source} must not be empty or contain whitespace"
        )));
    }
    Ok(())
}
