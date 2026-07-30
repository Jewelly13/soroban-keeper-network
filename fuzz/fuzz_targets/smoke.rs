//! Smoke test fuzz target for the Keeper Registry contract.
//!
//! This target verifies basic linking and contract functionality:
//! - Environment creation
//! - Contract deployment
//! - Registry initialization
//! - Version check
//!
//! This is the simplest possible fuzz target that ensures the fuzzing
//! infrastructure links correctly against the real contract.

#![no_main]

use keeper_registry::VERSION;
use keeper_registry_fuzz::support::RegistryHarness;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|_data: &[u8]| {
    // Create a fresh registry harness
    let harness = RegistryHarness::new();
    let client = harness.client();

    // Verify the contract is deployed and accessible
    let version = client.version();

    // Basic assertion: version should match the contract constant
    assert_eq!(version, VERSION, "Contract version should be {}", VERSION);

    // Verify the contract is initialized
    let admin = client
        .admin()
        .expect("an initialized registry has an admin");
    assert_eq!(
        admin, harness.admin,
        "admin() must return the configured admin"
    );

    let reward_token = client
        .reward_token_address()
        .expect("an initialized registry has a reward token");
    assert_eq!(
        reward_token, harness.reward_token,
        "Reward token should be set"
    );

    // Verify fee bps is accessible (defaults to 0)
    let fee_bps = client.get_fee_bps();
    assert!(fee_bps <= 10_000, "Fee bps should be <= 10000");

    // Verify paused state is accessible (defaults to false)
    let paused = client.is_paused();
    assert!(!paused, "Contract should not be paused initially");

    // If we reach here, the harness linked successfully
    // No panics means the smoke test passes
});
