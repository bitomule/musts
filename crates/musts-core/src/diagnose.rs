//! Workspace-health checks that explain *why* the loop is asking.
//!
//! `validate` answers "what is pending". It never answered "pending
//! because the tree changed" versus "pending because this workspace has
//! no validation state at all" — and those need completely different
//! responses. The first means do the work. The second means the ledger
//! did not travel here, and running the suite proves nothing that was not
//! already proven on `main`.
//!
//! The expensive version of this: a repo gitignored
//! `.musts/ledger.lock.yaml`, so every `git worktree` started with no
//! ledger and every check was dirty there while `main` was fully green.
//! It reads as an unconditional pre-commit block, and `.mustsignore` does
//! not help — the check is dirty from missing state, not from the diff.
//! Nothing in any output said so.

use std::path::Path;
use std::process::Command;

use crate::report::ManifestIssue;
use crate::state::LedgerLock;

/// Pseudo-path used as the `manifest` field for workspace-level findings,
/// which belong to no single manifest.
const WORKSPACE: &str = ".musts";

/// Health findings for the current workspace, as report warnings.
///
/// `has_pending` gates the "no state here" finding: on a clean workspace
/// an empty ledger is unremarkable, and saying so would be noise on every
/// run of a repo that simply has nothing recorded yet.
pub fn workspace_warnings(
    workspace_root: &Path,
    musts_dir: &Path,
    ledger: &LedgerLock,
    has_pending: bool,
) -> Vec<ManifestIssue> {
    let mut out = Vec::new();
    let lock_path = crate::state::lock::path(musts_dir);

    if is_gitignored(workspace_root, &lock_path) {
        out.push(ManifestIssue {
            manifest: WORKSPACE.to_string(),
            message: "ledger.lock.yaml is gitignored. Validation state will not be inherited \
                      by worktrees, teammates, or CI, so every check is dirty in a fresh \
                      checkout even when main is green. Remove it from .gitignore and commit \
                      it — it is the record of what has been validated, not a build artefact."
                .to_string(),
        });
    } else if has_pending && ledger.satisfied.is_empty() {
        // Distinguishing this from "the tree changed" is the whole point:
        // the fix is to obtain the ledger, not to re-run the suite.
        out.push(ManifestIssue {
            manifest: WORKSPACE.to_string(),
            message: format!(
                "no validation state in this workspace — {} is absent or empty. The tasks \
                 below are pending because nothing has been recorded here yet, not because \
                 the tree changed. If main is green, the ledger did not travel to this \
                 checkout.",
                lock_path
                    .strip_prefix(workspace_root)
                    .unwrap_or(&lock_path)
                    .display()
            ),
        });
    }

    out
}

/// Is `path` ignored by git?
///
/// Delegates to `git check-ignore` rather than parsing `.gitignore`:
/// precedence across nested ignore files, `core.excludesFile`, and
/// `.git/info/exclude` are exactly the rules that make a stray entry hard
/// to spot by hand, so re-implementing them would reproduce the bug this
/// is meant to catch.
///
/// Any failure — git missing, not a repo, an unexpected exit — is read as
/// "not ignored". A health hint must never be the reason `validate`
/// behaves differently.
fn is_gitignored(workspace_root: &Path, path: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("check-ignore")
        .arg("-q")
        .arg(path)
        .output()
        .is_ok_and(|out| out.status.code() == Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repo_with_gitignore(body: Option<&str>) -> TempDir {
        let dir = TempDir::new().unwrap();
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["init", "-q"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git init failed; these tests need git on PATH");
        std::fs::create_dir_all(dir.path().join(".musts")).unwrap();
        if let Some(b) = body {
            std::fs::write(dir.path().join(".gitignore"), b).unwrap();
        }
        dir
    }

    fn ledger_with(entries: &[(&str, &str)]) -> LedgerLock {
        let mut l = LedgerLock::default();
        for (c, h) in entries {
            l.record(*c, *h);
        }
        l
    }

    #[test]
    fn gitignored_ledger_is_reported() {
        let dir = repo_with_gitignore(Some(".musts/ledger.lock.yaml\n"));
        let w = workspace_warnings(
            dir.path(),
            &dir.path().join(".musts"),
            &ledger_with(&[("a", "h")]),
            false,
        );
        assert_eq!(w.len(), 1);
        assert!(w[0].message.contains("gitignored"));
        assert!(w[0].message.contains("worktrees"));
    }

    /// The exact shape found in the wild: the repo carefully keeps
    /// `.musts/extensions/` and ignores the sqlite cache, then ignores
    /// the ledger alongside them as if it were another cache.
    #[test]
    fn ledger_ignored_among_legitimate_musts_ignores_is_still_caught() {
        let dir = repo_with_gitignore(Some(
            ".musts/.lock\n.musts/*.sqlite\n.musts/evidence/\n.musts/ledger.lock.yaml\n!.musts/extensions/\n",
        ));
        let w = workspace_warnings(
            dir.path(),
            &dir.path().join(".musts"),
            &ledger_with(&[("a", "h")]),
            false,
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].message.contains("gitignored"));
    }

    #[test]
    fn a_tracked_ledger_is_not_reported() {
        let dir = repo_with_gitignore(Some(".musts/state.sqlite\n.musts/.lock\n"));
        let w = workspace_warnings(
            dir.path(),
            &dir.path().join(".musts"),
            &ledger_with(&[("a", "h")]),
            true,
        );
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn empty_ledger_with_pending_tasks_says_it_is_missing_state() {
        let dir = repo_with_gitignore(None);
        let w = workspace_warnings(
            dir.path(),
            &dir.path().join(".musts"),
            &LedgerLock::default(),
            true,
        );
        assert_eq!(w.len(), 1);
        assert!(w[0].message.contains("no validation state"));
        assert!(
            w[0].message.contains("not because"),
            "must distinguish missing state from a changed tree: {}",
            w[0].message
        );
    }

    /// A fresh repo with nothing pending has an empty ledger and that is
    /// entirely normal. Warning there would fire on every run.
    #[test]
    fn empty_ledger_with_nothing_pending_is_silent() {
        let dir = repo_with_gitignore(None);
        let w = workspace_warnings(
            dir.path(),
            &dir.path().join(".musts"),
            &LedgerLock::default(),
            false,
        );
        assert!(w.is_empty(), "{w:?}");
    }

    /// A gitignored ledger is the *cause* of the empty one, so report the
    /// cause and not both.
    #[test]
    fn gitignored_and_empty_reports_only_the_cause() {
        let dir = repo_with_gitignore(Some(".musts/ledger.lock.yaml\n"));
        let w = workspace_warnings(
            dir.path(),
            &dir.path().join(".musts"),
            &LedgerLock::default(),
            true,
        );
        assert_eq!(w.len(), 1);
        assert!(w[0].message.contains("gitignored"));
    }

    #[test]
    fn outside_a_git_repo_nothing_is_reported_as_ignored() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".musts")).unwrap();
        assert!(!is_gitignored(
            dir.path(),
            &dir.path().join(".musts/ledger.lock.yaml")
        ));
    }
}
