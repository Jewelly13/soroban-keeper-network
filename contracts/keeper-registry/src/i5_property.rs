//! Test utilities for invariant I-5: administrative operations must not
//! alter task escrow or credited keeper balances.
//!
//! I-5 deliberately distinguishes task escrow and credited keeper balances
//! from accrued fees. `sweep_fees` may reduce the registry's token balance by
//! transferring accrued fees, but it must not alter either of those protected
//! buckets.

#![cfg(any(test, fuzzing))]

extern crate std;

use std::{format, string::String, vec::Vec};

/// The balances relevant to I-5 at one point in a state-machine run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsolationSnapshot {
    /// Rewards of all tasks that were open when the snapshot was taken.
    pub open_task_rewards: Vec<(u64, i128)>,
    /// Credited balances of every keeper observed by the harness.
    pub keeper_balances: Vec<(u32, i128)>,
}

/// Assert that an administrative operation changed no task escrow or keeper
/// credit.
pub fn assert_i5_isolated(
    operation: &str,
    before: &IsolationSnapshot,
    after: &IsolationSnapshot,
) -> Result<(), String> {
    if before.open_task_rewards != after.open_task_rewards {
        return Err(format!(
            "I-5 violated by {operation}: open task rewards changed from {:?} to {:?}",
            before.open_task_rewards, after.open_task_rewards
        ));
    }

    if before.keeper_balances != after.keeper_balances {
        return Err(format!(
            "I-5 violated by {operation}: keeper balances changed from {:?} to {:?}",
            before.keeper_balances, after.keeper_balances
        ));
    }

    Ok(())
}

/// Assert that an administrative operation which is not `sweep_fees` moved
/// no registry token balance. `sweep_fees` is excluded because it is allowed
/// to transfer accrued fees.
pub fn assert_non_sweep_token_balance_unchanged(
    operation: &str,
    before: i128,
    after: i128,
) -> Result<(), String> {
    if before != after {
        return Err(format!(
            "I-5 violated by {operation}: registry token balance changed from {before} to {after}"
        ));
    }

    Ok(())
}

/// Execute an administrative operation while checking task escrow and keeper
/// isolation. The caller supplies snapshots because reading tasks and keeper
/// balances is specific to the Soroban test fixture.
pub fn check_admin_operation<F>(
    operation: &str,
    snapshot: impl Fn() -> IsolationSnapshot,
    call: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    let before = snapshot();
    call()?;
    let after = snapshot();
    assert_i5_isolated(operation, &before, &after)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(seed: u32) -> IsolationSnapshot {
        IsolationSnapshot {
            open_task_rewards: vec![(0, i128::from(seed) + 10), (1, 200)],
            keeper_balances: vec![(seed % 3, i128::from(seed) * 7)],
        }
    }

    #[test]
    fn i5_accepts_unchanged_buckets_for_100_sequences() {
        for seed in 0..100 {
            let before = snapshot(seed);
            let after = before.clone();
            assert_eq!(
                assert_i5_isolated("random-admin-operation", &before, &after),
                Ok(())
            );
        }
    }

    #[test]
    fn i5_reports_task_escrow_changes() {
        let before = snapshot(1);
        let mut after = before.clone();
        after.open_task_rewards[0].1 += 1;

        let error = assert_i5_isolated("sweep_fees", &before, &after).unwrap_err();
        assert!(error.contains("I-5"));
        assert!(error.contains("open task rewards"));
    }

    #[test]
    fn i5_reports_keeper_balance_changes() {
        let before = snapshot(1);
        let mut after = before.clone();
        after.keeper_balances[0].1 += 1;

        let error = assert_i5_isolated("pause", &before, &after).unwrap_err();
        assert!(error.contains("I-5"));
        assert!(error.contains("keeper balances"));
    }

    #[test]
    fn i5_allows_sweep_to_change_the_registry_balance() {
        assert_eq!(
            assert_i5_isolated("sweep_fees", &snapshot(1), &snapshot(1)),
            Ok(())
        );
    }

    #[test]
    fn i5_reports_non_sweep_registry_balance_changes() {
        let error =
            assert_non_sweep_token_balance_unchanged("upgrade", 1_000, 999).unwrap_err();
        assert!(error.contains("I-5"));
        assert!(error.contains("registry token balance"));
    }
}
