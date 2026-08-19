//! `MUSTS.yml` parser per `docs/PLAN.md` §4.3 (`manifest::parser`).
//!
//! Behaviour:
//! - Requires `version: 1`. Any other version is rejected.
//! - Requires `checks` to be a map of `<local_id> → { uses, with, paths }`.
//! - `with` is captured opaquely as `serde_json::Value`; schema validation
//!   against the extension's JSON Schema happens later, in
//!   `manifest::with_validation` (Phase 2).
//! - `paths` is an optional list of gitignore-style glob patterns that
//!   narrow the check's effective scope to files matching at least one
//!   pattern. Patterns are validated at parse time so a malformed glob
//!   becomes a manifest error.
//! - `exclude_paths` is an optional list of the same shape that carves
//!   files back **out** of the effective scope (applied after `paths`).
//!   A file in scope for `exclude_paths` never contributes to the check's
//!   scope hash, so edits to it don't re-open the check.
//! - Leading-`!` patterns (gitignore negation) are **rejected** in both
//!   fields: `globset::Glob` treats `!` as a literal, so an author writing
//!   `!foo` used to get a pattern that silently matched nothing. Use
//!   `exclude_paths` for exclusions instead.
//! - Duplicate local IDs **inside the same manifest** are rejected with a
//!   clear error pointing at the offending id.
//! - Unrecognised keys are **warned about, not rejected** — collected into
//!   `Manifest::warnings` and surfaced by `validate`. See
//!   [`ManifestWarning`] for why silence was the wrong default and why
//!   this is not yet a hard error.

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
    /// Keys musts did not recognise, in file order. Non-fatal, but
    /// surfaced by `validate` — see [`ManifestWarning`].
    pub warnings: Vec<ManifestWarning>,
}

/// A key musts parsed past rather than acted on.
///
/// Silently ignoring an unknown key is the worst possible behaviour for a
/// *filter*. `excludes:` where the real key is `exclude_paths:` reads as a
/// working exclusion and does nothing, so the check fires on everything it
/// was meant to skip — forever, with no signal. Found in the wild: one
/// repo had been paying for that typo since the file was written.
///
/// A hard parse error is the honest answer and is where this ends up. It
/// is a warning for one release so that manifests carrying a stray key do
/// not all go red on upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestWarning {
    /// `None` for a key at the top level of the file.
    pub check_local_id: Option<String>,
    /// The key as written.
    pub key: String,
    /// Closest valid key, when one is close enough to be worth naming.
    pub suggestion: Option<&'static str>,
}

impl std::fmt::Display for ManifestWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match &self.check_local_id {
            Some(id) => format!("check `{id}`: unknown key `{}`", self.key),
            None => format!("unknown top-level key `{}`", self.key),
        };
        match self.suggestion {
            Some(s) => write!(f, "{what} — did you mean `{s}`? It is being ignored"),
            None => write!(f, "{what} is being ignored"),
        }
    }
}

/// Keys a check may declare. Anything else is reported.
const KNOWN_CHECK_KEYS: &[&str] = &["uses", "with", "paths", "exclude_paths"];

/// Keys the file may declare at the top level.
const KNOWN_TOP_KEYS: &[&str] = &["version", "checks"];

/// A single declared check.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    /// As declared in the manifest, e.g. `login-build`.
    pub local_id: String,
    /// Fully-qualified capability reference, e.g. `bazel/build`.
    pub uses: String,
    /// Extension-owned `with` payload, captured opaquely.
    pub with_payload: serde_json::Value,
    /// Optional gitignore-style glob patterns. When non-empty, the
    /// check's effective scope is narrowed to files matching at least
    /// one pattern (relative to the workspace root). An empty vector
    /// means "no filter — apply to every file under the declaring
    /// manifest's folder, modulo the standard same-capability carve-out".
    pub paths: Vec<String>,
    /// Optional gitignore-style glob patterns that subtract files from
    /// the effective scope after `paths` has been applied. An empty
    /// vector means "subtract nothing".
    pub exclude_paths: Vec<String>,
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
    #[serde(default = "default_with")]
    with: serde_yaml::Value,
    #[serde(default)]
    paths: RawPaths,
    #[serde(default)]
    exclude_paths: RawPaths,
}

/// `paths:` accepts either a single string or a list of strings. Absent
/// is treated as an empty list.
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum RawPaths {
    #[default]
    Absent,
    One(String),
    Many(Vec<String>),
}

impl RawPaths {
    fn into_vec(self) -> Vec<String> {
        match self {
            RawPaths::Absent => Vec::new(),
            RawPaths::One(s) => vec![s],
            RawPaths::Many(v) => v,
        }
    }
}

