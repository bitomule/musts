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

    #[error("extension `{capability}` timed out after {timeout_seconds}s")]
    ExtensionTimeout {
        capability: String,
        timeout_seconds: u64,
    },

    #[error("no extension implements capability `{capability}` referenced by check `{check_id}` in {manifest_path}")]
    MissingExtension {
        manifest_path: PathBuf,
        check_id: String,
        capability: String,
    },

    #[error(".harness/ is not writable; harness needs to create state.sqlite")]
    StateDirReadOnly,

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

    #[error("another harness process is running — retry shortly")]
    LockBusy,
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
            | Error::LockBusy => 2,
            Error::Io { .. } | Error::Db { .. } => 70,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(source: rusqlite::Error) -> Self {
        Error::Db { source }
    }
}
