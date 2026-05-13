//! Workspace-root resolution.
//!
//! Implements `docs/PLAN.md` §5 rules:
//! 1. Explicit `--workspace <path>`: canonicalise and use verbatim.
//! 2. Walk upward from canonicalised cwd to the nearest ancestor with a
//!    `.git` *directory*, **or** a `.git` *file* (gitlink) that points
//!    into `.git/worktrees/` — a worktree is a standalone validation
//!    boundary, even though it shares the underlying git database with
//!    the main checkout. Submodule gitlinks (`.git/modules/`) stay
//!    transparent so cwd inside a submodule resolves to the outer repo.
//! 3. Else: walk upward to the nearest ancestor containing a `MUSTS.yml`.
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
            "no .git directory or MUSTS.yml found from {}; pass --workspace <path>",
            start.display()
        ),
    })
}

fn canonicalise(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .map_err(|source| Error::WorkspaceCanonicalisation { source })
}

/// Walk upward looking for the nearest workspace boundary.
///
/// A boundary is:
/// - a `.git` directory (regular checkout), or
/// - a `.git` file whose `gitdir:` points into `<…>/worktrees/<name>`
///   (a `git worktree add` checkout — a separate working tree that
///   shares the object database with another checkout, but should be
///   validated on its own).
///
/// Submodule gitlinks (`gitdir:` pointing into `<…>/modules/<name>`)
/// stay transparent so cwd inside a submodule resolves to the outer repo.
fn find_git_anchor(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(".git");
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.is_dir() => return Some(ancestor.to_path_buf()),
            Ok(_) => {
                if is_worktree_gitlink(&candidate) {
                    return Some(ancestor.to_path_buf());
                }
                continue;
            }
            Err(_) => continue,
        }
    }
    None
}

/// Read a `.git` gitlink file and decide whether it identifies a
/// `git worktree`-style checkout (as opposed to a submodule).
///
/// The gitlink body is a single `gitdir: <path>` line. Worktrees keep
/// their per-worktree git directory under `<main>/.git/worktrees/<name>`;
/// submodules use `<parent>/.git/modules/<name>`. We accept any path
/// whose final two segments are `worktrees/<name>` as a worktree —
/// good enough to disambiguate the two cases without parsing git's
/// internal layout further.
fn is_worktree_gitlink(path: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some(rest) = body.lines().find_map(|l| l.strip_prefix("gitdir:")) else {
        return false;
    };
    let gitdir = Path::new(rest.trim());
    let mut comps = gitdir.components().rev();
    let _leaf = comps.next();
    matches!(
        comps.next(),
        Some(std::path::Component::Normal(name)) if name == std::ffi::OsStr::new("worktrees")
    )
}

/// Walk upward looking for the nearest ancestor containing `MUSTS.yml`.
/// Stop at the first match — do not climb past it.
fn find_manifest_anchor(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        if ancestor.join("MUSTS.yml").is_file() {
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
        fs::write(nested.join("MUSTS.yml"), "version: 1\nchecks: {}\n").unwrap();
        let resolved = resolve(None, &nested).unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn submodule_gitlink_is_transparent() {
        // `.git` as a file pointing into `.git/modules/…` → keep walking.
        let outer = tmp();
        fs::create_dir(outer.path().join(".git")).unwrap();
        let inner = outer.path().join("submodule");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join(".git"), "gitdir: ../.git/modules/submodule\n").unwrap();
        let resolved = resolve(None, &inner).unwrap();
        assert_eq!(resolved, outer.path().canonicalize().unwrap());
    }

    #[test]
    fn worktree_gitlink_anchors_locally() {
        // `.git` as a file pointing into `.git/worktrees/…` → stop here.
        // A `git worktree add` checkout is a standalone validation boundary
        // even though it shares the object database with the main checkout.
        let outer = tmp();
        fs::create_dir(outer.path().join(".git")).unwrap();
        let inner = outer.path().join("wt");
        fs::create_dir(&inner).unwrap();
        fs::write(
            inner.join(".git"),
            "gitdir: /tmp/main/.git/worktrees/feature\n",
        )
        .unwrap();
        let resolved = resolve(None, &inner).unwrap();
        assert_eq!(resolved, inner.canonicalize().unwrap());
    }

    #[test]
    fn manifest_fallback_when_no_git() {
        let dir = tmp();
        fs::write(dir.path().join("MUSTS.yml"), "version: 1\nchecks: {}\n").unwrap();
        let nested = dir.path().join("a");
        fs::create_dir(&nested).unwrap();
        let resolved = resolve(None, &nested).unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn manifest_fallback_stops_at_first_match() {
        let outer = tmp();
        fs::write(outer.path().join("MUSTS.yml"), "version: 1\nchecks: {}\n").unwrap();
        let inner = outer.path().join("a");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join("MUSTS.yml"), "version: 1\nchecks: {}\n").unwrap();
        let cwd = inner.join("b");
        fs::create_dir(&cwd).unwrap();
        // From `a/b/`, the nearest MUSTS.yml is `a/MUSTS.yml`, not the outer one.
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
