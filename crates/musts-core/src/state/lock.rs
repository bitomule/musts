//! Portable, repo-committed "validated state" ledger.
//!
//! Lives in `<workspace_root>/.musts/ledger/`, one file per check. Each
//! file holds a single line: the scope hash the check was last proven
//! green for. Only the current state is stored, never history.
//!
//! One file per check is what makes git (and GitHub, which ignores merge
//! drivers) merge the ledger without help: two branches that validate
//! different checks touch different files and never conflict, while two
//! branches that revalidate the same check edit the same file and do
//! conflict — correctly, because the combined tree was validated by
//! neither.
//!
//! The only record of what is validated: `validate` reads nothing else,
//! so a check is green locally exactly when it is green in CI for the
//! same committed tree. A file is replaced after every accepted
//! submission.
//!
//! Releases before 0.10.0 kept one `.musts/ledger.lock.yaml` holding every
//! `(check, scope_hash)` pair ever proven. It is still read, and
//! [`migrate_legacy`] folds it into the per-check files.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const SUPPORTED_VERSION: u32 = 1;
pub const LOCK_FILENAME: &str = "ledger.lock.yaml";
pub const LEDGER_DIRNAME: &str = "ledger";

/// `.gitattributes` inside `.musts/`. Releases before 0.10.0 wrote a
/// `merge=union` rule for the old lock there; [`migrate_legacy`] removes
/// exactly those lines.
const GITATTRIBUTES_FILENAME: &str = ".gitattributes";
const LEGACY_GITATTRIBUTES_BODY: &str = "\
# Managed by musts. The validated-state ledger is an append-only set, so
# taking both sides is always the right merge — a hand resolution would
# drop entries that were legitimately proven green on one branch.
ledger.lock.yaml merge=union
";

/// The validated state of a workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    /// The hash each check is currently proven green for.
    pub current: BTreeMap<String, String>,
    /// Entries of a not-yet-migrated `ledger.lock.yaml`.
    pub legacy: LedgerLock,
}

impl Ledger {
    /// Is this `(check_id, scope_hash)` already proven satisfied?
    pub fn contains(&self, check: &str, scope_hash: &str) -> bool {
        self.current.get(check).is_some_and(|h| h == scope_hash)
            || self.legacy.contains(check, scope_hash)
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_empty() && self.legacy.satisfied.is_empty()
    }

    /// Every `(check, scope_hash)` pair known, current and legacy.
    pub fn entries(&self) -> BTreeSet<SatisfiedEntry> {
        let current = self.current.iter().map(|(c, h)| SatisfiedEntry {
            check: c.clone(),
            scope_hash: h.clone(),
        });
        current
            .chain(self.legacy.satisfied.iter().cloned())
            .collect()
    }
}

/// The pre-0.10.0 single-file ledger: every pair ever proven.
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
    pub fn contains(&self, check: &str, scope_hash: &str) -> bool {
        self.satisfied
            .iter()
            .any(|e| e.check == check && e.scope_hash == scope_hash)
    }

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

/// Directory holding the per-check files.
pub fn ledger_dir(musts_dir: &Path) -> PathBuf {
    musts_dir.join(LEDGER_DIRNAME)
}

/// Path to the pre-0.10.0 lock file.
pub fn path(musts_dir: &Path) -> PathBuf {
    musts_dir.join(LOCK_FILENAME)
}

/// File name of a check: its id with every byte outside
/// `[A-Za-z0-9._-]` percent-encoded (so `root/clippy` is
/// `root%2Fclippy`). Flat, so `a` and `a/b` never collide as a file and a
/// directory; deterministic and reversible. A leading `.` is encoded too,
/// because dot-files are not ledger entries.
pub fn entry_file_name(check: &str) -> String {
    let mut name = String::with_capacity(check.len());
    for (i, byte) in check.bytes().enumerate() {
        let plain =
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' || (byte == b'.' && i > 0);
        if plain {
            name.push(byte as char);
        } else {
            name.push_str(&format!("%{byte:02X}"));
        }
    }
    name
}

