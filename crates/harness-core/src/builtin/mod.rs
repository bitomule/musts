//! Built-in capabilities implemented entirely by the core.
//!
//! Per `docs/PLAN.md` §6.0, these capabilities require no extension
//! descriptor — manifests can use them on any workspace with no
//! `.harness/extensions/` setup. The orchestrator's capability lookup
//! tries real (descriptor-backed) extensions first; on miss, it falls
//! back to this registry.

use harness_protocol::{
    EvidenceValidationRequest, EvidenceValidationResponse, ResolveRequest, ResolveResponse,
};
use serde_json::Value as JsonValue;

use crate::error::Error;

pub mod agent;

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

const REGISTRY: &[BuiltinCapability] = &[BuiltinCapability {
    uses: "agent",
    schema: agent::schema,
    resolve: agent::resolve,
    evidence: agent::evidence,
}];
