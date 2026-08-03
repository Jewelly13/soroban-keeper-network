//! Instance-storage TTL renewal.

use soroban_sdk::{
    testutils::{Address as _, Deployer as _, Events as _},
    Address,
};

use super::common::*;
use crate::{KeeperError, INSTANCE_BUMP_LEDGERS, INSTANCE_BUMP_THRESHOLD};

// ─────────────────────────────────────────────────────────────────────────────
// Instance TTL renewal
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_instance_ttl_renewed_by_mutation_stays_alive_past_initial_window() {
    let s = setup();

    // initialize() already bumped the instance TTL to ~INSTANCE_BUMP_LEDGERS.
    let ttl_after_init = s
        .env
        .deployer()
        .get_contract_instance_ttl(&s.registry.address);
    assert!(ttl_after_init > INSTANCE_BUMP_THRESHOLD);

    // Advance far enough that remaining TTL drops below the renewal
    // threshold, but not so far that the entry actually expires.
    advance(
        &s.env,
        INSTANCE_BUMP_LEDGERS - INSTANCE_BUMP_THRESHOLD + 1_000,
        0,
    );
    let ttl_before_mutation = s
        .env
        .deployer()
        .get_contract_instance_ttl(&s.registry.address);
    assert!(
        ttl_before_mutation < INSTANCE_BUMP_THRESHOLD,
        "test setup should cross the renewal threshold"
    );

    // A state-mutating admin call renews the TTL back up to
    // ~INSTANCE_BUMP_LEDGERS from the current ledger. Uses an instance-only
    // mutation (no persistent Task entry involved) so this test isolates
    // instance TTL renewal from per-task TTL, which is a separate mechanism
    // covered by `save_task`.
    s.registry.set_min_reward(&s.admin, &0i128);
    let ttl_after_mutation = s
        .env
        .deployer()
        .get_contract_instance_ttl(&s.registry.address);
    assert!(ttl_after_mutation > INSTANCE_BUMP_LEDGERS - 1_000);

    // Advance well past where the *original* TTL window (from initialize)
    // would have expired the instance — total ledgers advanced now exceeds
    // INSTANCE_BUMP_LEDGERS. Without the interim renewal above, the instance
    // would be archived here and every call below would fail.
    advance(&s.env, INSTANCE_BUMP_LEDGERS - 1_000, 0);

    // The contract is still fully usable: reads and further mutations both
    // succeed against the (still-live) instance storage.
    assert_eq!(s.registry.task_count(), 0u64);
    s.registry.set_fee_bps(&s.admin, &500u32);
    assert_eq!(s.registry.get_fee_bps(), 500u32);
}

// Issue 0122: docs/ARCHITECTURE.md's "TTL / archival strategy" section
// explicitly accepts that "a registry that is completely idle ... for the
// full TTL window can still archive" as a tradeoff for not bumping TTL on
// side-effect-free reads. This test proves that failure mode actually
// happens rather than just being documented: with zero mutating calls after
// `initialize()` (the only `bump_instance` call this test ever makes),
// advancing well past `INSTANCE_BUMP_LEDGERS` genuinely lapses the
// instance's TTL. `Deployer::get_contract_instance_ttl`'s host
// implementation computes `live_until_ledger.checked_sub(current_ledger)`,
// which underflows (panics) once the current ledger has actually passed
// the entry's expiry -- so this is a direct proof of archival, not an
// inference.
#[test]
#[should_panic]
fn test_instance_ttl_lapses_when_registry_is_fully_idle_past_bump_window() {
    let s = setup();

    advance(&s.env, INSTANCE_BUMP_LEDGERS + 1_000, 0);

    let _ = s
        .env
        .deployer()
        .get_contract_instance_ttl(&s.registry.address);
}

// Regression test for issue #18: `upgrade` previously emitted no event at
// all, so there was no on-chain, indexable record of who authorised an
// upgrade or which WASM hash it moved to. This asserts the rejection path
// specifically emits nothing — a non-admin's rejected attempt must not
// produce an `Upgraded` event, since `require_admin` fails before
// `emit_upgraded` is ever reached.
//
// The success path (`emit_upgraded` fires with the correct hash before
// `update_current_contract_wasm` swaps the executable) is not covered here
// for the same reason `resource_report` above excludes `upgrade`: exercising
// it for real needs a separately-deployed WASM hash already present on the
// ledger, and `update_current_contract_wasm` only takes effect — success or
// failure — once the whole invocation completes, so a bogus hash can't be
// used to observe the event in isolation without also rolling it back.
#[test]
fn test_upgrade_by_non_admin_fails() {
    let s = setup();
    let stranger = Address::generate(&s.env);
    let bogus = soroban_sdk::BytesN::from_array(&s.env, &[0u8; 32]);

    assert_eq!(
        s.registry.try_upgrade(&stranger, &bogus),
        Err(Ok(KeeperError::Unauthorized))
    );

    // `events().all()` reflects only the most recent top-level invocation
    // (see the note in `test_withdraw_transfers_balance_and_zeroes_it`), so
    // this is checked immediately after the single `try_upgrade` call above
    // rather than via a before/after count.
    assert!(
        s.env.events().all().is_empty(),
        "a rejected non-admin upgrade must not emit an Upgraded event"
    );
}
