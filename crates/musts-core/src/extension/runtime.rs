//! Spawn an extension binary and exchange one JSON request/response over
//! stdin/stdout per `docs/PLAN.md` §4.6.
//!
//! Hard rules implemented here:
//! - Core writes the request to stdin and **immediately closes the
//!   handle** before reading stdout. Without this, extensions that parse
//!   with `serde_json::from_reader(stdin())` deadlock waiting for EOF.
//! - stdout must contain **exactly one** JSON document. Garbage before
//!   or after, multiple concatenated documents, and non-JSON output are
//!   all protocol errors.
//! - Max response size is 4 MiB. Larger responses are rejected.
//! - Timeout: 30s default, configurable via `MUSTS_EXTENSION_TIMEOUT_SECS`.
//! - `protocol_version` in the response must equal 1.
//! - On non-zero exit, stderr is surfaced verbatim.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use musts_protocol::{
    EvidenceValidationRequest, EvidenceValidationResponse, ResolveRequest, ResolveResponse,
    PROTOCOL_VERSION,
};
use serde::{de::DeserializeOwned, Serialize};

use crate::error::{Error, Result};

use super::descriptor::Command;

/// Hard cap on extension response size (4 MiB).
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Default IPC timeout in seconds.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Environment variable that overrides [`DEFAULT_TIMEOUT_SECS`].
pub const TIMEOUT_ENV: &str = "MUSTS_EXTENSION_TIMEOUT_SECS";

/// Tunables for the extension runtime. Defaults come from env vars and
/// the constants above.
#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    /// Wall-clock cap on the child process.
    pub timeout: Duration,
    /// Hard cap on response size. Always [`MAX_RESPONSE_BYTES`] in
    /// production; tests may shrink this.
    pub max_response_bytes: usize,
    /// `workspace_root` the child is invoked from. Must be absolute.
    pub workspace_root: PathBuf,
}

impl RuntimeOptions {
    /// Read defaults from env / constants.
    pub fn from_env(workspace_root: PathBuf) -> Self {
        let timeout_secs = std::env::var(TIMEOUT_ENV)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        Self {
            timeout: Duration::from_secs(timeout_secs),
            max_response_bytes: MAX_RESPONSE_BYTES,
            workspace_root,
        }
    }
}

/// One configured runner. Cheap to construct; carries the descriptor
/// root so relative `bin/…` paths resolve correctly.
pub struct ExtensionRunner<'a> {
    /// Capability id for error messages (e.g. `bazel/build`).
    pub capability: String,
    /// Directory containing the extension's `extension.yml`.
    pub descriptor_root: &'a Path,
    pub options: RuntimeOptions,
}

impl<'a> ExtensionRunner<'a> {
    /// Send a resolve request and parse the response.
    pub fn resolve(&self, command: &Command, request: &ResolveRequest) -> Result<ResolveResponse> {
        self.exchange(command, request)
    }

    /// Send an evidence-validation request and parse the response.
    pub fn evidence(
        &self,
        command: &Command,
        request: &EvidenceValidationRequest,
    ) -> Result<EvidenceValidationResponse> {
        self.exchange(command, request)
    }

