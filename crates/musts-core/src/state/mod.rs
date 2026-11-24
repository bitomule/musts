//! Workspace state under `.musts/`.
//!
//! [`lock`] is the committed ledger: the only record of what is
//! validated. [`cache`] is machine-local scratch that never says so.

pub mod cache;
pub mod lock;

pub use lock::{Ledger, LedgerLock, SatisfiedEntry};