pub(crate) fn decode_file_name(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Read the ledger. A workspace with neither the directory nor the old
/// lock yields an empty ledger (nothing previously validated).
pub fn load(musts_dir: &Path) -> Result<Ledger> {
    Ok(Ledger {
        current: load_current(musts_dir)?,
        legacy: load_legacy(musts_dir)?,
    })
}

fn load_current(musts_dir: &Path) -> Result<BTreeMap<String, String>> {
    let dir = ledger_dir(musts_dir);
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => return Err(Error::Io { path: dir, source }),
    };
    let mut current = BTreeMap::new();
    for item in read {
        let item = item.map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
        let file_name = item.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('.') {
            continue;
        }
        let Some(check) = decode_file_name(&file_name) else {
            continue;
        };
        let p = item.path();
        let body = std::fs::read(&p).map_err(|source| Error::Io {
            path: p.clone(),
            source,
        })?;
        if has_conflict_markers(&body) {
            return Err(Error::LedgerLock {
                path: p,
                message: format!(
                    "unresolved merge conflict: two branches revalidated `{check}`, and the \
                     merged tree was validated by neither. Delete the file's conflict \
                     markers, then run `musts validate` and revalidate `{check}`"
                ),
            });
        }
        let hash = String::from_utf8_lossy(&body).trim().to_string();
        if hash.is_empty() {
            return Err(Error::LedgerLock {
                path: p,
                message: format!("empty ledger entry for `{check}`"),
            });
        }
        current.insert(check, hash);
    }
    Ok(current)
}

/// Record that `check` is green for `scope_hash`, replacing whatever the
/// check held. The write is atomic: a crash leaves the old entry or the
/// new one, never a torn file.
pub fn record(musts_dir: &Path, check: &str, scope_hash: &str) -> Result<()> {
    let dir = ledger_dir(musts_dir);
    std::fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;
    let name = entry_file_name(check);
    let target = dir.join(&name);
    if std::fs::read_to_string(&target).is_ok_and(|b| b.trim() == scope_hash) {
        return Ok(());
    }
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, format!("{scope_hash}\n"))
        .and_then(|()| std::fs::rename(&tmp, &target))
        .map_err(|source| Error::Io {
            path: target,
            source,
        })
}

