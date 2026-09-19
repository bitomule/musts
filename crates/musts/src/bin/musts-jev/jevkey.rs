//! Where the jev API key comes from, and in what order.
//!
//! The order is the one current CLI guidance converges on — most secure source available
//! wins — with one deliberate departure at the end.
//!
//! 1. `MUSTS_JEV_API_KEY` in the environment. For CI, where the platform owns the secret.
//!    Not recommended interactively: an environment variable shows up in process listings,
//!    gets picked up by crash reporters, and is inherited by every child process.
//! 2. The system keychain, on macOS. Measured on a real machine: writing and reading through
//!    `/usr/bin/security` from a non-interactive process returns the value with NO
//!    authorisation prompt, and it keeps working from a different parent binary. The reason
//!    matters — the keychain ACL is per *accessing binary*, and when `security` is the one
//!    accessing, the binary in the ACL is `/usr/bin/security`, which never changes. So
//!    rebuilding or reinstalling musts does not start prompting. (An item created long ago
//!    by some other application would still prompt; one written here by `security` does not.)
//! 3. `~/.config/bitomule/musts/config.json`, mode 0600.
//! 4. Nothing. The check fails red and says so; it does not quietly pass.
//!
//! The departure from the guidance is step 3: the advice is that a plaintext file is the
//! thing to avoid. It stays because on Linux there is no universal keychain — `secret-tool`
//! needs a session D-Bus and a running keyring daemon, neither of which exists on a headless
//! server or inside a container, and that is where a validation CLI actually runs. A tool
//! that can only store a secret in a keychain cannot be used in half the places musts runs.
//!
//! And deliberately absent: jevi's own key is never read. Inheriting another tool's
//! credential silently is how "it works on my machine and I do not know why" starts.

use std::path::PathBuf;

/// One name, in one place, so pointing several tools at a shared secret is a one-line change
/// if that is ever decided.
pub const ENV_VAR: &str = "MUSTS_JEV_API_KEY";
pub const KEYCHAIN_SERVICE: &str = "musts-jev";

/// Where a key came from — printed, because otherwise the only way to know is to remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Env,
    Keychain,
    File,
}

impl Source {
    pub fn describe(self) -> String {
        match self {
            Source::Env => format!("the {ENV_VAR} environment variable"),
            Source::Keychain => format!("the system keychain (service `{KEYCHAIN_SERVICE}`)"),
            Source::File => format!("{}", config_path().display()),
        }
    }
}

pub fn config_path() -> PathBuf {
    // Inside the namespace that already exists rather than a new top-level directory: the
    // objection this answers was "too many configs scattered around", and one more beside
    // the others is not the same as one more place to look.
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("bitomule").join("musts").join("config.json")
}

fn from_keychain() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

fn from_file() -> Option<String> {
    let raw = std::fs::read_to_string(config_path()).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let v = doc.get("jev_api_key")?.as_str()?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// The key and where it came from, or nothing.
pub fn resolve() -> Option<(String, Source)> {
    if let Some(v) = std::env::var(ENV_VAR).ok().filter(|v| !v.trim().is_empty()) {
        return Some((v, Source::Env));
    }
    if let Some(v) = from_keychain() {
        return Some((v, Source::Keychain));
    }
    from_file().map(|v| (v, Source::File))
}

/// What to tell someone who has no key. Every line is something they can act on.
pub fn missing_key_message() -> String {
    format!(
        "no jev API key. `uses: jev` cannot judge anything without one, and a check that \
         cannot work must not look like a check that worked.\n  \
         set one with:   musts-jev set-key < key.txt\n  \
         or in CI:       export {ENV_VAR}=...\n  \
         musts looks in, in order: {ENV_VAR}, the system keychain (service \
         `{KEYCHAIN_SERVICE}`), then {}.",
        config_path().display()
    )
}

/// Store a key, and say where it went. Reads stdin rather than taking an argument so the
/// secret never reaches the shell history.
pub fn set_key() -> Result<(), String> {
    use std::io::Read;
    let mut key = String::new();
    std::io::stdin()
        .read_to_string(&mut key)
        .map_err(|e| format!("cannot read the key from stdin: {e}"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err("no key on stdin. Usage: musts-jev set-key < key.txt".into());
    }

    // Keychain first where there is one; it is the only store here that is not a file
    // anything running as this user can read.
    if cfg!(target_os = "macos") {
        let ok = std::process::Command::new("/usr/bin/security")
            .args([
                "add-generic-password",
                "-a",
                "musts",
                "-s",
                KEYCHAIN_SERVICE,
                "-w",
                key,
                "-U",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("key stored in {}", Source::Keychain.describe());
            return Ok(());
        }
        eprintln!("musts-jev: keychain unavailable, falling back to a file");
    }

    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let body = serde_json::json!({ "jev_api_key": key }).to_string();
    std::fs::write(&path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot set 0600 on {}: {e}", path.display()))?;
    }
    println!("key stored in {} (mode 0600)", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The precedence has to be visible in a test, not only in a README: the one thing
    /// nobody can check by reading is which source wins when two are set.
    #[test]
    fn the_environment_variable_wins_over_everything_else() {
        // Not a live keychain read — that would make the test depend on the machine. What is
        // asserted is the branch order: with the variable set, `resolve` returns it and its
        // source, whatever else exists underneath.
        temp_env_var(ENV_VAR, Some("from-env"), || {
            let (key, source) = resolve().expect("a key");
            assert_eq!(key, "from-env");
            assert_eq!(source, Source::Env);
        });
    }

    #[test]
    fn an_empty_variable_is_not_a_key() {
        // An exported-but-empty variable is the classic CI mistake; treating it as a key
        // would send an empty credential and fail confusingly far from the cause.
        temp_env_var(ENV_VAR, Some("   "), || {
            let from_env = matches!(resolve(), Some((_, Source::Env)));
            assert!(!from_env, "whitespace is not a credential");
        });
    }

    #[test]
    fn every_source_says_where_it_came_from() {
        // Printed on every run, because otherwise the only way to know which key was used is
        // to remember what you configured.
        assert!(Source::Env.describe().contains(ENV_VAR));
        assert!(Source::Keychain.describe().contains(KEYCHAIN_SERVICE));
        assert!(Source::File.describe().contains("config.json"));
    }

    #[test]
    fn the_file_lives_beside_the_other_tools_not_in_a_new_place() {
        let p = config_path();
        let s = p.to_string_lossy();
        assert!(
            s.contains("bitomule"),
            "expected the existing namespace: {s}"
        );
        assert!(s.ends_with("musts/config.json"), "{s}");
    }

    #[test]
    fn the_message_for_a_missing_key_says_what_to_do() {
        let m = missing_key_message();
        assert!(m.contains("musts-jev set-key"));
        assert!(m.contains(ENV_VAR));
        // And it says plainly why it is not simply skipping.
        assert!(m.contains("must not look like a check that worked"));
    }

    fn temp_env_var(name: &str, value: Option<&str>, f: impl FnOnce()) {
        let old = std::env::var_os(name);
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
        f();
        match old {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }
}
