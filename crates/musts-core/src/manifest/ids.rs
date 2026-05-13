//! Stable check IDs per `docs/PLAN.md` §4.4.
//!
//! Global format: `<scope_path>/<local_id>`. The root manifest's scope is
//! literally `root` (not `.`). Conflicts are rejected at parse time inside
//! a single manifest; cross-manifest collisions are impossible by
//! construction because the scope path is part of the ID.

use std::path::Path;

/// The scope-path literal used for the root manifest's checks.
pub const ROOT_SCOPE: &str = "root";

/// Compute the scope-path string for a manifest at `manifest_rel_path`
/// (relative to the workspace root). For a root manifest (`MUSTS.yml`
/// at the workspace root), returns the [`ROOT_SCOPE`] sentinel.
///
/// Path components are joined with `/` regardless of host OS — the value is
/// part of stable IDs and must hash identically across platforms.
pub fn scope_path_for(manifest_rel_path: &Path) -> String {
    let parent = manifest_rel_path.parent();
    match parent {
        Some(p) if p.as_os_str().is_empty() => ROOT_SCOPE.to_string(),
        None => ROOT_SCOPE.to_string(),
        Some(p) => p
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/"),
    }
}

/// Build a globally stable check ID: `<scope_path>/<local_id>`.
pub fn check_id(scope_path: &str, local_id: &str) -> String {
    format!("{scope_path}/{local_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn root_manifest_has_root_scope() {
        let p = PathBuf::from("MUSTS.yml");
        assert_eq!(scope_path_for(&p), ROOT_SCOPE);
    }

    #[test]
    fn nested_manifest_scope_uses_forward_slashes() {
        let p = PathBuf::from("App/Login/MUSTS.yml");
        assert_eq!(scope_path_for(&p), "App/Login");
    }

    #[test]
    fn check_id_format_is_stable() {
        assert_eq!(check_id(ROOT_SCOPE, "app-build"), "root/app-build");
        assert_eq!(
            check_id("App/Login", "login-build"),
            "App/Login/login-build"
        );
    }

    #[test]
    fn same_local_id_under_different_scopes_yields_distinct_globals() {
        let a = check_id(ROOT_SCOPE, "login-build");
        let b = check_id("App/Login", "login-build");
        assert_ne!(a, b);
    }
}
