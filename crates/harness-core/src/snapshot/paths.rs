//! Path normalisation for hash stability across macOS APFS/HFS+ and Linux.
//!
//! Per `docs/PLAN.md` §4.5:
//! - Every relative path that feeds into a hash is **NFC-normalised**.
//! - On case-insensitive filesystems the path is additionally lowercased
//!   *for hashing purposes only*. The original casing on disk is preserved
//!   for everything surfaced to users and extensions.
//! - The detection happens once per workspace by probing the filesystem.

use std::path::Path;

use unicode_normalization::UnicodeNormalization;

/// Probe the directory for case-insensitive behaviour by creating a tiny
/// file with mixed casing and checking if it can be opened with the
/// opposite case. Returns `false` on any I/O error — we prefer the more
/// conservative "case-sensitive" assumption when in doubt because it never
/// produces *false equivalences* (only false invalidations).
pub fn is_case_insensitive_fs(probe_dir: &Path) -> bool {
    let probe = probe_dir.join(".harness-case-probe-XyZ");
    if std::fs::write(&probe, b"").is_err() {
        return false;
    }
    let alt = probe_dir.join(".harness-case-probe-xyz");
    let case_insensitive = alt.exists();
    let _ = std::fs::remove_file(&probe);
    case_insensitive
}

/// NFC-normalise a relative path, optionally lowercasing for the
/// case-insensitive-FS case. The returned string uses `/` separators and is
/// stable across host operating systems.
pub fn normalise_rel_path(rel_path: &Path, case_insensitive: bool) -> String {
    let mut s = rel_path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
        .nfc()
        .collect::<String>();
    if case_insensitive {
        s = s.to_lowercase();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn normalises_to_nfc_and_uses_forward_slashes() {
        // U+00E9 (NFC é) vs U+0065 U+0301 (NFD é) must hash to the same string.
        let nfd_e_accent = "e\u{0301}";
        let path_nfc = PathBuf::from("é/file.txt");
        let path_nfd = PathBuf::from(format!("{nfd_e_accent}/file.txt"));
        assert_eq!(normalise_rel_path(&path_nfc, false), "é/file.txt");
        assert_eq!(normalise_rel_path(&path_nfd, false), "é/file.txt");
        assert_eq!(
            normalise_rel_path(&path_nfc, false),
            normalise_rel_path(&path_nfd, false)
        );
    }

    #[test]
    fn case_insensitive_lowercases() {
        let p = PathBuf::from("App/Login/View.swift");
        assert_eq!(normalise_rel_path(&p, false), "App/Login/View.swift");
        assert_eq!(normalise_rel_path(&p, true), "app/login/view.swift");
    }
}
