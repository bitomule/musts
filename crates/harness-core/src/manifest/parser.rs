//! `HARNESS.yml` parser per `docs/PLAN.md` §4.3 (`manifest::parser`).
//!
//! Behaviour:
//! - Requires `version: 1`. Any other version is rejected.
//! - Requires `checks` to be a map of `<local_id> → { uses, with }`.
//! - `with` is captured opaquely as `serde_json::Value`; schema validation
//!   against the extension's JSON Schema happens later, in
//!   `manifest::with_validation` (Phase 2).
//! - Duplicate local IDs **inside the same manifest** are rejected with a
//!   clear error pointing at the offending id.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

const SUPPORTED_VERSION: u32 = 1;

/// A parsed manifest file.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// Absolute or workspace-relative path the manifest was loaded from.
    pub path: PathBuf,
    /// Manifest version (currently always [`SUPPORTED_VERSION`]).
    pub version: u32,
    /// Checks keyed by local id. `BTreeMap` keeps iteration deterministic.
    pub checks: BTreeMap<String, Check>,
}

/// A single declared check.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    /// As declared in the manifest, e.g. `login-build`.
    pub local_id: String,
    /// Fully-qualified capability reference, e.g. `bazel/build`.
    pub uses: String,
    /// Extension-owned `with` payload, captured opaquely.
    pub with_payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ManifestFile {
    version: u32,
    /// We deserialise into a `Vec<(String, RawCheck)>` via a wrapper because
    /// `serde_yaml` happily accepts duplicate keys in a YAML mapping by
    /// silently overwriting earlier ones. We need to *detect* duplicates
    /// rather than silently lose them.
    #[serde(default)]
    checks: serde_yaml::Mapping,
}

#[derive(Debug, Deserialize)]
struct RawCheck {
    uses: String,
    #[serde(default)]
    with: serde_yaml::Value,
}

/// Parse a `HARNESS.yml` file from bytes. `path` is used for error messages
/// (and as the `path` field of the resulting [`Manifest`]).
pub fn parse(path: &Path, bytes: &[u8]) -> Result<Manifest> {
    let file: ManifestFile =
        serde_yaml::from_slice(bytes).map_err(|source| Error::ManifestYaml {
            path: path.to_path_buf(),
            source,
        })?;

    if file.version != SUPPORTED_VERSION {
        return Err(Error::Manifest {
            path: path.to_path_buf(),
            message: format!(
                "unsupported version {} (only version {} is supported)",
                file.version, SUPPORTED_VERSION
            ),
        });
    }

    let mut checks: BTreeMap<String, Check> = BTreeMap::new();
    for (raw_key, raw_value) in &file.checks {
        let local_id = match raw_key.as_str() {
            Some(s) => s.to_string(),
            None => {
                return Err(Error::Manifest {
                    path: path.to_path_buf(),
                    message: "check keys must be strings".into(),
                });
            }
        };

        let raw_check: RawCheck =
            serde_yaml::from_value(raw_value.clone()).map_err(|source| Error::ManifestYaml {
                path: path.to_path_buf(),
                source,
            })?;

        // We deliberately do **not** convert from serde_yaml::Value to
        // serde_json::Value via Display — `serde_yaml::to_value` then
        // `serde_json::to_value` round-trips structure cleanly.
        let with_payload = yaml_to_json(&raw_check.with).map_err(|message| Error::Manifest {
            path: path.to_path_buf(),
            message,
        })?;

        if checks
            .insert(
                local_id.clone(),
                Check {
                    local_id: local_id.clone(),
                    uses: raw_check.uses,
                    with_payload,
                },
            )
            .is_some()
        {
            return Err(Error::Manifest {
                path: path.to_path_buf(),
                message: format!("duplicate check id `{local_id}` in the same manifest"),
            });
        }
    }

    Ok(Manifest {
        path: path.to_path_buf(),
        version: file.version,
        checks,
    })
}

