//! Persistent state management for keeper-bot-v2.
//!
//! This module manages durable task state across process restarts:
//! - `schema`: the core persistent schema tracking task outcomes
//! - Future: `outcomes` (issue #282) for idempotent outcome recording with on-chain verification

pub mod schema;

pub use schema::{Store, TaskOutcome, TaskStatus};
