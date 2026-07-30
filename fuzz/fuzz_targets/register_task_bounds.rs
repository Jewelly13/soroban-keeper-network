//! Boundary-focused fuzz target for register_task parameter validation.
//!
//! The generated values are deliberately concentrated around the documented
//! lock and TTL limits so that just-inside, just-outside, and distant values
//! are exercised frequently.

#![no_main]

use arbitrary::Arbitrary;
use keeper_registry::{KeeperError, MAX_LOCK_LEDGERS, MIN_LOCK_LEDGERS, MIN_TTL_LEDGERS};
use keeper_registry_fuzz::support::{arbitrary_bytes, arbitrary_task_type, RegistryHarness};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct RegisterTaskBoundsInput {
    lock_seed: u32,
    ttl_seed: u32,
}

fn boundary_biased_lock(seed: u32) -> u32 {
    match seed % 8 {
        0 => 0,
        1 => MIN_LOCK_LEDGERS - 1,
        2 => MIN_LOCK_LEDGERS,
        3 => MIN_LOCK_LEDGERS + 1,
        4 => MAX_LOCK_LEDGERS - 1,
        5 => MAX_LOCK_LEDGERS,
        6 => MAX_LOCK_LEDGERS + 1,
        _ => u32::MAX,
    }
}

fn boundary_biased_ttl(seed: u32) -> u32 {
    match seed % 8 {
        0 => 0,
        1 => MIN_TTL_LEDGERS - 1,
        2 => MIN_TTL_LEDGERS,
        3 => MIN_TTL_LEDGERS + 1,
        4 => MIN_TTL_LEDGERS + 2,
        5 => u32::MAX,
        6 => MIN_TTL_LEDGERS + (seed % 1_000),
        _ => seed.max(MIN_TTL_LEDGERS),
    }
}

fuzz_target!(|input: RegisterTaskBoundsInput| {
    let lock_ledgers = boundary_biased_lock(input.lock_seed);
    let ttl_ledgers = boundary_biased_ttl(input.ttl_seed);

    let harness = RegistryHarness::new();
    let env = &harness.env;
    let client = harness.client();
    let user = harness.user.clone();

    let task_type = arbitrary_task_type(env, 0);
    let calldata = arbitrary_bytes(env, &[]);
    let reward: i128 = 1;
    let deadline = env.ledger().timestamp() + 1;

    let result = client.try_register_task(
        &user,
        &task_type,
        &calldata,
        &reward,
        &deadline,
        &ttl_ledgers,
        &lock_ledgers,
    );

    // A host-level failure indicates a trap or invocation failure rather than
    // the contract's typed parameter-validation error.
    let contract_result = result.expect("register_task must not trap");
    let lock_valid = (MIN_LOCK_LEDGERS..=MAX_LOCK_LEDGERS).contains(&lock_ledgers);
    let ttl_valid = ttl_ledgers >= MIN_TTL_LEDGERS;

    if lock_valid && ttl_valid {
        contract_result.expect("valid lock and TTL values must be accepted");
    } else {
        let error = contract_result.expect_err(
            "an out-of-range lock or TTL value must be rejected",
        );
        assert!(
            matches!(error, KeeperError::InvalidTaskParams),
            "parameter-bound rejection must be InvalidTaskParams, got {:?}",
            error
        );
    }
});