    fn exchange<Req, Resp>(&self, command: &Command, request: &Req) -> Result<Resp>
    where
        Req: Serialize,
        Resp: DeserializeOwned + HasProtocolVersion,
    {
        let request_bytes = serde_json::to_vec(request).map_err(|err| Error::ExtensionFailure {
            capability: self.capability.clone(),
            message: format!("could not serialise request: {err}"),
        })?;

        let program = command.program_path(self.descriptor_root);
        let mut child = StdCommand::new(&program)
            .args(command.args())
            .current_dir(&self.options.workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| Error::ExtensionFailure {
                capability: self.capability.clone(),
                message: format!("could not spawn {}: {err}", program.display()),
            })?;

        // Write the request and CLOSE stdin before reading stdout.
        {
            let mut stdin = child.stdin.take().expect("piped");
            stdin
                .write_all(&request_bytes)
                .map_err(|err| Error::ExtensionFailure {
                    capability: self.capability.clone(),
                    message: format!("could not write request: {err}"),
                })?;
            // `stdin` drops here, closing the pipe so the child sees EOF.
        }

        let mut stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let max = self.options.max_response_bytes;

        // Stream stdout and stderr from threads so the main thread can
        // enforce the timeout without blocking on a read syscall.
        let (stdout_tx, stdout_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = Vec::with_capacity(8 * 1024);
            // `take(max + 1)` lets us detect oversized responses.
            let res = (&mut stdout).take((max as u64) + 1).read_to_end(&mut buf);
            let _ = stdout_tx.send((buf, res));
        });
        let (stderr_tx, stderr_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            let _ = stderr_tx.send(buf);
        });

        // Poll for completion with timeout.
        let deadline = Instant::now() + self.options.timeout;
        let timed_out;
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    timed_out = false;
                    break Some(status);
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        break None;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => {
                    return Err(Error::ExtensionFailure {
                        capability: self.capability.clone(),
                        message: format!("waiting on child failed: {err}"),
                    });
                }
            }
        };

        // Drain output channels once — every failure branch below folds
        // `stderr` into the surfaced error so PLAN.md §4.6 ("stderr
        // captured and surfaced verbatim on non-zero exit or on
        // protocol error") and §7.3 scenario 9 are honoured uniformly.
        let (stdout_buf, stdout_res) = stdout_rx.recv().unwrap_or_else(|_| (Vec::new(), Ok(0)));
        let stderr_buf = stderr_rx.recv().unwrap_or_default();
        let stderr_text = String::from_utf8_lossy(&stderr_buf).trim().to_string();
        let stderr_suffix = if stderr_text.is_empty() {
            String::new()
        } else {
            format!(" — stderr: {stderr_text}")
        };

        if timed_out {
            return Err(Error::ExtensionTimeout {
                capability: self.capability.clone(),
                timeout_seconds: self.options.timeout.as_secs(),
                stderr: stderr_text.clone(),
            });
        }
        let exit_status = exit_status.expect("non-timeout path sets a status");

        if let Err(err) = stdout_res {
            return Err(Error::ExtensionFailure {
                capability: self.capability.clone(),
                message: format!("reading stdout failed: {err}{stderr_suffix}"),
            });
        }

        // Check the size cap **before** the exit status: an oversized
        // response will eventually SIGPIPE the child (because we stop
        // reading at max+1 bytes), making the exit status non-zero. We
        // want the user to see the actually-actionable "exceeds cap"
        // message rather than the broken-pipe symptom.
        if stdout_buf.len() > max {
            return Err(Error::ExtensionFailure {
                capability: self.capability.clone(),
                message: format!(
                    "response exceeds {max}-byte cap (got at least {} bytes){stderr_suffix}",
                    stdout_buf.len()
                ),
            });
        }

        if !exit_status.success() {
            return Err(Error::ExtensionFailure {
                capability: self.capability.clone(),
                message: format!("exited with status {exit_status}{stderr_suffix}"),
            });
        }

        let response = parse_single_json::<Resp>(&self.capability, &stdout_buf)
            .map_err(|err| with_stderr_suffix(err, &stderr_suffix))?;
        if response.protocol_version() != PROTOCOL_VERSION {
            return Err(Error::ExtensionFailure {
                capability: self.capability.clone(),
                message: format!(
                    "response declares protocol_version {} but core requires {}{stderr_suffix}",
                    response.protocol_version(),
                    PROTOCOL_VERSION
                ),
            });
        }
        Ok(response)
    }
}

/// Convenience wrappers — most call sites have a runner already configured
/// but the free-function form keeps tests terse.
pub fn run_resolve(
    runner: &ExtensionRunner<'_>,
    command: &Command,
    request: &ResolveRequest,
) -> Result<ResolveResponse> {
    runner.resolve(command, request)
}

