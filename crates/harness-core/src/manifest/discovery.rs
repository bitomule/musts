//! Find every `HARNESS.yml` under the workspace root.
//!
//! Per `docs/PLAN.md` §4.5 we always do a full parallel walk (no dir-mtime
//! optimisation, which is unreliable on APFS/HFS+ and across git checkouts).
//! The `ignore` crate honours `.gitignore` and our built-in ignore list.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::MANIFEST_FILE;

/// One discovered manifest file (path workspace-relative).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub rel_path: PathBuf,
    pub abs_path: PathBuf,
}

/// Walk `workspace_root` and return every `HARNESS.yml` encountered, sorted
/// by `rel_path` for determinism.
pub fn discover(workspace_root: &Path) -> Result<Vec<ManifestEntry>> {
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let walker = ignore::WalkBuilder::new(workspace_root)
        .standard_filters(true)
        .git_ignore(true)
        .git_exclude(true)
        // Honour `.gitignore` even outside a git repo (the workspace might
        // not be a git checkout, e.g. when discovered via the
        // HARNESS.yml fallback rule).
        .require_git(false)
        .hidden(false)
        .follow_links(false)
        .filter_entry(skip_built_in_ignores)
        .build();

    for entry in walker {
        let entry = entry.map_err(|err| Error::Io {
            path: workspace_root.to_path_buf(),
            source: err
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("manifest discovery error")),
        })?;
        if entry.file_type().is_some_and(|t| t.is_file()) && entry.file_name() == MANIFEST_FILE {
            let abs_path = entry.path().to_path_buf();
            let rel_path = abs_path
                .strip_prefix(workspace_root)
                .unwrap_or(&abs_path)
                .to_path_buf();
            entries.push(ManifestEntry { rel_path, abs_path });
        }
    }
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(entries)
}

/// Built-in ignore predicate. These directories never contain manifests we
/// care about, and walking into them on huge monorepos is wasteful.
fn skip_built_in_ignores(entry: &ignore::DirEntry) -> bool {
    let Some(name) = entry.file_name().to_str() else {
        return true;
    };
    !matches!(
        name,
        ".git"
            | ".harness"
            | "node_modules"
            | "target"
            | "bazel-bin"
            | "bazel-out"
            | "bazel-testlogs"
            | "DerivedData"
            | "xcuserdata"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn finds_root_and_nested_manifests_sorted() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(&root.join("HARNESS.yml"), "version: 1\nchecks: {}\n");
        write(
            &root.join("App/Login/HARNESS.yml"),
            "version: 1\nchecks: {}\n",
        );
        write(
            &root.join("App/Checkout/HARNESS.yml"),
            "version: 1\nchecks: {}\n",
        );

        let entries = discover(root).unwrap();
        let rels: Vec<_> = entries
            .iter()
            .map(|e| e.rel_path.to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            rels,
            vec![
                "App/Checkout/HARNESS.yml".to_string(),
                "App/Login/HARNESS.yml".to_string(),
                "HARNESS.yml".to_string(),
            ]
        );
    }

    #[test]
    fn skips_built_in_ignored_dirs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(&root.join("HARNESS.yml"), "version: 1\nchecks: {}\n");
        // These should be ignored.
        write(&root.join(".git/HARNESS.yml"), "version: 1\nchecks: {}\n");
        write(
            &root.join(".harness/extensions/HARNESS.yml"),
            "version: 1\nchecks: {}\n",
        );
        write(
            &root.join("node_modules/foo/HARNESS.yml"),
            "version: 1\nchecks: {}\n",
        );
        write(&root.join("target/HARNESS.yml"), "version: 1\nchecks: {}\n");
        write(
            &root.join("bazel-bin/HARNESS.yml"),
            "version: 1\nchecks: {}\n",
        );
        write(
            &root.join("DerivedData/HARNESS.yml"),
            "version: 1\nchecks: {}\n",
        );

        let entries = discover(root).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rel_path, PathBuf::from("HARNESS.yml"));
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let dir = TempDir::new().unwrap();
        let entries = discover(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn obeys_gitignore() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "ignored-dir/\n");
        write(&root.join("HARNESS.yml"), "version: 1\nchecks: {}\n");
        write(
            &root.join("ignored-dir/HARNESS.yml"),
            "version: 1\nchecks: {}\n",
        );
        let entries = discover(root).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rel_path, PathBuf::from("HARNESS.yml"));
    }
}