/// Absent `with:` defaults to an empty mapping rather than `null`. This
/// lets extensions that take no parameters declare a strict schema
/// (`{"type":"object","additionalProperties":false}`) without also
/// having to allow `null`.
fn default_with() -> serde_yaml::Value {
    serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
}

/// Parse a `MUSTS.yml` file from bytes. `path` is used for error messages
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

    // Unknown keys are collected, not rejected — see [`ManifestWarning`].
    // The top-level scan re-parses the file as a bare mapping because
    // `ManifestFile` has already dropped anything it did not name.
    let mut warnings = Vec::new();
    if let Ok(top) = serde_yaml::from_slice::<serde_yaml::Mapping>(bytes) {
        warnings.extend(unknown_keys(&top, KNOWN_TOP_KEYS, None));
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

        if let serde_yaml::Value::Mapping(map) = raw_value {
            warnings.extend(unknown_keys(map, KNOWN_CHECK_KEYS, Some(&local_id)));
        }

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

        let paths = raw_check.paths.into_vec();
        validate_path_patterns(path, &local_id, "paths", &paths)?;
        let exclude_paths = raw_check.exclude_paths.into_vec();
        validate_path_patterns(path, &local_id, "exclude_paths", &exclude_paths)?;

        if checks
            .insert(
                local_id.clone(),
                Check {
                    local_id: local_id.clone(),
                    uses: raw_check.uses,
                    with_payload,
                    paths,
                    exclude_paths,
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
        warnings,
    })
}

/// Collect every key in `mapping` that is not in `known`.
fn unknown_keys(
    mapping: &serde_yaml::Mapping,
    known: &[&'static str],
    check_local_id: Option<&str>,
) -> Vec<ManifestWarning> {
    mapping
        .keys()
        .filter_map(serde_yaml::Value::as_str)
        .filter(|k| !known.contains(k))
        .map(|k| ManifestWarning {
            check_local_id: check_local_id.map(str::to_string),
            key: k.to_string(),
            suggestion: nearest_key(k, known),
        })
        .collect()
}

/// Closest valid key to `written`, or `None` when nothing is close
/// enough to name without misleading.
///
/// Ranked on shared prefix first, edit distance second. Raw edit distance
/// alone gets the real-world case wrong: `excludes` is 5 edits from
/// `paths` but 6 from `exclude_paths`, so it would suggest the one key
/// that changes the check's meaning instead of the one the author meant.
fn nearest_key(written: &str, known: &[&'static str]) -> Option<&'static str> {
    known
        .iter()
        .map(|cand| {
            (
                common_prefix_len(written, cand),
                edit_distance(written, cand),
                *cand,
            )
        })
        .filter(|(prefix, distance, _)| *prefix >= 3 || *distance <= 2)
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)))
        .map(|(_, _, cand)| cand)
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Plain Levenshtein. Keys are a handful of chars, so the quadratic
/// table is free and not worth a dependency.
fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut cur = vec![0; b_chars.len() + 1];
    for (i, ac) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, bc) in b_chars.iter().enumerate() {
            let substitution = prev[j] + usize::from(ac != *bc);
            cur[j + 1] = substitution.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b_chars.len()]
}

