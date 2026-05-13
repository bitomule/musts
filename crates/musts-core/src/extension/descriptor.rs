//! Extension descriptors live at `.musts/extensions/<name>/extension.yml`.
//!
//! Per `docs/PLAN.md` §4.6, the `command` field accepts either:
//! - an argv array (preferred): `["bin/foo", "resolve", "build"]`
//! - a string parsed with `shell-words` (POSIX-ish) rules.
//!
//! Shell metacharacters (`|`, `;`, `&`, `<`, `>`, `$`, backticks) are
//! **rejected** in the string form so the contract is free of any
//! implicit shell layer. The argv form is the only way to embed those
//! characters intentionally.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::error::{Error, Result};

/// One parsed `.musts/extensions/<name>/extension.yml`.
#[derive(Debug, Clone)]
pub struct ExtensionDescriptor {
    /// Directory containing `extension.yml`. Relative paths inside the
    /// descriptor are resolved against this.
    pub root: PathBuf,
    pub name: String,
    pub version: String,
    /// Keyed by the local capability name (e.g. `build`, `expect`).
    pub capabilities: BTreeMap<String, Capability>,
    /// Raw bytes of `extension.yml` — used for the
    /// `ext_descriptor_hash` input to scope hashing (PLAN.md §4.5).
    pub descriptor_bytes: Vec<u8>,
}

/// One capability declared by an extension (`build`, `expect`, …).
#[derive(Debug, Clone)]
pub struct Capability {
    /// Fully qualified capability reference, e.g. `bazel/build`.
    pub uses: String,
    /// Loaded JSON Schema for `with` payloads. `None` if the descriptor
    /// did not declare a schema.
    pub schema: Option<JsonValue>,
    /// Filesystem path of the schema (absolute), for error messages.
    pub schema_path: Option<PathBuf>,
    pub resolve: Command,
    pub evidence: Command,
}

/// An argv-resolved command. Always at least one entry (the program).
#[derive(Debug, Clone)]
pub struct Command {
    pub argv: Vec<String>,
}

impl Command {
    /// Absolute path of the program — relative entries are resolved against
    /// the descriptor root.
    pub fn program_path(&self, descriptor_root: &Path) -> PathBuf {
        let first = Path::new(&self.argv[0]);
        if first.is_absolute() {
            first.to_path_buf()
        } else {
            descriptor_root.join(first)
        }
    }

