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

/// `.gitattributes` inside `.musts/`, owned by musts.
const GITATTRIBUTES_FILENAME: &str = ".gitattributes";

/// Why we write it: `satisfied` is an append-only set. Two branches that
/// each record evidence both insert entries into the same sorted list, so
/// git reports a content conflict on the lock even though the two edits
/// can never actually disagree. Whoever resolves it by hand picks a side
/// and silently throws away proven-green entries — which is how "the
/// ledger did not survive the merge" gets diagnosed as a squash-merge bug.
///
/// `merge=union` is a built-in git driver (no `git config` needed) and it
/// is the *correct* resolution here, not a shortcut: the union of two
/// append-only sets is the set. Ordering can come out interleaved; that
/// is cosmetic, since `LedgerLock` deserialises into a `BTreeSet` and the
/// next `save` rewrites the file sorted.
///
/// Union merging is only sound because an entry occupies exactly one
/// line — see [`save`] for why that had to change and how it stayed
/// backward compatible.
const GITATTRIBUTES_BODY: &str = "\
# Managed by musts. The validated-state ledger is an append-only set, so
# taking both sides is always the right merge — a hand resolution would
# drop entries that were legitimately proven green on one branch.
ledger.lock.yaml merge=union
";

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

/// Install the `merge=union` attribute for the lock file, so a merge
/// that touches it on both sides resolves to the union instead of a
/// conflict. See [`GITATTRIBUTES_BODY`].
///
/// Idempotent and non-destructive: an existing `.gitattributes` that
/// already says something about `ledger.lock.yaml` is left alone (the
/// project has made its own choice), anything else is appended to.
///
/// Best-effort by design — this is a convenience, never a reason for
/// `validate` to fail, so IO errors are logged and swallowed.
pub fn ensure_union_merge_attribute(musts_dir: &Path) {
    let p = musts_dir.join(GITATTRIBUTES_FILENAME);
    let existing = match std::fs::read_to_string(&p) {
        Ok(body) => Some(body),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::debug!(path = ?p, %err, "could not read .musts/.gitattributes");
            return;
        }
    };
    let body = match existing {
        Some(body) if mentions_lock_file(&body) => return,
        Some(body) => {
            let sep = if body.is_empty() || body.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{body}{sep}{GITATTRIBUTES_BODY}")
        }
        None => GITATTRIBUTES_BODY.to_string(),
    };
    if let Err(err) = std::fs::write(&p, body) {
        tracing::debug!(path = ?p, %err, "could not write .musts/.gitattributes");
    }
}

