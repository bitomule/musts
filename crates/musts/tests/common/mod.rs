//! Shared test helpers.
//!
//! Not every E2E test file uses every helper, so we silence the
//! "unused" warnings emitted for files that import `mod common;`
//! without exercising the whole API.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

/// Locate the on-disk path to a workspace binary, building it on
/// demand. Required because `cargo test --workspace` does not
/// guarantee that sibling-crate binaries are built before integration
/// tests run.
///
/// The path comes from cargo's own build report rather than from where
/// the test executable sits: newer cargo layouts put test binaries under
/// `target/<profile>/build/`, so a path derived from the executable no
/// longer points at the profile directory.
pub fn workspace_binary(package: &str, bin_name: &str) -> PathBuf {
    static BUILT: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let built = BUILT.get_or_init(|| Mutex::new(HashMap::new()));
    let key = format!("{package}/{bin_name}");
    let mut guard = built.lock().unwrap();
    if let Some(path) = guard.get(&key) {
        return path.clone();
    }
    let path = build(package, bin_name);
    guard.insert(key, path.clone());
    path
}

fn is_release_profile() -> bool {
    std::env::current_exe()
        .unwrap()
        .components()
        .any(|c| c.as_os_str() == "release")
}

fn build(package: &str, bin_name: &str) -> PathBuf {
    let mut cmd = std::process::Command::new(env!("CARGO"));
    cmd.args([
        "build",
        "-p",
        package,
        "--bin",
        bin_name,
        "--message-format=json-render-diagnostics",
    ]);
    if is_release_profile() {
        cmd.arg("--release");
    }
    let output = cmd
        .stderr(Stdio::inherit())
        .output()
        .unwrap_or_else(|e| panic!("could not invoke `cargo build`: {e}"));
    if !output.status.success() {
        panic!("`cargo build -p {package} --bin {bin_name}` failed");
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|msg| msg["reason"] == "compiler-artifact" && msg["target"]["name"] == bin_name)
        .find_map(|msg| msg["executable"].as_str().map(PathBuf::from))
        .unwrap_or_else(|| panic!("cargo did not report an executable for `{bin_name}`"))
}