/// Read the pre-0.10.0 lock file; empty when absent.
fn load_legacy(musts_dir: &Path) -> Result<LedgerLock> {
    let p = path(musts_dir);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(LedgerLock::default()),
        Err(source) => return Err(Error::Io { path: p, source }),
    };
    if has_conflict_markers(&bytes) {
        return Err(Error::LedgerLock {
            path: p,
            message: "unresolved merge conflict. Keep every entry by deleting the `<<<<<<<`, \
                      `=======` and `>>>>>>>` lines; musts then migrates the file into \
                      `.musts/ledger/`"
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

/// Fold a pre-0.10.0 `ledger.lock.yaml` into per-check files and retire
/// it, along with the `merge=union` lines musts put in
/// `.musts/.gitattributes`.
///
/// `candidates` maps every declared check to the hashes that count as its
/// current state (the current scope hash, then the legacy one). A check
/// gets the first candidate the old lock proves; hashes of older trees
/// are history and are dropped, as are checks that are no longer
/// declared. A check that already has a file keeps it: it is newer than
/// anything in the old lock.
///
/// Returns the number of files written, or `None` when there was nothing
/// to migrate. Idempotent.
pub fn migrate_legacy(
    musts_dir: &Path,
    candidates: &BTreeMap<String, Vec<String>>,
) -> Result<Option<usize>> {
    let legacy_path = path(musts_dir);
    if !legacy_path.exists() {
        return Ok(None);
    }
    let legacy = load_legacy(musts_dir)?;
    let existing = load_current(musts_dir)?;
    let mut written = 0;
    for (check, hashes) in candidates {
        if existing.contains_key(check) {
            continue;
        }
        if let Some(hash) = hashes.iter().find(|h| legacy.contains(check, h)) {
            record(musts_dir, check, hash)?;
            written += 1;
        }
    }
    std::fs::remove_file(&legacy_path).map_err(|source| Error::Io {
        path: legacy_path,
        source,
    })?;
    remove_legacy_merge_rule(musts_dir);
    Ok(Some(written))
}

/// Drop the lines musts wrote for the old lock, keeping anything the
/// project added. Best-effort: a failure here must not fail `validate`.
fn remove_legacy_merge_rule(musts_dir: &Path) {
    let p = musts_dir.join(GITATTRIBUTES_FILENAME);
    let Ok(body) = std::fs::read_to_string(&p) else {
        return;
    };
    let ours: BTreeSet<&str> = LEGACY_GITATTRIBUTES_BODY.lines().collect();
    let kept: Vec<&str> = body.lines().filter(|l| !ours.contains(l.trim())).collect();
    let result = if kept.iter().all(|l| l.trim().is_empty()) {
        std::fs::remove_file(&p)
    } else {
        std::fs::write(&p, format!("{}\n", kept.join("\n")))
    };
    if let Err(err) = result {
        tracing::debug!(path = ?p, %err, "could not clean .musts/.gitattributes");
    }
}

/// Conflict markers must be caught before parsing: `<<<<<<< HEAD`
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

    fn candidates(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(c, hs)| {
                (
                    (*c).to_string(),
                    hs.iter().map(|h| (*h).to_string()).collect(),
                )
            })
            .collect()
    }

    fn write_legacy(dir: &Path, body: &str) {
        std::fs::write(path(dir), body).unwrap();
    }

    #[test]
    fn load_missing_state_yields_empty_ledger() {
        let dir = tmp();
        assert!(load(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn record_then_load_roundtrips() {
        let dir = tmp();
        record(dir.path(), "root/fmt", "abc123").unwrap();
        record(dir.path(), "root/clippy", "def456").unwrap();
        let ledger = load(dir.path()).unwrap();
        assert!(ledger.contains("root/fmt", "abc123"));
        assert!(ledger.contains("root/clippy", "def456"));
        assert!(!ledger.contains("root/fmt", "def456"));
    }

    #[test]
    fn recording_again_replaces_the_hash_and_keeps_one_file() {
        let dir = tmp();
        record(dir.path(), "root/fmt", "old").unwrap();
        record(dir.path(), "root/fmt", "new").unwrap();
        let ledger = load(dir.path()).unwrap();
        assert!(ledger.contains("root/fmt", "new"));
        assert!(!ledger.contains("root/fmt", "old"));
        let files = std::fs::read_dir(ledger_dir(dir.path())).unwrap().count();
        assert_eq!(files, 1);
    }

    #[test]
    fn an_entry_is_one_file_holding_one_line() {
        let dir = tmp();
        record(dir.path(), "root/build-ios", "aa").unwrap();
        let body =
            std::fs::read_to_string(ledger_dir(dir.path()).join("root%2Fbuild-ios")).unwrap();
        assert_eq!(body, "aa\n");
    }

    #[test]
    fn file_names_are_deterministic_and_reversible() {
        for check in [
            "root/clippy",
            "a",
            "a/b",
            ".hidden/x",
            "root/build, ios: \"x\"",
            "ünï/çödé",
            "100%/real",
        ] {
            let name = entry_file_name(check);
            assert!(!name.contains('/'), "{name}");
            assert!(!name.starts_with('.'), "{name}");
            assert_eq!(decode_file_name(&name).as_deref(), Some(check), "{name}");
        }
        assert_eq!(entry_file_name("root/clippy"), "root%2Fclippy");
        assert_ne!(entry_file_name("a"), entry_file_name("a/b"));
    }

    #[test]
    fn check_ids_with_awkward_characters_round_trip() {
        let dir = tmp();
        record(dir.path(), "root/build, ios: \"x\"", "aa").unwrap();
        assert!(load(dir.path())
            .unwrap()
            .contains("root/build, ios: \"x\"", "aa"));
    }

    #[test]
    fn leftover_temp_files_are_not_entries() {
        let dir = tmp();
        record(dir.path(), "a", "h").unwrap();
        std::fs::write(ledger_dir(dir.path()).join(".b.tmp"), "junk\n").unwrap();
        assert_eq!(load(dir.path()).unwrap().current.len(), 1);
    }

    #[test]
    fn unresolved_conflict_markers_name_the_file_and_the_check() {
        let dir = tmp();
        std::fs::create_dir_all(ledger_dir(dir.path())).unwrap();
        std::fs::write(
            ledger_dir(dir.path()).join("root%2Ffmt"),
            "<<<<<<< HEAD\nh1\n=======\nh2\n>>>>>>> feature\n",
        )
        .unwrap();
        let message = load(dir.path()).unwrap_err().to_string();
        assert!(message.contains("root%2Ffmt"), "{message}");
        assert!(message.contains("root/fmt"), "{message}");
        assert!(message.contains("merge conflict"), "{message}");
    }

    #[test]
    fn unsupported_legacy_version_is_rejected() {
        let dir = tmp();
        write_legacy(dir.path(), "version: 2\nsatisfied: []\n");
        assert!(matches!(
            load(dir.path()).unwrap_err(),
            Error::LedgerLock { .. }
        ));
    }

    #[test]
    fn malformed_legacy_yaml_surfaces_as_ledger_lock_error() {
        let dir = tmp();
        write_legacy(dir.path(), "::: not yaml");
        assert!(matches!(
            load(dir.path()).unwrap_err(),
            Error::LedgerLock { .. }
        ));
    }

    #[test]
    fn legacy_entries_still_answer_until_migrated() {
        let dir = tmp();
        write_legacy(
            dir.path(),
            "version: 1\nsatisfied:\n- {check: \"root/fmt\", scope_hash: \"aa\"}\n",
        );
        let ledger = load(dir.path()).unwrap();
        assert!(ledger.contains("root/fmt", "aa"));
        assert!(!ledger.is_empty());
    }

    #[test]
    fn migration_keeps_only_the_hash_matching_the_current_tree() {
        let dir = tmp();
        write_legacy(
            dir.path(),
            "version: 1\nsatisfied:\n\
             - {check: \"root/fmt\", scope_hash: \"old1\"}\n\
             - {check: \"root/fmt\", scope_hash: \"now\"}\n\
             - {check: \"root/fmt\", scope_hash: \"old2\"}\n",
        );
        let n = migrate_legacy(dir.path(), &candidates(&[("root/fmt", &["now"])])).unwrap();
        assert_eq!(n, Some(1));
        let ledger = load(dir.path()).unwrap();
        assert_eq!(
            ledger.current.get("root/fmt").map(String::as_str),
            Some("now")
        );
        assert!(ledger.legacy.satisfied.is_empty());
        assert!(!path(dir.path()).exists(), "the old file is retired");
    }

    #[test]
    fn migration_falls_back_to_the_legacy_scope_hash() {
        let dir = tmp();
        write_legacy(
            dir.path(),
            "version: 1\nsatisfied:\n- {check: \"root/fmt\", scope_hash: \"legacy\"}\n",
        );
        migrate_legacy(dir.path(), &candidates(&[("root/fmt", &["new", "legacy"])])).unwrap();
        assert!(load(dir.path()).unwrap().contains("root/fmt", "legacy"));
    }

    #[test]
    fn migration_drops_undeclared_checks_and_checks_without_a_match() {
        let dir = tmp();
        write_legacy(
            dir.path(),
            "version: 1\nsatisfied:\n\
             - {check: \"gone/check\", scope_hash: \"h\"}\n\
             - {check: \"root/stale\", scope_hash: \"old\"}\n\
             - {check: \"root/ok\", scope_hash: \"h\"}\n",
        );
        let n = migrate_legacy(
            dir.path(),
            &candidates(&[("root/stale", &["new"]), ("root/ok", &["h"])]),
        )
        .unwrap();
        assert_eq!(n, Some(1));
        let ledger = load(dir.path()).unwrap();
        assert_eq!(ledger.current.keys().collect::<Vec<_>>(), ["root/ok"]);
    }

    #[test]
    fn migration_reads_block_style_locks_from_older_releases() {
        let dir = tmp();
        write_legacy(
            dir.path(),
            "version: 1\nsatisfied:\n- check: root/build-ios\n  scope_hash: aa\n\
             - check: root/build-ios\n  scope_hash: bb\n",
        );
        migrate_legacy(dir.path(), &candidates(&[("root/build-ios", &["bb"])])).unwrap();
        assert!(load(dir.path()).unwrap().contains("root/build-ios", "bb"));
    }

    #[test]
    fn migration_never_overwrites_a_newer_per_check_file() {
        let dir = tmp();
        record(dir.path(), "root/fmt", "newer").unwrap();
        write_legacy(
            dir.path(),
            "version: 1\nsatisfied:\n- {check: \"root/fmt\", scope_hash: \"older\"}\n",
        );
        migrate_legacy(dir.path(), &candidates(&[("root/fmt", &["older"])])).unwrap();
        assert!(load(dir.path()).unwrap().contains("root/fmt", "newer"));
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = tmp();
        write_legacy(
            dir.path(),
            "version: 1\nsatisfied:\n- {check: \"root/fmt\", scope_hash: \"h\"}\n",
        );
        let c = candidates(&[("root/fmt", &["h"])]);
        assert_eq!(migrate_legacy(dir.path(), &c).unwrap(), Some(1));
        assert_eq!(migrate_legacy(dir.path(), &c).unwrap(), None);
        assert!(load(dir.path()).unwrap().contains("root/fmt", "h"));
    }

    #[test]
    fn migration_without_a_legacy_lock_does_nothing() {
        let dir = tmp();
        assert_eq!(migrate_legacy(dir.path(), &BTreeMap::new()).unwrap(), None);
        assert!(!ledger_dir(dir.path()).exists());
    }

    #[test]
    fn migration_with_conflict_markers_fails_and_keeps_the_old_file() {
        let dir = tmp();
        write_legacy(
            dir.path(),
            "version: 1\nsatisfied:\n<<<<<<< HEAD\n=======\n>>>>>>> x\n",
        );
        assert!(migrate_legacy(dir.path(), &BTreeMap::new()).is_err());
        assert!(path(dir.path()).exists());
    }

    #[test]
    fn migration_removes_the_merge_union_file_musts_wrote() {
        let dir = tmp();
        write_legacy(dir.path(), "version: 1\nsatisfied: []\n");
        std::fs::write(
            dir.path().join(GITATTRIBUTES_FILENAME),
            LEGACY_GITATTRIBUTES_BODY,
        )
        .unwrap();
        migrate_legacy(dir.path(), &BTreeMap::new()).unwrap();
        assert!(!dir.path().join(GITATTRIBUTES_FILENAME).exists());
    }

    #[test]
    fn migration_keeps_attributes_the_project_added() {
        let dir = tmp();
        write_legacy(dir.path(), "version: 1\nsatisfied: []\n");
        std::fs::write(
            dir.path().join(GITATTRIBUTES_FILENAME),
            format!("*.bin binary\n{LEGACY_GITATTRIBUTES_BODY}"),
        )
        .unwrap();
        migrate_legacy(dir.path(), &BTreeMap::new()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(GITATTRIBUTES_FILENAME)).unwrap(),
            "*.bin binary\n"
        );
    }

    #[test]
    fn recording_never_writes_a_merge_rule() {
        let dir = tmp();
        record(dir.path(), "root/fmt", "h").unwrap();
        assert!(!dir.path().join(GITATTRIBUTES_FILENAME).exists());
    }
}
