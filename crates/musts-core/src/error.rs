//! Crate-wide error type.
//!
//! Variants map onto the CLI exit codes documented in `docs/PLAN.md` §5.

use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    // -----------------------------------------------------------------------
    // Configuration errors (exit code 2)
    // -----------------------------------------------------------------------
    #[error("workspace root not found: {message}")]
    WorkspaceNotFound { message: String },

    #[error("could not canonicalise workspace path: {source}; pass --workspace <path>")]
    WorkspaceCanonicalisation {
        #[source]
        source: std::io::Error,
    },

    #[error("manifest error in {path}: {message}")]
    Manifest { path: PathBuf, message: String },

    #[error("manifest YAML at {path} is invalid: {source}")]
    ManifestYaml {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    #[error("manifest {manifest_path}: check `{check_id}` `with` payload violates {capability} schema at `{pointer}`: {message}")]
    WithSchema {
        manifest_path: PathBuf,
        check_id: String,
        capability: String,
        pointer: String,
        message: String,
    },

    #[error("extension descriptor at {path}: {message}")]
    ExtensionDescriptor { path: PathBuf, message: String },

    #[error("extension `{capability}` failed: {message}")]
    ExtensionFailure { capability: String, message: String },

    #[error("extension `{capability}` timed out after {timeout_seconds}s{stderr}",
        stderr = if stderr.is_empty() { String::new() } else { format!(" — stderr: {stderr}") })]
    ExtensionTimeout {
        capability: String,
        timeout_seconds: u64,
        /// Trimmed contents of the child's stderr stream when the
        /// timeout fired. Empty when the child wrote nothing.
        stderr: String,
    },

    #[error("no extension implements capability `{capability}` referenced by check `{check_id}` in {manifest_path}")]
    MissingExtension {
        manifest_path: PathBuf,
        check_id: String,
        capability: String,
    },

    #[error(".musts/ is not writable; musts needs to create state.sqlite")]
    StateDirReadOnly,

    // -----------------------------------------------------------------------
    // Evidence errors (exit code 2 for stale/unknown, 1 for extension reject)
    // -----------------------------------------------------------------------
    #[error("task `{task_id}` no longer applies — run `musts validate`")]
    TaskNotFound { task_id: String },

    #[error(
        "evidence for task `{task_id}` is stale: files covered by this task changed after the task was issued — run `musts validate` again and follow the new task list"
    )]
    EvidenceStale { task_id: String },

    #[error(
        "extension `{capability}` over-claimed satisfies for task `{task_id}`: unexpected check_id(s) {unexpected:?}"
    )]
    EvidenceOverclaim {
        task_id: String,
        capability: String,
        unexpected: Vec<String>,
    },

    /// Returned when the extension's evidence response is `accepted: false`.
    /// The CLI maps this to exit code 1 (vs. 2 for stale/configuration).
    #[error("evidence for task `{task_id}` was rejected by `{capability}`: {message}")]
    EvidenceRejected {
        task_id: String,
        capability: String,
        message: String,
    },

    // -----------------------------------------------------------------------
    // Internal / I/O errors (exit code 70)
    // -----------------------------------------------------------------------
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("database error: {source}")]
    Db {
        #[source]
        source: rusqlite::Error,
    },

    #[error("another musts process is running — retry shortly")]
    LockBusy,

    #[error("ledger lock at {path}: {message}")]
    LedgerLock { path: PathBuf, message: String },

    #[error(
        "case-only path collision under {workspace_root}: {first} and {second} normalise to the same hash key — rename one (their lowercase forms collide, so the scope hash would be ambiguous)"
    )]
    CasePathCollision {
        workspace_root: PathBuf,
        first: String,
        second: String,
    },
}

impl Error {
    /// CLI exit code per `docs/PLAN.md` §5.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::WorkspaceNotFound { .. }
            | Error::WorkspaceCanonicalisation { .. }
            | Error::Manifest { .. }
            | Error::ManifestYaml { .. }
            | Error::WithSchema { .. }
            | Error::ExtensionDescriptor { .. }
            | Error::ExtensionFailure { .. }
            | Error::ExtensionTimeout { .. }
            | Error::MissingExtension { .. }
            | Error::StateDirReadOnly
            | Error::LockBusy
            | Error::TaskNotFound { .. }
            | Error::EvidenceStale { .. }
            | Error::EvidenceOverclaim { .. }
            | Error::LedgerLock { .. }
            | Error::CasePathCollision { .. } => 2,
            Error::EvidenceRejected { .. } => 1,
            Error::Io { .. } | Error::Db { .. } => 70,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(source: rusqlite::Error) -> Self {
        Error::Db { source }
    }
}
