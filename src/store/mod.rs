//! Durable state: SQLite through sqlx (SPEC §9.2).
//!
//! This module is the single controlled write path for scientific records.
//! Model agents return *proposed* records; nothing here accepts a write that
//! has not passed the §4.3 and §7 gates.

mod lock;
mod owners;
mod passages;
mod queries;
mod search;
mod tasks;
#[cfg(test)]
mod tests;
mod uploads;
mod writes;

pub use lock::ProjectLock;
pub use owners::{OwnerDigest, SourceFilter};
pub use passages::{OwnedPassageHit, PassageHit, StoredPassage, sanitize_match_query};
pub use queries::{ItemSummary, RunOverview};
pub use search::StoredWork;
pub use tasks::BudgetUsage;
pub use uploads::Upload;
pub use writes::RecordBatch;

use crate::records::RunId;
use crate::{Error, Result};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Relative location of the state database within a project (SPEC §9.2).
pub const STATE_DB_RELATIVE: &str = ".uruk/state.sqlite";

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Handle to the durable store.
#[derive(Clone, Debug)]
pub struct Store {
    pool: SqlitePool,
    /// Serializes write transactions within this process so concurrent tasks
    /// queue on a mutex rather than on SQLite's busy handler.
    write_lock: Arc<Mutex<()>>,
    /// The project directory: `runs/` and artifacts live under it, and stored
    /// artifact paths are relative to it so a moved project still resolves.
    project_dir: PathBuf,
}

/// A write transaction holding the single-writer lock.
pub(crate) struct WriteTxn<'a> {
    tx: sqlx::Transaction<'a, sqlx::Sqlite>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl WriteTxn<'_> {
    /// The underlying connection, inside the transaction.
    pub(crate) fn conn(&mut self) -> &mut sqlx::SqliteConnection {
        &mut self.tx
    }

    pub(crate) async fn commit(self) -> Result<()> {
        self.tx.commit().await?;
        Ok(())
    }
}

impl Store {
    /// Open (creating if absent) the store at `path` and run migrations.
    ///
    /// `path` is `<project>/.uruk/state.sqlite`; the project directory is
    /// derived from it.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let project_dir = absolute
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", absolute.display()))
            .map_err(Error::Storage)?
            .create_if_missing(true)
            // WAL keeps readers (`uruk status`) from blocking the scheduler.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Full)
            .foreign_keys(true)
            // Another process (`uruk approve`, `uruk stop`) may write at
            // the same time. Write transactions begin with `BEGIN IMMEDIATE`
            // (see `begin_write`) so a second writer waits on this timeout
            // instead of failing with SQLITE_BUSY_SNAPSHOT after reading.
            .busy_timeout(std::time::Duration::from_secs(30));

        Self::from_options(opts, project_dir).await
    }

    /// Open an in-memory store, for tests.
    pub async fn open_in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .map_err(Error::Storage)?
            .foreign_keys(true);
        // A single connection: each new connection would get a fresh database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(opts)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self {
            pool,
            write_lock: Arc::new(Mutex::new(())),
            project_dir: std::env::temp_dir().join(format!("uruk-mem-{}", uuid::Uuid::new_v4())),
        })
    }

    async fn from_options(opts: SqliteConnectOptions, project_dir: PathBuf) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self {
            pool,
            write_lock: Arc::new(Mutex::new(())),
            project_dir,
        })
    }

    /// Begin a write transaction, serialized against other writers.
    ///
    /// `BEGIN IMMEDIATE` takes SQLite's write lock up front. A deferred
    /// transaction that reads and then writes fails with
    /// `SQLITE_BUSY_SNAPSHOT` when another process committed in between, and
    /// no busy timeout retries that. SPEC §9.2 requires short transactions
    /// that never span an external call, so this never blocks on a model or
    /// subprocess wait.
    pub(crate) async fn begin_write(&self) -> Result<WriteTxn<'_>> {
        let guard = self.write_lock.clone().lock_owned().await;
        let tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Ok(WriteTxn { tx, _guard: guard })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// The project directory this store belongs to.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    /// Stable project identity: the id is derived from the canonical
    /// project directory (as resolved by [`Store::open`]), so the CLI and
    /// the web API agree on which project row a run belongs to regardless
    /// of how the path was spelled on the command line.
    pub fn project_identity(&self) -> (crate::records::ProjectId, String) {
        let name = self.project_dir.to_string_lossy().into_owned();
        let id = crate::records::ProjectId::from_raw(format!(
            "prj_{}",
            crate::records::ContentHash::of_str(&name).short()
        ));
        (id, name)
    }

    /// Where a run's exported files live: `<project>/runs/<run-id>/`.
    pub fn run_dir(&self, run_id: &RunId) -> PathBuf {
        self.project_dir.join("runs").join(run_id.as_str())
    }

    /// Where a run's artifacts are written.
    pub fn artifact_dir(&self, run_id: &RunId) -> PathBuf {
        self.run_dir(run_id).join("artifacts")
    }

    /// Resolve a stored artifact path. Paths are stored relative to the
    /// project so exports survive a move; absolute paths are used as-is.
    pub fn resolve_path(&self, stored: &str) -> PathBuf {
        let p = Path::new(stored);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.project_dir.join(p)
        }
    }

    /// The form in which an artifact path is stored: relative to the project
    /// when it lies inside it.
    pub fn storable_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.project_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// Write `bytes` to `path` so that a crash leaves either the complete file
/// or nothing (SPEC §9.2: finalize artifacts before committing references).
///
/// Temporary file, flush, fsync, rename, then fsync the directory.
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let dir = path.parent().unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(dir).await?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".to_string());
    let tmp = path.with_file_name(format!("{name}.partial"));

    {
        let mut file = tokio::fs::File::create(&tmp).await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
    }
    tokio::fs::rename(&tmp, path).await?;

    // The rename is only durable once the directory entry is.
    #[cfg(unix)]
    {
        if let Ok(d) = tokio::fs::File::open(dir).await {
            let _ = d.sync_all().await;
        }
    }
    Ok(())
}

/// Current time, truncated to whole microseconds so a value round-trips
/// through RFC-3339 storage unchanged.
pub(crate) fn now() -> time::OffsetDateTime {
    let t = time::OffsetDateTime::now_utc();
    t.replace_nanosecond(t.microsecond() * 1_000).unwrap_or(t)
}

pub(crate) fn to_rfc3339(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| t.unix_timestamp().to_string())
}

pub(crate) fn from_rfc3339(s: &str) -> Result<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .map_err(|e| Error::validation(format!("bad timestamp {s:?}: {e}")))
}
