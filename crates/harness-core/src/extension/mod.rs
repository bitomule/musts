//! Extension loading + IPC per `docs/PLAN.md` §4.3 (`extension::*`) and §4.6.

pub mod descriptor;
pub mod runtime;

pub use descriptor::{
    discover_descriptors, load_descriptor, Capability, Command, ExtensionDescriptor,
};
pub use runtime::{run_evidence, run_resolve, ExtensionRunner, RuntimeOptions};
