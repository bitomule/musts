//! Machine-local scratch state under `.musts/cache/`.
//!
//! Nothing here can make a check validated: only `.musts/ledger/`, the
//! committed record CI reads, does that. A machine-local copy of
//! "validated" is exactly how `validate` once said clean on a laptop
//! while CI, on the same commit, found the check pending.
//!
//! What lives here is what is cheap to lose:
//!
//! - `fingerprints.json` — file hashes keyed by `(mtime, size)`, so a
//!   warm `validate` does not re-read every file.
//! - `tasks.json` — the tasks the last `validate` emitted, for `run` and
//!   `evidence` to look up and to reject evidence for a tree that moved.
//! - `failures.json` — the last failed `musts run` of each task with the
//!   fingerprint of the files it ran against, so an unchanged tree is not
//!   run again.
//! - `history.jsonl` — one line per evidence submission, accepted or
//!   rejected, read only by `musts admin stats`.
//!
//! A missing or unreadable file reads as empty. The directory carries its
//! own `.gitignore`, so no repo has to list it.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::{Error, Result};

pub const CACHE_DIRNAME: &str = "cache";
const FINGERPRINTS_FILENAME: &str = "fingerprints.json";
const TASKS_FILENAME: &str = "tasks.json";
const HISTORY_FILENAME: &str = "history.jsonl";
const FAILURES_FILENAME: &str = "failures.json";
const LEGACY_DB_FILENAMES: [&str; 3] = ["state.sqlite", "state.sqlite-shm", "state.sqlite-wal"];

pub fn dir(musts_dir: &Path) -> PathBuf {
    musts_dir.join(CACHE_DIRNAME)
}

fn ensure_dir(musts_dir: &Path) -> Result<PathBuf> {
    let dir = dir(musts_dir);
    std::fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n").map_err(|source| Error::Io {
            path: gitignore,
            source,
        })?;
    }
    Ok(dir)
}

fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(bytes) = std::fs::read(path) else {
        return T::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        tracing::debug!(path = %path.display(), %err, "unreadable cache file; starting empty");
        T::default()
    })
}

fn store<T: Serialize>(musts_dir: &Path, filename: &str, value: &T) -> Result<()> {
    let path = ensure_dir(musts_dir)?.join(filename);
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let bytes = serde_json::to_vec(value).expect("cache values serialise");
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|source| Error::Io { path, source })
}

/// The hash of a file as it was when it had this mtime and size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub mtime_ns: i128,
    pub size_bytes: u64,
    pub content_hash: String,
}

/// Workspace-relative path (real case) → fingerprint.
pub type Fingerprints = BTreeMap<String, Fingerprint>;

pub fn load_fingerprints(musts_dir: &Path) -> Fingerprints {
    load(&dir(musts_dir).join(FINGERPRINTS_FILENAME))
}

pub fn store_fingerprints(musts_dir: &Path, fingerprints: &Fingerprints) -> Result<()> {
    store(musts_dir, FINGERPRINTS_FILENAME, fingerprints)
}

/// A task as the last `validate` emitted it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTask {
    pub capability: String,
    pub satisfies: Vec<String>,
    /// Check id → the scope hash it had when the task was emitted.
    pub scope_hashes: BTreeMap<String, String>,
    pub task_snapshot_hash: String,
    pub task: musts_protocol::Task,
}

/// Task id → task.
pub type Tasks = BTreeMap<String, StoredTask>;

pub fn load_tasks(musts_dir: &Path) -> Tasks {
    load(&dir(musts_dir).join(TASKS_FILENAME))
}

/// Replace the stored tasks with exactly `tasks`.
pub fn store_tasks(musts_dir: &Path, tasks: &Tasks) -> Result<()> {
    store(musts_dir, TASKS_FILENAME, tasks)
}

/// `None` is the "task no longer applies" signal: no `validate` emitted
/// it, or a later one emitted a different set.
pub fn fetch_task(musts_dir: &Path, task_id: &str) -> Option<StoredTask> {
    load_tasks(musts_dir).remove(task_id)
}

/// A `musts run` that exited non-zero, and the files it ran against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedRun {
    pub fingerprint: String,
    pub command: String,
    pub code: Option<i32>,
    pub log_path: PathBuf,
}

/// Task id → its last failed run.
pub type FailedRuns = BTreeMap<String, FailedRun>;

pub fn load_failures(musts_dir: &Path) -> FailedRuns {
    load(&dir(musts_dir).join(FAILURES_FILENAME))
}

pub fn store_failures(musts_dir: &Path, failures: &FailedRuns) -> Result<()> {
    store(musts_dir, FAILURES_FILENAME, failures)
}

/// One evidence submission for one check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub check_id: String,
    pub scope_hash: String,
    pub accepted: bool,
    /// Characters of agent-written text, when the submission had any.
    pub text_chars: Option<usize>,
    pub submitted_at: i64,
}

