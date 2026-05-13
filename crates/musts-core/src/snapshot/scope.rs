//! Scope-hash construction per `docs/PLAN.md` §4.5.
//!
//! ```text
//! scope_hash = blake3(
//!     sorted_join(rel_path || "\0" || file_hash) || "\0" ||
//!     manifest_hash || "\0" ||
//!     ext_descriptor_hash || "\0" ||
//!     sorted_join(descendant_manifest_rel_path)
//! )
//! ```
//!
//! Including descendant manifest *paths* (not contents — those are hashed
//! into their own scopes) means adding/removing a child manifest
//! invalidates the parent's idea of "what's applicable to me." Including
//! the extension descriptor hash means swapping an extension invalidates
//! evidence.
//!
//! This module just builds the hash. The carve-out logic (which files go
//! in) lives in the validate orchestrator (Phase 3).

use std::cmp::Ordering;

use super::fingerprint::HashDigest;

/// Identifier for a scope hash. Hex-encoded blake3, 64 chars.
pub type ScopeHash = String;

/// Inputs to a scope hash. Owned to keep the API simple; scopes hold
/// O(hundreds) of entries even on large repos, so allocation cost is
/// negligible.
#[derive(Debug, Clone, Default)]
pub struct ScopeInput {
    /// Normalised rel-path + content-hash pairs of every file in the
    /// effective scope. Ordering provided by the caller doesn't matter —
    /// the computation sorts internally.
    pub files: Vec<(String, HashDigest)>,
    /// blake3 of the declaring manifest's file bytes.
    pub manifest_hash: HashDigest,
    /// Aggregate hash of every loaded extension descriptor's bytes.
    /// Same value for every scope in a given run.
    pub ext_descriptor_hash: HashDigest,
    /// Sorted-internally list of descendant-manifest rel-paths (no contents).
    pub descendant_manifest_paths: Vec<String>,
}

/// Compute the scope hash for the inputs.
pub fn compute_scope_hash(input: &ScopeInput) -> ScopeHash {
    let mut hasher = blake3::Hasher::new();

    // Files, sorted lexically (post-normalisation done by the caller).
    let mut files = input.files.clone();
    files.sort_by(|a, b| match a.0.cmp(&b.0) {
        Ordering::Equal => a.1.cmp(&b.1),
        other => other,
    });
    for (rel, hash) in &files {
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(hash.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(input.manifest_hash.as_bytes());
    hasher.update(b"\0");
    hasher.update(input.ext_descriptor_hash.as_bytes());
    hasher.update(b"\0");

    let mut desc = input.descendant_manifest_paths.clone();
    desc.sort();
    for p in &desc {
        hasher.update(p.as_bytes());
        hasher.update(b"\0");
    }

    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(content: &str) -> HashDigest {
        super::super::fingerprint::hash_bytes(content.as_bytes())
    }

    fn base() -> ScopeInput {
        ScopeInput {
            files: vec![
                ("a.swift".into(), fp("a-body")),
                ("b/c.swift".into(), fp("c-body")),
            ],
            manifest_hash: fp("manifest"),
            ext_descriptor_hash: fp("ext"),
            descendant_manifest_paths: vec![],
        }
    }

    #[test]
    fn is_deterministic_and_sorted_internally() {
        let a = compute_scope_hash(&base());
        let mut shuffled = base();
        shuffled.files.reverse();
        let b = compute_scope_hash(&shuffled);
        assert_eq!(a, b, "scope hash must be order-independent");
    }

    #[test]
    fn changing_a_file_changes_the_hash() {
        let original = compute_scope_hash(&base());
        let mut modified = base();
        modified.files[0].1 = fp("a-body-modified");
        assert_ne!(original, compute_scope_hash(&modified));
    }

    #[test]
    fn manifest_hash_affects_scope_hash() {
        let original = compute_scope_hash(&base());
        let mut modified = base();
        modified.manifest_hash = fp("manifest-changed");
        assert_ne!(original, compute_scope_hash(&modified));
    }

    #[test]
    fn ext_descriptor_hash_affects_scope_hash() {
        let original = compute_scope_hash(&base());
        let mut modified = base();
        modified.ext_descriptor_hash = fp("ext-upgrade");
        assert_ne!(original, compute_scope_hash(&modified));
    }

    #[test]
    fn adding_a_descendant_manifest_changes_the_hash() {
        let original = compute_scope_hash(&base());
        let mut modified = base();
        modified.descendant_manifest_paths = vec!["App/Login/MUSTS.yml".into()];
        assert_ne!(original, compute_scope_hash(&modified));
    }

    #[test]
    fn descendant_paths_order_independent() {
        let mut a = base();
        a.descendant_manifest_paths = vec!["a/MUSTS.yml".into(), "b/MUSTS.yml".into()];
        let mut b = base();
        b.descendant_manifest_paths = vec!["b/MUSTS.yml".into(), "a/MUSTS.yml".into()];
        assert_eq!(compute_scope_hash(&a), compute_scope_hash(&b));
    }
}
