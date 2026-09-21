//! First-run bootstrap and cross-process lock per `docs/PLAN.md` §4.5.1.
//!
//! Order of operations for any state-writing command:
//! 1. Ensure `<workspace>/.musts/` exists (mkdir_p).
//! 2. Open-or-create `<workspace>/.musts/.lock` (never `create_new`).
//! 3. `try_lock_exclusive` on the lock handle. On contention → exit 2,
//!    naming the holder (see [`LockOwner`]).
//! 4. Open `<workspace>/.musts/state.sqlite` and migrate.
//! 5. Do the work. Drop the handle on exit; the OS releases the lock.
//!
//! The lock is per workspace root, and a `git worktree` checkout is its
//! own workspace root (see [`crate::workspace`]), so two worktrees of the
//! same repository never contend with each other. Contention is always
//! *this* directory against another process in *this* directory.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::{open as open_db, Db};

/// Sidecar naming the process that currently holds the lock.
const OWNER_FILENAME: &str = ".lock.owner";

/// Who holds the workspace lock, so contention can name a process
/// instead of leaving the loser to guess.
///
/// Written to `.musts/.lock.owner` rather than into `.lock` itself: the
/// `flock` lives on the `.lock` *inode*, so rewriting that file through a
/// temp-and-rename would hand the next opener a different inode and the
/// lock would stop excluding anything. The sidecar can be replaced
/// atomically without touching the inode that carries the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockOwner {
    pub pid: u32,
    /// Working directory of the holder — the one thing that tells two
    /// worktrees of the same repository apart in a report.
    pub cwd: String,
    /// The holder's command line, e.g. `musts run cargo-test-root`.
    pub command: String,
    /// Unix seconds at which the holder took the lock.
    pub since_unix: i64,
}

