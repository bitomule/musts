//! Core library for the `harness` CLI.
//!
//! Phases land per `docs/PLAN.md`:
//! - Phase 1 (this commit): `error`, `workspace`, `manifest`, `snapshot`, `state`.
//! - Phase 2: `extension` (loading + IPC).
//! - Phase 3: `validate`, `report`.
//! - Phase 4: `evidence`.

pub mod error;
pub mod manifest;
pub mod snapshot;
pub mod state;
pub mod workspace;

pub use error::{Error, Result};
