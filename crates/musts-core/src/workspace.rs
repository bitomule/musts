//! Workspace-root resolution.
//!
//! Implements `docs/PLAN.md` §5 rules:
//! 1. Explicit `--workspace <path>`: canonicalise and use verbatim.
//! 2. Walk upward from canonicalised cwd to the nearest ancestor with a
//!    `.git` *directory* (not a `.git` file/gitlink — those are submodules
//!    and we transparently keep walking).
//! 3. Else: walk upward to the nearest ancestor containing a `HARNESS.yml`.
//!    Stop at the first one — do not climb across that boundary.
//! 4. Else: not-found error suggesting `--workspace`.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Resolve a workspace root.
///
/// `explicit` corresponds to `--workspace <path>` from the CLI.
/// `cwd` is the caller's current directory (usually `std::env::current_dir()`).
pub fn resolve(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return canonicalise(path);
    }
    let start = canonicalise(cwd)?;
    if let Some(root) = find_git_anchor(&start) {
        return Ok(root);
    }
    if let Some(root) = find_manifest_anchor(&start) {
        return Ok(root);
    }
    Err(Error::WorkspaceNotFound {
        message: format!(
            "no .git directory or HARNESS.yml found from {}; pass --workspace <path>",
            start.display()
        ),
    })
}

fn canonicalise(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .map_err(|source| Error::WorkspaceCanonicalisation { source })
}

/// Walk upward looking for a `.git` **directory**. `.git` *files* (gitlinks
/// used by submodules and worktrees) are transparent — we keep walking so
/// cwd inside a submodule resolves to the outer repo.
fn find_git_anchor(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(".git");
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.is_dir() => return Some(ancestor.to_path_buf()),
            // `.git` is a file (gitlink) or symlink → submodule/worktree → keep walking.
            _ => continue,
        }
    }
    None
}

/// Walk upward looking for the nearest ancestor containing `HARNESS.yml`.
/// Stop at the first match — do not climb past it.
fn find_manifest_anchor(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        if ancestor.join("HARNESS.yml").is_file() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn explicit_workspace_is_used_verbatim() {
        let dir = tmp();
        let resolved = resolve(Some(dir.path()), Path::new("/")).unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn git_anchor_wins_over_deeper_manifest() {
        let dir = tmp();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("HARNESS.yml"), "version: 1\nchecks: {}\n").unwrap();
        let resolved = resolve(None, &nested).unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn submodule_gitlink_is_transparent() {
        // `.git` as a file (gitlink) → keep walking; outer `.git` dir wins.
        let outer = tmp();
        fs::create_dir(outer.path().join(".git")).unwrap();
        let inner = outer.path().join("submodule");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join(".git"), "gitdir: ../.git/modules/submodule\n").unwrap();
        let resolved = resolve(None, &inner).unwrap();
        assert_eq!(resolved, outer.path().canonicalize().unwrap());
    }

    #[test]
    fn manifest_fallback_when_no_git() {
        let dir = tmp();
        fs::write(dir.path().join("HARNESS.yml"), "version: 1\nchecks: {}\n").unwrap();
        let nested = dir.path().join("a");
        fs::create_dir(&nested).unwrap();
        let resolved = resolve(None, &nested).unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn manifest_fallback_stops_at_first_match() {
        let outer = tmp();
        fs::write(outer.path().join("HARNESS.yml"), "version: 1\nchecks: {}\n").unwrap();
        let inner = outer.path().join("a");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join("HARNESS.yml"), "version: 1\nchecks: {}\n").unwrap();
        let cwd = inner.join("b");
        fs::create_dir(&cwd).unwrap();
        // From `a/b/`, the nearest HARNESS.yml is `a/HARNESS.yml`, not the outer one.
        let resolved = resolve(None, &cwd).unwrap();
        assert_eq!(resolved, inner.canonicalize().unwrap());
    }

    #[test]
    fn missing_returns_workspace_not_found() {
        let dir = tmp();
        let err = resolve(None, dir.path()).unwrap_err();
        assert!(matches!(err, Error::WorkspaceNotFound { .. }));
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn broken_cwd_returns_canonicalisation_error() {
        // A path that does not exist canonicalises with an error.
        let err = resolve(None, Path::new("/nope/this/path/does/not/exist")).unwrap_err();
        assert!(matches!(err, Error::WorkspaceCanonicalisation { .. }));
        assert_eq!(err.exit_code(), 2);
    }
}