/// Validate every path pattern as a gitignore-style glob. `field` names
/// the manifest key (`paths` / `exclude_paths`) so errors point at the
/// right place. Returns a manifest error naming the offending check and
/// pattern on the first failure. Empty patterns are rejected because they
/// would otherwise match nothing and silently disable the check. A leading
/// `!` is rejected too: `globset::Glob` treats it as a literal character
/// rather than gitignore negation, so `!foo` silently matched nothing;
/// authors who want exclusions must use `exclude_paths`.
fn validate_path_patterns(
    manifest_path: &Path,
    local_id: &str,
    field: &str,
    paths: &[String],
) -> Result<()> {
    for pat in paths {
        if pat.trim().is_empty() {
            return Err(Error::Manifest {
                path: manifest_path.to_path_buf(),
                message: format!("check `{local_id}`: `{field}` contains an empty pattern"),
            });
        }
        if pat.trim_start().starts_with('!') {
            return Err(Error::Manifest {
                path: manifest_path.to_path_buf(),
                message: format!(
                    "check `{local_id}`: `{field}` pattern `{pat}` uses `!` negation, which musts \
                     does not support (it would silently match nothing). Move the exclusion into \
                     an `exclude_paths:` entry without the `!`."
                ),
            });
        }
        globset::Glob::new(pat).map_err(|err| Error::Manifest {
            path: manifest_path.to_path_buf(),
            message: format!("check `{local_id}`: invalid glob `{pat}`: {err}"),
        })?;
    }
    Ok(())
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
        PathBuf::from("/repo/MUSTS.yml")
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
    fn absent_with_defaults_to_empty_object() {
        // Extensions that take no `with` parameters should be able to
        // declare a strict schema (`{"type":"object",
        // "additionalProperties":false}`); defaulting absent `with` to
        // `{}` instead of `null` lets that schema match.
        let yaml = br#"
version: 1
checks:
  free:
    uses: custom/noop
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(manifest.checks["free"].with_payload, serde_json::json!({}));
    }

    #[test]
    fn explicit_null_with_is_preserved() {
        // `with: null` written by hand stays null — the default only
        // applies to a fully-absent field.
        let yaml = br#"
version: 1
checks:
  explicit:
    uses: custom/noop
    with: null
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(
            manifest.checks["explicit"].with_payload,
            serde_json::Value::Null
        );
    }

    #[test]
    fn paths_defaults_to_empty() {
        let yaml = br#"
version: 1
checks:
  free:
    uses: custom/noop
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert!(manifest.checks["free"].paths.is_empty());
    }

    #[test]
    fn paths_accepts_single_string() {
        let yaml = br#"
version: 1
checks:
  tracking-tests:
    uses: cargo/test
    paths: "**/Tracking*.swift"
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(
            manifest.checks["tracking-tests"].paths,
            vec!["**/Tracking*.swift".to_string()]
        );
    }

    #[test]
    fn paths_accepts_list() {
        let yaml = br#"
version: 1
checks:
  tracking-tests:
    uses: cargo/test
    paths:
      - "**/Tracking*.swift"
      - "**/TrackingEvents/**"
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(
            manifest.checks["tracking-tests"].paths,
            vec![
                "**/Tracking*.swift".to_string(),
                "**/TrackingEvents/**".to_string(),
            ]
        );
    }

    #[test]
    fn rejects_invalid_glob() {
        // An unterminated character class is a globset parse error.
        let yaml = br#"
version: 1
checks:
  bad:
    uses: cargo/test
    paths:
      - "src/[a-z"
"#;
        let err = parse(&p(), yaml).unwrap_err();
        let Error::Manifest { message, .. } = err else {
            panic!("expected Manifest error");
        };
        assert!(message.contains("bad"));
        assert!(message.contains("invalid glob"));
    }

    #[test]
    fn rejects_empty_pattern() {
        let yaml = br#"
version: 1
checks:
  bad:
    uses: cargo/test
    paths:
      - ""
"#;
        let err = parse(&p(), yaml).unwrap_err();
        let Error::Manifest { message, .. } = err else {
            panic!("expected Manifest error");
        };
        assert!(message.contains("empty pattern"));
    }

    #[test]
    fn exclude_paths_defaults_to_empty() {
        let yaml = br#"
version: 1
checks:
  free:
    uses: custom/noop
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert!(manifest.checks["free"].exclude_paths.is_empty());
    }

    #[test]
    fn parses_exclude_paths_list_and_single() {
        let yaml = br#"
version: 1
checks:
  build:
    uses: bazel/build
    with: { target: //x }
    exclude_paths:
      - "tools/config.bzl"
      - "**/*.generated.swift"
  unit:
    uses: cargo/test
    paths: "**/*.swift"
    exclude_paths: "**/*Snapshot*.swift"
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(
            manifest.checks["build"].exclude_paths,
            vec![
                "tools/config.bzl".to_string(),
                "**/*.generated.swift".to_string(),
            ]
        );
        assert_eq!(
            manifest.checks["unit"].paths,
            vec!["**/*.swift".to_string()]
        );
        assert_eq!(
            manifest.checks["unit"].exclude_paths,
            vec!["**/*Snapshot*.swift".to_string()]
        );
    }

    #[test]
    fn rejects_bang_negation_in_paths_with_guidance() {
        let yaml = br#"
version: 1
checks:
  bad:
    uses: cargo/test
    paths:
      - "**/*.swift"
      - "!**/*Snapshot*.swift"
"#;
        let err = parse(&p(), yaml).unwrap_err();
        let Error::Manifest { message, .. } = err else {
            panic!("expected Manifest error");
        };
        assert!(message.contains("bad"));
        assert!(message.contains('!'));
        assert!(message.contains("exclude_paths"));
    }

    #[test]
    fn rejects_bang_negation_with_leading_whitespace() {
        // A stray leading space must not sneak a `!` pattern past the
        // guard (it would compile to a literal glob that matches nothing).
        let yaml = br#"
version: 1
checks:
  bad:
    uses: cargo/test
    paths:
      - " !**/*Snapshot*.swift"
"#;
        let err = parse(&p(), yaml).unwrap_err();
        let Error::Manifest { message, .. } = err else {
            panic!("expected Manifest error");
        };
        assert!(message.contains("exclude_paths"));
    }

    #[test]
    fn rejects_bang_negation_in_exclude_paths() {
        let yaml = br#"
version: 1
checks:
  bad:
    uses: cargo/test
    exclude_paths:
      - "!keep.swift"
"#;
        let err = parse(&p(), yaml).unwrap_err();
        let Error::Manifest { message, .. } = err else {
            panic!("expected Manifest error");
        };
        assert!(message.contains("exclude_paths"));
    }

    #[test]
    fn rejects_empty_exclude_pattern() {
        let yaml = br#"
version: 1
checks:
  bad:
    uses: cargo/test
    exclude_paths:
      - ""
"#;
        let err = parse(&p(), yaml).unwrap_err();
        let Error::Manifest { message, .. } = err else {
            panic!("expected Manifest error");
        };
        assert!(message.contains("empty pattern"));
        assert!(message.contains("exclude_paths"));
    }

    #[test]
    fn unknown_check_key_is_warned_not_rejected() {
        let yaml = br#"
version: 1
checks:
  unit-tests:
    uses: agent
    paths: ["src/**"]
    excludes: ["src/UI/**"]
"#;
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(manifest.warnings.len(), 1);
        let w = &manifest.warnings[0];
        assert_eq!(w.check_local_id.as_deref(), Some("unit-tests"));
        assert_eq!(w.key, "excludes");
        assert_eq!(w.suggestion, Some("exclude_paths"));
        // The typo is ignored, which is exactly the damage being reported.
        assert!(manifest.checks["unit-tests"].exclude_paths.is_empty());
    }

    #[test]
    fn excludes_suggests_exclude_paths_not_paths() {
        // Raw Levenshtein makes `paths` (5 edits) look closer than
        // `exclude_paths` (6), and `paths` is the one key whose
        // substitution silently changes what the check covers.
        assert_eq!(
            nearest_key("excludes", KNOWN_CHECK_KEYS),
            Some("exclude_paths")
        );
    }

    #[test]
    fn near_misses_are_suggested() {
        assert_eq!(nearest_key("use", KNOWN_CHECK_KEYS), Some("uses"));
        assert_eq!(nearest_key("path", KNOWN_CHECK_KEYS), Some("paths"));
        assert_eq!(nearest_key("wth", KNOWN_CHECK_KEYS), Some("with"));
        assert_eq!(
            nearest_key("exclude_path", KNOWN_CHECK_KEYS),
            Some("exclude_paths")
        );
    }

    #[test]
    fn unrelated_keys_get_no_suggestion() {
        assert_eq!(nearest_key("zzzzzzzz", KNOWN_CHECK_KEYS), None);
        assert_eq!(nearest_key("timeout_seconds", KNOWN_CHECK_KEYS), None);
    }

    #[test]
    fn unknown_top_level_key_is_warned() {
        let yaml = b"version: 1\nchecks: {}\nsettings: {}\n";
        let manifest = parse(&p(), yaml).unwrap();
        assert_eq!(manifest.warnings.len(), 1);
        assert!(manifest.warnings[0].check_local_id.is_none());
        assert_eq!(manifest.warnings[0].key, "settings");
    }

    #[test]
    fn a_valid_manifest_warns_about_nothing() {
        let yaml = br#"
version: 1
checks:
  c:
    uses: agent
    paths: ["a/**"]
    exclude_paths: ["a/gen/**"]
    with: { facts: ["f"] }
"#;
        assert!(parse(&p(), yaml).unwrap().warnings.is_empty());
    }

    #[test]
    fn warning_display_names_the_check_the_key_and_the_fix() {
        let w = ManifestWarning {
            check_local_id: Some("unit-tests".into()),
            key: "excludes".into(),
            suggestion: Some("exclude_paths"),
        };
        let s = w.to_string();
        assert!(s.contains("unit-tests"));
        assert!(s.contains("excludes"));
        assert!(s.contains("exclude_paths"));
        assert!(s.contains("ignored"));
    }

    #[test]
    fn edit_distance_is_symmetric_and_zero_on_equal() {
        assert_eq!(edit_distance("paths", "paths"), 0);
        assert_eq!(edit_distance("paths", "path"), 1);
        assert_eq!(edit_distance("abc", "cba"), edit_distance("cba", "abc"));
    }
}
