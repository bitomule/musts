//! Path normalisation for scope-hash stability across host operating
//! systems.
//!
//! The scope hash treats two rel-paths as equal iff their NFC-normalised,
//! ASCII-lowercased forms match. This makes the lock file in
//! `.musts/ledger.lock.yaml` portable: a hash computed on macOS APFS
//! (case-insensitive by default) matches the one computed on Linux ext4
//! (case-sensitive) for the same repo contents.
//!
//! The cost of always lowercasing is that two files differing only in case
//! (`Foo.txt` and `foo.txt`) on a case-sensitive filesystem collide to the
//! same hash key. The validate orchestrator detects this and returns
//! [`crate::error::Error::CasePathCollision`] before any hash is consumed.
//! Case-insensitive filesystems can't have those two files coexist, so the
//! collision only matters on Linux/Windows-case-sensitive setups — and even
//! there it represents a misconfigured repo that already breaks any clone
//! onto macOS or Windows.

use std::path::Path;

use unicode_normalization::UnicodeNormalization;

/// NFC-normalise and ASCII-lowercase a relative path. The returned string
/// uses `/` separators and is stable across host operating systems.
///
/// Always lowercases regardless of the host filesystem's case sensitivity
/// — see the module docstring for the rationale.
pub fn normalise_rel_path(rel_path: &Path) -> String {
    rel_path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
        .nfc()
        .collect::<String>()
        .to_lowercase()
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
        assert_eq!(normalise_rel_path(&path_nfc), "é/file.txt");
        assert_eq!(normalise_rel_path(&path_nfd), "é/file.txt");
        assert_eq!(normalise_rel_path(&path_nfc), normalise_rel_path(&path_nfd));
    }

    #[test]
    fn always_lowercases_regardless_of_host_fs() {
        let p = PathBuf::from("App/Login/View.swift");
        assert_eq!(normalise_rel_path(&p), "app/login/view.swift");
    }

    #[test]
    fn lowercases_unicode_paths() {
        let p = PathBuf::from("Café/Über.txt");
        assert_eq!(normalise_rel_path(&p), "café/über.txt");
    }
}