fn mentions_lock_file(body: &str) -> bool {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .any(|l| l.contains(LOCK_FILENAME))
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
    if has_conflict_markers(&bytes) {
        return Err(Error::LedgerLock {
            path: p,
            message: "unresolved merge conflict. The ledger is an append-only set, so keeping \
                      both sides is the correct resolution: delete the `<<<<<<<`, `=======` and \
                      `>>>>>>>` lines and keep every entry. `.musts/.gitattributes` sets \
                      `merge=union` to do this automatically — commit it so future merges \
                      resolve themselves"
                .to_string(),
        });
    }
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
///
/// Written by hand rather than via `serde_yaml::to_string` for one
/// reason: each entry has to occupy **exactly one line**, as a flow
/// mapping:
///
/// ```yaml
/// - {check: "root/build-ios", scope_hash: "833bc590…"}
/// ```
///
/// The default block style splits an entry across two lines, and every
/// entry for the same check then shares an identical `- check: …` first
/// line. git's line-based merge aligns on those, so a union merge of two
/// branches that both recorded evidence for that check splices their
/// `scope_hash:` lines into a single mapping with a duplicate key — an
/// unparseable ledger. One line per entry makes the record atomic, which
/// is what lets `merge=union` (see [`GITATTRIBUTES_BODY`]) be correct.
///
/// A flow mapping is still an ordinary YAML mapping, so this is a pure
/// formatting change: the file stays `version: 1`, older musts releases
/// keep reading it, and block-style locks written by them keep loading
/// here. No migration step, and none of the mixed-version breakage a
/// new entry shape would have caused.
pub fn save(musts_dir: &Path, lock: &LedgerLock) -> Result<()> {
    let p = path(musts_dir);
    let mut body = format!("version: {}\nsatisfied:\n", lock.version);
    if lock.satisfied.is_empty() {
        body = format!("version: {}\nsatisfied: []\n", lock.version);
    }
    for entry in &lock.satisfied {
        body.push_str(&format!(
            "- {{check: {}, scope_hash: {}}}\n",
            yaml_quote(&entry.check),
            yaml_quote(&entry.scope_hash)
        ));
    }
    std::fs::write(&p, body).map_err(|source| Error::Io { path: p, source })?;
    ensure_union_merge_attribute(musts_dir);
    Ok(())
}

/// Double-quote a scalar for the flow mapping. Always quoting sidesteps
/// the flow-context characters (`,`, `{`, `}`, `:`) that a check id
/// derived from a manifest key could contain.
fn yaml_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

/// Conflict markers must be caught before YAML parsing: `<<<<<<< HEAD`
/// happens to be *valid* YAML in some positions, so without this a
/// half-resolved ledger can load with entries missing instead of failing.
fn has_conflict_markers(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.lines()
        .any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "=======")
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

    /// The property `merge=union` depends on: one entry, one line.
    #[test]
    fn each_entry_occupies_exactly_one_line() {
        let dir = tmp();
        let mut lock = LedgerLock::default();
        lock.record("root/build-ios", "aa");
        lock.record("root/build-ios", "bb");
        save(dir.path(), &lock).unwrap();
        let body = std::fs::read_to_string(path(dir.path())).unwrap();
        assert_eq!(
            body,
            "version: 1\nsatisfied:\n\
             - {check: \"root/build-ios\", scope_hash: \"aa\"}\n\
             - {check: \"root/build-ios\", scope_hash: \"bb\"}\n",
            "{body}"
        );
    }

    /// Block-style locks written by earlier releases must keep loading —
    /// the flow style is a formatting change, not a format change.
    #[test]
    fn block_style_entries_written_by_older_releases_still_load() {
        let dir = tmp();
        std::fs::write(
            path(dir.path()),
            "version: 1\nsatisfied:\n- check: root/build-ios\n  scope_hash: aa\n\
             - check: root/build-ios\n  scope_hash: bb\n",
        )
        .unwrap();

        let lock = load(dir.path()).unwrap();
        assert!(lock.contains("root/build-ios", "aa"));
        assert!(lock.contains("root/build-ios", "bb"));
        assert_eq!(lock.version, SUPPORTED_VERSION);
    }

    #[test]
    fn flow_style_output_is_still_read_by_a_plain_serde_derive() {
        // Stand-in for an older musts release, which deserialises the
        // lock with exactly this derive and no flow-style awareness.
        #[derive(Deserialize)]
        struct OldLock {
            version: u32,
            satisfied: Vec<SatisfiedEntry>,
        }
        let dir = tmp();
        let mut lock = LedgerLock::default();
        lock.record("root/build-ios", "aa");
        save(dir.path(), &lock).unwrap();

        let bytes = std::fs::read(path(dir.path())).unwrap();
        let old: OldLock = serde_yaml::from_slice(&bytes).expect("older musts must still parse");
        assert_eq!(old.version, 1);
        assert_eq!(old.satisfied.len(), 1);
        assert_eq!(old.satisfied[0].check, "root/build-ios");
        assert_eq!(old.satisfied[0].scope_hash, "aa");
    }

    #[test]
    fn check_ids_with_flow_context_characters_round_trip() {
        let dir = tmp();
        let mut lock = LedgerLock::default();
        lock.record("root/build, ios: \"x\"", "aa");
        save(dir.path(), &lock).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert!(loaded.contains("root/build, ios: \"x\"", "aa"));
    }

    #[test]
    fn an_empty_lock_round_trips() {
        let dir = tmp();
        save(dir.path(), &LedgerLock::default()).unwrap();
        assert!(load(dir.path()).unwrap().satisfied.is_empty());
    }

    #[test]
    fn saving_installs_the_union_merge_attribute() {
        let dir = tmp();
        save(dir.path(), &LedgerLock::default()).unwrap();
        let body = std::fs::read_to_string(dir.path().join(GITATTRIBUTES_FILENAME)).unwrap();
        assert!(body.contains("ledger.lock.yaml merge=union"), "{body}");
    }

    #[test]
    fn existing_rule_for_the_lock_file_is_left_alone() {
        let dir = tmp();
        let attrs = dir.path().join(GITATTRIBUTES_FILENAME);
        std::fs::write(&attrs, "ledger.lock.yaml merge=ours\n").unwrap();
        ensure_union_merge_attribute(dir.path());
        assert_eq!(
            std::fs::read_to_string(&attrs).unwrap(),
            "ledger.lock.yaml merge=ours\n",
            "a project that made its own choice must keep it"
        );
    }

    #[test]
    fn unrelated_attributes_are_appended_to_not_replaced() {
        let dir = tmp();
        let attrs = dir.path().join(GITATTRIBUTES_FILENAME);
        std::fs::write(&attrs, "*.bin binary").unwrap();
        ensure_union_merge_attribute(dir.path());
        let body = std::fs::read_to_string(&attrs).unwrap();
        assert!(body.starts_with("*.bin binary\n"), "{body}");
        assert!(body.contains("ledger.lock.yaml merge=union"), "{body}");
    }

    #[test]
    fn installing_the_attribute_twice_does_not_duplicate_it() {
        let dir = tmp();
        ensure_union_merge_attribute(dir.path());
        ensure_union_merge_attribute(dir.path());
        let body = std::fs::read_to_string(dir.path().join(GITATTRIBUTES_FILENAME)).unwrap();
        assert_eq!(body.matches("merge=union").count(), 1, "{body}");
    }

    #[test]
    fn unresolved_conflict_markers_are_reported_as_such() {
        let dir = tmp();
        std::fs::write(
            path(dir.path()),
            "version: 1\nsatisfied:\n<<<<<<< HEAD\n- check: a\n  scope_hash: h1\n=======\n\
             - check: b\n  scope_hash: h2\n>>>>>>> feature\n",
        )
        .unwrap();
        let err = load(dir.path()).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("merge conflict"),
            "expected a conflict-specific message, got: {message}"
        );
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
