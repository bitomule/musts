//! `HARNESS.yml` manifests: discovery, parsing, and stable IDs.
//!
//! See `docs/PLAN.md` §4.3 (modules) and §4.4 (stable IDs).

pub mod discovery;
pub mod ids;
pub mod parser;
pub mod with_validation;

pub use discovery::{discover, ManifestEntry};
pub use ids::{check_id, scope_path_for, ROOT_SCOPE};
pub use parser::{parse, Check, Manifest};
pub use with_validation::validate_with_payload;

/// Conventional manifest filename.
pub const MANIFEST_FILE: &str = "HARNESS.yml";
