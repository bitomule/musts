//! Portable, repo-committed "validated state" ledger.
//!
//! Lives at `<workspace_root>/.musts/ledger.lock.yaml`. Contains the
//! minimum set of tuples `(check_id, scope_hash)` core needs to answer
//! the satisfaction question without consulting `state.sqlite`. Format
//! is sorted, deterministic YAML so diffs and merges are reviewable.
//!
//! Loaded once per `validate`/`evidence` and consulted alongside the
//! local `evidence_records` table; appended to after every accepted
//! submission. `state.sqlite` and `evidence/` stay machine-local; the
//! lock file is the only piece that travels with the repo.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const SUPPORTED_VERSION: u32 = 1;
pub const LOCK_FILENAME: &str = "ledger.lock.yaml";

/// The committed validated-state ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerLock {
    pub version: u32,
    #[serde(default)]
    pub satisfied: BTreeSet<SatisfiedEntry>,
}

impl Default for LedgerLock {
    fn default() -> Self {
        Self {
            version: SUPPORTED_VERSION,
            satisfied: BTreeSet::new(),
        }
    }
}

impl LedgerLock {
    /// Is this `(check_id, scope_hash)` already proven satisfied?
    pub fn contains(&self, check: &str, scope_hash: &str) -> bool {
        // `BTreeSet::contains` needs an owned key (we don't have a Borrow
        // impl for `(str, str)`), so do a linear short-circuit. The set
        // is small in practice — one entry per (check, accepted snapshot).
        self.satisfied
            .iter()
            .any(|e| e.check == check && e.scope_hash == scope_hash)
    }

    /// Idempotent insert. Returns `true` when the entry was new.
    pub fn record(&mut self, check: impl Into<String>, scope_hash: impl Into<String>) -> bool {
        self.satisfied.insert(SatisfiedEntry {
            check: check.into(),
            scope_hash: scope_hash.into(),
        })
    }
}

/// One satisfied `(check_id, scope_hash)` pair.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct SatisfiedEntry {
    pub check: String,
    pub scope_hash: String,
}

/// Path to the lock file inside `<workspace_root>/.musts/`.
pub fn path(musts_dir: &Path) -> PathBuf {
    musts_dir.join(LOCK_FILENAME)
}

/// Read the lock file. Returns `Ok(LedgerLock::default())` when the
/// file is absent (fresh workspace, nothing previously validated).
pub fn load(musts_dir: &Path) -> Result<LedgerLock> {
    let p = path(musts_dir);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(LedgerLock::default()),
        Err(source) => return Err(Error::Io { path: p, source }),
    };
    let lock: LedgerLock = serde_yaml::from_slice(&bytes).map_err(|err| Error::LedgerLock {
        path: p.clone(),
        message: format!("invalid YAML: {err}"),
    })?;
    if lock.version != SUPPORTED_VERSION {
        return Err(Error::LedgerLock {
            path: p,
            message: format!(
                "unsupported lock version {} (only {SUPPORTED_VERSION} is supported)",
                lock.version
            ),
        });
    }
    Ok(lock)
}

/// Write the lock file. Output is deterministic: `satisfied` is a
/// `BTreeSet` so iteration is sorted alphabetically by `(check,
/// scope_hash)`. Callers should pass a `&LedgerLock` already containing
/// every entry they want persisted — this function does no merging.
pub fn save(musts_dir: &Path, lock: &LedgerLock) -> Result<()> {
    let p = path(musts_dir);
    let body = serde_yaml::to_string(lock).map_err(|err| Error::LedgerLock {
        path: p.clone(),
        message: format!("could not serialise: {err}"),
    })?;
    std::fs::write(&p, body).map_err(|source| Error::Io { path: p, source })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        TempDir::new().expect("tempdir")
    }

    #[test]
    fn load_missing_file_yields_empty_lock() {
        let dir = tmp();
        let lock = load(dir.path()).unwrap();
        assert_eq!(lock.version, SUPPORTED_VERSION);
        assert!(lock.satisfied.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tmp();
        let mut lock = LedgerLock::default();
        assert!(lock.record("root/fmt", "abc123"));
        assert!(lock.record("root/clippy", "def456"));
        save(dir.path(), &lock).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded, lock);
    }

    #[test]
    fn record_is_idempotent() {
        let mut lock = LedgerLock::default();
        assert!(lock.record("c", "h"));
        assert!(!lock.record("c", "h"));
        assert_eq!(lock.satisfied.len(), 1);
    }

    #[test]
    fn contains_matches_only_exact_pair() {
        let mut lock = LedgerLock::default();
        lock.record("c", "h1");
        assert!(lock.contains("c", "h1"));
        assert!(!lock.contains("c", "h2"));
        assert!(!lock.contains("other", "h1"));
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let dir = tmp();
        std::fs::write(path(dir.path()), "version: 2\nsatisfied: []\n").unwrap();
        let err = load(dir.path()).unwrap_err();
        assert!(matches!(err, Error::LedgerLock { .. }));
    }

    #[test]
    fn malformed_yaml_surfaces_as_ledger_lock_error() {
        let dir = tmp();
        std::fs::write(path(dir.path()), "::: not yaml").unwrap();
        let err = load(dir.path()).unwrap_err();
        assert!(matches!(err, Error::LedgerLock { .. }));
    }

    #[test]
    fn saved_format_is_deterministic_and_sorted() {
        let dir = tmp();
        let mut lock = LedgerLock::default();
        lock.record("z/check", "h");
        lock.record("a/check", "h");
        save(dir.path(), &lock).unwrap();
        let body = std::fs::read_to_string(path(dir.path())).unwrap();
        let a = body.find("a/check").unwrap();
        let z = body.find("z/check").unwrap();
        assert!(a < z, "satisfied entries should serialise sorted");
    }
}