pub fn append_history(musts_dir: &Path, submissions: &[Submission]) -> Result<()> {
    let path = ensure_dir(musts_dir)?.join(HISTORY_FILENAME);
    let mut lines = String::new();
    for s in submissions {
        lines.push_str(&serde_json::to_string(s).expect("submissions serialise"));
        lines.push('\n');
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(lines.as_bytes()))
        .map_err(|source| Error::Io { path, source })
}

pub fn history_exists(musts_dir: &Path) -> bool {
    dir(musts_dir).join(HISTORY_FILENAME).is_file()
}

/// Every submission recorded on this machine. A malformed line is
/// skipped: a stats command must never fail on its own history.
pub fn load_history(musts_dir: &Path) -> Vec<Submission> {
    std::fs::read_to_string(dir(musts_dir).join(HISTORY_FILENAME))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Delete the SQLite database older releases kept in `.musts/`. Nothing
/// is carried over: what was validated is already in the ledger.
pub fn remove_legacy_database(musts_dir: &Path) {
    for name in LEGACY_DB_FILENAMES {
        let path = musts_dir.join(name);
        if let Err(err) = std::fs::remove_file(&path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(path = %path.display(), %err, "could not remove the old state database");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn task(id: &str) -> musts_protocol::Task {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "extension": "agent",
            "title": "t",
            "satisfies": ["root/c"],
            "instructions": [],
            "evidence_contract": {"text": {"required": true}},
        }))
        .unwrap()
    }

    #[test]
    fn missing_files_read_as_empty() {
        let dir = TempDir::new().unwrap();
        assert!(load_fingerprints(dir.path()).is_empty());
        assert!(load_tasks(dir.path()).is_empty());
        assert!(load_history(dir.path()).is_empty());
        assert!(!history_exists(dir.path()));
    }

    #[test]
    fn corrupt_files_read_as_empty() {
        let dir = TempDir::new().unwrap();
        let cache = ensure_dir(dir.path()).unwrap();
        std::fs::write(cache.join(FINGERPRINTS_FILENAME), "{not json").unwrap();
        std::fs::write(cache.join(TASKS_FILENAME), "[").unwrap();
        assert!(load_fingerprints(dir.path()).is_empty());
        assert!(fetch_task(dir.path(), "x").is_none());
    }

    #[test]
    fn fingerprints_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut fps = Fingerprints::new();
        fps.insert(
            "a.swift".into(),
            Fingerprint {
                mtime_ns: 1_700_000_000_123_456_789,
                size_bytes: 99,
                content_hash: "h".into(),
            },
        );
        store_fingerprints(dir.path(), &fps).unwrap();
        assert_eq!(load_fingerprints(dir.path()), fps);
    }

    #[test]
    fn storing_tasks_replaces_the_previous_set() {
        let dir = TempDir::new().unwrap();
        let stored = |id: &str| StoredTask {
            capability: "agent".into(),
            satisfies: vec!["root/c".into()],
            scope_hashes: BTreeMap::from([("root/c".into(), "h".into())]),
            task_snapshot_hash: "s".into(),
            task: task(id),
        };
        store_tasks(dir.path(), &Tasks::from([("a".into(), stored("a"))])).unwrap();
        store_tasks(dir.path(), &Tasks::from([("b".into(), stored("b"))])).unwrap();
        assert!(fetch_task(dir.path(), "a").is_none());
        assert_eq!(fetch_task(dir.path(), "b"), Some(stored("b")));
    }

    #[test]
    fn history_appends_and_skips_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let s = |accepted| Submission {
            check_id: "root/c".into(),
            scope_hash: "h".into(),
            accepted,
            text_chars: Some(3),
            submitted_at: 1,
        };
        append_history(dir.path(), &[s(false)]).unwrap();
        let path = dir.path().join(CACHE_DIRNAME).join(HISTORY_FILENAME);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"garbage\n").unwrap();
        append_history(dir.path(), &[s(true)]).unwrap();
        assert_eq!(load_history(dir.path()), vec![s(false), s(true)]);
    }

    #[test]
    fn the_cache_directory_ignores_itself() {
        let dir = TempDir::new().unwrap();
        store_tasks(dir.path(), &Tasks::new()).unwrap();
        let body =
            std::fs::read_to_string(dir.path().join(CACHE_DIRNAME).join(".gitignore")).unwrap();
        assert_eq!(body, "*\n");
    }

    #[test]
    fn the_old_database_is_removed() {
        let dir = TempDir::new().unwrap();
        for name in LEGACY_DB_FILENAMES {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        remove_legacy_database(dir.path());
        for name in LEGACY_DB_FILENAMES {
            assert!(!dir.path().join(name).exists(), "{name}");
        }
        remove_legacy_database(dir.path());
    }
}
