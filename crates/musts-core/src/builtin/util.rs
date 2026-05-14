//! Shared helpers used by every built-in capability.
//!
//! Kept private (`pub(super)`) so the surface stays internal to the
//! built-in registry; third-party Rust extensions reach for the same
//! helpers in [`musts_extension_util::asset_kind`].

use musts_protocol::EvidenceAsset;

/// Turn a scope path into the slug used in task IDs.
///
/// `""` and `"root"` both collapse to `"root"`; nested scopes have `/`
/// replaced with `-` and are lowercased so the slug is stable across
/// case-insensitive filesystems.
pub(super) fn scope_slug(scope: &str) -> String {
    if scope.is_empty() || scope == "root" {
        "root".into()
    } else {
        scope.replace('/', "-").to_lowercase()
    }
}

/// `text/*` or `application/octet-stream` — the MIME shape of a captured
/// log file when the agent pipes stdout/stderr through a redirect.
pub(super) fn is_log_or_text(asset: &EvidenceAsset) -> bool {
    asset.mime.starts_with("text/") || asset.mime == "application/octet-stream"
}

pub(super) fn is_image(asset: &EvidenceAsset) -> bool {
    asset.mime.starts_with("image/")
}

pub(super) fn is_video(asset: &EvidenceAsset) -> bool {
    asset.mime.starts_with("video/")
}

pub(super) fn is_json(asset: &EvidenceAsset) -> bool {
    asset.mime == "application/json"
}
