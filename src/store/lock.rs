//! Single-writer project lock (SPEC §9.2).
//!
//! "One scheduler process owns a project at a time, enforced by an OS-held
//! project lock. Other CLI invocations may inspect state or submit durable
//! control requests; they must not start a competing scheduler."

use crate::{Error, Result};
use fs4::fs_std::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// An OS-held exclusive lock on a project directory.
///
/// The lock is released when this value is dropped, or by the OS if the
/// process dies — so a crashed scheduler does not leave the project wedged.
#[derive(Debug)]
pub struct ProjectLock {
    file: File,
    path: PathBuf,
}

impl ProjectLock {
    /// Try to acquire the scheduler lock for `project_dir`.
    ///
    /// Returns `Err(Error::Permission)` if another process holds it, rather
    /// than blocking, so a second `uruk run` fails fast with a clear message.
    pub fn acquire(project_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = project_dir.as_ref().join(".uruk");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("scheduler.lock");

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;

        // fs4 signals contention as `WouldBlock`; anything else is a real
        // I/O failure and must not be reported as "someone else holds it".
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {
                // Record the owning pid for a useful error message elsewhere.
                use std::io::Write;
                let mut f = &file;
                let _ = f.set_len(0);
                let _ = write!(f, "{}", std::process::id());
                let _ = f.flush();
                Ok(Self { file, path })
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == Some(libc_ewouldblock()) =>
            {
                Err(Error::permission(format!(
                    "another Uruk scheduler already owns this project (lock: {}). \
                     Use `uruk status` to inspect, or stop the running scheduler.",
                    path.display()
                )))
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `EWOULDBLOCK`, which on macOS and Linux is the errno for a held flock.
fn libc_ewouldblock() -> i32 {
    35
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquisition_is_refused_then_allowed_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = ProjectLock::acquire(dir.path()).unwrap();
        // Same process, second handle: the OS lock is per file handle here,
        // so this exercises the refusal path.
        let second = ProjectLock::acquire(dir.path());
        assert!(second.is_err(), "second lock should be refused");
        drop(first);
        assert!(
            ProjectLock::acquire(dir.path()).is_ok(),
            "lock should free on drop"
        );
    }
}
