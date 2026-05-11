//! Content fingerprints and scope hashes per `docs/PLAN.md` §4.5.
//!
//! Phase 1 ships the primitives:
//! - `paths`: NFC normalisation + case-insensitivity probe.
//! - `fingerprint`: blake3 hashing with an mtime+size cache key.
//! - `scope`: aggregate scope hash construction.
//!
//! The carve-out (a check's effective scope excludes files under deeper
//! same-capability manifests) is computed by the validate orchestrator in
//! Phase 3 by feeding the right set of file fingerprints into
//! [`scope::compute`].

pub mod fingerprint;
pub mod paths;
pub mod scope;

pub use fingerprint::{hash_bytes, hash_file, FileFingerprint, HashDigest};
pub use paths::{is_case_insensitive_fs, normalise_rel_path};
pub use scope::{compute_scope_hash, ScopeHash, ScopeInput};
