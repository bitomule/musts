//! Blake3 content hashing with an mtime+size cache key.
//!
//! Per `docs/PLAN.md` §4.5:
//! - Hash function: blake3, 256-bit, hex-encoded.
//! - On warm runs we only rehash a file when (mtime OR size) changes.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};

/// Hex-encoded blake3 digest (64 chars).
pub type HashDigest = String;

/// Cached fingerprint of a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    pub size_bytes: u64,
    pub mtime_ns: i128,
    pub content_hash: HashDigest,
}

impl FileFingerprint {
    /// If `(size, mtime)` match `previous`, return its hash without
    /// recomputing. Otherwise return `None` and the caller must rehash.
    pub fn cached_hash(
        previous: &FileFingerprint,
        size_bytes: u64,
        mtime_ns: i128,
    ) -> Option<HashDigest> {
        if previous.size_bytes == size_bytes && previous.mtime_ns == mtime_ns {
            Some(previous.content_hash.clone())
        } else {
            None
        }
    }
}

/// Compute a blake3 digest over a byte slice.
pub fn hash_bytes(bytes: &[u8]) -> HashDigest {
    blake3::hash(bytes).to_hex().to_string()
}

/// Compute a [`FileFingerprint`] by hashing the file at `path`.
///
/// Reads in 64 KiB chunks to keep memory bounded for large files.
pub fn hash_file(path: &Path) -> Result<FileFingerprint> {
    let metadata = std::fs::metadata(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let size_bytes = metadata.len();
    let mtime_ns = mtime_nanos(&metadata);

    let mut file = File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(FileFingerprint {
        size_bytes,
        mtime_ns,
        content_hash: hasher.finalize().to_hex().to_string(),
    })
}

fn mtime_nanos(metadata: &std::fs::Metadata) -> i128 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn hash_bytes_is_deterministic() {
        assert_eq!(hash_bytes(b"hello"), hash_bytes(b"hello"));
        assert_ne!(hash_bytes(b"hello"), hash_bytes(b"world"));
        // Sanity: hex is 64 chars (256 bits).
        assert_eq!(hash_bytes(b"hello").len(), 64);
    }

    #[test]
    fn hash_file_matches_hash_bytes() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, b"contents").unwrap();
        let fp = hash_file(&p).unwrap();
        assert_eq!(fp.size_bytes, 8);
        assert_eq!(fp.content_hash, hash_bytes(b"contents"));
    }

    #[test]
    fn cached_hash_returns_previous_on_metadata_match() {
        let fp = FileFingerprint {
            size_bytes: 100,
            mtime_ns: 12345,
            content_hash: "deadbeef".into(),
        };
        assert_eq!(
            FileFingerprint::cached_hash(&fp, 100, 12345),
            Some("deadbeef".into())
        );
    }

    #[test]
    fn cached_hash_returns_none_on_size_change() {
        let fp = FileFingerprint {
            size_bytes: 100,
            mtime_ns: 12345,
            content_hash: "deadbeef".into(),
        };
        assert!(FileFingerprint::cached_hash(&fp, 101, 12345).is_none());
    }

    #[test]
    fn cached_hash_returns_none_on_mtime_change() {
        let fp = FileFingerprint {
            size_bytes: 100,
            mtime_ns: 12345,
            content_hash: "deadbeef".into(),
        };
        assert!(FileFingerprint::cached_hash(&fp, 100, 12346).is_none());
    }

    #[test]
    fn hash_file_handles_large_files_in_chunks() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("big.bin");
        // 200 KiB, > the 64 KiB chunk size.
        let bytes = vec![0x42u8; 200 * 1024];
        std::fs::write(&p, &bytes).unwrap();
        let fp = hash_file(&p).unwrap();
        assert_eq!(fp.size_bytes, bytes.len() as u64);
        assert_eq!(fp.content_hash, hash_bytes(&bytes));
    }
}