    /// Arguments after the program.
    pub fn args(&self) -> &[String] {
        &self.argv[1..]
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Walk `<workspace_root>/.musts/extensions/` and return one
/// [`ExtensionDescriptor`] per `extension.yml` found. Missing
/// `.musts/extensions/` is not an error — it yields an empty Vec.
pub fn discover_descriptors(workspace_root: &Path) -> Result<Vec<ExtensionDescriptor>> {
    let ext_dir = workspace_root.join(".musts").join("extensions");
    if !ext_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let read = std::fs::read_dir(&ext_dir).map_err(|source| Error::Io {
        path: ext_dir.clone(),
        source,
    })?;
    for entry in read {
        let entry = entry.map_err(|source| Error::Io {
            path: ext_dir.clone(),
            source,
        })?;
        if !entry.path().is_dir() {
            continue;
        }
        let descriptor_path = entry.path().join("extension.yml");
        if descriptor_path.is_file() {
            out.push(load_descriptor(&descriptor_path)?);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Load and validate a single descriptor file.
pub fn load_descriptor(path: &Path) -> Result<ExtensionDescriptor> {
    let bytes = std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let raw: RawDescriptor =
        serde_yaml::from_slice(&bytes).map_err(|err| Error::ExtensionDescriptor {
            path: path.to_path_buf(),
            message: format!("invalid YAML: {err}"),
        })?;
    let root = path
        .parent()
        .ok_or_else(|| Error::ExtensionDescriptor {
            path: path.to_path_buf(),
            message: "descriptor path has no parent directory".into(),
        })?
        .to_path_buf();

    let mut capabilities = BTreeMap::new();
    for (local, raw_cap) in raw.capabilities {
        if local.is_empty() {
            return Err(Error::ExtensionDescriptor {
                path: path.to_path_buf(),
                message: "capability key must not be empty".into(),
            });
        }
        let resolve = parse_command(path, &local, "resolve", raw_cap.resolve.command)?;
        let evidence = parse_command(path, &local, "evidence", raw_cap.evidence.command)?;
        let (schema, schema_path) = match raw_cap.schema {
            Some(rel) => {
                let schema_path = root.join(&rel);
                let schema_bytes = std::fs::read(&schema_path).map_err(|source| Error::Io {
                    path: schema_path.clone(),
                    source,
                })?;
                let schema: JsonValue = serde_json::from_slice(&schema_bytes).map_err(|err| {
                    Error::ExtensionDescriptor {
                        path: path.to_path_buf(),
                        message: format!(
                            "schema at {} is not valid JSON: {err}",
                            schema_path.display()
                        ),
                    }
                })?;
                (Some(schema), Some(schema_path))
            }
            None => (None, None),
        };
        capabilities.insert(
            local,
            Capability {
                uses: raw_cap.uses,
                schema,
                schema_path,
                resolve,
                evidence,
            },
        );
    }

    Ok(ExtensionDescriptor {
        root,
        name: raw.name,
        version: raw.version,
        capabilities,
        descriptor_bytes: bytes,
    })
}

fn parse_command(
    descriptor_path: &Path,
    capability: &str,
    kind: &str,
    raw: RawCommand,
) -> Result<Command> {
    let argv = match raw {
        RawCommand::Argv(items) => {
            if items.is_empty() {
                return Err(Error::ExtensionDescriptor {
                    path: descriptor_path.to_path_buf(),
                    message: format!("capability `{capability}` {kind}.command argv is empty"),
                });
            }
            items
        }
        RawCommand::String(s) => {
            reject_shell_metacharacters(descriptor_path, capability, kind, &s)?;
            let parsed = shell_words::split(&s).map_err(|err| Error::ExtensionDescriptor {
                path: descriptor_path.to_path_buf(),
                message: format!(
                    "capability `{capability}` {kind}.command failed shell-words parse: {err}"
                ),
            })?;
            if parsed.is_empty() {
                return Err(Error::ExtensionDescriptor {
                    path: descriptor_path.to_path_buf(),
                    message: format!(
                        "capability `{capability}` {kind}.command parsed to an empty argv"
                    ),
                });
            }
            parsed
        }
    };
    Ok(Command { argv })
}

fn reject_shell_metacharacters(
    descriptor_path: &Path,
    capability: &str,
    kind: &str,
    s: &str,
) -> Result<()> {
    const FORBIDDEN: &[char] = &['|', ';', '&', '<', '>', '$', '`'];
    for ch in s.chars() {
        if FORBIDDEN.contains(&ch) {
            return Err(Error::ExtensionDescriptor {
                path: descriptor_path.to_path_buf(),
                message: format!(
                    "capability `{capability}` {kind}.command contains forbidden shell metacharacter `{ch}` — use the argv array form for intentional special characters"
                ),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Wire-format types (private)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawDescriptor {
    name: String,
    version: String,
    #[serde(default)]
    capabilities: BTreeMap<String, RawCapability>,
}

#[derive(Debug, Deserialize)]
struct RawCapability {
    uses: String,
    #[serde(default)]
    schema: Option<String>,
    resolve: RawCapEndpoint,
    evidence: RawCapEndpoint,
}

#[derive(Debug, Deserialize)]
struct RawCapEndpoint {
    command: RawCommand,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawCommand {
    Argv(Vec<String>),
    String(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_descriptor(dir: &Path, body: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join("extension.yml");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn loads_argv_form_descriptor() {
        let dir = TempDir::new().unwrap();
        let path = write_descriptor(
            &dir.path().join(".musts/extensions/bazel"),
            r#"
name: bazel
version: 0.1.0
capabilities:
  build:
    uses: bazel/build
    resolve:
      command: ["bin/bazel-extension", "resolve", "build"]
    evidence:
      command: ["bin/bazel-extension", "evidence", "build"]
"#,
        );
        let descriptor = load_descriptor(&path).unwrap();
        assert_eq!(descriptor.name, "bazel");
        assert_eq!(descriptor.capabilities["build"].uses, "bazel/build");
        assert_eq!(
            descriptor.capabilities["build"].resolve.argv,
            vec!["bin/bazel-extension", "resolve", "build"]
        );
    }

    #[test]
    fn loads_string_form_descriptor() {
        let dir = TempDir::new().unwrap();
        let path = write_descriptor(
            &dir.path().join(".musts/extensions/bazel"),
            r#"
name: bazel
version: 0.1.0
capabilities:
  build:
    uses: bazel/build
    resolve:
      command: "bin/bazel-extension resolve build"
    evidence:
      command: "bin/bazel-extension evidence build"
"#,
        );
        let descriptor = load_descriptor(&path).unwrap();
        assert_eq!(
            descriptor.capabilities["build"].resolve.argv,
            vec!["bin/bazel-extension", "resolve", "build"]
        );
    }

    #[test]
    fn rejects_shell_metacharacters_in_string_form() {
        let dir = TempDir::new().unwrap();
        let path = write_descriptor(
            &dir.path().join(".musts/extensions/bad"),
            r#"
name: bad
version: 0.1.0
capabilities:
  shell:
    uses: bad/shell
    resolve:
      command: "bash -c 'echo a | tee out'"
    evidence:
      command: "true"
"#,
        );
        let err = load_descriptor(&path).unwrap_err();
        let message = format!("{err}");
        assert!(
            message.contains("forbidden shell metacharacter"),
            "{message}"
        );
    }

    #[test]
    fn argv_form_allows_special_chars() {
        // The argv array escapes the shell entirely, so `|` is fine there.
        let dir = TempDir::new().unwrap();
        let path = write_descriptor(
            &dir.path().join(".musts/extensions/argv"),
            r#"
name: argv
version: 0.1.0
capabilities:
  foo:
    uses: argv/foo
    resolve:
      command: ["bash", "-c", "echo a | tee out"]
    evidence:
      command: ["true"]
"#,
        );
        let descriptor = load_descriptor(&path).unwrap();
        assert_eq!(
            descriptor.capabilities["foo"].resolve.argv,
            vec!["bash", "-c", "echo a | tee out"]
        );
    }

    #[test]
    fn rejects_empty_argv() {
        let dir = TempDir::new().unwrap();
        let path = write_descriptor(
            &dir.path().join(".musts/extensions/empty"),
            r#"
name: empty
version: 0.1.0
capabilities:
  c:
    uses: empty/c
    resolve:
      command: []
    evidence:
      command: ["true"]
"#,
        );
        let err = load_descriptor(&path).unwrap_err();
        assert!(format!("{err}").contains("argv is empty"));
    }

    #[test]
    fn loads_schema_file_when_present() {
        let dir = TempDir::new().unwrap();
        let cap_dir = dir.path().join(".musts/extensions/bazel");
        fs::create_dir_all(cap_dir.join("schemas")).unwrap();
        fs::write(
            cap_dir.join("schemas/build.schema.json"),
            r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","required":["target"],"properties":{"target":{"type":"string"}},"additionalProperties":false}"#,
        )
        .unwrap();
        let path = write_descriptor(
            &cap_dir,
            r#"
name: bazel
version: 0.1.0
capabilities:
  build:
    uses: bazel/build
    schema: schemas/build.schema.json
    resolve:
      command: ["bin/bazel-extension", "resolve", "build"]
    evidence:
      command: ["bin/bazel-extension", "evidence", "build"]
"#,
        );
        let descriptor = load_descriptor(&path).unwrap();
        let schema = descriptor.capabilities["build"].schema.as_ref().unwrap();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn discover_returns_empty_when_extensions_dir_missing() {
        let dir = TempDir::new().unwrap();
        let entries = discover_descriptors(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn discover_walks_extensions_dir_and_sorts() {
        let dir = TempDir::new().unwrap();
        for name in ["bazel", "mav"] {
            let cap_dir = dir.path().join(".musts/extensions").join(name);
            fs::create_dir_all(&cap_dir).unwrap();
            fs::write(
                cap_dir.join("extension.yml"),
                format!(
                    r#"
name: {name}
version: 0.1.0
capabilities:
  noop:
    uses: {name}/noop
    resolve:
      command: ["true"]
    evidence:
      command: ["true"]
"#
                ),
            )
            .unwrap();
        }
        let entries = discover_descriptors(dir.path()).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["bazel".to_string(), "mav".to_string()]);
    }

    #[test]
    fn program_path_resolves_relative_against_descriptor_root() {
        let cmd = Command {
            argv: vec!["bin/foo".into(), "x".into()],
        };
        let descriptor_root = Path::new("/repo/.musts/extensions/bazel");
        let resolved = cmd.program_path(descriptor_root);
        assert_eq!(
            resolved,
            PathBuf::from("/repo/.musts/extensions/bazel/bin/foo")
        );

        let abs_cmd = Command {
            argv: vec!["/usr/bin/true".into()],
        };
        assert_eq!(
            abs_cmd.program_path(descriptor_root),
            PathBuf::from("/usr/bin/true")
        );
    }
}
