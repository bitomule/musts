//! Convenience helpers for Rust-authored harness extensions.
//!
//! The harness ⇄ extension contract is documented in
//! `docs/PLAN.md` §4.6:
//! - exactly one JSON request on stdin, EOF-terminated;
//! - exactly one JSON response on stdout;
//! - stderr is freeform but surfaced on failure.
//!
//! This crate gives Rust extensions a 4-line `main()` and ships
//! kind-by-MIME helpers used by every reference extension.

use std::io::{Read, Write};
use std::process::ExitCode;

use harness_protocol::EvidenceAsset;
use serde::{de::DeserializeOwned, Serialize};

/// Read the entire request from stdin and deserialise it. Returns
/// `Err` with a clean message if the input is missing or malformed.
pub fn read_request<T: DeserializeOwned>() -> Result<T, String> {
    let mut buf = Vec::with_capacity(8 * 1024);
    std::io::stdin()
        .read_to_end(&mut buf)
        .map_err(|err| format!("could not read stdin: {err}"))?;
    serde_json::from_slice(&buf).map_err(|err| format!("request is not valid JSON: {err}"))
}

/// Serialise `response` to a single JSON document on stdout.
pub fn write_response<T: Serialize>(response: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(response).map_err(|err| format!("could not serialise response: {err}"))?;
    std::io::stdout()
        .write_all(&bytes)
        .map_err(|err| format!("could not write stdout: {err}"))?;
    Ok(())
}

/// Minimal extension `main()`. Pass two handlers, one for `resolve` and
/// one for `evidence`. Each receives the deserialised request and
/// returns the response, or an error string that is surfaced on
/// stderr and translated into a non-zero exit code.
pub fn ipc_main<Req1, Resp1, Req2, Resp2>(
    resolve: impl FnOnce(Req1) -> Result<Resp1, String>,
    evidence: impl FnOnce(Req2) -> Result<Resp2, String>,
) -> ExitCode
where
    Req1: DeserializeOwned,
    Resp1: Serialize,
    Req2: DeserializeOwned,
    Resp2: Serialize,
{
    let mode = std::env::args().nth(1).unwrap_or_default();
    let outcome: Result<(), String> = match mode.as_str() {
        "resolve" => read_request::<Req1>().and_then(|req| {
            let resp = resolve(req)?;
            write_response(&resp)
        }),
        "evidence" => read_request::<Req2>().and_then(|req| {
            let resp = evidence(req)?;
            write_response(&resp)
        }),
        "" => Err("missing subcommand (expected `resolve` or `evidence`)".into()),
        other => Err(format!("unknown subcommand `{other}` (expected `resolve` or `evidence`)")),
    };
    match outcome {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("extension: {err}");
            ExitCode::from(2)
        }
    }
}

/// Asset classification helpers per `docs/PLAN.md` §6.
pub mod asset_kind {
    use super::EvidenceAsset;

    /// True when the asset's MIME is `image/*`.
    pub fn is_image(asset: &EvidenceAsset) -> bool {
        asset.mime.starts_with("image/")
    }

    /// True when the asset's MIME is `video/*`.
    pub fn is_video(asset: &EvidenceAsset) -> bool {
        asset.mime.starts_with("video/")
    }

    /// True when the asset's MIME is `text/*` or `application/octet-stream`
    /// (logs commonly land as the latter when extension matching fails).
    pub fn is_log_or_text(asset: &EvidenceAsset) -> bool {
        asset.mime.starts_with("text/") || asset.mime == "application/octet-stream"
    }

    /// True when the asset's MIME is `application/json`.
    pub fn is_json(asset: &EvidenceAsset) -> bool {
        asset.mime == "application/json"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::EvidenceAsset;

    fn asset(mime: &str) -> EvidenceAsset {
        EvidenceAsset {
            path: "a".into(),
            mime: mime.into(),
            size: 1,
        }
    }

    #[test]
    fn asset_kind_classifies_by_mime_prefix() {
        assert!(asset_kind::is_image(&asset("image/png")));
        assert!(asset_kind::is_image(&asset("image/svg+xml")));
        assert!(!asset_kind::is_image(&asset("text/plain")));

        assert!(asset_kind::is_video(&asset("video/mp4")));
        assert!(!asset_kind::is_video(&asset("audio/mp3")));

        assert!(asset_kind::is_log_or_text(&asset("text/plain")));
        assert!(asset_kind::is_log_or_text(&asset("application/octet-stream")));
        assert!(!asset_kind::is_log_or_text(&asset("image/png")));

        assert!(asset_kind::is_json(&asset("application/json")));
        assert!(!asset_kind::is_json(&asset("text/json")));
    }
}
