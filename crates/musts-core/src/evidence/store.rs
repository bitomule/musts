//! Evidence asset description.
//!
//! musts no longer archives evidence assets into
//! `.musts/evidence/<task>/submission-NNN/`. Accepted evidence is recorded
//! in the portable ledger (`evidence_records` + the committed
//! `.musts/ledger.lock.yaml`) keyed by `(check_id, scope_hash)` — that is
//! the durable, reviewable record. The asset itself is validated **in
//! place** and never copied, so the loop stops accumulating dozens of
//! identical build-log snapshots (a build re-validated 52 times used to
//! leave `submission-001…052/`).
//!
//! This module describes an on-disk asset (absolute path, MIME, size) for
//! the validation request and mints a submission id for the ledger row.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use musts_protocol::EvidenceAsset;

use crate::error::{Error, Result};

/// Describe an on-disk asset without copying it. The returned
/// `EvidenceAsset.path` is **absolute** so capability validators (which
/// read `workspace_root.join(path)`) resolve it wherever it lives —
/// including a `$TMPDIR` log captured by `musts run`.
pub fn describe_asset(source: &Path) -> Result<EvidenceAsset> {
    let abs = std::fs::canonicalize(source).map_err(|source_err| Error::Io {
        path: source.to_path_buf(),
        source: source_err,
    })?;
    let meta = std::fs::metadata(&abs).map_err(|source_err| Error::Io {
        path: abs.clone(),
        source: source_err,
    })?;
    if !meta.is_file() {
        return Err(Error::Io {
            path: abs.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "asset path is not a regular file",
            ),
        });
    }
    let mime = mime_guess::from_path(&abs)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    Ok(EvidenceAsset {
        path: abs.to_string_lossy().into_owned(),
        mime,
        size: meta.len(),
    })
}

/// Mint a unique submission id without creating any directory. Evidence is
/// no longer archived, but the ledger still keys rows by submission id, so
/// the value must be unique per submission. Nanosecond precision plus the
/// process id makes collisions between concurrent submitters vanishingly
/// unlikely.
pub fn new_submission_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("submission-{nanos}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn describe_asset_reports_mime_and_size_without_copying() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("img.png");
        std::fs::write(&src, [0x89, 0x50, 0x4E, 0x47]).unwrap();
        let asset = describe_asset(&src).unwrap();
        assert_eq!(asset.size, 4);
        assert_eq!(asset.mime, "image/png");
        // Path is absolute and points at the ORIGINAL file (not a copy).
        assert!(Path::new(&asset.path).is_absolute());
        assert!(Path::new(&asset.path).is_file());
        // No `.musts/evidence` directory was created.
        assert!(!dir.path().join(".musts/evidence").exists());
    }

    #[test]
    fn describe_asset_rejects_missing_file() {
        let dir = TempDir::new().unwrap();
        let err = describe_asset(&dir.path().join("nope.log")).unwrap_err();
        assert!(matches!(err, Error::Io { .. }));
    }

    #[test]
    fn describe_asset_rejects_directory() {
        let dir = TempDir::new().unwrap();
        let err = describe_asset(dir.path()).unwrap_err();
        assert!(matches!(err, Error::Io { .. }));
    }

    #[test]
    fn new_submission_id_is_prefixed_and_carries_pid() {
        let id = new_submission_id();
        assert!(id.starts_with("submission-"));
        assert!(id.ends_with(&format!("-{}", std::process::id())));
    }
}