impl LockOwner {
    /// Describe the process that is running right now.
    fn current() -> Self {
        let command = std::env::args()
            .enumerate()
            .map(|(i, arg)| {
                if i == 0 {
                    Path::new(&arg)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(arg)
                } else {
                    arg
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            pid: std::process::id(),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown>".to_string()),
            command,
            since_unix: time::OffsetDateTime::now_utc().unix_timestamp(),
        }
    }

    /// How long the holder has been holding, in seconds. Saturates at 0
    /// so a clock that went backwards reads as "just now" rather than as
    /// a negative age.
    pub fn held_for_secs(&self) -> i64 {
        (time::OffsetDateTime::now_utc().unix_timestamp() - self.since_unix).max(0)
    }

    /// Is the recorded process still alive?
    ///
    /// `None` on platforms where we cannot ask. This only ever refines
    /// the *message*: `flock` is released by the kernel when a process
    /// dies, `kill -9` included, so a busy lock always has a live holder.
    /// A dead pid here therefore means the sidecar is stale and the real
    /// holder is some other process — which is worth saying out loud
    /// rather than reporting a pid the reader would go looking for.
    pub fn is_alive(&self) -> Option<bool> {
        #[cfg(unix)]
        {
            use rustix::process::{test_kill_process, Pid};
            // `kill(pid, 0)` performs the existence and permission checks
            // and sends nothing. `EPERM` means the process is there and
            // belongs to someone else, which is still "alive" — only
            // `ESRCH` (no such process) makes the record stale.
            let pid = i32::try_from(self.pid).ok().and_then(Pid::from_raw)?;
            Some(match test_kill_process(pid) {
                Ok(()) => true,
                Err(err) => err != rustix::io::Errno::SRCH,
            })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// The multi-line body of a lock-busy report.
    pub fn describe(&self) -> String {
        let age = format_duration(self.held_for_secs());
        let liveness = match self.is_alive() {
            Some(false) => {
                return format!(
                    "  holder: unrecorded. The last process to take it (pid {}) is gone, so this \
                     record is stale and the lock is held by a process musts cannot name.",
                    self.pid
                )
            }
            Some(true) | None => String::new(),
        };
        format!(
            "  holder: pid {}, holding for {age}{liveness}\n  cwd:    {}\n  doing:  {}",
            self.pid, self.cwd, self.command
        )
    }
}

/// `4m 12s`, `38s`, `1h 03m`. Small enough to be read at a glance, which
/// is the whole point of putting it in the error.
fn format_duration(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// The held workspace lock plus the sidecar naming its holder.
///
/// Dropping releases the `flock` (the OS does it even if we are killed)
/// and removes the sidecar, so a stale record does not outlive a clean
/// exit.
#[derive(Debug)]
struct WorkspaceLock {
    _handle: File,
    owner_path: PathBuf,
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        // Best effort: an orphaned sidecar is handled by the pid check in
        // `LockOwner::is_alive`, so failing to remove it is never fatal.
        let _ = std::fs::remove_file(&self.owner_path);
    }
}

/// Live state-writing session: holds the lock and an open SQLite handle.
/// Dropping releases the lock; SQLite WAL files persist as usual.
pub struct StateSession {
    pub workspace_root: PathBuf,
    pub musts_dir: PathBuf,
    pub db: Db,
    // Held for the life of the session — drops release the file lock.
    // `None` only for the duration of [`StateSession::unlocked`].
    lock: Option<WorkspaceLock>,
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
        Self::acquire_waiting(workspace_root, lock_wait())
    }

    /// [`Self::acquire`] with an explicit patience. `Duration::ZERO` is
    /// the original fail-on-first-contention behaviour.
    pub fn acquire_waiting(workspace_root: &Path, wait: std::time::Duration) -> Result<Self> {
        let musts_dir = workspace_root.join(".musts");
        ensure_state_dir(&musts_dir)?;
        let lock = acquire_lock(&musts_dir, wait)?;
        let db = open_db(&musts_dir.join("state.sqlite"))?;
        Ok(Self {
            workspace_root: workspace_root.to_path_buf(),
            musts_dir,
            db,
            lock: Some(lock),
        })
    }

    /// Run `f` with the workspace lock released, then take it back.
    ///
    /// For work that takes minutes and touches no state — the point of
    /// the lock is to serialise writes to `state.sqlite` and
    /// `ledger.lock.yaml`, not to serialise the test suite whose result
    /// is about to be written. `musts run` held it for 60,23 s of a
    /// 60,28 s run before this existed, which made a check into a
    /// workspace-wide stop sign for its whole duration.
    ///
    /// Re-acquisition **blocks** rather than failing: by the time `f`
    /// returns we are holding a result that cost minutes to produce, and
    /// dropping it because someone else is mid-write would be strictly
    /// worse than waiting for a write that now lasts milliseconds.
    pub fn unlocked<T>(&mut self, f: impl FnOnce() -> T) -> Result<T> {
        let owner_path = self.musts_dir.join(OWNER_FILENAME);
        drop(self.lock.take());
        let out = f();
        let handle = open_lock_handle(&self.musts_dir)?;
        handle.lock_exclusive().map_err(|source| Error::Io {
            path: self.musts_dir.join(".lock"),
            source,
        })?;
        write_owner(&owner_path, &LockOwner::current());
        self.lock = Some(WorkspaceLock {
            _handle: handle,
            owner_path,
        });
        Ok(out)
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

fn open_lock_handle(musts_dir: &Path) -> Result<File> {
    let lock_path = musts_dir.join(".lock");
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| Error::Io {
            path: lock_path,
            source,
        })
}

/// Read the sidecar, if there is a readable one.
///
/// Every failure is the same answer — "nobody recorded it" — because the
/// sidecar is a convenience: an older musts wrote no sidecar at all, and
/// a half-written one is indistinguishable from a missing one for the
/// purpose of a message.
pub fn read_owner(musts_dir: &Path) -> Option<LockOwner> {
    let body = std::fs::read_to_string(musts_dir.join(OWNER_FILENAME)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Replace the sidecar atomically. Best effort: failing to name
/// ourselves must never stop a run that legitimately holds the lock.
fn write_owner(owner_path: &Path, owner: &LockOwner) {
    let Ok(body) = serde_json::to_string(owner) else {
        return;
    };
    // temp-and-rename so a reader never sees a half-written record. The
    // temp name carries our pid so two processes cannot collide on it.
    let tmp = owner_path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, body.as_bytes()).is_ok() && std::fs::rename(&tmp, owner_path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// How long to keep trying before reporting contention.
///
/// Zero in the original design ("the agent loop can retry trivially"),
/// which was the right call when the lock covered a whole test suite:
/// waiting on a ten-minute holder is worse than being told to come back.
/// It is the wrong call now that a holder writes for milliseconds — two
/// agents starting in the same second would fail one of them over an
/// 18 ms write. Measured windows after the `unlocked` change: 6 ms to
/// read the task, 18 ms to record the evidence.
///
/// A holder that really is long-lived still loses nothing: the wait ends
/// and the error names it, which is the case this whole change is for.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Override for [`LOCK_WAIT`], in milliseconds. `0` restores the old
/// fail-immediately behaviour, which is what the lock tests want so they
/// do not each pay the wait.
const LOCK_WAIT_ENV: &str = "MUSTS_LOCK_WAIT_MS";

fn lock_wait() -> std::time::Duration {
    std::env::var(LOCK_WAIT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(LOCK_WAIT, std::time::Duration::from_millis)
}

/// Gap between attempts. Short enough that the common case (someone
/// mid-write) is invisible, long enough not to spin a core.
const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(20);

fn acquire_lock(musts_dir: &Path, wait: std::time::Duration) -> Result<WorkspaceLock> {
    let handle = open_lock_handle(musts_dir)?;
    let deadline = std::time::Instant::now() + wait;
    loop {
        match handle.try_lock_exclusive() {
            Ok(()) => break,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    // Read the holder now rather than on the first failed
                    // attempt: a process that has only just taken the lock
                    // has not written its sidecar yet, and reporting
                    // "holder unknown" for that microsecond-wide race is
                    // exactly the unhelpful message this change removes.
                    return Err(Error::LockBusy {
                        holder: read_owner(musts_dir),
                    });
                }
                std::thread::sleep(LOCK_POLL);
            }
            Err(err) => {
                return Err(Error::Io {
                    path: musts_dir.join(".lock"),
                    source: err,
                })
            }
        }
    }
    let owner_path = musts_dir.join(OWNER_FILENAME);
    write_owner(&owner_path, &LockOwner::current());
    Ok(WorkspaceLock {
        _handle: handle,
        owner_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
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
        let err = StateSession::acquire_waiting(dir.path(), Duration::ZERO).unwrap_err();
        assert!(matches!(err, Error::LockBusy { .. }));
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

    #[test]
    fn contention_names_the_holding_process() {
        let dir = TempDir::new().unwrap();
        let first = StateSession::acquire(dir.path()).unwrap();
        let err = StateSession::acquire_waiting(dir.path(), Duration::ZERO).unwrap_err();
        let Error::LockBusy { holder } = &err else {
            panic!("expected LockBusy, got {err:?}");
        };
        let holder = holder.as_ref().expect("the holder must be recorded");
        assert_eq!(holder.pid, std::process::id());
        assert_eq!(holder.is_alive(), Some(true));

        let message = err.to_string();
        assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "the pid must be in the message: {message}"
        );
        assert!(
            message.contains(&holder.cwd),
            "the holder's cwd must be in the message: {message}"
        );
        assert!(
            message.contains("holding for"),
            "the message must say since when: {message}"
        );
        drop(first);
    }

    #[test]
    fn a_clean_exit_leaves_no_owner_record_behind() {
        let dir = TempDir::new().unwrap();
        {
            let _session = StateSession::acquire(dir.path()).unwrap();
            assert!(read_owner(&dir.path().join(".musts")).is_some());
        }
        assert!(
            read_owner(&dir.path().join(".musts")).is_none(),
            "the sidecar must not outlive the session that wrote it"
        );
    }

    #[test]
    fn a_dead_pid_in_the_record_is_reported_as_stale_not_as_a_holder() {
        let dir = TempDir::new().unwrap();
        let musts_dir = dir.path().join(".musts");
        std::fs::create_dir_all(&musts_dir).unwrap();
        let owner = LockOwner {
            // A pid no process can hold: macOS caps pids far below
            // this, and signalling it is a no-op either way since
            // `test_kill_process` sends nothing.
            pid: i32::MAX as u32,
            cwd: "/somewhere".to_string(),
            command: "musts run x".to_string(),
            since_unix: time::OffsetDateTime::now_utc().unix_timestamp() - 90,
        };
        write_owner(&musts_dir.join(OWNER_FILENAME), &owner);

        let read_back = read_owner(&musts_dir).unwrap();
        assert_eq!(read_back.is_alive(), Some(false));
        let described = read_back.describe();
        assert!(
            described.contains("stale"),
            "a dead pid must be reported as a stale record: {described}"
        );
    }

    #[test]
    fn the_lock_is_free_while_unlocked_runs_and_held_again_after() {
        let dir = TempDir::new().unwrap();
        let mut session = StateSession::acquire(dir.path()).unwrap();
        let taken_by_someone_else = session
            .unlocked(|| {
                // A second acquisition must succeed *during* the closure:
                // that is the whole property under test.
                StateSession::acquire(dir.path()).is_ok()
            })
            .unwrap();
        assert!(
            taken_by_someone_else,
            "the lock must be free while `unlocked` runs"
        );
        // And it must be ours again afterwards.
        assert!(matches!(
            StateSession::acquire_waiting(dir.path(), Duration::ZERO).unwrap_err(),
            Error::LockBusy { .. }
        ));
    }

    #[test]
    fn unlocked_rewrites_the_owner_record_on_the_way_back_in() {
        let dir = TempDir::new().unwrap();
        let mut session = StateSession::acquire(dir.path()).unwrap();
        session.unlocked(|| {}).unwrap();
        let owner = read_owner(&dir.path().join(".musts")).expect("re-acquisition must record us");
        assert_eq!(owner.pid, std::process::id());
    }

    #[test]
    fn a_brief_holder_is_waited_out_rather_than_reported() {
        // The case the wait exists for: two commands start together, one
        // is mid-write for a few milliseconds. Failing the other over
        // that is what `musts run` used to do to its own sibling.
        let dir = TempDir::new().unwrap();
        let first = StateSession::acquire(dir.path()).unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(first);
        });
        let second = StateSession::acquire_waiting(dir.path(), Duration::from_secs(5));
        releaser.join().unwrap();
        assert!(
            second.is_ok(),
            "a holder that releases inside the wait must not produce an error"
        );
    }

    #[test]
    fn durations_read_the_way_a_person_would_say_them() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(38), "38s");
        assert_eq!(format_duration(252), "4m 12s");
        assert_eq!(format_duration(3780), "1h 03m");
    }
}
