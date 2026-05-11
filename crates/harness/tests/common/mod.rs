//! Shared test helpers.
//!
//! Not every E2E test file uses every helper, so we silence the
//! "unused" warnings emitted for files that import `mod common;`
//! without exercising the whole API.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;

/// Locate the on-disk path to a workspace binary, building it on
/// demand. Required because `cargo test --workspace` does not
/// guarantee that sibling-crate binaries are built before integration
/// tests run.
pub fn workspace_binary(package: &str, bin_name: &str) -> PathBuf {
    let path = profile_dir().join(bin_name);
    ensure_built(package, bin_name);
    if !path.is_file() {
        panic!(
            "expected `{bin_name}` at {} after `cargo build -p {package}`",
            path.display()
        );
    }
    path
}

fn profile_dir() -> PathBuf {
    let test_bin = std::env::current_exe().unwrap();
    test_bin
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("test binary path")
}

fn ensure_built(package: &str, bin_name: &str) {
    static SEEN: OnceLock<std::sync::Mutex<Vec<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    let key = format!("{package}/{bin_name}");
    {
        let mut guard = seen.lock().unwrap();
        if guard.contains(&key) {
            return;
        }
        guard.push(key);
    }
    let path = profile_dir().join(bin_name);
    if path.is_file() {
        return;
    }
    let mut cmd = std::process::Command::new(env!("CARGO"));
    cmd.args(["build", "-p", package, "--bin", bin_name]);
    if profile_dir()
        .file_name()
        .map(|n| n == "release")
        .unwrap_or(false)
    {
        cmd.arg("--release");
    }
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("could not invoke `cargo build`: {e}"));
    if !status.success() {
        panic!("`cargo build -p {package} --bin {bin_name}` failed");
    }
}
