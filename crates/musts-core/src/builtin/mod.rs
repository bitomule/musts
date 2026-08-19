//! Built-in capabilities implemented entirely by the core.
//!
//! Per `docs/PLAN.md` §6.0, these capabilities require no extension
//! descriptor — manifests can use them on any workspace with no
//! `.musts/extensions/` setup. The orchestrator's capability lookup
//! tries real (descriptor-backed) extensions first; on miss, it falls
//! back to this registry.

use musts_protocol::{
    EvidenceValidationRequest, EvidenceValidationResponse, ResolveRequest, ResolveResponse,
};
use serde_json::Value as JsonValue;

use crate::error::Error;

pub mod agent;
pub mod bazel_build;
pub mod bazel_test;
pub mod cargo;
pub mod mav_expect;
mod util;

/// One built-in capability — same surface as an external extension,
/// minus the IPC. Internal-only.
pub struct BuiltinCapability {
    /// Fully qualified capability id (e.g. `"agent"`).
    pub uses: &'static str,
    /// JSON Schema used by `manifest::with_validation` for this
    /// capability's `with` payloads.
    pub schema: fn() -> &'static JsonValue,
    pub resolve: fn(&ResolveRequest) -> Result<ResolveResponse, Error>,
    pub evidence: fn(&EvidenceValidationRequest) -> Result<EvidenceValidationResponse, Error>,
}

/// Look up the built-in implementor of `uses`. Returns `None` for
/// capabilities that must be provided by an external descriptor.
pub fn lookup(uses: &str) -> Option<&'static BuiltinCapability> {
    REGISTRY.iter().find(|c| c.uses == uses)
}

/// Every capability id this build implements without an extension.
pub fn registered_capabilities() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|c| c.uses)
}

const REGISTRY: &[BuiltinCapability] = &[
    BuiltinCapability {
        uses: "agent",
        schema: agent::schema,
        resolve: agent::resolve,
        evidence: agent::evidence,
    },
    BuiltinCapability {
        uses: "cargo/fmt",
        schema: cargo::schema,
        resolve: cargo::resolve,
        evidence: cargo::evidence,
    },
    BuiltinCapability {
        uses: "cargo/clippy",
        schema: cargo::schema,
        resolve: cargo::resolve,
        evidence: cargo::evidence,
    },
    BuiltinCapability {
        uses: "cargo/test",
        schema: cargo::schema,
        resolve: cargo::resolve,
        evidence: cargo::evidence,
    },
    BuiltinCapability {
        uses: "bazel/build",
        schema: bazel_build::schema,
        resolve: bazel_build::resolve,
        evidence: bazel_build::evidence,
    },
    BuiltinCapability {
        uses: "bazel/test",
        schema: bazel_test::schema,
        resolve: bazel_test::resolve,
        evidence: bazel_test::evidence,
    },
    BuiltinCapability {
        uses: "mav/expect",
        schema: mav_expect::schema,
        resolve: mav_expect::resolve,
        evidence: mav_expect::evidence,
    },
];