/// Convert a `serde_yaml::Value` into a `serde_json::Value`. Returns
/// `Err(message)` if the YAML uses features JSON cannot represent (e.g.
/// non-string map keys, which the manifest schema does not allow).
fn yaml_to_json(value: &serde_yaml::Value) -> std::result::Result<serde_json::Value, String> {
    use serde_yaml::Value as Y;
    match value {
        Y::Null => Ok(serde_json::Value::Null),
        Y::Bool(b) => Ok(serde_json::Value::Bool(*b)),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(serde_json::Value::from(i))
            } else if let Some(u) = n.as_u64() {
                Ok(serde_json::Value::from(u))
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .ok_or_else(|| "non-finite number is not representable in JSON".to_string())
            } else {
                Err("unsupported numeric type".into())
            }
        }
        Y::String(s) => Ok(serde_json::Value::String(s.clone())),
        Y::Sequence(items) => Ok(serde_json::Value::Array(
            items
                .iter()
                .map(yaml_to_json)
                .collect::<std::result::Result<_, _>>()?,
        )),
        Y::Mapping(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let key = k.as_str().ok_or_else(|| {
                    "YAML mappings must have string keys for `with` payloads".to_string()
                })?;
                out.insert(key.to_string(), yaml_to_json(v)?);
            }
            Ok(serde_json::Value::Object(out))
        }
        Y::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> PathBuf {
        PathBuf::from("/repo/HARNESS.yml")
    }

    #[test]
    fn parses_minimal_manifest() {
        let yaml = b"version: 1\nchecks: {}\n";
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(manifest.version, 1);
        assert!(manifest.checks.is_empty());
    }

    #[test]
    fn parses_two_checks_with_payload() {
        let yaml = br#"
version: 1
checks:
  app-build:
    uses: bazel/build
    with:
      target: //App:App
  app-test:
    uses: bazel/test
    with:
      targets:
        - //App:Tests
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(manifest.checks.len(), 2);
        let build = &manifest.checks["app-build"];
        assert_eq!(build.uses, "bazel/build");
        assert_eq!(
            build.with_payload,
            serde_json::json!({ "target": "//App:App" })
        );
        let test = &manifest.checks["app-test"];
        assert_eq!(
            test.with_payload,
            serde_json::json!({ "targets": ["//App:Tests"] })
        );
    }

    #[test]
    fn rejects_unsupported_version() {
        let yaml = b"version: 2\nchecks: {}\n";
        let err = parse(&p(), yaml).unwrap_err();
        assert!(matches!(err, Error::Manifest { .. }));
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn rejects_missing_version() {
        let yaml = b"checks: {}\n";
        let err = parse(&p(), yaml).unwrap_err();
        assert!(matches!(err, Error::ManifestYaml { .. }));
    }

    #[test]
    fn rejects_duplicate_check_keys() {
        // serde_yaml's Mapping preserves entries but BTreeMap will see the
        // duplicate when we insert. (Note: serde_yaml folds equal keys; we
        // craft this differently via two different strings that happen to
        // serialise the same. Easier: rely on the BTreeMap insertion to
        // catch the YAML mapping returning two equivalent keys.)
        let yaml = br#"
version: 1
checks:
  dup:
    uses: bazel/build
    with: { target: a }
  dup:
    uses: bazel/build
    with: { target: b }
"#;
        // serde_yaml 0.9 emits a duplicate-key error itself for identical scalar keys.
        let err = parse(&p(), yaml).unwrap_err();
        assert!(matches!(err, Error::ManifestYaml { .. }));
    }

    #[test]
    fn missing_uses_field_is_rejected() {
        let yaml = br#"
version: 1
checks:
  bad:
    with: { target: a }
"#;
        let err = parse(&p(), yaml).unwrap_err();
        assert!(matches!(err, Error::ManifestYaml { .. }));
    }

    #[test]
    fn with_can_be_absent() {
        // The core deliberately accepts absent `with` and passes Null to
        // the extension; schema validation lives in Phase 2.
        let yaml = br#"
version: 1
checks:
  free:
    uses: custom/noop
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(
            manifest.checks["free"].with_payload,
            serde_json::Value::Null
        );
    }
}
