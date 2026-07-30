//! Boundary-focused fuzz target for `register_task`'s calldata length check.
//!
//! Weighted toward `MAX_CALLDATA_LEN` (just under, exactly at, just over, far
//! over) so the `CalldataTooLarge` rejection surface is exercised precisely,
//! mirroring `register_task_bounds.rs` for lock/TTL (issue 0064 / #188).

#![no_main]

use arbitrary::Arbitrary;
use keeper_registry::{KeeperError, TaskType, MAX_CALLDATA_LEN};
use keeper_registry_fuzz::support::{arbitrary_bytes, RegistryHarness};
use libfuzzer_sys::fuzz_target;

/// Fuzzer input: a seed that is mapped onto boundary-biased calldata lengths.
///
/// Corpus seeds use the `Arbitrary` layout of this struct (little-endian
/// `u32`) with values that land on the `MAX_CALLDATA_LEN - 1`,
/// `MAX_CALLDATA_LEN`, and `MAX_CALLDATA_LEN + 1` buckets — see
/// `fuzz/corpus/register_task_calldata_bound/`.
#[derive(Arbitrary, Debug)]
struct CalldataBoundInput {
    length_seed: u32,
}

/// Map a free seed onto lengths concentrated at the calldata bound.
///
/// Buckets (by `seed % 10`):
/// - 0: empty
/// - 1: 1 byte
/// - 2: `MAX_CALLDATA_LEN - 1` (just under)
/// - 3: `MAX_CALLDATA_LEN` (exactly at — must accept)
/// - 4: `MAX_CALLDATA_LEN + 1` (just over — must be `CalldataTooLarge`)
/// - 5: `MAX_CALLDATA_LEN + 64` (modestly over)
/// - 6: `2 * MAX_CALLDATA_LEN` (far over, still cheap to allocate)
/// - 7..=9: scattered under / over the limit for coverage
fn boundary_biased_calldata_len(seed: u32) -> u32 {
    let max = MAX_CALLDATA_LEN;
    match seed % 10 {
        0 => 0,
        1 => 1,
        2 => max.saturating_sub(1),
        3 => max,
        4 => max.saturating_add(1),
        5 => max.saturating_add(64),
        6 => max.saturating_mul(2),
        7 => seed % max.max(1),
        8 => max.saturating_add(seed % 256),
        _ => (seed % (max.saturating_mul(2).saturating_add(1))).max(1),
    }
}

fuzz_target!(|input: CalldataBoundInput| {
    let len = boundary_biased_calldata_len(input.length_seed);

    let harness = RegistryHarness::new();
    let env = &harness.env;
    let client = harness.client();
    let user = harness.user.clone();

    // Fixed, valid parameters so only calldata length can cause rejection.
    let reward: i128 = 1_000;
    let deadline = env.ledger().timestamp() + 3_600;
    let ttl_ledgers: u32 = 20_000;
    let lock_ledgers: u32 = 120;

    // Deterministic contents; length is what matters for this target.
    let raw = vec![0xABu8; len as usize];
    let calldata = arbitrary_bytes(env, &raw);

    let result = client.try_register_task(
        &user,
        &TaskType::Liquidation,
        &calldata,
        &reward,
        &deadline,
        &ttl_ledgers,
        &lock_ledgers,
        &None,
    );

    match result {
        Ok(Ok(_task_id)) => {
            assert!(
                len <= MAX_CALLDATA_LEN,
                "register_task accepted calldata of length {len} (> MAX_CALLDATA_LEN={MAX_CALLDATA_LEN})"
            );
        }
        Ok(Err(_)) => {
            panic!(
                "register_task returned Ok but the value failed host conversion — ABI mismatch"
            );
        }
        Err(Ok(KeeperError::CalldataTooLarge)) => {
            assert!(
                len > MAX_CALLDATA_LEN,
                "CalldataTooLarge for in-bound length {len} (MAX_CALLDATA_LEN={MAX_CALLDATA_LEN})"
            );
        }
        Err(Ok(other)) => {
            panic!(
                "expected acceptance or CalldataTooLarge for valid non-calldata params, got {other:?} (len={len})"
            );
        }
        Err(Err(_)) => {
            panic!("register_task host-errored/trapped instead of typed KeeperError (len={len})");
        }
    }
});