pub fn run_evidence(
    runner: &ExtensionRunner<'_>,
    command: &Command,
    request: &EvidenceValidationRequest,
) -> Result<EvidenceValidationResponse> {
    runner.evidence(command, request)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Fold a captured stderr suffix into an [`Error::ExtensionFailure`] so
/// every IPC failure path surfaces the child's stderr verbatim per
/// `docs/PLAN.md` §4.6.
fn with_stderr_suffix(err: Error, suffix: &str) -> Error {
    if suffix.is_empty() {
        return err;
    }
    match err {
        Error::ExtensionFailure {
            capability,
            message,
        } => Error::ExtensionFailure {
            capability,
            message: format!("{message}{suffix}"),
        },
        other => other,
    }
}

trait HasProtocolVersion {
    fn protocol_version(&self) -> u32;
}
impl HasProtocolVersion for ResolveResponse {
    fn protocol_version(&self) -> u32 {
        self.protocol_version
    }
}
impl HasProtocolVersion for EvidenceValidationResponse {
    fn protocol_version(&self) -> u32 {
        self.protocol_version
    }
}

fn parse_single_json<T: DeserializeOwned>(capability: &str, bytes: &[u8]) -> Result<T> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value: T = T::deserialize(&mut deserializer).map_err(|err| Error::ExtensionFailure {
        capability: capability.into(),
        message: format!("response is not valid JSON: {err}"),
    })?;
    // Confirm there is no trailing JSON content beyond optional whitespace.
    if let Err(err) = deserializer.end() {
        return Err(Error::ExtensionFailure {
            capability: capability.into(),
            message: format!(
                "response contains data after the JSON document (extensions must write exactly one object): {err}"
            ),
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    //! Unit-level tests for the parser; the IPC end-to-end paths are
    //! exercised by integration tests against the stub binary.
    use super::*;

    #[test]
    fn parse_single_json_accepts_one_object() {
        let bytes = br#"{"protocol_version":1,"accepted":true}"#;
        let value: serde_json::Value =
            parse_single_json("test/cap", bytes).expect("valid single JSON");
        assert_eq!(value["protocol_version"], 1);
    }

    #[test]
    fn parse_single_json_rejects_garbage_after_object() {
        // Two concatenated JSON documents must be rejected.
        let bytes = br#"{"protocol_version":1}garbage"#;
        let err =
            parse_single_json::<serde_json::Value>("test/cap", bytes).expect_err("must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("data after the JSON document"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_single_json_rejects_two_concatenated_objects() {
        let bytes = br#"{"protocol_version":1}{"protocol_version":1}"#;
        let err =
            parse_single_json::<serde_json::Value>("test/cap", bytes).expect_err("must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("data after the JSON document"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_single_json_rejects_non_json() {
        let bytes = b"hello world";
        let err =
            parse_single_json::<serde_json::Value>("test/cap", bytes).expect_err("must reject");
        let msg = format!("{err}");
        assert!(msg.contains("not valid JSON"), "unexpected error: {msg}");
    }

    #[test]
    fn from_env_default_and_override() {
        // `cargo test` runs unit tests in parallel and `std::env` is
        // process-global, so we can't split these into two tests. Drive
        // the read-default + read-override paths back-to-back instead.
        std::env::remove_var(TIMEOUT_ENV);
        let default_opts = RuntimeOptions::from_env(PathBuf::from("/tmp"));
        assert_eq!(
            default_opts.timeout,
            Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        );

        std::env::set_var(TIMEOUT_ENV, "5");
        let override_opts = RuntimeOptions::from_env(PathBuf::from("/tmp"));
        assert_eq!(override_opts.timeout, Duration::from_secs(5));
        std::env::remove_var(TIMEOUT_ENV);
    }
}
