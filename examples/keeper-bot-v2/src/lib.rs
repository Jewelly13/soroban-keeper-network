//! Keeper Bot v2 — Production keeper with persistent state and concurrent processing.
//!
//! This is a complete rewrite of examples/keeper-bot aimed at operators running
//! keepers competitively. It includes:
//!
//! - Persistent task state across restarts (issue #252)
//! - Concurrent round processing with safe claim locking (issue #253)
//! - Idempotent outcome recording with on-chain verification (issue #282)
//! - Configurable executors and profitability checks
//!
//! Unlike the v1 example (which is deliberately kept simple for newcomers),
//! v2 can handle production workloads: multiple tasks in flight, database state,
//! and complex executor logic without losing track of what it has already done.

pub mod state;
