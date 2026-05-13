//! JSON Schema validation of manifest `with` payloads against the
//! capability's extension-declared schema.
//!
//! Per `docs/PLAN.md` §4.3 (`manifest::with_validation`), runs after
//! extensions are loaded but before any `resolve` call. Schema failures
//! surface as **manifest errors** (`Error::WithSchema`, exit 2), not as
//! extension failures, and include the manifest path and a JSON pointer
//! to the offending field.

use std::path::Path;

use serde_json::Value as JsonValue;

use crate::error::{Error, Result};

/// Validate one `with` payload. `manifest_path` is workspace-relative;
/// `capability` is the fully qualified capability id (e.g. `bazel/build`).
/// `check_id` is the global check ID.
///
/// Returns `Ok(())` when valid or when the capability did not declare a
/// schema (extensions are allowed to omit schemas in v1).
pub fn validate_with_payload(
    manifest_path: &Path,
    check_id: &str,
    capability: &str,
    schema: Option<&JsonValue>,
    payload: &JsonValue,
) -> Result<()> {
    let Some(schema) = schema else {
        return Ok(());
    };
    let validator = jsonschema::validator_for(schema).map_err(|err| Error::WithSchema {
        manifest_path: manifest_path.to_path_buf(),
        check_id: check_id.into(),
        capability: capability.into(),
        pointer: "".into(),
        message: format!("schema is invalid: {err}"),
    })?;
    if let Some(first) = validator.iter_errors(payload).next() {
        // Take the first error — agents only need one actionable
        // pointer, and the others usually cascade from it.
        let pointer = first.instance_path().to_string();
        let pointer = if pointer.is_empty() {
            "/".to_string()
        } else {
            pointer
        };
        return Err(Error::WithSchema {
            manifest_path: manifest_path.to_path_buf(),
            check_id: check_id.into(),
            capability: capability.into(),
            pointer,
            message: first.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn schema() -> JsonValue {
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["target"],
            "properties": {
                "target": { "type": "string" }
            },
            "additionalProperties": false
        })
    }

    fn p() -> PathBuf {
        PathBuf::from("MUSTS.yml")
    }

    #[test]
    fn accepts_valid_payload() {
        let payload = serde_json::json!({ "target": "//App:App" });
        validate_with_payload(
            &p(),
            "root/app-build",
            "bazel/build",
            Some(&schema()),
            &payload,
        )
        .unwrap();
    }

    #[test]
    fn missing_schema_is_a_pass() {
        let payload = serde_json::json!({ "anything": "goes" });
        validate_with_payload(&p(), "root/x", "x/y", None, &payload).unwrap();
    }

    #[test]
    fn rejects_payload_missing_required_field() {
        let payload = serde_json::json!({});
        let err = validate_with_payload(
            &p(),
            "root/app-build",
            "bazel/build",
            Some(&schema()),
            &payload,
        )
        .unwrap_err();
        match err {
            Error::WithSchema {
                manifest_path,
                check_id,
                capability,
                pointer: _,
                message,
            } => {
                assert_eq!(manifest_path, PathBuf::from("MUSTS.yml"));
                assert_eq!(check_id, "root/app-build");
                assert_eq!(capability, "bazel/build");
                assert!(
                    message.contains("target") || message.contains("required"),
                    "message: {message}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn rejects_additional_properties() {
        let payload = serde_json::json!({ "target": "//x", "extra": 1 });
        let err = validate_with_payload(&p(), "root/x", "bazel/build", Some(&schema()), &payload)
            .unwrap_err();
        assert!(matches!(err, Error::WithSchema { .. }));
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn rejects_wrong_type_with_pointer_to_field() {
        let payload = serde_json::json!({ "target": 42 });
        let err = validate_with_payload(&p(), "root/x", "bazel/build", Some(&schema()), &payload)
            .unwrap_err();
        match err {
            Error::WithSchema { pointer, .. } => {
                assert!(
                    pointer.contains("target") || pointer == "/",
                    "expected target pointer, got: {pointer}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
