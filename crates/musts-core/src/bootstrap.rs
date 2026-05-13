//! First-run bootstrap and cross-process lock per `docs/PLAN.md` §4.5.1.
//!
//! Order of operations for any state-writing command:
//! 1. Ensure `<workspace>/.musts/` exists (mkdir_p).
//! 2. Open-or-create `<workspace>/.musts/.lock` (never `create_new`).
//! 3. `try_lock_exclusive` on the lock handle. On contention → exit 2.
//! 4. Open `<workspace>/.musts/state.sqlite` and migrate.
//! 5. Do the work. Drop the handle on exit; the OS releases the lock.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::error::{Error, Result};
use crate::state::{open as open_db, Db};

/// Live state-writing session: holds the lock and an open SQLite handle.
/// Dropping releases the lock; SQLite WAL files persist as usual.
pub struct StateSession {
    pub workspace_root: PathBuf,
    pub musts_dir: PathBuf,
    pub db: Db,
    // Held for the life of the session — drops release the file lock.
    _lock: File,
}

impl std::fmt::Debug for StateSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateSession")
            .field("workspace_root", &self.workspace_root)
            .field("musts_dir", &self.musts_dir)
            .finish_non_exhaustive()
    }
}

impl StateSession {
    /// Acquire the lock and open the state DB. Returns
    /// [`Error::StateDirReadOnly`], [`Error::LockBusy`], or [`Error::Io`]
    /// as documented in PLAN.md §4.5.1.
    pub fn acquire(workspace_root: &Path) -> Result<Self> {
        let musts_dir = workspace_root.join(".musts");
        ensure_state_dir(&musts_dir)?;
        let lock = acquire_lock(&musts_dir)?;
        let db = open_db(&musts_dir.join("state.sqlite"))?;
        Ok(Self {
            workspace_root: workspace_root.to_path_buf(),
            musts_dir,
            db,
            _lock: lock,
        })
    }
}

fn ensure_state_dir(musts_dir: &Path) -> Result<()> {
    // mkdir_p is idempotent and safe for two concurrent first runs —
    // both succeed and one of them wins the lock race in the next step.
    if let Err(err) = std::fs::create_dir_all(musts_dir) {
        if err.kind() == std::io::ErrorKind::PermissionDenied {
            return Err(Error::StateDirReadOnly);
        }
        return Err(Error::Io {
            path: musts_dir.to_path_buf(),
            source: err,
        });
    }
    // Sanity: ensure we can write a sentinel; on read-only mounts the
    // mkdir above may succeed (already exists) but writes will fail
    // later in a much less actionable way.
    let probe = musts_dir.join(".write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(Error::StateDirReadOnly)
        }
        Err(err) => Err(Error::Io {
            path: probe,
            source: err,
        }),
    }
}

fn acquire_lock(musts_dir: &Path) -> Result<File> {
    let lock_path = musts_dir.join(".lock");
    let handle = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| Error::Io {
            path: lock_path.clone(),
            source,
        })?;
    handle
        .try_lock_exclusive()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::WouldBlock => Error::LockBusy,
            _ => Error::Io {
                path: lock_path,
                source: err,
            },
        })?;
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_creates_musts_dir_and_opens_db() {
        let dir = TempDir::new().unwrap();
        let session = StateSession::acquire(dir.path()).unwrap();
        assert!(dir.path().join(".musts").is_dir());
        assert!(dir.path().join(".musts/state.sqlite").is_file());
        assert!(dir.path().join(".musts/.lock").is_file());
        drop(session);
    }

    #[test]
    fn second_acquire_in_same_process_is_lock_busy() {
        let dir = TempDir::new().unwrap();
        let first = StateSession::acquire(dir.path()).unwrap();
        let err = StateSession::acquire(dir.path()).unwrap_err();
        assert!(matches!(err, Error::LockBusy));
        assert_eq!(err.exit_code(), 2);
        drop(first);
    }

    #[test]
    fn acquire_succeeds_after_previous_session_drops() {
        let dir = TempDir::new().unwrap();
        {
            let _first = StateSession::acquire(dir.path()).unwrap();
        }
        // Second acquisition after drop should succeed.
        let _second = StateSession::acquire(dir.path()).unwrap();
    }
}
