//! Persistent state stored at `.harness/state.sqlite`.
//!
//! Schema lives in `schema.rs` and matches `docs/PLAN.md` §4.7. Migrations
//! are versioned and idempotent.
//!
//! Phase 1 ships the open-and-migrate primitive plus the `manifest_index`
//! and `file_fingerprints` CRUD helpers. The remaining tables are created
//! by the same migration up front so later phases can use them without a
//! schema bump.

pub mod db;
pub mod lock;
pub mod schema;

pub use db::{open, Db};
pub use lock::{LedgerLock, SatisfiedEntry};
