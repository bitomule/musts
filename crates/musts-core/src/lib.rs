//! Core library for the `musts` CLI.
//!
//! Phases land per `docs/PLAN.md`:
//! - Phase 1: `error`, `workspace`, `manifest`, `snapshot`, `state`.
//! - Phase 2: `extension` (descriptor + IPC), `manifest::with_validation`.
//! - Phase 3: `bootstrap`, `validate`, `report`.
//! - Phase 4 (this commit): `evidence`.

pub mod bootstrap;
pub mod builtin;
pub mod diagnose;
pub mod error;
pub mod evidence;
pub mod extension;
pub mod manifest;
pub mod report;
pub mod run;
pub mod snapshot;
pub mod state;
pub mod stats;
pub mod validate;
pub mod workspace;

pub use error::{Error, Result};
