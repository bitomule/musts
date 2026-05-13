//! Evidence asset store at `.harness/evidence/<task>/submission-NNN/`.
//!
//! Responsibilities per `docs/PLAN.md` §4.4.1 + §4.2:
//! - Allocate the next `submission-NNN` directory.
//! - Copy each user-supplied asset into the directory.
//! - Compute MIME type and byte size for each asset.
//! - Hold off writing `evidence.json` until *after* the ledger
//!   transaction commits (that step is performed by `submit::submit`).

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Metadata for one asset copied into a submission directory.
#[derive(Debug, Clone)]
pub struct SubmissionAsset {
    /// Absolute path **inside** the evidence store after copying.
    pub stored_path: PathBuf,
    /// Workspace-relative form of [`Self::stored_path`].
    pub workspace_rel: String,
    /// Detected MIME type (best-effort via `mime_guess`; defaults to
    /// `application/octet-stream`).
    pub mime: String,
    /// File size in bytes.
    pub size: u64,
}

/// Live submission directory for a given task.
pub struct EvidenceStore {
    pub task_id: String,
    pub submission_id: String,
    pub dir: PathBuf,
    /// Workspace root so we can compute workspace-relative paths.
    pub workspace_root: PathBuf,
}

impl EvidenceStore {
    /// Allocate a fresh `submission-NNN/` directory for `task_id`.
    pub fn allocate(workspace_root: &Path, evidence_root: &Path, task_id: &str) -> Result<Self> {
        let task_dir = evidence_root.join(task_id);
        std::fs::create_dir_all(&task_dir).map_err(|source| Error::Io {
            path: task_dir.clone(),
            source,
        })?;
        let next = next_submission_number(&task_dir)?;
        let submission_id = format!("submission-{next:03}");
        let dir = task_dir.join(&submission_id);
        std::fs::create_dir_all(&dir).map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
        Ok(Self {
            task_id: task_id.to_string(),
            submission_id,
            dir,
            workspace_root: workspace_root.to_path_buf(),
        })
    }

    /// Copy an asset from `source` into the submission directory.
    /// Returns metadata for the stored copy.
    pub fn add_asset(&self, source: &Path) -> Result<SubmissionAsset> {
        if !source.is_file() {
            return Err(Error::Io {
                path: source.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "asset path is not a regular file",
                ),
            });
        }
        let filename = source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "asset.bin".to_string());
        let dest = unique_dest(&self.dir, &filename);
        std::fs::copy(source, &dest).map_err(|source_err| Error::Io {
            path: dest.clone(),
            source: source_err,
        })?;
        let size = std::fs::metadata(&dest)
            .map_err(|source_err| Error::Io {
                path: dest.clone(),
                source: source_err,
            })?
            .len();
        let mime = mime_guess::from_path(&dest)
            .first_or_octet_stream()
            .essence_str()
            .to_string();
        let workspace_rel = dest
            .strip_prefix(&self.workspace_root)
            .unwrap_or(&dest)
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/");
        Ok(SubmissionAsset {
            stored_path: dest,
            workspace_rel,
            mime,
            size,
        })
    }

    /// Write `evidence.json` last so a crashed submission is detectable
    /// by [`crate::validate::gc_orphan_submissions`]. The body is the
    /// raw bytes the caller passes in (typically a normalised JSON
    /// record).
    pub fn write_marker(&self, body: &[u8]) -> Result<()> {
        let path = self.dir.join("evidence.json");
        std::fs::write(&path, body).map_err(|source| Error::Io { path, source })
    }
}

/// Compute the next `submission-NNN` integer for a task directory by
/// scanning existing entries.
fn next_submission_number(task_dir: &Path) -> Result<u32> {
    let mut max = 0u32;
    let read = match std::fs::read_dir(task_dir) {
        Ok(r) => r,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(1),
        Err(err) => {
            return Err(Error::Io {
                path: task_dir.to_path_buf(),
                source: err,
            });
        }
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(suffix) = name.strip_prefix("submission-") {
            if let Ok(n) = suffix.parse::<u32>() {
                if n > max {
                    max = n;
                }
            }
        }
    }
    Ok(max + 1)
}

fn unique_dest(dir: &Path, filename: &str) -> PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match filename.rfind('.') {
        Some(idx) if idx > 0 => (&filename[..idx], &filename[idx..]),
        _ => (filename, ""),
    };
    for n in 1..1000 {
        let next = dir.join(format!("{stem}-{n}{ext}"));
        if !next.exists() {
            return next;
        }
    }
    dir.join(filename) // pathological — let the copy fail loudly later
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn allocate_starts_at_001_and_increments() {
        let workspace = TempDir::new().unwrap();
        let evidence = workspace.path().join(".harness/evidence");
        let one = EvidenceStore::allocate(workspace.path(), &evidence, "t").unwrap();
        assert!(one.submission_id.ends_with("001"));
        let two = EvidenceStore::allocate(workspace.path(), &evidence, "t").unwrap();
        assert!(two.submission_id.ends_with("002"));
    }

    #[test]
    fn add_asset_copies_and_reports_size() {
        let workspace = TempDir::new().unwrap();
        let evidence = workspace.path().join(".harness/evidence");
        let store = EvidenceStore::allocate(workspace.path(), &evidence, "t").unwrap();
        let src = workspace.path().join("img.png");
        std::fs::write(&src, [0x89, 0x50, 0x4E, 0x47]).unwrap();
        let asset = store.add_asset(&src).unwrap();
        assert!(asset.stored_path.is_file());
        assert_eq!(asset.size, 4);
        assert_eq!(asset.mime, "image/png");
        assert!(asset.workspace_rel.contains("submission-001"));
    }

    #[test]
    fn add_asset_avoids_filename_collisions() {
        let workspace = TempDir::new().unwrap();
        let evidence = workspace.path().join(".harness/evidence");
        let store = EvidenceStore::allocate(workspace.path(), &evidence, "t").unwrap();
        let src = workspace.path().join("a.log");
        std::fs::write(&src, b"first").unwrap();
        let first = store.add_asset(&src).unwrap();
        std::fs::write(&src, b"second").unwrap();
        let second = store.add_asset(&src).unwrap();
        assert_ne!(first.stored_path, second.stored_path);
    }

    #[test]
    fn write_marker_creates_evidence_json() {
        let workspace = TempDir::new().unwrap();
        let evidence = workspace.path().join(".harness/evidence");
        let store = EvidenceStore::allocate(workspace.path(), &evidence, "t").unwrap();
        store.write_marker(br#"{"x":1}"#).unwrap();
        let p = store.dir.join("evidence.json");
        assert_eq!(std::fs::read(p).unwrap(), b"{\"x\":1}");
    }
}
