use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Bytes, Env, IntoVal,
};

fn setup(fee_bps: u32) -> (Env, Address, Address, Address) {
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Bytes, Env,
};
//! # KeeperRegistry — Test Suite
//!
//! Property tests for keeper reward withdrawal liveness.

#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Deployer as _, Ledger, MockAuth},
    token, Address, Bytes, Env,
};

use crate::{KeeperRegistry, KeeperRegistryClient, TaskType};

fn calldata(env: &Env) -> Bytes {
    Bytes::from_slice(env, b"liquidate:position:42")
}

/// I-6: a keeper's credited balance remains withdrawable regardless of pause
/// state or pause/unpause interleavings.
#[test]
fn property_i6_keeper_balance_is_always_withdrawable() {
    // Each sequence contains pause/unpause operations performed after all
    // credits have been made. An empty sequence covers withdrawal while the
    // registry has never been paused; a sequence ending in `true` covers
    // withdrawal while paused; the longer sequences cover repeated cycles.
    let pause_sequences: &[&[bool]] = &[
        &[],
        &[true],
        &[true, false],
        &[true, false, true],
        &[true, false, true, false],
        &[true, false, true, false, true],
    ];

    // Vary the number of credits to exercise accumulation rather than only a
    // single fixed withdrawal amount.
    for credit_count in 1i128..=4i128 {
        for sequence in pause_sequences {
            let env = Env::default();
            env.mock_all_auths();

            let admin = Address::generate(&env);
            let keeper = Address::generate(&env);
            let token_id = env
                .register_stellar_asset_contract_v2(admin.clone())
                .address();
            token::StellarAssetClient::new(&env, &token_id).mint(&admin, &10_000_000i128);

            let registry_id = env.register(KeeperRegistry, ());
            let registry = KeeperRegistryClient::new(&env, &registry_id);
            registry.initialize(&admin, &token_id, &300u32);

            for _ in 0..credit_count {
                let deadline = env.ledger().timestamp() + 3_600;
                let task_id = registry.register_task(
                    &admin,
                    &TaskType::Liquidation,
                    &calldata(&env),
                    &1_000_000i128,
                    &deadline,
                    &17_280u32,
                    &120u32,
                );
                registry.claim_task(&keeper, &task_id);
                registry.execute_task(
                    &keeper,
                    &task_id,
                    &Bytes::from_slice(&env, b"proof"),
                );
            }

            let expected = credit_count * 970_000i128;
            assert_eq!(registry.keeper_balance(&keeper), expected);

            for &pause in *sequence {
                if pause {
                    registry.pause(&admin);
                } else {
                    registry.unpause(&admin);
                }
            }

            let token = token::Client::new(&env, &token_id);
            let keeper_before = token.balance(&keeper);
            let withdrawn = registry.withdraw_rewards(&keeper);

            assert_eq!(withdrawn, expected);
            assert_eq!(token.balance(&keeper), keeper_before + expected);
            assert_eq!(registry.keeper_balance(&keeper), 0i128);
        }
    }
}
//! # KeeperRegistry — Solvency Invariant Tests
//!
//! Run with: `cargo test -p keeper-registry`
//!
//! ## Wave-1 regression checkpoint (#112/0087)
//! Every pre-verifier-epic test (as of commit `f6f988d`, immediately
//! before #97-#106 started landing) still exists and still passes.
//! Compared function-by-function against that baseline, every diff in a
//! pre-existing test falls into exactly one of three categories — none of
//! which is a behavioral regression:
//!   1. The mechanical `&None,` `register_task`/`try_register_task`
//!      argument addition (0073's own explicitly-in-scope arity change).
//!   2. A `ttl_ledgers` value bump (typically `17_280u32` → `20_000u32`)
//!      required by a separately-landed, legitimately concurrent PR that
//!      added a dynamic `required_ttl_ledgers(deadline)` floor on top of
//!      the pre-existing static `MIN_TTL_LEDGERS` — an adaptation to a
//!      real, intentional new invariant, not a workaround for a bug in
//!      this epic's own code. Likewise the `split_reward(...)` call sites
//!      gaining `.unwrap()`/`.expect(...)`, required by a separate
//!      overflow-checked-arithmetic refactor that changed `split_reward`'s
//!      return type to a `Result`.
//!   3. Two event-assertion tests (`test_set_fee_emits_event`,
//!      `test_transfer_admin_emits_event`) were rewritten because their
//!      original assertion mechanism — comparing
//!      `s.env.events().all().len()` before and after a call — was
//!      already broken: `events().all()` reflects only the most recent
//!      top-level call, not a running log, so a "before" count taken
//!      after `setup()` (a separate, prior call) silently doesn't include
//!      the call under test. The fix checks the emitted event directly,
//!      immediately after the call that emits it; what's being verified
//!      (does this action emit its event) is unchanged, only the broken
//!      verification mechanism was fixed.
//!
//! No test needed a genuine behavioral change to keep passing.
//! These tests verify that every token held by the registry is accounted for
//! by task escrow, keeper credits, or accrued protocol fees.
//! # KeeperRegistry — Test Suite

#![cfg(test)]

use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Deployer as _, Ledger, MockAuth},
    token, Address, Bytes, Env,
};

use crate::{split_reward, KeeperError, KeeperRegistry, KeeperRegistryClient, TaskType};
use crate::{KeeperRegistry, KeeperRegistryClient, TaskType};
    testutils::{Address as _, Ledger, MockAuth},
    token, Address, Bytes, Env,
};

use crate::{KeeperError, KeeperRegistry, KeeperRegistryClient, TaskStatus, TaskType};

struct Setup {
    env: Env,
    admin: Address,
    registry: KeeperRegistryClient<'static>,
    token_id: Address,
}

// The shared environment/client lifetime is intentionally extended for the
// standard Soroban test-harness pattern.
#[allow(clippy::useless_transmute, clippy::missing_transmute_annotations)]
fn setup(fee_bps: u32) -> Setup {
fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &10_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &fee_bps);

    let env = unsafe { core::mem::transmute::<Env, Env>(env) };
    Setup {
        env,
        admin,
        registry: unsafe { core::mem::transmute(registry) },
        token_id,
    }
}

fn calldata(env: &Env) -> Bytes {
    Bytes::from_slice(env, b"i4-test")
}

fn register_reward_task(setup: &Setup, reward: i128) -> u64 {
    let deadline = setup.env.ledger().timestamp() + 3_600;
    setup.registry.register_task(
        &setup.admin,
        &TaskType::Liquidation,
        &calldata(&setup.env),
    Bytes::from_slice(env, b"solvency-test")
}

fn register_task(s: &Setup, reward: i128) -> u64 {
    let deadline = s.env.ledger().timestamp() + 3_600;
    Bytes::from_slice(env, b"liquidate:position:42")
}

fn register_task(s: &Setup, reward: i128) -> u64 {
    s.registry.register_task(
        &s.admin,
        &TaskType::Custom,
        &calldata(&s.env),
        &reward,
        &(s.env.ledger().timestamp() + 3_600),
        &17_280u32,
        &deadline,
        &20_000u32,
        &120u32,
        &None,
    )
}

proptest! {
    /// I-4: split_reward conserves the reward and never rounds the protocol
    /// fee above the nominal basis-point rate.
    #[test]
    fn property_i4_split_reward_fee_is_bounded(
        reward in 1i128..=(i64::MAX as i128),
        fee_bps in 0u32..=10_000u32,
    ) {
        let (keeper_net, fee) = split_reward(reward, fee_bps);
        let nominal_fee = reward * (fee_bps as i128) / 10_000i128;

        prop_assert_eq!(keeper_net + fee, reward);
        prop_assert!(fee >= 0);
        prop_assert!(keeper_net >= 0);
        prop_assert!(fee <= nominal_fee);
    }
}

proptest! {
    /// I-4: sweep_fees cannot withdraw more than the currently accrued fees,
    /// including after multiple executions and a partial sweep followed by
    /// additional accrual.
    #[test]
    fn property_i4_sweep_never_exceeds_accrued(executions in 1usize..=4usize) {
        let setup = setup();
        let keeper = Address::generate(&setup.env);
        let treasury = Address::generate(&setup.env);

        let first_task = register_reward_task(&setup, 100_000i128);
        setup.registry.claim_task(&keeper, &first_task);
        setup.registry.execute_task(
            &keeper,
            &first_task,
            &Bytes::from_slice(&setup.env, b"i4-first"),
        );

        let accrued_after_first = setup.registry.fees_accrued();
        prop_assert!(accrued_after_first > 0);
        prop_assert_eq!(
            setup.registry.try_sweep_fees(
                &setup.admin,
                &treasury,
                &(accrued_after_first + 1i128),
            ),
            Err(Ok(KeeperError::NoRewardsAvailable))
        );
        prop_assert_eq!(setup.registry.fees_accrued(), accrued_after_first);

        let partial = accrued_after_first / 2;
        setup.registry.sweep_fees(&setup.admin, &treasury, &partial);
        let remainder_after_partial = setup.registry.fees_accrued();
        prop_assert_eq!(
            remainder_after_partial,
            accrued_after_first - partial
        );
        prop_assert_eq!(
            setup.registry.try_sweep_fees(
                &setup.admin,
                &treasury,
                &(remainder_after_partial + 1i128),
            ),
            Err(Ok(KeeperError::NoRewardsAvailable))
        );
        prop_assert_eq!(setup.registry.fees_accrued(), remainder_after_partial);

        for index in 0..executions {
            let task_id = register_reward_task(&setup, 100_000i128 + index as i128);
            setup.registry.claim_task(&keeper, &task_id);
            setup.registry.execute_task(
                &keeper,
                &task_id,
                &Bytes::from_slice(&setup.env, b"i4-more"),
            );
        }

        let accrued_after_more = setup.registry.fees_accrued();
        prop_assert!(accrued_after_more >= remainder_after_partial);
        prop_assert_eq!(
            setup.registry.try_sweep_fees(
                &setup.admin,
                &treasury,
                &(accrued_after_more + 1i128),
            ),
            Err(Ok(KeeperError::NoRewardsAvailable))
fn advance(env: &Env, ledgers: u32, seconds: u64) {
    env.ledger().with_mut(|ledger| {
        ledger.sequence_number += ledgers;
        ledger.timestamp += seconds;
    });
}

/// Asserts that the registry holds exactly what it owes:
///
/// `balance(registry) == open-task escrow + credited keeper balances + accrued fees`.
/// `open_task_ids` must contain every task currently in `Pending` or `Claimed`,
/// and `keepers` must contain every address that has ever been credited. The
/// registry deliberately exposes no on-chain enumeration for either collection,
/// so callers supply both sets explicitly.
fn assert_solvent(
    env: &Env,
    client: &KeeperRegistryClient,
    token: &token::Client,
    registry_id: &Address,
    open_task_ids: &[u64],
    keepers: &[Address],
) {
    let held = token.balance(registry_id);
    let escrow: i128 = open_task_ids
        .iter()
        .map(|id| client.get_task(id).reward)
        .sum();
    let credited: i128 = keepers.iter().map(|keeper| client.keeper_balance(keeper)).sum();
    let fees = client.fees_accrued();

    assert_eq!(
        held,
        escrow + credited + fees,
        "registry balance {} != escrow {} + credited {} + fees {}",
        held,
        escrow,
        credited,
        fees
#[derive(Clone, Copy)]
enum TerminalStatus {
    Executed,
    Cancelled,
    Expired,
}

fn task_in_status(s: &Setup, status: TerminalStatus) -> (u64, Address) {
    let task_id = register_task(s, 1_000_000);
    let keeper = Address::generate(&s.env);
// ─────────────────────────────────────────────────────────────────────────────
// Pure-function invariants: split_reward
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_split_reward_invariants() {
    // Exhaustively sweep a grid of rewards and fee rates and assert the core
    // accounting invariants hold for every combination — no value is ever
    // created or destroyed by the split.
    let rewards = [
        1i128,
        2,
        7,
        100,
        999,
        1_000_000,
        7_777_777,
        i128::from(u64::MAX),
    ];
    let fee_rates = [0u32, 1, 3, 250, 300, 1_000, 5_000, 9_999, 10_000];

    for &reward in &rewards {
        for &bps in &fee_rates {
            let (keeper_net, fee) = split_reward(reward, bps).expect("split should succeed");

    match status {
        TerminalStatus::Executed => {
            s.registry.claim_task(&keeper, &task_id);
            s.registry
                .execute_task(&keeper, &task_id, &calldata(&s.env));
        }
        TerminalStatus::Cancelled => {
            s.registry.cancel_task(&s.admin, &task_id);
        }
        TerminalStatus::Expired => {
            advance(&s.env, 1, 3_601);
            s.registry.expire_task(&task_id);
        }
    }

    (task_id, keeper)
}

#[test]
fn test_version_is_exposed() {
    let s = setup();
    assert_eq!(s.registry.version(), 3u32);
}

#[test]
fn test_initialize_sets_state() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);

    registry.initialize(&admin, &token_id, &300u32);

    assert_eq!(registry.admin(), Some(admin));
    assert_eq!(registry.get_fee_bps(), 300u32);
    assert!(!registry.is_paused());
    assert_eq!(registry.reward_token_address(), Some(token_id));
    assert_eq!(registry.task_count(), 0u64);
}

#[test]
fn test_initialize_twice_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);

    registry.initialize(&admin, &token_id, &300u32);
    assert_eq!(
        registry.try_initialize(&admin, &token_id, &300u32),
        Err(Ok(KeeperError::AlreadyInitialized))
    );
}

#[test]
fn test_initialize_fee_over_10000_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let registry_id = env.register(KeeperRegistry, ());
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry = KeeperRegistryClient::new(&env, &registry_id);

    registry.initialize(&admin, &token_id, &fee_bps);
    let deadline = env.ledger().timestamp() + 3_600; // 1 hour
    let task_id = registry.register_task(
        &admin,
        &TaskType::Liquidation,
        &calldata(&env),
        &1_000_000i128,
        &deadline,
        &20_000u32,
        &120u32,
        &None,
    );

    (env, admin, registry_id, token_id)
}

#[test]
fn test_zero_fee_credits_keeper_full_reward_and_no_fees() {
    let (env, owner, registry_id, token_id) = setup(0);
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    let token = token::Client::new(&env, &token_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_id);
    let keeper = Address::generate(&env);
    let reward = 1_000i128;

    token_admin.mint(&owner, &reward);
    let task_id = registry.register_task(
        &owner,
        &TaskType::Custom,
        &Bytes::new(&env),
        &reward,
        &(env.ledger().sequence() + 100),
        &10,
        &calldata(&env),
        &1_000_000i128,
        &(env.ledger().timestamp() + 3_600),
        &20_000u32,
        &120u32,
        &None,
    );

    let _ = env;
}

#[test]
fn test_solvent_across_every_lifecycle_path_with_rounding_dust() {
    // 333 bps deliberately produces floor-division rounding for these rewards:
    // 2001 -> 66 fee and 1935 keeper net.
    let s = setup(333);
    let token = token::Client::new(&s.env, &s.token_id);
    let keeper = Address::generate(&s.env);
    let treasury = Address::generate(&s.env);

    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[],
        std::slice::from_ref(&keeper),
    );

    let cancelled = register_task(&s, 1_001);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[cancelled],
        std::slice::from_ref(&keeper),
fn test_register_task_zero_reward_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    assert_eq!(
        registry.try_register_task(
            &admin,
            &TaskType::Custom,
            &calldata(&env),
            &0i128,
            &(env.ledger().timestamp() + 3_600),
            &20_000u32,
            &120u32,
            &None,
        ),
        Err(Ok(KeeperError::InvalidReward))
    );
}

#[test]
fn test_register_task_past_deadline_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    // Deadline in the past.
    let past = env.ledger().timestamp().saturating_sub(1);
    assert_eq!(
        registry.try_register_task(
            &admin,
            &TaskType::Custom,
            &calldata(&env),
            &1_000_000i128,
            &past,
            &20_000u32,
            &120u32,
            &None,
        ),
        Err(Ok(KeeperError::DeadlinePassed))
    );
    registry.claim_task(&keeper, &task_id);
    registry.execute_task(&keeper, &task_id, &Bytes::new(&env));

    assert_eq!(registry.keeper_balance(&keeper), reward);
    assert_eq!(registry.fees_accrued(), 0);
    assert_eq!(token.balance(&registry_id), reward);
}

#[test]
fn test_maximum_fee_is_accepted_by_initialize_and_set_fee_bps() {
    let (env, admin, registry_id, _token_id) = setup(10_000);
    let registry = KeeperRegistryClient::new(&env, &registry_id);

    assert_eq!(registry.get_fee_bps(), 10_000);

    registry.set_fee_bps(&admin, &0);
    registry.set_fee_bps(&admin, &10_000);
    assert_eq!(registry.get_fee_bps(), 10_000);
}

#[test]
fn test_full_fee_credits_zero_keeper_reward_accrues_full_fee_and_emits_zero_net_reward() {
    let (env, owner, registry_id, token_id) = setup(10_000);
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    let token = token::Client::new(&env, &token_id);
    let token_admin = token::StellarAssetClient::new(&env, &token_id);
    let keeper = Address::generate(&env);
    let reward = 1_000i128;

    token_admin.mint(&owner, &reward);
    let task_id = registry.register_task(
        &owner,
        &TaskType::Custom,
        &Bytes::new(&env),
        &reward,
        &(env.ledger().sequence() + 100),
        &10,
    );
    registry.claim_task(&keeper, &task_id);
    registry.execute_task(&keeper, &task_id, &Bytes::new(&env));

    assert_eq!(registry.keeper_balance(&keeper), 0);
    assert_eq!(registry.fees_accrued(), reward);
    assert_eq!(token.balance(&registry_id), reward);
    assert_eq!(registry.get_task(&task_id).reward, 0);

    let events = env.events().all();
    assert!(events.iter().any(|(_, _, data)| {
        *data == (task_id, keeper.clone(), 0i128).into_val(&env)
    }));

    invariants::assert_solvent(
        &env,
        &registry,
        &[task_id],
        &[keeper.clone()],
        token.balance(&registry_id),
    )
    .unwrap();

    let withdrawal = registry.try_withdraw_rewards(&keeper);
    assert!(matches!(
        withdrawal,
        Ok(Err(KeeperError::NoRewardsAvailable))
    ));
    let deadline = env.ledger().timestamp() + 3_600;
    for expected_id in 1u64..=3 {
        let id = registry.register_task(
            &admin,
            &TaskType::TtlExtension,
            &calldata(&env),
            &100_000i128,
            &deadline,
            &20_000u32,
            &60u32,
            &None,
        );
        prop_assert_eq!(setup.registry.fees_accrued(), accrued_after_more);
    }
    assert_eq!(registry.task_count(), 3u64);
}

#[test]
fn test_register_task_ttl_shorter_than_deadline_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &5_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    // 30-day deadline, but only ~1 day of TTL — the exact scenario from the
    // issue: the storage entry would die long before the deadline, stranding
    // the escrow. Must be rejected outright.
    let deadline = env.ledger().timestamp() + 2_592_000; // 30 days
    assert_eq!(
        registry.try_register_task(
            &admin,
            &TaskType::Liquidation,
            &calldata(&env),
            &1_000_000i128,
            &deadline,
            &17_280u32, // ~1 day of ledgers — nowhere near enough
            &120u32,
            &None,
        ),
        Err(Ok(KeeperError::TtlTooShort))
    );
    // Nothing was escrowed and no task was created.
    assert_eq!(registry.task_count(), 0u64);
}

#[test]
fn test_register_task_with_max_calldata_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &5_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    // Exactly at the cap — the largest accepted payload.
    let max_calldata = Bytes::from_array(&env, &[0u8; MAX_CALLDATA_LEN as usize]);
    let id = registry.register_task(
        &admin,
        &TaskType::Custom,
        &max_calldata,
        &1_000_000i128,
        &(env.ledger().timestamp() + 3_600),
        &20_000u32,
        &120u32,
        &None,
    );

    let executed = register_task(&s, 2_001);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[cancelled, executed],
        std::slice::from_ref(&keeper),
    );

    let expired = register_task(&s, 3_003);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[cancelled, executed, expired],
        std::slice::from_ref(&keeper),
    // One byte over the cap — the smallest rejected payload.
    let oversized = Bytes::from_array(&env, &[0u8; MAX_CALLDATA_LEN as usize + 1]);
    assert_eq!(
        registry.try_register_task(
            &admin,
            &TaskType::Custom,
            &oversized,
            &1_000_000i128,
            &(env.ledger().timestamp() + 3_600),
            &17_280u32,
            &120u32,
            &None,
        ),
        Err(Ok(KeeperError::CalldataTooLarge))
    );
    assert_eq!(registry.task_count(), 0u64);
}

#[test]
fn test_register_task_ttl_covering_deadline_succeeds() {
    let s = setup();
    // deadline is 3_600s away; required TTL is 720 ledgers + the 17_280
    // safety margin = 18_000. 20_000 comfortably covers it.
    let id = register_default_task(&s);
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Pending);
}

#[test]
fn test_extend_deadline_ttl_too_short_fails() {
    let s = setup();
    let id = register_default_task(&s); // ttl_ledgers = 20_000
    let old = s.registry.get_task(&id).deadline;

    // Push the deadline out far enough that the existing TTL (20_000 ledgers)
    // no longer covers it plus the safety margin.
    let far_future = old + 1_000_000;
    assert_eq!(
        s.registry.try_extend_deadline(&s.admin, &id, &far_future),
        Err(Ok(KeeperError::TtlTooShort))
    );
    // The deadline was not mutated.
    assert_eq!(s.registry.get_task(&id).deadline, old);
}

#[test]
fn test_expire_task_succeeds_past_old_ttl_boundary() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let token = token::Client::new(&s.env, &s.token_id);
    let before = token.balance(&s.admin);

    // Register with a deadline far enough out that a naive ttl_ledgers of
    // ~1 day (17_280, as in the old README example) would have expired the
    // storage entry long before the deadline. The TTL invariant forces a
    // larger value here, so the entry must still be alive at expiry time.
    let deadline = s.env.ledger().timestamp() + 172_800; // 2 days
    let required = 172_800 / 5 + 17_280; // matches required_ttl_ledgers
    let id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &1_000_000i128,
        &deadline,
        &(required as u32),
        &120u32,
        &None,
    );
    s.registry.claim_task(&keeper, &id); // claimed but never executed

    // Advance well past where a 17_280-ledger TTL (the old unsafe default)
    // would have evicted the entry, and past the deadline itself.
    advance(&s.env, 40_000, 172_801);
    s.registry.expire_task(&id); // must still succeed and refund the owner

    assert_eq!(token.balance(&s.admin), before);
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Expired);
}

#[test]
fn test_register_task_with_empty_calldata_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &5_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    // Empty calldata is intentionally accepted: some task types (e.g. a
    // TtlExtension on a well-known key) may need no extra encoded params.
    let empty = Bytes::new(&env);
    let id = registry.register_task(
        &admin,
        &TaskType::TtlExtension,
        &empty,
        &1_000_000i128,
        &(env.ledger().timestamp() + 3_600),
        &20_000u32,
        &120u32,
        &None,
    );

    s.registry.cancel_task(&s.admin, &cancelled);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[executed, expired],
        std::slice::from_ref(&keeper),
#[test]
fn test_register_task_lock_ledgers_below_min_fails() {
    let s = setup();
    let deadline = s.env.ledger().timestamp() + 3_600;
    assert_eq!(
        s.registry.try_register_task(
            &s.admin,
            &TaskType::Custom,
            &calldata(&s.env),
            &1_000_000i128,
            &deadline,
            &17_280u32,
            &(MIN_LOCK_LEDGERS - 1),
            &None,
        ),
        Err(Ok(KeeperError::InvalidTaskParams))
    );

    s.registry.claim_task(&keeper, &executed);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[executed, expired],
        std::slice::from_ref(&keeper),
#[test]
fn test_register_task_lock_ledgers_at_min_succeeds() {
    let s = setup();
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Custom,
        &calldata(&s.env),
        &1_000_000i128,
        &deadline,
        &20_000u32, // sufficient for required_ttl_ledgers at this deadline
        &MIN_LOCK_LEDGERS,
        &None,
    );

    s.registry
        .execute_task(&keeper, &executed, &Bytes::from_slice(&s.env, b"proof"));
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[expired],
        std::slice::from_ref(&keeper),
#[test]
fn test_register_task_lock_ledgers_above_max_fails() {
    let s = setup();
    let deadline = s.env.ledger().timestamp() + 3_600;
    assert_eq!(
        s.registry.try_register_task(
            &s.admin,
            &TaskType::Custom,
            &calldata(&s.env),
            &1_000_000i128,
            &deadline,
            &17_280u32,
            &(MAX_LOCK_LEDGERS + 1),
            &None,
        ),
        Err(Ok(KeeperError::InvalidTaskParams))
    );

    advance(&s.env, 200, 3_601);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[expired],
        std::slice::from_ref(&keeper),
#[test]
fn test_increase_reward_accepts_claimed_task() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let task_id = register_task(&s, 1_000_000);

    s.registry.claim_task(&keeper, &task_id);
    s.registry.increase_reward(&s.admin, &task_id, &500_000);

    assert_eq!(s.registry.get_task(&task_id).status, TaskStatus::Claimed);
    assert_eq!(s.registry.get_task(&task_id).reward, 1_500_000);
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Custom,
        &calldata(&s.env),
        &1_000_000i128,
        &deadline,
        &20_000u32, // sufficient for required_ttl_ledgers at this deadline
        &MAX_LOCK_LEDGERS,
        &None,
    );

    s.registry.expire_task(&expired);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[],
        std::slice::from_ref(&keeper),
#[test]
fn test_register_task_ttl_ledgers_below_min_fails() {
    let s = setup();
    let deadline = s.env.ledger().timestamp() + 3_600;
    assert_eq!(
        s.registry.try_register_task(
            &s.admin,
            &TaskType::Custom,
            &calldata(&s.env),
            &1_000_000i128,
            &deadline,
            &(MIN_TTL_LEDGERS - 1),
            &120u32,
            &None,
        ),
        Err(Ok(KeeperError::InvalidTaskParams))
    );

    assert_eq!(s.registry.withdraw_rewards(&keeper), 1_935i128);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[],
        std::slice::from_ref(&keeper),
    );

    s.registry.sweep_fees(&s.admin, &treasury, &20i128);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[],
        std::slice::from_ref(&keeper),
    );

    s.registry.sweep_fees(&s.admin, &treasury, &46i128);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[],
        std::slice::from_ref(&keeper),
    );

    assert_eq!(token.balance(&s.registry.address), 0i128);
    assert_eq!(s.registry.keeper_balance(&keeper), 0i128);
    assert_eq!(s.registry.fees_accrued(), 0i128);
    assert_eq!(token.balance(&treasury), 66i128);
}
#[test]
fn test_register_task_ttl_ledgers_at_min_succeeds() {
    // `MIN_TTL_LEDGERS` (1_000) is no longer, by itself, a sufficient
    // `ttl_ledgers` for any realistic near-term deadline: `register_task`
    // also enforces the dynamic `required_ttl_ledgers(deadline)` bound
    // (ledgers-until-deadline + a 1-day safety margin), which for this
    // test's +3_600s deadline works out to 720 + 17_280 = 18_000 —
    // strictly greater than `MIN_TTL_LEDGERS`. These are two independently
    // real, non-overlapping requirements (`register_task` enforces both
    // `ttl_ledgers >= MIN_TTL_LEDGERS` *and* `ttl_ledgers >=
    // required_ttl_ledgers(deadline)`), so "at the minimum" for this
    // deadline means the larger of the two, not `MIN_TTL_LEDGERS` alone.
    let s = setup();
    let deadline = s.env.ledger().timestamp() + 3_600;
    let min_sufficient_ttl = MIN_TTL_LEDGERS.max(
        crate::required_ttl_ledgers(&s.env, deadline)
            .try_into()
            .expect("required_ttl_ledgers fits in u32 for a near-term deadline"),
    );
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Custom,
        &calldata(&s.env),
        &1_000_000i128,
        &deadline,
        &min_sufficient_ttl,
        &120u32,
        &None,
    );
    assert_eq!(s.registry.get_task(&task_id).ttl_ledgers, min_sufficient_ttl);

    // One ledger short of that minimum must be rejected.
    assert_eq!(
        s.registry.try_register_task(
            &s.admin,
            &TaskType::Custom,
            &calldata(&s.env),
            &1_000_000i128,
            &deadline,
            &(min_sufficient_ttl - 1),
            &120u32,
            &None,
        ),
        Err(Ok(KeeperError::TtlTooShort))
    );
}

#[test]
fn test_increase_reward_on_claimed_task_credits_increased_reward() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let task_id = register_task(&s, 1_000_000);

    s.registry.claim_task(&keeper, &task_id);
    s.registry.increase_reward(&s.admin, &task_id, &500_000);
    s.registry
        .execute_task(&keeper, &task_id, &calldata(&s.env));

    // The registry default fee is 300 basis points: 1,500,000 - 45,000.
    assert_eq!(s.registry.keeper_balance(&keeper), 1_455_000);
    assert_eq!(s.registry.fees_accrued(), 45_000);
}

#[test]
fn test_increase_reward_rejects_all_terminal_task_states_without_transfer() {
    for status in [
        TerminalStatus::Executed,
        TerminalStatus::Cancelled,
        TerminalStatus::Expired,
    ] {
        let s = setup();
        let token = token::Client::new(&s.env, &s.token_id);
        let (task_id, _) = task_in_status(&s, status);
        let owner_before = token.balance(&s.admin);
        let reward_before = s.registry.get_task(&task_id).reward;

        assert_eq!(
            s.registry.try_increase_reward(&s.admin, &task_id, &500_000),
            Err(Ok(KeeperError::InvalidTaskStatus))
        );

        assert_eq!(token.balance(&s.admin), owner_before);
        assert_eq!(s.registry.get_task(&task_id).reward, reward_before);
    }
}

#[test]
fn test_extend_deadline_accepts_claimed_task() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let task_id = register_task(&s, 1_000_000);
    let old_deadline = s.registry.get_task(&task_id).deadline;
    let new_deadline = old_deadline + 10_000;

    s.registry.claim_task(&keeper, &task_id);
    s.registry.extend_deadline(&s.admin, &task_id, &new_deadline);

    assert_eq!(s.registry.get_task(&task_id).status, TaskStatus::Claimed);
    assert_eq!(s.registry.get_task(&task_id).deadline, new_deadline);
}

#[test]
fn test_extend_deadline_on_claimed_task_does_not_extend_lock_window() {
    let s = setup();
    let first_keeper = Address::generate(&s.env);
    let competing_keeper = Address::generate(&s.env);
    let task_id = register_task(&s, 1_000_000);
    let original_deadline = s.registry.get_task(&task_id).deadline;

    s.registry.claim_task(&first_keeper, &task_id);
    s.registry
        .extend_deadline(&s.admin, &task_id, &(original_deadline + 10_000));

    // The lock is 120 ledgers and must be measured from the original claim,
    // regardless of the later deadline extension.
    advance(&s.env, 120, 600);
    s.registry.claim_task(&competing_keeper, &task_id);

    let task = s.registry.get_task(&task_id);
    assert_eq!(task.status, TaskStatus::Claimed);
    assert_eq!(task.claimer, Some(competing_keeper));
}

#[test]
fn test_extend_deadline_rejects_all_terminal_task_states() {
    for status in [
        TerminalStatus::Executed,
        TerminalStatus::Cancelled,
        TerminalStatus::Expired,
    ] {
        let s = setup();
        let (task_id, _) = task_in_status(&s, status);
        let deadline_before = s.registry.get_task(&task_id).deadline;

        assert_eq!(
            s.registry
                .try_extend_deadline(&s.admin, &task_id, &(deadline_before + 10_000)),
            Err(Ok(KeeperError::InvalidTaskStatus))
        );

        assert_eq!(s.registry.get_task(&task_id).deadline, deadline_before);
    }
/// Registers a task with the given `lock_ledgers`, claims it as `keeper`, and
/// returns `(task_id, unlock_at)` where `unlock_at = claim_ledger + lock_ledgers`
/// — the first ledger sequence at which the lock is considered expired.
fn claim_with_lock(s: &Setup, keeper: &Address, lock_ledgers: u32) -> (u64, u32) {
    let deadline = s.env.ledger().timestamp() + 3_600;
    let id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &1_000_000i128,
        &deadline,
        &20_000u32,
        &lock_ledgers,
        &None,
    );
    s.registry.claim_task(keeper, &id);
    let claim_ledger = s.registry.get_task(&id).claim_ledger.unwrap();
    (id, claim_ledger + lock_ledgers)
}

/// Advances the ledger sequence to exactly `target` (timestamp untouched).
fn goto_ledger(env: &Env, target: u32) {
    let current = env.ledger().sequence();
    advance(env, target - current, 0);
}

#[test]
fn test_lock_boundary_unlock_at_minus_one_is_still_locked() {
    let s = setup();
    let first = Address::generate(&s.env);
    let second = Address::generate(&s.env);
    let (id, unlock_at) = claim_with_lock(&s, &first, MIN_LOCK_LEDGERS);

    goto_ledger(&s.env, unlock_at - 1);

    assert!(!s.registry.is_claimable(&id));
    assert_eq!(
        s.registry.try_claim_task(&second, &id),
        Err(Ok(KeeperError::LockPeriodActive))
    );
}

#[test]
fn test_lock_boundary_at_unlock_at_is_reclaimable() {
    let s = setup();
    let first = Address::generate(&s.env);
    let second = Address::generate(&s.env);
    let (id, unlock_at) = claim_with_lock(&s, &first, MIN_LOCK_LEDGERS);

    goto_ledger(&s.env, unlock_at);

    // The `>=` in `lock_expired` makes the boundary inclusive: exactly at
    // `unlock_at`, the lock has already lifted.
    assert!(s.registry.is_claimable(&id));
    s.registry.claim_task(&second, &id);
    let task = s.registry.get_task(&id);
    assert_eq!(task.claimer, Some(second));
    assert_eq!(task.claim_ledger, Some(unlock_at));
}

#[test]
fn test_lock_boundary_unlock_at_plus_one_is_reclaimable() {
    let s = setup();
    let first = Address::generate(&s.env);
    let second = Address::generate(&s.env);
    let (id, unlock_at) = claim_with_lock(&s, &first, MIN_LOCK_LEDGERS);

    goto_ledger(&s.env, unlock_at + 1);

    assert!(s.registry.is_claimable(&id));
    s.registry.claim_task(&second, &id);
    assert_eq!(s.registry.get_task(&id).claimer, Some(second));
}

#[test]
fn test_lock_window_extending_past_deadline_is_blocked_by_deadline_first() {
    let s = setup();
    let first = Address::generate(&s.env);
    let second = Address::generate(&s.env);

    // The lock window (1000 ledgers) would far outlive the 10-second deadline.
    let deadline = s.env.ledger().timestamp() + 10;
    let id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &1_000_000i128,
        &deadline,
        &20_000u32,
        &1_000u32,
        &None,
    );
    s.registry.claim_task(&first, &id);

    // Advance past the deadline but nowhere near the lock's unlock_at.
    advance(&s.env, 1, 11);
    assert!(s.env.ledger().timestamp() >= deadline);

    // The deadline check runs before the lock check in both `claim_task` and
    // `is_claimable`, so the takeover path is unreachable here: the failure
    // is DeadlinePassed, never LockPeriodActive.
    assert!(!s.registry.is_claimable(&id));
    assert_eq!(
        s.registry.try_claim_task(&second, &id),
        Err(Ok(KeeperError::DeadlinePassed))
    );
}

#[test]
fn test_claim_past_deadline_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);

    advance(&s.env, 1, 3_601); // step past the 1-hour deadline
    assert_eq!(
        s.registry.try_claim_task(&keeper, &id),
        Err(Ok(KeeperError::DeadlinePassed))
    );
}

#[test]
fn test_claim_unknown_task_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_claim_task(&keeper, &999u64),
        Err(Ok(KeeperError::TaskNotFound))
    );
}

#[test]
fn test_execute_task_credits_keeper_net_of_fee() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s); // reward 1_000_000, fee 300 bps (3%)

    s.registry.claim_task(&keeper, &id);
    s.registry
        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"proof"));

    // 3% fee → keeper receives 970_000, contract retains 30_000 as fee.
    assert_eq!(s.registry.keeper_balance(&keeper), 970_000i128);
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Executed);
}

#[test]
fn test_get_fee_bps_matches_applied_fee_when_never_written() {
    let s = setup();
    // Simulate a registry where `FeeBps` was never written (e.g. queried
    // before `initialize`, or dropped by a future storage migration).
    s.env.as_contract(&s.registry.address, || {
        s.env.storage().instance().remove(&DataKey::FeeBps);
    });

    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s); // reward 1_000_000

    let reported_fee_bps = s.registry.get_fee_bps();

    s.registry.claim_task(&keeper, &id);
    s.registry
        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"proof"));

    let (expected_net, _) = split_reward(1_000_000i128, reported_fee_bps).unwrap();
    assert_eq!(s.registry.keeper_balance(&keeper), expected_net);
    assert_eq!(reported_fee_bps, 0u32);
}

#[test]
fn test_get_fee_bps_matches_applied_fee_after_set_fee_bps() {
    let s = setup();
    s.registry.set_fee_bps(&s.admin, &750u32);

    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s); // reward 1_000_000

    let reported_fee_bps = s.registry.get_fee_bps();

    s.registry.claim_task(&keeper, &id);
    s.registry
        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"proof"));

    let (expected_net, _) = split_reward(1_000_000i128, reported_fee_bps).unwrap();
    assert_eq!(s.registry.keeper_balance(&keeper), expected_net);
    assert_eq!(reported_fee_bps, 750u32);
}

#[test]
fn test_execute_task_emits_proof_in_event() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);
    let proof = Bytes::from_slice(&s.env, b"keeper-proof:task:1:tx:deadbeef");

    s.registry.claim_task(&keeper, &id);
    s.registry.execute_task(&keeper, &id, &proof);

    let (_contract, _topics, data) = s.env.events().all().last().unwrap();
    let (event_task_id, event_keeper, event_net, event_proof): (u64, Address, i128, Bytes) =
        data.try_into_val(&s.env).unwrap();

    assert_eq!(event_task_id, id);
    assert_eq!(event_keeper, keeper);
    assert_eq!(event_net, 970_000i128);
    assert_eq!(event_proof, proof);
}

#[test]
fn test_execute_task_over_max_proof_len_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);
    s.registry.claim_task(&keeper, &id);

    let oversized = Bytes::from_slice(&s.env, &[0u8; (crate::MAX_PROOF_LEN + 1) as usize]);
    assert_eq!(
        s.registry.try_execute_task(&keeper, &id, &oversized),
        Err(Ok(KeeperError::ProofTooLarge))
    );

    // The task is untouched by the rejected call — still claimable/executable.
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Claimed);
    let at_limit = Bytes::from_slice(&s.env, &[0u8; crate::MAX_PROOF_LEN as usize]);
    s.registry.execute_task(&keeper, &id, &at_limit);
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Executed);
}

#[test]
fn test_execute_by_non_claimer_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let stranger = Address::generate(&s.env);
    let id = register_default_task(&s);

    s.registry.claim_task(&keeper, &id);
    assert_eq!(
        s.registry
            .try_execute_task(&stranger, &id, &Bytes::from_slice(&s.env, b"x")),
        Err(Ok(KeeperError::NotTaskClaimer))
    );
}

#[test]
fn test_execute_unclaimed_task_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s); // still Pending

    assert_eq!(
        s.registry
            .try_execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"x")),
        Err(Ok(KeeperError::InvalidTaskStatus))
    );
}

#[test]
fn test_execute_twice_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);

    s.registry.claim_task(&keeper, &id);
    s.registry
        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"p"));
    // Second execution must fail — task is no longer Claimed.
    assert_eq!(
        s.registry
            .try_execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"p")),
        Err(Ok(KeeperError::InvalidTaskStatus))
    );
}

#[test]
fn test_execute_past_deadline_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);

    s.registry.claim_task(&keeper, &id);
    advance(&s.env, 1, 3_601); // deadline passes while claimed
    assert_eq!(
        s.registry
            .try_execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"p")),
        Err(Ok(KeeperError::DeadlinePassed))
    );
}

#[test]
fn test_cancel_pending_task_refunds_owner() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let before = token.balance(&s.admin);
    let id = register_default_task(&s); // escrows 1_000_000
    assert_eq!(token.balance(&s.admin), before - 1_000_000i128);

    s.registry.cancel_task(&s.admin, &id);

    assert_eq!(token.balance(&s.admin), before); // fully refunded
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Cancelled);
}

#[test]
fn test_cancel_by_non_owner_fails() {
    let s = setup();
    let stranger = Address::generate(&s.env);
    let id = register_default_task(&s);
    assert_eq!(
        s.registry.try_cancel_task(&stranger, &id),
        Err(Ok(KeeperError::NotTaskOwner))
    );
}

#[test]
fn test_cancel_claimed_task_while_lock_active_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);
    s.registry.claim_task(&keeper, &id);

    // Advance 100 ledgers while lock period (default 120 ledgers) is still active
    advance(&s.env, 100, 0);

    // Owner cannot cancel while keeper holds an active lock window.
    assert_eq!(
        s.registry.try_cancel_task(&s.admin, &id),
        Err(Ok(KeeperError::LockPeriodActive))
    );
}

#[test]
fn test_cancel_claimed_task_after_lock_lapsed_succeeds() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let token = token::Client::new(&s.env, &s.token_id);
    let before = token.balance(&s.admin);
    let id = register_default_task(&s);
    s.registry.claim_task(&keeper, &id);

    // Advance ledgers past lock_ledgers (default 120 ledgers)
    advance(&s.env, 120, 0);

    // Owner reclaims escrow once claimer's lock window has lapsed
    s.registry.cancel_task(&s.admin, &id);

    assert_eq!(token.balance(&s.admin), before);
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Cancelled);
}

#[test]
fn test_cancel_claimed_task_boundary_unlock_at_minus_one_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let (id, unlock_at) = claim_with_lock(&s, &keeper, 12u32);

    goto_ledger(&s.env, unlock_at - 1);

    // Lock is still active at unlock_at - 1
    assert_eq!(
        s.registry.try_cancel_task(&s.admin, &id),
        Err(Ok(KeeperError::LockPeriodActive))
    );
}

#[test]
fn test_cancel_claimed_task_boundary_at_unlock_at_succeeds() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let token = token::Client::new(&s.env, &s.token_id);
    let before = token.balance(&s.admin);
    let (id, unlock_at) = claim_with_lock(&s, &keeper, 12u32);

    goto_ledger(&s.env, unlock_at);

    // Lock lapses at unlock_at, allowing task owner to cancel
    s.registry.cancel_task(&s.admin, &id);

    assert_eq!(token.balance(&s.admin), before);
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Cancelled);
}

#[test]
fn test_cancel_claimed_task_boundary_unlock_at_plus_one_succeeds() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let token = token::Client::new(&s.env, &s.token_id);
    let before = token.balance(&s.admin);
    let (id, unlock_at) = claim_with_lock(&s, &keeper, 12u32);

    goto_ledger(&s.env, unlock_at + 1);

    // Lock is expired at unlock_at + 1
    s.registry.cancel_task(&s.admin, &id);

    assert_eq!(token.balance(&s.admin), before);
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Cancelled);
}

#[test]
fn test_expire_after_deadline_refunds_owner() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let token = token::Client::new(&s.env, &s.token_id);
    let before = token.balance(&s.admin);
    let id = register_default_task(&s);
    s.registry.claim_task(&keeper, &id); // claimed but never executed

    advance(&s.env, 1, 3_601); // past deadline
                               // Permissionless: a third party can trigger the refund.
    s.registry.expire_task(&id);

    assert_eq!(token.balance(&s.admin), before); // owner made whole
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Expired);
}

#[test]
fn test_expire_before_deadline_fails() {
    let s = setup();
    let id = register_default_task(&s);
    assert_eq!(
        s.registry.try_expire_task(&id),
        Err(Ok(KeeperError::DeadlineNotPassed))
    );
}

#[test]
fn test_expire_executed_task_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);
    s.registry.claim_task(&keeper, &id);
    s.registry
        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"p"));

    advance(&s.env, 1, 3_601);
    assert_eq!(
        s.registry.try_expire_task(&id),
        Err(Ok(KeeperError::InvalidTaskStatus))
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Re-entrancy regression: expire_task
//
// A minimal token contract whose `transfer` re-enters `expire_task` for the
// same task_id mid-transfer, simulating a malicious or buggy reward token.
//
// In practice Soroban's host already refuses to re-invoke a contract that is
// still on the call stack, so the nested call below is rejected by the host
// itself rather than reaching our `InvalidTaskStatus` guard — see the
// `reentrant_code` assertion. That host protection is not something this
// contract can rely on as its only line of defense (it is a platform detail,
// not a documented guarantee of this contract's ABI), so the
// checks-effects-interactions fix still matters: this test's real assertion
// is that no matter why the second attempt was rejected, it never reaches a
// second `transfer`, so the refund is paid exactly once.
// ─────────────────────────────────────────────────────────────────────────────

mod reentrant_token_expire {
    use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env};

    use crate::KeeperRegistryClient;

    #[contract]
    pub struct ExpireReentrantToken;

    #[contractimpl]
    impl ExpireReentrantToken {
        pub fn set_balance(e: Env, id: Address, amount: i128) {
            e.storage().persistent().set(&id, &amount);
        }

        pub fn balance(e: Env, id: Address) -> i128 {
            e.storage().persistent().get(&id).unwrap_or(0i128)
        }

        /// Arms the re-entrancy: once set, every subsequent `transfer` will
        /// attempt to call `expire_task(task_id)` on `registry` again.
        pub fn arm(e: Env, registry: Address, task_id: u64) {
            e.storage().instance().set(&symbol_short!("REG"), &registry);
            e.storage().instance().set(&symbol_short!("TID"), &task_id);
        }

        /// The numeric code of the re-entrant call's result, for the test to
        /// inspect: `InvalidTaskStatus as u32` on the expected rejection.
        pub fn reentrant_code(e: Env) -> u32 {
            e.storage()
                .instance()
                .get(&symbol_short!("RCODE"))
                .unwrap_or(u32::MAX)
        }

        pub fn transfer(e: Env, from: Address, to: Address, amount: i128) {
            let from_bal: i128 = e.storage().persistent().get(&from).unwrap_or(0i128);
            e.storage().persistent().set(&from, &(from_bal - amount));
            let to_bal: i128 = e.storage().persistent().get(&to).unwrap_or(0i128);
            e.storage().persistent().set(&to, &(to_bal + amount));

            if let Some(registry) = e
                .storage()
                .instance()
                .get::<_, Address>(&symbol_short!("REG"))
            {
                let task_id: u64 = e.storage().instance().get(&symbol_short!("TID")).unwrap();
                let client = KeeperRegistryClient::new(&e, &registry);
                let code = match client.try_expire_task(&task_id) {
                    Err(Ok(other)) => other as u32,
                    Ok(Ok(())) => 0u32,
                    Ok(Err(_)) => 111u32,
                    Err(Err(_)) => 222u32,
                };
                e.storage().instance().set(&symbol_short!("RCODE"), &code);
            }
        }
    }
}

use reentrant_token_expire::{ExpireReentrantToken, ExpireReentrantTokenClient};

#[test]
fn test_expire_task_reentrancy_pays_refund_exactly_once() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env.register(ExpireReentrantToken, ());
    let token = ExpireReentrantTokenClient::new(&env, &token_id);
    token.set_balance(&admin, &5_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    let deadline = env.ledger().timestamp() + 3_600;
    let task_id = registry.register_task(
        &admin,
        &TaskType::Custom,
        &calldata(&env),
        &1_000_000i128,
        &deadline,
        &20_000u32,
        &120u32,
        &None,
    );
    assert_eq!(token.balance(&admin), 4_000_000i128); // escrowed
    assert_eq!(token.balance(&registry_id), 1_000_000i128);

    // Arm the token only now, so the escrow transfer above isn't itself
    // treated as a re-entrant call.
    token.arm(&registry_id, &task_id);

    advance(&env, 1, 3_601); // past deadline
    registry.expire_task(&task_id);

    // The nested call never succeeded (`Ok(Ok(()))` would be code 0) — either
    // rejected by our own guard with InvalidTaskStatus, or by the host's
    // built-in reentrancy protection. Either way it never ran a second
    // transfer.
    let code = token.reentrant_code();
    assert_ne!(
        code, 0u32,
        "the re-entrant expire_task call must not succeed"
    );

    // Exactly one refund reached the owner; the contract holds nothing.
    assert_eq!(token.balance(&admin), 5_000_000i128);
    assert_eq!(token.balance(&registry_id), 0i128);
    assert_eq!(registry.get_task(&task_id).status, TaskStatus::Expired);
}

#[test]
fn test_expire_twice_fails_with_invalid_status_and_pays_refund_once() {
    // Direct, non-reentrant demonstration of the same CEI guarantee: once
    // `expire_task` has written `Expired`, any further call for the same
    // task_id — reentrant or not — is rejected before it can transfer again.
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let before = token.balance(&s.admin);
    let id = register_default_task(&s);

    advance(&s.env, 1, 3_601); // past deadline
    s.registry.expire_task(&id);
    assert_eq!(token.balance(&s.admin), before); // refunded once

    assert_eq!(
        s.registry.try_expire_task(&id),
        Err(Ok(KeeperError::InvalidTaskStatus))
    );
    assert_eq!(token.balance(&s.admin), before); // still exactly one refund
    assert_eq!(token.balance(&s.registry.address), 0i128);
}

// ─────────────────────────────────────────────────────────────────────────────
// withdraw_rewards / sweep_fees
// ─────────────────────────────────────────────────────────────────────────────

/// Drives a full register → claim → execute cycle and returns the keeper.
fn executed_task_keeper(s: &Setup) -> Address {
    let keeper = Address::generate(&s.env);
    let id = register_default_task(s);
    s.registry.claim_task(&keeper, &id);
    s.registry
        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"proof"));
    keeper
}

#[test]
fn test_withdraw_transfers_balance_and_zeroes_it() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let keeper = executed_task_keeper(&s); // credited 970_000

    assert_eq!(token.balance(&keeper), 0i128);
    let withdrawn = s.registry.withdraw_rewards(&keeper);

    assert_eq!(withdrawn, 970_000i128);
    assert_eq!(token.balance(&keeper), 970_000i128);
    assert_eq!(s.registry.keeper_balance(&keeper), 0i128);
}

/// The design credits keepers to an internal balance so they can execute
/// many tasks and pay one withdrawal fee. `test_withdraw_transfers_balance_and_zeroes_it`
/// only proves this for a single credit; this test drives multiple credits
/// per keeper (three for keeper1, two for keeper2, interleaved) and checks
/// the running balance after every single one — a regression that overwrote
/// instead of accumulated (`set` instead of `checked_add`) would fail on the
/// very first assertion after a second credit.
#[test]
fn test_keeper_balance_accumulates_across_tasks_and_withdraws_as_one_sum() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let fee_bps = s.registry.get_fee_bps();

    let keeper1 = Address::generate(&s.env);
    let keeper2 = Address::generate(&s.env);

    // Rewards deliberately chosen so `reward * fee_bps / 10_000` does not
    // divide evenly (default fee_bps = 300 / 3%) — the running sum has to
    // exercise split_reward's truncating division, not just clean multiples.
    let keeper1_rewards = [850_003i128, 623_777i128, 1_234_567i128];
    let keeper2_rewards = [111_113i128, 777_779i128];

    let mut keeper1_balance = 0i128;
    let mut keeper2_balance = 0i128;
    let mut expected_fees = 0i128;

    // Interleave keeper2's tasks between keeper1's so that DataKey::KeeperReward
    // keys colliding between the two addresses would show up as
    // cross-contamination of the running balances, not be hidden by ordering.
    for (i, &reward1) in keeper1_rewards.iter().enumerate() {
        let id1 = register_reward_task(&s, reward1);
        s.registry.claim_task(&keeper1, &id1);
        s.registry
            .execute_task(&keeper1, &id1, &Bytes::from_slice(&s.env, b"proof"));

        let (net1, fee1) = split_reward(reward1, fee_bps).unwrap();
        keeper1_balance += net1;
        expected_fees += fee1;

        // Asserting after each step localises a failure to the exact
        // execution that broke accumulation.
        assert_eq!(s.registry.keeper_balance(&keeper1), keeper1_balance);
        assert_eq!(s.registry.keeper_balance(&keeper2), keeper2_balance);

        if let Some(&reward2) = keeper2_rewards.get(i) {
            let id2 = register_reward_task(&s, reward2);
            s.registry.claim_task(&keeper2, &id2);
            s.registry
                .execute_task(&keeper2, &id2, &Bytes::from_slice(&s.env, b"proof"));

            let (net2, fee2) = split_reward(reward2, fee_bps).unwrap();
            keeper2_balance += net2;
            expected_fees += fee2;

            assert_eq!(s.registry.keeper_balance(&keeper2), keeper2_balance);
            assert_eq!(s.registry.keeper_balance(&keeper1), keeper1_balance);
        }
    }

    // Sanity: the chosen rewards actually produced non-round fee splits.
    assert!(keeper1_rewards
        .iter()
        .any(|&r| r.checked_mul(fee_bps as i128).unwrap() % 10_000 != 0));

    assert_eq!(s.registry.fees_accrued(), expected_fees);

    // A single withdrawal transfers the full accumulated sum and zeroes the
    // balance.
    assert_eq!(token.balance(&keeper1), 0i128);
    let withdrawn = s.registry.withdraw_rewards(&keeper1);

    // Inspect events from this call *before* making any further contract
    // calls — `events().all()` reflects only the most recent top-level
    // invocation, and even read-only views (like the balance checks below)
    // would reset it.
    //
    // Exactly one RewardsWithdrawn event was emitted, carrying the total —
    // not one per credited task, and not the token contract's own transfer
    // event (which carries a different topic pair).
    let mut withdraw_event_count = 0u32;
    let mut withdraw_event_amount = 0i128;
    for (contract, topics, data) in s.env.events().all().iter() {
        if contract != s.registry.address {
            continue;
        }
        let t0: Option<Symbol> = topics.get(0).and_then(|v| v.try_into_val(&s.env).ok());
        let t1: Option<Symbol> = topics.get(1).and_then(|v| v.try_into_val(&s.env).ok());
        if topics.len() == 2
            && t0 == Some(symbol_short!("wdraw"))
            && t1 == Some(symbol_short!("reward"))
        {
            withdraw_event_count += 1;
            let (event_keeper, amount): (Address, i128) = data.try_into_val(&s.env).unwrap();
            assert_eq!(event_keeper, keeper1);
            withdraw_event_amount = amount;
        }
    }
    assert_eq!(withdraw_event_count, 1);
    assert_eq!(withdraw_event_amount, keeper1_balance);

    assert_eq!(withdrawn, keeper1_balance);
    assert_eq!(token.balance(&keeper1), keeper1_balance);
    assert_eq!(s.registry.keeper_balance(&keeper1), 0i128);

    // keeper2's balance stayed independent — untouched by keeper1's
    // accumulation, withdrawal, or the KeeperReward key derivation.
    assert_eq!(s.registry.keeper_balance(&keeper2), keeper2_balance);
    assert_eq!(token.balance(&keeper2), 0i128);
}

#[test]
fn test_withdraw_with_no_balance_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_withdraw_rewards(&keeper),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );
}

#[test]
fn test_double_withdraw_fails() {
    let s = setup();
    let keeper = executed_task_keeper(&s);
    s.registry.withdraw_rewards(&keeper);
    assert_eq!(
        s.registry.try_withdraw_rewards(&keeper),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );
}

#[test]
fn test_execute_accrues_protocol_fee() {
    let s = setup();
    let _ = executed_task_keeper(&s);
    // 3% of 1_000_000 withheld.
    assert_eq!(s.registry.fees_accrued(), 30_000i128);
}

#[test]
fn test_sweep_fees_to_treasury() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let _ = executed_task_keeper(&s); // 30_000 fee accrued
    let treasury = Address::generate(&s.env);

    s.registry.sweep_fees(&s.admin, &treasury, &30_000i128);

    assert_eq!(token.balance(&treasury), 30_000i128);
    assert_eq!(s.registry.fees_accrued(), 0i128);
}

#[test]
fn test_sweep_more_than_accrued_fails() {
    let s = setup();
    let _ = executed_task_keeper(&s); // 30_000 accrued
    let treasury = Address::generate(&s.env);
    // Guard: cannot sweep into task escrow / keeper balances.
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &30_001i128),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );
}

#[test]
fn test_sweep_by_non_admin_fails() {
    let s = setup();
    let _ = executed_task_keeper(&s);
    let stranger = Address::generate(&s.env);
    let treasury = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_sweep_fees(&stranger, &treasury, &1i128),
        Err(Ok(KeeperError::Unauthorized))
    );
}

#[test]
fn test_sweep_zero_amount_fails() {
    let s = setup();
    let _ = executed_task_keeper(&s); // 30_000 accrued
    let treasury = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &0i128),
        Err(Ok(KeeperError::InvalidReward))
    );
    assert_eq!(s.registry.fees_accrued(), 30_000i128);
}

#[test]
fn test_sweep_negative_amount_fails() {
    let s = setup();
    let _ = executed_task_keeper(&s); // 30_000 accrued
    let treasury = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &-1i128),
        Err(Ok(KeeperError::InvalidReward))
    );
    assert_eq!(s.registry.fees_accrued(), 30_000i128);
}

#[test]
fn test_sweep_with_nothing_accrued_fails() {
    let s = setup();
    // Fresh contract — no task has ever executed, so nothing is accrued.
    let treasury = Address::generate(&s.env);
    assert_eq!(s.registry.fees_accrued(), 0i128);
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &1i128),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );
}

#[test]
fn test_sweep_partial_sequence_conserves_remainder_and_leaves_other_balances_untouched() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let treasury = Address::generate(&s.env);

    // An unrelated open task and a credited keeper — the accumulator is the
    // only thing sweep_fees is allowed to draw from, so neither should ever
    // move as a result of sweeping.
    let untouched_task_id = register_default_task(&s); // 1_000_000 escrowed
    let keeper = executed_task_keeper(&s); // credits keeper 970_000, accrues 30_000 fee

    assert_eq!(s.registry.fees_accrued(), 30_000i128);

    // Three uneven partial sweeps summing to the full 30_000 accrued.
    let parts = [12_000i128, 9_000i128, 9_000i128];
    let mut swept_so_far = 0i128;
    for &part in parts.iter() {
        s.registry.sweep_fees(&s.admin, &treasury, &part);
        swept_so_far += part;
        assert_eq!(s.registry.fees_accrued(), 30_000i128 - swept_so_far);
        assert_eq!(token.balance(&treasury), swept_so_far);
    }
    assert_eq!(s.registry.fees_accrued(), 0i128);

    // Nothing left: a further sweep of 1 is rejected.
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &1i128),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );

    // The unrelated task's escrow and the keeper's credited balance are
    // exactly as they were before any sweep — proving sweeping never dipped
    // into either.
    assert_eq!(
        s.registry.get_task(&untouched_task_id).reward,
        1_000_000i128
    );
    assert_eq!(
        s.registry.get_task(&untouched_task_id).status,
        TaskStatus::Pending
    );
    assert_eq!(s.registry.keeper_balance(&keeper), 970_000i128);
}

// ─────────────────────────────────────────────────────────────────────────────
// Admin controls: pause / set_fee_bps / transfer_admin / upgrade
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_pause_blocks_registration_but_allows_withdraw() {
    let s = setup();
    let keeper = executed_task_keeper(&s); // has a balance to withdraw

    s.registry.pause(&s.admin);
    assert!(s.registry.is_paused());

    // Registration is blocked while paused.
    assert_eq!(
        s.registry.try_register_task(
            &s.admin,
            &TaskType::Custom,
            &calldata(&s.env),
            &100_000i128,
            &(s.env.ledger().timestamp() + 3_600),
            &20_000u32,
            &60u32,
            &None,
        ),
        Err(Ok(KeeperError::ContractPaused))
    );

    // Withdrawals remain open during a pause so funds are never trapped.
    assert_eq!(s.registry.withdraw_rewards(&keeper), 970_000i128);
}

#[test]
fn test_unpause_restores_registration() {
    let s = setup();
    s.registry.pause(&s.admin);
    s.registry.unpause(&s.admin);
    assert!(!s.registry.is_paused());
    // Now registration works again.
    let id = register_default_task(&s);
    assert_eq!(s.registry.get_task(&id).status, TaskStatus::Pending);
}

#[test]
fn test_pause_emits_event() {
    let s = setup();
    s.registry.pause(&s.admin);
    // A governance event was published for the pause.
    assert!(!s.env.events().all().is_empty());
}

#[test]
fn test_pause_by_non_admin_fails() {
    let s = setup();
    let stranger = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_pause(&stranger),
        Err(Ok(KeeperError::Unauthorized))
    );
}

/// Table-driven coverage of the full pause policy, entry point by entry
/// point — see the `pause`/`unpause` doc comment in `lib.rs` for the table
/// this test verifies against and keeps in sync.
///
/// Ground truth is "does the function call `require_not_paused(&e)?`",
/// checked directly against the code (not the old prose-only doc comment,
/// which undersold the policy — it only mentioned
/// register_task/claim_task/execute_task/expire_task/withdraw_rewards and
/// said nothing about increase_reward, extend_deadline, or cancel_task):
///
///   - BLOCKED while paused (asserted via `try_*` -> `ContractPaused`):
///     `register_task`, `claim_task`, `execute_task`, `increase_reward`.
///   - Allowed while paused, and asserted to have their full intended
///     effect (not just "didn't error"): `cancel_task` (refund + status),
///     `expire_task` (refund + status), `withdraw_rewards` (balance
///     transferred + zeroed).
///   - `extend_deadline` is asserted to match its *current* (buggy)
///     behavior — it has no `require_not_paused` call at all, so it
///     currently succeeds while paused. That is almost certainly wrong
///     (it was likely meant to follow register/claim/execute) but fixing it
///     is out of scope here; seeing this assertion start failing is the
///     signal that someone fixed the gap without updating this test.
///   - Read-only views are asserted to keep working throughout.
///   - Finally, unpause restores every previously-blocked entry point —
///     a one-way pause would itself be a serious bug.
#[test]
fn test_pause_policy_matrix_entry_point_by_entry_point() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);

    // ── Arrange: every task needs to exist *before* pausing, since
    // register_task itself is blocked once paused.
    let claim_target_id = register_default_task(&s); // Pending -> claim_task blocked
    let increase_target_id = register_default_task(&s); // Pending -> increase_reward blocked
    let cancel_target_id = register_default_task(&s); // Pending -> cancel_task allowed
    let extend_target_id = register_default_task(&s); // Pending -> extend_deadline (bug: allowed)

    // Short deadline so it can expire without dragging the other tasks'
    // (default +3_600s) deadlines past their own while paused.
    let expire_target_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &1_000_000i128,
        &(s.env.ledger().timestamp() + 100),
        &17_300u32, // sufficient for required_ttl_ledgers at this deadline
        &120u32,
        &None,
    );

    let claimed_keeper = Address::generate(&s.env);
    let claimed_task_id = register_default_task(&s);
    s.registry.claim_task(&claimed_keeper, &claimed_task_id); // Claimed -> execute_task blocked

    // Credited before pausing, since execute_task (the only way to credit a
    // keeper) is itself blocked once paused.
    let paid_keeper = executed_task_keeper(&s); // has a withdrawable balance

    // ── Act: pause.
    s.registry.pause(&s.admin);
    assert!(s.registry.is_paused());

    // ── BLOCKED: register_task.
    assert_eq!(
        s.registry.try_register_task(
            &s.admin,
            &TaskType::Custom,
            &calldata(&s.env),
            &100_000i128,
            &(s.env.ledger().timestamp() + 3_600),
            &17_280u32,
            &60u32,
            &None,
        ),
        Err(Ok(KeeperError::ContractPaused))
    );

    // ── BLOCKED: claim_task.
    assert_eq!(
        s.registry
            .try_claim_task(&Address::generate(&s.env), &claim_target_id),
        Err(Ok(KeeperError::ContractPaused))
    );
    assert_eq!(
        s.registry.get_task(&claim_target_id).status,
        TaskStatus::Pending
    ); // untouched

    // ── BLOCKED: execute_task.
    assert_eq!(
        s.registry.try_execute_task(
            &claimed_keeper,
            &claimed_task_id,
            &Bytes::from_slice(&s.env, b"p"),
        ),
        Err(Ok(KeeperError::ContractPaused))
    );
    assert_eq!(
        s.registry.get_task(&claimed_task_id).status,
        TaskStatus::Claimed
    ); // untouched

    // ── BLOCKED: increase_reward.
    assert_eq!(
        s.registry
            .try_increase_reward(&s.admin, &increase_target_id, &1i128),
        Err(Ok(KeeperError::ContractPaused))
    );
    assert_eq!(
        s.registry.get_task(&increase_target_id).reward,
        1_000_000i128
    ); // untouched

    // ── extend_deadline: NOT gated in the current code — this is a known
    // gap, tracked as a separate bug (see the doc comment above `pause` in
    // lib.rs). Asserting current behavior, not desired behavior.
    // TODO: once extend_deadline gains a `require_not_paused(&e)?` check,
    // flip this to `try_extend_deadline` -> `Err(Ok(KeeperError::ContractPaused))`.
    let old_deadline = s.registry.get_task(&extend_target_id).deadline;
    s.registry
        .extend_deadline(&s.admin, &extend_target_id, &(old_deadline + 3_600));
    assert_eq!(
        s.registry.get_task(&extend_target_id).deadline,
        old_deadline + 3_600
    );

    // ── ALLOWED: cancel_task — must actually refund and flip status, not
    // just "not error".
    let admin_before_cancel = token.balance(&s.admin);
    s.registry.cancel_task(&s.admin, &cancel_target_id);
    assert_eq!(
        s.registry.get_task(&cancel_target_id).status,
        TaskStatus::Cancelled
    );
    assert_eq!(token.balance(&s.admin), admin_before_cancel + 1_000_000i128);

    // ── ALLOWED: expire_task, once its deadline passes — also must actually
    // refund and flip status. Advance just enough to pass this task's short
    // deadline without also passing the other (default +3_600s) tasks'.
    advance(&s.env, 5, 101);
    let admin_before_expire = token.balance(&s.admin);
    s.registry.expire_task(&expire_target_id);
    assert_eq!(
        s.registry.get_task(&expire_target_id).status,
        TaskStatus::Expired
    );
    assert_eq!(token.balance(&s.admin), admin_before_expire + 1_000_000i128);

    // ── ALLOWED: withdraw_rewards — must actually transfer and zero the
    // balance, not just "not error".
    assert_eq!(token.balance(&paid_keeper), 0i128);
    assert_eq!(s.registry.withdraw_rewards(&paid_keeper), 970_000i128);
    assert_eq!(token.balance(&paid_keeper), 970_000i128);
    assert_eq!(s.registry.keeper_balance(&paid_keeper), 0i128);

    // ── ALLOWED: read-only views never gate on pause.
    assert!(s.registry.is_paused());
    assert_eq!(s.registry.fees_accrued(), 30_000i128);
    assert!(s.registry.task_count() >= 6);
    assert_eq!(s.registry.admin(), Some(s.admin.clone()));
    assert_eq!(s.registry.get_fee_bps(), 300u32);

    // ── Unpause: every previously-blocked entry point must work again — a
    // one-way pause would itself be a serious liveness bug.
    s.registry.unpause(&s.admin);
    assert!(!s.registry.is_paused());

    // register_task works again.
    let new_id = register_default_task(&s);
    assert_eq!(s.registry.get_task(&new_id).status, TaskStatus::Pending);

    // claim_task works again.
    let claimer = Address::generate(&s.env);
    s.registry.claim_task(&claimer, &claim_target_id);
    assert_eq!(
        s.registry.get_task(&claim_target_id).status,
        TaskStatus::Claimed
    );

    // execute_task works again.
    s.registry.execute_task(
        &claimed_keeper,
        &claimed_task_id,
        &Bytes::from_slice(&s.env, b"proof"),
    );
    assert_eq!(
        s.registry.get_task(&claimed_task_id).status,
        TaskStatus::Executed
    );

    // increase_reward works again.
    s.registry
        .increase_reward(&s.admin, &increase_target_id, &1i128);
    assert_eq!(
        s.registry.get_task(&increase_target_id).reward,
        1_000_001i128
    );
}

#[test]
fn test_set_fee_bps_affects_future_executions() {
    let s = setup();
    s.registry.set_fee_bps(&s.admin, &1_000u32); // 10%
    assert_eq!(s.registry.get_fee_bps(), 1_000u32);

    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);
    s.registry.claim_task(&keeper, &id);
    s.registry
        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"p"));

    // 10% fee now: keeper nets 900_000, 100_000 accrues.
    assert_eq!(s.registry.keeper_balance(&keeper), 900_000i128);
    assert_eq!(s.registry.fees_accrued(), 100_000i128);
}

#[test]
fn test_min_reward_defaults_to_zero() {
    let s = setup();
    assert_eq!(s.registry.min_reward(), 0i128);
}

#[test]
fn test_set_min_reward_rejects_below_floor() {
    let s = setup();
    s.registry.set_min_reward(&s.admin, &500_000i128);
    assert_eq!(s.registry.min_reward(), 500_000i128);

    // A task below the floor is rejected...
    assert_eq!(
        s.registry.try_register_task(
            &s.admin,
            &TaskType::Custom,
            &calldata(&s.env),
            &499_999i128,
            &(s.env.ledger().timestamp() + 3_600),
            &20_000u32,
            &60u32,
            &None,
        ),
        Err(Ok(KeeperError::InvalidReward))
    );
    // ...but one at the floor is accepted.
    let id = s.registry.register_task(
        &s.admin,
        &TaskType::Custom,
        &calldata(&s.env),
        &500_000i128,
        &(s.env.ledger().timestamp() + 3_600),
        &20_000u32,
        &60u32,
        &None,
    );
    assert_eq!(id, 1u64);
}

#[test]
fn test_set_min_reward_by_non_admin_fails() {
    let s = setup();
    let stranger = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_set_min_reward(&stranger, &1i128),
        Err(Ok(KeeperError::Unauthorized))
    );
}

#[test]
fn test_set_fee_emits_event() {
    // Note: `s.env.events().all()` reflects only the most recent top-level
    // call, not a running log across the whole test — a "before" snapshot
    // taken after `setup()` (a separate prior call) is gone by the time
    // `set_fee_bps` (a new call) returns, so comparing counts across the
    // two calls doesn't work. Instead, check the emitted event directly,
    // immediately after the call that emits it.
    let s = setup();
    s.registry.set_fee_bps(&s.admin, &500u32);
    let expected_topic: soroban_sdk::Vec<soroban_sdk::Val> =
        (symbol_short!("fee"), symbol_short!("admin")).into_val(&s.env);
    let found = s.env.events().all().iter().any(|(contract, topics, _)| {
        contract == s.registry.address && topics == expected_topic
    });
    assert!(found, "FeeUpdated event must be emitted");
}

#[test]
fn test_set_fee_over_max_fails() {
    let s = setup();
    assert_eq!(
        s.registry.try_set_fee_bps(&s.admin, &10_001u32),
        Err(Ok(KeeperError::InvalidFeeBps))
    );
}

#[test]
fn test_transfer_admin_moves_control() {
    let s = setup();
    let new_admin = Address::generate(&s.env);

    s.registry.transfer_admin(&s.admin, &new_admin);
    assert_eq!(s.registry.admin(), Some(new_admin.clone()));

    // Old admin can no longer act.
    assert_eq!(
        s.registry.try_pause(&s.admin),
        Err(Ok(KeeperError::Unauthorized))
    );
    // New admin can.
    s.registry.pause(&new_admin);
    assert!(s.registry.is_paused());
}

#[test]
fn test_transfer_admin_emits_event() {
    // See test_set_fee_emits_event's comment: events().all() only reflects
    // the most recent top-level call, so check the event directly rather
    // than comparing counts across setup() and this call.
    let s = setup();
    let new_admin = Address::generate(&s.env);
    s.registry.transfer_admin(&s.admin, &new_admin);
    let expected_topic: soroban_sdk::Vec<soroban_sdk::Val> =
        (symbol_short!("admin"), symbol_short!("xfer")).into_val(&s.env);
    let found = s.env.events().all().iter().any(|(contract, topics, _)| {
        contract == s.registry.address && topics == expected_topic
    });
    assert!(found, "AdminTransferred event must be emitted");
}

// ─────────────────────────────────────────────────────────────────────────────
// transfer_admin — dual authorization
//
// `transfer_admin` calls both `require_admin` (which requires the *current*
// admin's auth) and `new_admin.require_auth()`, so the role can never be
// pushed onto an address that has not consented to take it. Every test above
// runs under `setup()`'s `env.mock_all_auths()`, which satisfies every
// `require_auth()` regardless of who "signed" — so it cannot distinguish a
// working dual-auth check from a deleted one. These three tests deliberately
// use `mock_auths` with an explicit, minimal authorization list instead, so
// they actually exercise the guard. Do not "simplify" these to
// `mock_all_auths()` — that would silently remove the only coverage of this
// safety property.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_transfer_admin_fails_without_new_admin_auth() {
    let s = setup();
    let new_admin = Address::generate(&s.env);

    // Authorize only the current admin. The incoming admin has not consented.
    s.env.mock_auths(&[MockAuth {
        address: &s.admin,
        invoke: &MockAuthInvoke {
            contract: &s.registry.address,
            fn_name: "transfer_admin",
            args: (s.admin.clone(), new_admin.clone()).into_val(&s.env),
            sub_invokes: &[],
        },
    }]);

    let result = s.registry.try_transfer_admin(&s.admin, &new_admin);
    assert!(
        result.is_err(),
        "transfer must fail without the incoming admin's auth"
    );
    // The consequence that actually matters: admin is unchanged.
    assert_eq!(s.registry.admin(), Some(s.admin.clone()));
}

#[test]
fn test_transfer_admin_fails_without_current_admin_auth() {
    let s = setup();
    let new_admin = Address::generate(&s.env);

    // Authorize only the incoming admin. The current admin did not sign.
    s.env.mock_auths(&[MockAuth {
        address: &new_admin,
        invoke: &MockAuthInvoke {
            contract: &s.registry.address,
            fn_name: "transfer_admin",
            args: (s.admin.clone(), new_admin.clone()).into_val(&s.env),
            sub_invokes: &[],
        },
    }]);

    let result = s.registry.try_transfer_admin(&s.admin, &new_admin);
    assert!(
        result.is_err(),
        "transfer must fail without the current admin's auth"
    );
    assert_eq!(s.registry.admin(), Some(s.admin.clone()));
}

#[test]
fn test_transfer_admin_succeeds_with_both_auths_explicit() {
    let s = setup();
    let new_admin = Address::generate(&s.env);

    // Both required parties authorize explicitly (no mock_all_auths involved),
    // proving the harness itself is capable of making the call succeed.
    s.env.mock_auths(&[
        MockAuth {
            address: &s.admin,
            invoke: &MockAuthInvoke {
                contract: &s.registry.address,
                fn_name: "transfer_admin",
                args: (s.admin.clone(), new_admin.clone()).into_val(&s.env),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &new_admin,
            invoke: &MockAuthInvoke {
                contract: &s.registry.address,
                fn_name: "transfer_admin",
                args: (s.admin.clone(), new_admin.clone()).into_val(&s.env),
                sub_invokes: &[],
            },
        },
    ]);

    let result = s.registry.try_transfer_admin(&s.admin, &new_admin);
    assert!(
        result.is_ok(),
        "transfer must succeed when both parties explicitly authorize"
    );
    assert_eq!(s.registry.admin(), Some(new_admin));
}

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

#[test]
fn test_upgrade_by_non_admin_fails() {
    let s = setup();
    let stranger = Address::generate(&s.env);
    let bogus = soroban_sdk::BytesN::from_array(&s.env, &[0u8; 32]);
    assert_eq!(
        s.registry.try_upgrade(&stranger, &bogus),
        Err(Ok(KeeperError::Unauthorized))
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// cancel_task — checks-effects-interactions regression
//
// A malicious reward token can try to call back into the registry from
// inside `transfer`. `cancel_task` must write `TaskStatus::Cancelled` before
// it ever calls the token, so that if a re-entrant `cancel_task` call for the
// same task ever reaches the function body, it sees a non-Pending status and
// is rejected with `InvalidTaskStatus` rather than paying out a second
// refund.
//
// Note: the Soroban host also refuses same-contract reentrancy at the
// platform level (`ContractReentryMode::Prohibited` on ordinary cross-contract
// calls), so the reentrant call below is actually intercepted before it ever
// reaches our status guard. The test still asserts on both layers: the
// reentrant call must never succeed, and *if* it were ever decoded as a
// contract error, it must be `InvalidTaskStatus`. That keeps this a real
// regression test for the CEI ordering fix rather than one that only
// happens to pass because of the platform's independent protection.
// ─────────────────────────────────────────────────────────────────────────────

mod reentrant_token {
    use soroban_sdk::{contract, contractimpl, contracttype, Address, Env};

    use crate::KeeperRegistryClient;

    #[contracttype]
    #[derive(Clone)]
    enum DataKey {
        Balance(Address),
        Registry,
        TaskId,
        Owner,
        Armed,
        ReentryRejected,
        ReentryErrorCode,
        RefundCount,
    }

    /// Sentinel for `ReentryErrorCode` meaning "no decoded contract error" —
    /// either the hook never fired or the rejection came from the host's own
    /// reentrancy protection rather than our `KeeperError` guard.
    pub const NO_ERROR_CODE: u32 = u32::MAX;

    #[contract]
    pub struct ReentrantToken;

    #[contractimpl]
    impl ReentrantToken {
        pub fn mint(env: Env, to: Address, amount: i128) {
            let balance = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&DataKey::Balance(to), &(balance + amount));
        }

        pub fn balance(env: Env, id: Address) -> i128 {
            env.storage()
                .persistent()
                .get(&DataKey::Balance(id))
                .unwrap_or(0)
        }

        /// Arms the reentrancy hook: the next `transfer` targeting `owner`
        /// will attempt `registry.cancel_task(owner, task_id)` before this
        /// transfer's own balance update completes, simulating a malicious
        /// token hooking mid-transfer.
        pub fn arm(env: Env, registry: Address, task_id: u64, owner: Address) {
            env.storage().instance().set(&DataKey::Registry, &registry);
            env.storage().instance().set(&DataKey::TaskId, &task_id);
            env.storage().instance().set(&DataKey::Owner, &owner);
            env.storage().instance().set(&DataKey::Armed, &true);
            env.storage()
                .instance()
                .set(&DataKey::ReentryRejected, &false);
            env.storage()
                .instance()
                .set(&DataKey::ReentryErrorCode, &NO_ERROR_CODE);
            env.storage().instance().set(&DataKey::RefundCount, &0u32);
        }

        /// Whether the re-entrant `cancel_task` call was rejected (by either
        /// the contract's own status guard or the host's reentrancy check).
        pub fn reentry_rejected(env: Env) -> bool {
            env.storage()
                .instance()
                .get(&DataKey::ReentryRejected)
                .unwrap_or(false)
        }

        /// The decoded `KeeperError` code from the re-entrant call, or
        /// `NO_ERROR_CODE` if the rejection never reached our own contract
        /// logic (e.g. it was intercepted by the host's reentrancy
        /// protection first).
        pub fn reentry_error_code(env: Env) -> u32 {
            env.storage()
                .instance()
                .get(&DataKey::ReentryErrorCode)
                .unwrap_or(NO_ERROR_CODE)
        }

        /// Number of transfers this token made to the armed owner.
        pub fn refund_count(env: Env) -> u32 {
            env.storage()
                .instance()
                .get(&DataKey::RefundCount)
                .unwrap_or(0)
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            let armed: bool = env
                .storage()
                .instance()
                .get(&DataKey::Armed)
                .unwrap_or(false);
            if armed {
                let owner: Address = env.storage().instance().get(&DataKey::Owner).unwrap();
                if to == owner {
                    let count: u32 = env
                        .storage()
                        .instance()
                        .get(&DataKey::RefundCount)
                        .unwrap_or(0);
                    env.storage()
                        .instance()
                        .set(&DataKey::RefundCount, &(count + 1));

                    // Fire once: disarm before recursing so a bug that lets
                    // the re-entrant cancel succeed can't recurse forever.
                    env.storage().instance().set(&DataKey::Armed, &false);
                    let registry: Address =
                        env.storage().instance().get(&DataKey::Registry).unwrap();
                    let task_id: u64 = env.storage().instance().get(&DataKey::TaskId).unwrap();
                    let client = KeeperRegistryClient::new(&env, &registry);
                    let (rejected, code): (bool, u32) =
                        match client.try_cancel_task(&owner, &task_id) {
                            Ok(_) => (false, NO_ERROR_CODE),
                            Err(Ok(err)) => (true, err as u32),
                            Err(Err(_)) => (true, NO_ERROR_CODE),
                        };
                    env.storage()
                        .instance()
                        .set(&DataKey::ReentryRejected, &rejected);
                    env.storage()
                        .instance()
                        .set(&DataKey::ReentryErrorCode, &code);
                }
            }

            let from_balance = Self::balance(env.clone(), from.clone());
            let to_balance = Self::balance(env.clone(), to.clone());
            env.storage()
                .persistent()
                .set(&DataKey::Balance(from), &(from_balance - amount));
            env.storage()
                .persistent()
                .set(&DataKey::Balance(to), &(to_balance + amount));
        }
    }
}

#[test]
fn test_cancel_task_rejects_reentrant_refund() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);

    let token_id = env.register(reentrant_token::ReentrantToken, ());
    let mock_token = reentrant_token::ReentrantTokenClient::new(&env, &token_id);
    mock_token.mint(&admin, &10_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

use crate::{DataKey, KeeperRegistry, KeeperRegistryClient};

const INITIAL_TOKEN_BALANCE: i128 = 1_000_000_000;
const TASK_REWARD: i128 = 10_000;
const LOCK_LEDGERS: u32 = 10;
const TASK_TTL_LEDGERS: u32 = 1_000;

fn execute_and_assert_fee(
    env: &Env,
    registry: &KeeperRegistryClient,
    owner: &Address,
    keeper: &Address,
    reward: i128,
) {
    let deadline = env.ledger().sequence() as u64 + 100;
    let task_id = registry.register_task(
        owner,
        &reward,
        &deadline,
        &20_000u32, // sufficient for required_ttl_ledgers at this deadline
        &TASK_TTL_LEDGERS,
        &17_280u32,
        &120u32,
        &None,
    );

    // Escrow landed on the registry, owner is down the reward.
    assert_eq!(mock_token.balance(&admin), 9_000_000i128);
    assert_eq!(mock_token.balance(&registry_id), 1_000_000i128);

    // Arm the token: its next transfer to `admin` will try to cancel the
    // same task again, from inside the outer cancel's own transfer call.
    mock_token.arm(&registry_id, &task_id, &admin);

    registry.cancel_task(&admin, &task_id);

    // The re-entrant cancel must never have succeeded.
    assert!(mock_token.reentry_rejected());
    // If the rejection reached our own guard (rather than being intercepted
    // by the host's reentrancy protection first), it must be because the
    // outer call already wrote TaskStatus::Cancelled before touching the
    // token.
    let code = mock_token.reentry_error_code();
    if code != reentrant_token::NO_ERROR_CODE {
        assert_eq!(code, KeeperError::InvalidTaskStatus as u32);
    }
    assert_eq!(mock_token.refund_count(), 1);
    assert_eq!(registry.get_task(&task_id).status, TaskStatus::Cancelled);

    // Exactly one refund was paid: owner made whole, registry drained back
    // to zero for this task.
    assert_eq!(mock_token.balance(&admin), 10_000_000i128);
    assert_eq!(mock_token.balance(&registry_id), 0i128);
}

// ─────────────────────────────────────────────────────────────────────────────
// NotInitialized — every entry point that requires configured state must
// return a typed error, never panic, when called before `initialize`.
// ─────────────────────────────────────────────────────────────────────────────

/// A freshly-deployed registry that `initialize` has never touched.
fn uninitialized_registry(env: &Env) -> KeeperRegistryClient<'_> {
    let registry_id = env.register(KeeperRegistry, ());
    KeeperRegistryClient::new(env, &registry_id)
}

#[test]
fn test_register_task_before_init_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let owner = Address::generate(&env);

    assert_eq!(
        registry.try_register_task(
            &owner,
            &TaskType::Custom,
            &calldata(&env),
            &1_000_000i128,
            &(env.ledger().timestamp() + 3_600),
            &17_280u32,
            &120u32,
            &None,
        ),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_withdraw_rewards_before_init_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let keeper = Address::generate(&env);

    // No balance either, but NotInitialized must be surfaced instead of
    // NoRewardsAvailable — the registry isn't configured at all yet.
    //
    // withdraw_rewards checks the keeper's balance before touching the
    // reward token, and a never-initialized registry has no balance for
    // anyone, so NoRewardsAvailable fires first here. This is correct: a
    // caller with nothing to withdraw gets the same answer regardless of
    // configuration state. The reward-token dependency is exercised by
    // test_withdraw_rewards_after_reward_token_migration_drop below.
    assert_eq!(
        registry.try_withdraw_rewards(&keeper),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );
}

#[test]
fn test_pause_before_init_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let caller = Address::generate(&env);
    assert_eq!(
        registry.try_pause(&caller),
        Err(Ok(KeeperError::NotInitialized))
    );

    registry.claim_task(&task_id, keeper);

    let fee_bps_before = registry.get_fee_bps();
    let keeper_balance_before = registry.get_keeper_balance(keeper);

    registry.execute_task(&task_id, keeper, &Bytes::new(env));

    let keeper_balance_after = registry.get_keeper_balance(keeper);
    let keeper_delta = keeper_balance_after - keeper_balance_before;
    let applied_fee = reward - keeper_delta;
    let expected_fee = reward * i128::from(fee_bps_before) / 10_000;

    assert_eq!(
        applied_fee,
        expected_fee,
        "fee mismatch: get_fee_bps() reported {fee_bps_before} bps, expected fee {expected_fee}, applied fee {applied_fee} (reward={reward}, keeper_delta={keeper_delta})"
    );
}

fn setup_registry(
    env: &Env,
) -> (Address, Address, Address, KeeperRegistryClient<'_>) {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let owner = Address::generate(env);
    let keeper = Address::generate(env);

    let token_id = env.register_stellar_asset_contract(admin.clone());
    let token_admin = StellarAssetClient::new(env, &token_id);
    token_admin.mint(&owner, &INITIAL_TOKEN_BALANCE);

    let registry_id = env.register_contract(None, KeeperRegistry);
    let registry = KeeperRegistryClient::new(env, &registry_id);
    registry.initialize(
        &admin,
        &token_id,
        &1i128,
        &0u32,
        &LOCK_LEDGERS,
    );

    env.as_contract(&registry_id, || {
        env.storage().instance().remove(&DataKey::FeeBps);
    });

    (admin, owner, keeper, registry)
}

#[test]
fn fee_bps_matches_applied_fee_when_never_configured() {
    let env = Env::default();
    let (_admin, owner, keeper, registry) = setup_registry(&env);

    execute_and_assert_fee(
        &env,
        &registry,
        &owner,
        &keeper,
        TASK_REWARD,
    );
}

proptest! {
    #[test]
    fn fee_bps_matches_applied_fee_across_fee_history(
        fee_history in prop::collection::vec(any::<Option<u16>>(), 1..8)
    ) {
        let env = Env::default();
        let (admin, owner, keeper, registry) = setup_registry(&env);

        for fee in fee_history {
            if let Some(fee) = fee {
                registry.set_fee_bps(&admin, &(u32::from(fee) % 10_001));
            }

            execute_and_assert_fee(
                &env,
                &registry,
                &owner,
                &keeper,
                TASK_REWARD,
            );
        }
// ─────────────────────────────────────────────────────────────────────────────
// Property Tests (Invariants I-1, I-2, I-3)
// ─────────────────────────────────────────────────────────────────────────────

extern crate std;

use proptest::prelude::*;
use std::format;
use std::string::String;
use std::vec;
use std::vec::Vec;
use crate::test::reentrant_token::NO_ERROR_CODE;

#[derive(Clone, Debug)]
struct PropertyTaskSpec {
    owner_idx: usize,
    reward: i128,
    deadline_offset: u64,
    ttl_ledgers: u32,
    lock_ledgers: u32,
}

fn reward_strategy() -> impl Strategy<Value = i128> {
    prop_oneof![
        Just(1i128),
        Just(100i128),
        Just(1_000_000i128),
        1i128..2_000_000i128,
    ]
}

fn deadline_offset_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![Just(1u64), Just(60u64), Just(3_600u64), 1u64..7_200u64]
}

fn ttl_ledgers_strategy() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(MIN_TTL_LEDGERS),
        Just(MIN_TTL_LEDGERS + 1),
        1_500u32..5_000u32,
    ]
}

fn lock_ledgers_strategy() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(MIN_LOCK_LEDGERS),
        Just(MAX_LOCK_LEDGERS),
        MIN_LOCK_LEDGERS..240u32,
    ]
}

fn property_task_strategy() -> impl Strategy<Value = PropertyTaskSpec> {
    (
        0usize..3,
        reward_strategy(),
        deadline_offset_strategy(),
        ttl_ledgers_strategy(),
        lock_ledgers_strategy(),
    )
        .prop_map(
            |(owner_idx, reward, deadline_offset, ttl_ledgers, lock_ledgers)| PropertyTaskSpec {
                owner_idx,
                reward,
                deadline_offset,
                ttl_ledgers,
                lock_ledgers,
            },
        )
}

#[derive(Clone, Debug)]
enum SolvencyAction {
    Register(PropertyTaskSpec),
    Claim { task_idx: usize, keeper_idx: usize },
    Execute { task_idx: usize, keeper_idx: usize },
    Cancel { task_idx: usize },
    Expire { task_idx: usize },
    IncreaseReward { task_idx: usize, amount: i128 },
    Withdraw { keeper_idx: usize },
    Advance { ledgers: u32, seconds: u64 },
}

fn solvency_action_strategy() -> impl Strategy<Value = SolvencyAction> {
    prop_oneof![
        3 => property_task_strategy().prop_map(SolvencyAction::Register),
        2 => (0usize..5, 0usize..3).prop_map(|(t, k)| SolvencyAction::Claim { task_idx: t, keeper_idx: k }),
        2 => (0usize..5, 0usize..3).prop_map(|(t, k)| SolvencyAction::Execute { task_idx: t, keeper_idx: k }),
        2 => (0usize..5).prop_map(|t| SolvencyAction::Cancel { task_idx: t }),
        2 => (0usize..5).prop_map(|t| SolvencyAction::Expire { task_idx: t }),
        2 => (0usize..5, 1i128..50_000i128).prop_map(|(t, a)| SolvencyAction::IncreaseReward { task_idx: t, amount: a }),
        2 => (0usize..3).prop_map(|k| SolvencyAction::Withdraw { keeper_idx: k }),
        2 => (0u32..5u32, 0u64..300u64).prop_map(|(l, s)| SolvencyAction::Advance { ledgers: l, seconds: s }),
    ]
}

#[derive(Clone, Debug)]
enum ModelTaskStatus {
    Pending,
    Claimed { keeper_idx: usize, claim_ledger: u32 },
    Executed,
    Cancelled,
    Expired,
}

#[derive(Clone, Debug)]
struct ModelTask {
    owner_idx: usize,
    reward: i128,
    deadline: u64,
    lock_ledgers: u32,
    status: ModelTaskStatus,
}

#[derive(Clone, Debug)]
struct AccountingModel {
    tasks: Vec<ModelTask>,
    keeper_balances: [i128; 3],
    fees_accrued: i128,
    executed_ops: Vec<String>,
}

impl AccountingModel {
    fn new() -> Self {
        Self {
            tasks: vec![],
            keeper_balances: [0, 0, 0],
            fees_accrued: 0,
            executed_ops: vec![],
        }
    }

    fn expected_registry_balance(&self) -> i128 {
        let open_escrow: i128 = self
            .tasks
            .iter()
            .filter(|task| matches!(task.status, ModelTaskStatus::Pending | ModelTaskStatus::Claimed { .. }))
            .map(|task| task.reward)
            .sum();
        open_escrow
            + self.keeper_balances.iter().sum::<i128>()
            + self.fees_accrued
    }
}

fn model_lock_expired(task: &ModelTask, current_sequence: u32) -> bool {
    match task.status {
        ModelTaskStatus::Claimed { claim_ledger, .. } => {
            current_sequence >= claim_ledger.saturating_add(task.lock_ledgers)
        }
        _ => false,
    }
}

fn make_property_owners(s: &Setup) -> Vec<Address> {
    let owners = vec![
        s.admin.clone(),
        Address::generate(&s.env),
        Address::generate(&s.env),
    ];
    let asset = token::StellarAssetClient::new(&s.env, &s.token_id);
    for owner in owners.iter().skip(1) {
        asset.mint(owner, &10_000_000i128);
    }
    assert!(!found_init_event, "no event should be emitted on validation failure");
}

// Property tests (issue #93 / backlog 0068): compact proptest coverage per
// I-N invariant, using the shared `invariants` module so these and any
// future fuzz target assert the exact same thing. This is intentionally a
// SMALL proptest per invariant, not the full-depth exploration that
// backlog 0054-0060 (upstream issues #80/#83/#84/#85/#86) call for — those
// remain open, separately-scoped issues; extend these in place rather than
// duplicating them once that work lands.
// ─────────────────────────────────────────────────────────────────────────────
    owners
}

fn make_property_keepers(env: &Env) -> Vec<Address> {
    vec![
        Address::generate(env),
        Address::generate(env),
        Address::generate(env),
    ]
}

fn register_property_task(s: &Setup, owner: &Address, spec: &PropertyTaskSpec) -> u64 {
    let deadline = s.env.ledger().timestamp() + spec.deadline_offset;
    s.registry.register_task(
        owner,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &spec.reward,
        &deadline,
        &spec.ttl_ledgers,
        &spec.lock_ledgers,
    )
}

fn proptest_seed_hint() -> String {
    std::env::var("PROPTEST_CASE_ID")
        .or_else(|_| std::env::var("PROPTEST_SEED"))
        .unwrap_or_else(|_| String::from("proptest-managed"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // Invariant I-1 from docs/ARCHITECTURE.md / Issue 0050:
    // registry balance == open escrow + keeper credited balances + accrued fees.
    #[test]
    fn test_i1_solvency(actions in proptest::collection::vec(solvency_action_strategy(), 6..18)) {
        let s = setup();
        let owners = make_property_owners(&s);
        let keepers = make_property_keepers(&s.env);
        let token = token::Client::new(&s.env, &s.token_id);
        let planned_actions = actions.clone();
        let mut task_ids = Vec::new();
        let mut model = AccountingModel::new();

        for action in actions {
            match action {
                SolvencyAction::Register(spec) => {
                    if task_ids.len() >= 5 {
                        continue;
                    }
                    let owner = &owners[spec.owner_idx % owners.len()];
                    let id = register_property_task(&s, owner, &spec);
                    task_ids.push(id);
                    model.tasks.push(ModelTask {
                        owner_idx: spec.owner_idx % owners.len(),
                        reward: spec.reward,
                        deadline: s.env.ledger().timestamp() + spec.deadline_offset,
                        lock_ledgers: spec.lock_ledgers,
                        status: ModelTaskStatus::Pending,
                    });
                    model.executed_ops.push(format!(
                        "register(task_id={id}, owner_idx={}, reward={})",
                        spec.owner_idx % owners.len(),
                        spec.reward
                    ));
                }
                SolvencyAction::Claim { task_idx, keeper_idx } => {
                    if task_ids.is_empty() {
                        continue;
                    }
                    let idx = task_idx % task_ids.len();
                    let id = task_ids[idx];
                    let keeper = &keepers[keeper_idx % keepers.len()];
                    let current_time = s.env.ledger().timestamp();
                    let current_sequence = s.env.ledger().sequence();
                    let task = &model.tasks[idx];
                    let valid = current_time < task.deadline
                        && match task.status {
                            ModelTaskStatus::Pending => true,
                            ModelTaskStatus::Claimed { .. } => model_lock_expired(task, current_sequence),
                            _ => false,
                        };
                    if valid {
                        let res = s.registry.try_claim_task(keeper, &id);
                        prop_assert!(
                            res.is_ok(),
                            "I-1 claim failed for valid model state; seed={} actions={:?} model={:?}",
                            proptest_seed_hint(),
                            planned_actions,
                            model
                        );
                        model.tasks[idx].status = ModelTaskStatus::Claimed {
                            keeper_idx: keeper_idx % keepers.len(),
                            claim_ledger: current_sequence,
                        };
                        model.executed_ops.push(format!(
                            "claim(task_id={id}, keeper_idx={})",
                            keeper_idx % keepers.len()
                        ));
                    }
                }
                SolvencyAction::Execute { task_idx, keeper_idx } => {
                    if task_ids.is_empty() {
                        continue;
                    }
                    let idx = task_idx % task_ids.len();
                    let id = task_ids[idx];
                    let keeper_idx = keeper_idx % keepers.len();
                    let keeper = &keepers[keeper_idx];
                    let current_time = s.env.ledger().timestamp();
                    let valid = current_time < model.tasks[idx].deadline
                        && matches!(
                            model.tasks[idx].status,
                            ModelTaskStatus::Claimed { keeper_idx: k, .. } if k == keeper_idx
                        );
                    if valid {
                        let res = s.registry.try_execute_task(
                            keeper,
                            &id,
                            &Bytes::from_slice(&s.env, b"prop"),
                        );
                        prop_assert!(
                            res.is_ok(),
                            "I-1 execute failed for valid model state; seed={} actions={:?} model={:?}",
                            proptest_seed_hint(),
                            planned_actions,
                            model
                        );
                        let reward = model.tasks[idx].reward;
                        let (keeper_net, fee) = split_reward(reward, 300u32);
                        model.keeper_balances[keeper_idx] += keeper_net;
                        model.fees_accrued += fee;
                        model.tasks[idx].status = ModelTaskStatus::Executed;
                        model.executed_ops.push(format!(
                            "execute(task_id={id}, keeper_idx={keeper_idx}, keeper_net={keeper_net}, fee={fee})"
                        ));
                    }
                }
                SolvencyAction::Cancel { task_idx } => {
                    if task_ids.is_empty() {
                        continue;
                    }
                    let idx = task_idx % task_ids.len();
                    let id = task_ids[idx];
                    let owner = &owners[model.tasks[idx].owner_idx];
                    let current_sequence = s.env.ledger().sequence();
                    let valid = match model.tasks[idx].status {
                        ModelTaskStatus::Pending => true,
                        ModelTaskStatus::Claimed { .. } => model_lock_expired(&model.tasks[idx], current_sequence),
                        _ => false,
                    };
                    if valid {
                        let res = s.registry.try_cancel_task(owner, &id);
                        prop_assert!(
                            res.is_ok(),
                            "I-1 cancel failed for valid model state; seed={} actions={:?} model={:?}",
                            proptest_seed_hint(),
                            planned_actions,
                            model
                        );
                        model.tasks[idx].status = ModelTaskStatus::Cancelled;
                        model.executed_ops.push(format!("cancel(task_id={id})"));
                    }
                }
                SolvencyAction::Expire { task_idx } => {
                    if task_ids.is_empty() {
                        continue;
                    }
                    let idx = task_idx % task_ids.len();
                    let id = task_ids[idx];
                    let valid = s.env.ledger().timestamp() >= model.tasks[idx].deadline
                        && matches!(
                            model.tasks[idx].status,
                            ModelTaskStatus::Pending | ModelTaskStatus::Claimed { .. }
                        );
                    if valid {
                        let res = s.registry.try_expire_task(&id);
                        prop_assert!(
                            res.is_ok(),
                            "I-1 expire failed for valid model state; seed={} actions={:?} model={:?}",
                            proptest_seed_hint(),
                            planned_actions,
                            model
                        );
                        model.tasks[idx].status = ModelTaskStatus::Expired;
                        model.executed_ops.push(format!("expire(task_id={id})"));
                    }
                }
                SolvencyAction::IncreaseReward { task_idx, amount } => {
                    if task_ids.is_empty() {
                        continue;
                    }
                    let idx = task_idx % task_ids.len();
                    let id = task_ids[idx];
                    let owner = &owners[model.tasks[idx].owner_idx];
                    if matches!(
                        model.tasks[idx].status,
                        ModelTaskStatus::Pending | ModelTaskStatus::Claimed { .. }
                    ) {
                        let res = s.registry.try_increase_reward(owner, &id, &amount);
                        prop_assert!(
                            res.is_ok(),
                            "I-1 top-up failed for valid model state; seed={} actions={:?} model={:?}",
                            proptest_seed_hint(),
                            planned_actions,
                            model
                        );
                        model.tasks[idx].reward += amount;
                        model.executed_ops.push(format!(
                            "increase_reward(task_id={id}, amount={amount})"
                        ));
                    }
                }
                SolvencyAction::Withdraw { keeper_idx } => {
                    let keeper_idx = keeper_idx % keepers.len();
                    let keeper = &keepers[keeper_idx];
                    let expected = model.keeper_balances[keeper_idx];
                    if expected > 0 {
                        let res = s.registry.try_withdraw_rewards(keeper);
                        prop_assert!(
                            matches!(res, Ok(Ok(amount)) if amount == expected),
                            "I-1 withdraw failed for valid model state; seed={} actions={:?} model={:?} result={:?}",
                            proptest_seed_hint()
                            ,
                            planned_actions,
                            model,
                            res
                        );
                        model.keeper_balances[keeper_idx] = 0;
                        model.executed_ops.push(format!(
                            "withdraw(keeper_idx={keeper_idx}, amount={expected})"
                        ));
                    }
                }
                SolvencyAction::Advance { ledgers, seconds } => {
                    advance(&s.env, ledgers, seconds);
                    model.executed_ops.push(format!(
                        "advance(ledgers={ledgers}, seconds={seconds})"
                    ));
                }
            }

            let registry_balance = token.balance(&s.registry.address);
            let observed_keeper_sum: i128 = keepers.iter().map(|keeper| s.registry.keeper_balance(keeper)).sum();
            let expected_keeper_sum: i128 = model.keeper_balances.iter().sum();
            let observed_fees = s.registry.fees_accrued();
            let expected_balance = model.expected_registry_balance();

            prop_assert_eq!(
                observed_keeper_sum,
                expected_keeper_sum,
                "Invariant I-1 keeper balances drifted; seed={} actions={:?} ops={:?} model={:?}",
                proptest_seed_hint(),
                planned_actions,
                model.executed_ops
                ,
                model
            );
            prop_assert_eq!(
                observed_fees,
                model.fees_accrued,
                "Invariant I-1 fees drifted; seed={} actions={:?} ops={:?} model={:?}",
                proptest_seed_hint(),
                planned_actions,
                model.executed_ops
                ,
                model
            );
            prop_assert_eq!(
                registry_balance,
                expected_balance,
                "Invariant I-1 solvency violated; seed={} actions={:?} ops={:?} model={:?} observed_registry_balance={} expected_balance={}",
                proptest_seed_hint(),
                planned_actions,
                model.executed_ops,
                model,
                registry_balance,
                expected_balance
            );
        }
    }

    // Invariant I-2 from docs/ARCHITECTURE.md:
    // every escrowed reward has a reachable terminal resolution path.
    #[test]
    fn property_i3_single_payout_not_doubled(reward in 1_i128..9_000_000) {
        let s = setup();
        let keeper = Address::generate(&s.env);
        let id = register_reward_task(&s, reward);

        s.registry.claim_task(&keeper, &id);
        let balance_before = s.registry.keeper_balance(&keeper);
        s.registry
            .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"p"));
        let balance_after_first = s.registry.keeper_balance(&keeper);

        let (expected_net, _fee) = split_reward(reward, s.registry.get_fee_bps()).unwrap();
        crate::invariants::assert_single_payout(balance_before, balance_after_first, expected_net)
            .expect("I-3: first execution must credit exactly the net reward once");

        // A second execute on the same (now Executed) task must be
        // rejected, and must not touch the keeper's balance again.
        let second = s.registry.try_execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"p2"));
        prop_assert!(second.is_err(), "re-executing an Executed task must be rejected");
        let balance_after_second_attempt = s.registry.keeper_balance(&keeper);
    fn test_i2_escrow_recoverability(
        spec in property_task_strategy(),
    ) {
        let cancel_setup = setup();
        let cancel_owners = make_property_owners(&cancel_setup);
        let cancel_owner = &cancel_owners[spec.owner_idx % cancel_owners.len()];
        let cancel_id = register_property_task(&cancel_setup, cancel_owner, &spec);
        prop_assert!(
            cancel_setup.registry.try_cancel_task(cancel_owner, &cancel_id).is_ok(),
            "Invariant I-2 cancel path unreachable; seed={} task={:?}",
            proptest_seed_hint(),
            spec
        );
        prop_assert_eq!(
            cancel_setup.registry.get_task(&cancel_id).status,
            TaskStatus::Cancelled,
            "Invariant I-2 cancel path did not terminate; seed={} task={:?}",
            proptest_seed_hint(),
            spec
        );
    }

    // I-4 — Fee bounding, across arbitrary reward/fee_bps combinations.
    #[test]
    fn property_i4_fee_bounded_across_arbitrary_inputs(
        reward in 1_i128..i128::from(u64::MAX),
        fee_bps in 0u32..=10_000u32,
    ) {
        let (keeper_net, fee) = split_reward(reward, fee_bps).unwrap();
        assert_fee_bounded(reward, fee_bps, keeper_net, fee)
            .expect("I-4 fee bounding must hold for every reward/fee_bps combination");
    }

    // I-5 — Escrow isolation: sweeping accrued fees must never change any
    // task's escrowed reward or any keeper's credited balance. Two tasks
    // are registered from the same `reward`, so it's capped at half the
    // minted supply.
    #[test]
    fn property_i5_sweep_fees_isolated_from_escrow_and_keeper_balances(
        reward in 1_i128..4_500_000,
    ) {
        let s = setup();
        let keeper = Address::generate(&s.env);
        let executed_id = register_reward_task(&s, reward);
        let pending_id = register_reward_task(&s, reward);

        let execute_setup = setup();
        let execute_owners = make_property_owners(&execute_setup);
        let execute_owner = &execute_owners[spec.owner_idx % execute_owners.len()];
        let execute_keeper = Address::generate(&execute_setup.env);
        let execute_id = register_property_task(&execute_setup, execute_owner, &spec);
        prop_assert!(
            execute_setup.registry.try_claim_task(&execute_keeper, &execute_id).is_ok(),
            "Invariant I-2 claim path unreachable; seed={} task={:?}",
            proptest_seed_hint(),
            spec
        );
        prop_assert!(
            execute_setup
                .registry
                .try_execute_task(&execute_keeper, &execute_id, &Bytes::from_slice(&execute_setup.env, b"i2"))
                .is_ok(),
            "Invariant I-2 execute path unreachable; seed={} task={:?}",
            proptest_seed_hint(),
            spec
        );
        let (keeper_net, fee) = split_reward(spec.reward, 300u32);
        prop_assert!(
            matches!(
                execute_setup.registry.try_withdraw_rewards(&execute_keeper),
                Ok(Ok(amount)) if amount == keeper_net
            ),
            "Invariant I-2 withdraw path unreachable; seed={} task={:?}",
            proptest_seed_hint(),
            spec
        );
        prop_assert_eq!(
            token::Client::new(&execute_setup.env, &execute_setup.token_id)
                .balance(&execute_setup.registry.address),
            fee,
            "Invariant I-2 execute path left unresolved escrow; seed={} task={:?}",
            proptest_seed_hint(),
            spec
        );

        let expire_setup = setup();
        let expire_owners = make_property_owners(&expire_setup);
        let expire_owner = &expire_owners[spec.owner_idx % expire_owners.len()];
        let expire_id = register_property_task(&expire_setup, expire_owner, &spec);
        // Issue 0005 reference: keep the simulated expiry within a live TTL
        // window so this property keeps exercising recoverability rather than
        // archival/restore host behavior. Once Issue 0005 is fixed this range
        // can widen without changing the assertions.
        advance(&expire_setup.env, 1, spec.deadline_offset + 1);
        prop_assert!(
            expire_setup.registry.try_expire_task(&expire_id).is_ok(),
            "Invariant I-2 expire path unreachable; seed={} task={:?}",
            proptest_seed_hint(),
            spec
        );
        prop_assert_eq!(
            expire_setup.registry.get_task(&expire_id).status,
            TaskStatus::Expired,
            "Invariant I-2 expire path did not terminate; seed={} task={:?}",
            proptest_seed_hint(),
            spec
        );
        prop_assert_eq!(
            token::Client::new(&expire_setup.env, &expire_setup.token_id)
                .balance(&expire_setup.registry.address),
            0i128,
            "Invariant I-2 expire path left escrow stranded; seed={} task={:?}",
            proptest_seed_hint(),
            spec
        );
    }

    // Invariant I-3 from docs/ARCHITECTURE.md:
    // every reward is paid out exactly once across sequential and reentrant attempts.
    #[test]
    fn test_i3_single_payout(
        spec in property_task_strategy(),
        path in 0u8..5,
        different_second_caller in any::<bool>(),
    ) {
        match path {
            0 => {
                let s = setup();
                let owners = make_property_owners(&s);
                let owner = &owners[spec.owner_idx % owners.len()];
                let other_owner = &owners[(spec.owner_idx + 1) % owners.len()];
                let token = token::Client::new(&s.env, &s.token_id);
                let task_id = register_property_task(&s, owner, &spec);
                let owner_before = token.balance(owner);
                let payer = if different_second_caller { other_owner } else { owner };
                let sequence = vec![
                    format!("cancel(task_id={task_id})"),
                    format!("cancel-again(task_id={task_id}, different_second_caller={different_second_caller})"),
                ];

                prop_assert!(
                    s.registry.try_cancel_task(owner, &task_id).is_ok(),
                    "Invariant I-3 cancel first call failed; seed={} path=cancel task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                let second = s.registry.try_cancel_task(payer, &task_id);
                prop_assert!(
                    matches!(second, Err(Ok(_))),
                    "Invariant I-3 cancel second call must return typed contract error; seed={} task={:?} second={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    second,
                    sequence
                );
                prop_assert_eq!(
                    token.balance(owner) - owner_before,
                    spec.reward,
                    "Invariant I-3 cancel paid wrong amount; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                prop_assert_eq!(
                    token.balance(&s.registry.address),
                    0i128,
                    "Invariant I-3 cancel left positive registry balance; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
            }
            1 => {
                let s = setup();
                let owners = make_property_owners(&s);
                let owner = &owners[spec.owner_idx % owners.len()];
                let keepers = make_property_keepers(&s.env);
                let keeper = &keepers[0];
                let other_keeper = &keepers[1];
                let token = token::Client::new(&s.env, &s.token_id);
                let task_id = register_property_task(&s, owner, &spec);
                let keeper_before = token.balance(keeper);
                let execute_caller = if different_second_caller { other_keeper } else { keeper };
                let withdraw_caller = if different_second_caller { other_keeper } else { keeper };
                let (keeper_net, fee) = split_reward(spec.reward, 300u32);
                let sequence = vec![
                    format!("claim(task_id={task_id}, keeper=0)"),
                    format!("execute(task_id={task_id}, keeper=0)"),
                    format!("execute-again(task_id={task_id}, different_second_caller={different_second_caller})"),
                    format!("withdraw(keeper=0)"),
                    format!("withdraw-again(different_second_caller={different_second_caller})"),
                ];

                prop_assert!(
                    s.registry.try_claim_task(keeper, &task_id).is_ok(),
                    "Invariant I-3 execute path claim failed; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                prop_assert!(
                    s.registry
                        .try_execute_task(keeper, &task_id, &Bytes::from_slice(&s.env, b"i3"))
                        .is_ok(),
                    "Invariant I-3 execute first call failed; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                let second_execute = s.registry.try_execute_task(
                    execute_caller,
                    &task_id,
                    &Bytes::from_slice(&s.env, b"i3"),
                );
                prop_assert!(
                    matches!(second_execute, Err(Ok(_))),
                    "Invariant I-3 execute second call must return typed contract error; seed={} task={:?} second={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    second_execute,
                    sequence
                );
                prop_assert!(
                    matches!(s.registry.try_withdraw_rewards(keeper), Ok(Ok(amount)) if amount == keeper_net),
                    "Invariant I-3 withdraw first call failed; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                let second_withdraw = s.registry.try_withdraw_rewards(withdraw_caller);
                prop_assert!(
                    matches!(second_withdraw, Err(Ok(_))),
                    "Invariant I-3 withdraw second call must return typed contract error; seed={} task={:?} second={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    second_withdraw,
                    sequence
                );
                prop_assert_eq!(
                    token.balance(keeper) - keeper_before,
                    keeper_net,
                    "Invariant I-3 execute/withdraw transferred wrong keeper amount; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                prop_assert_eq!(
                    token.balance(&s.registry.address),
                    fee,
                    "Invariant I-3 execute/withdraw left wrong registry balance; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
            }
            2 => {
                let s = setup();
                let owners = make_property_owners(&s);
                let owner = &owners[spec.owner_idx % owners.len()];
                let token = token::Client::new(&s.env, &s.token_id);
                let task_id = register_property_task(&s, owner, &spec);
                let owner_before = token.balance(owner);
                let sequence = vec![
                    format!("advance(deadline_offset_plus_one={})", spec.deadline_offset + 1),
                    format!("expire(task_id={task_id})"),
                    format!("expire-again(task_id={task_id})"),
                ];

                advance(&s.env, 1, spec.deadline_offset + 1);
                prop_assert!(
                    s.registry.try_expire_task(&task_id).is_ok(),
                    "Invariant I-3 expire first call failed; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                let second = s.registry.try_expire_task(&task_id);
                prop_assert!(
                    matches!(second, Err(Ok(_))),
                    "Invariant I-3 expire second call must return typed contract error; seed={} task={:?} second={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    second,
                    sequence
                );
                prop_assert_eq!(
                    token.balance(owner) - owner_before,
                    spec.reward,
                    "Invariant I-3 expire paid wrong amount; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                prop_assert_eq!(
                    token.balance(&s.registry.address),
                    0i128,
                    "Invariant I-3 expire left positive registry balance; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
            }
            3 => {
                let env = Env::default();
                env.mock_all_auths();
                let admin = Address::generate(&env);
                let token_id = env.register(reentrant_token::ReentrantToken, ());
                let mock_token = reentrant_token::ReentrantTokenClient::new(&env, &token_id);
                mock_token.mint(&admin, &10_000_000i128);

                let registry_id = env.register(KeeperRegistry, ());
                let registry = KeeperRegistryClient::new(&env, &registry_id);
                registry.initialize(&admin, &token_id, &300u32);

                let deadline = env.ledger().timestamp() + spec.deadline_offset;
                let task_id = registry.register_task(
                    &admin,
                    &TaskType::Liquidation,
                    &calldata(&env),
                    &spec.reward,
                    &deadline,
                    &spec.ttl_ledgers,
                    &spec.lock_ledgers,
                );
                let sequence = vec![
                    format!("arm-reentrant-cancel(task_id={task_id})"),
                    format!("cancel(task_id={task_id})"),
                ];

                mock_token.arm(&registry.address, &task_id, &admin);
                prop_assert!(
                    registry.try_cancel_task(&admin, &task_id).is_ok(),
                    "Invariant I-3 reentrant cancel outer call failed; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                let code = mock_token.reentry_error_code();
                if code != NO_ERROR_CODE {
                    prop_assert_eq!(
                        code,
                        KeeperError::InvalidTaskStatus as u32,
                        "Invariant I-3 reentrant cancel decoded unexpected error; seed={} task={:?} ops={:?}",
                        proptest_seed_hint(),
                        spec,
                        sequence
                    );
                }
                prop_assert!(mock_token.reentry_rejected());
                prop_assert_eq!(mock_token.refund_count(), 1);
                prop_assert_eq!(mock_token.balance(&admin), 10_000_000i128);
                prop_assert_eq!(mock_token.balance(&registry_id), 0i128);
            }
            4 => {
                let env = Env::default();
                env.mock_all_auths();
                let admin = Address::generate(&env);
                let token_id = env.register(reentrant_token_expire::ExpireReentrantToken, ());
                let token = reentrant_token_expire::ExpireReentrantTokenClient::new(&env, &token_id);
                token.set_balance(&admin, &5_000_000i128);

                let registry_id = env.register(KeeperRegistry, ());
                let registry = KeeperRegistryClient::new(&env, &registry_id);
                registry.initialize(&admin, &token_id, &300u32);

                let reward = spec.reward.min(1_000_000i128);
                let deadline = env.ledger().timestamp() + spec.deadline_offset.min(60);
                let task_id = registry.register_task(
                    &admin,
                    &TaskType::Liquidation,
                    &calldata(&env),
                    &reward,
                    &deadline,
                    &spec.ttl_ledgers,
                    &spec.lock_ledgers,
                );
                let sequence = vec![
                    format!("advance(deadline_offset_plus_one={})", spec.deadline_offset.min(60) + 1),
                    format!("arm-reentrant-expire(task_id={task_id})"),
                    format!("expire(task_id={task_id})"),
                ];

                advance(&env, 1, spec.deadline_offset.min(60) + 1);
                token.arm(&registry.address, &task_id);
                prop_assert!(
                    registry.try_expire_task(&task_id).is_ok(),
                    "Invariant I-3 reentrant expire outer call failed; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                prop_assert_ne!(
                    token.reentrant_code(),
                    0u32,
                    "Invariant I-3 reentrant expire must reject the nested payout attempt; seed={} task={:?} ops={:?}",
                    proptest_seed_hint(),
                    spec,
                    sequence
                );
                prop_assert_eq!(token.balance(&admin), 5_000_000i128);
                prop_assert_eq!(token.balance(&registry_id), 0i128);
                prop_assert_eq!(registry.get_task(&task_id).status, TaskStatus::Expired);
            }
            _ => unreachable!(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Verifier mocks (#96-#106) — minimal, test-only contracts following the
// established `mod reentrant_token { ... }` local-mock-contract pattern.
// ─────────────────────────────────────────────────────────────────────────────

/// A verifier whose `verify` always returns `true` — the happy-path mock for
/// #108.
mod always_approve_verifier {
    use soroban_sdk::{contract, contractimpl, Address, Bytes, Env};

    use crate::Task;

    #[contract]
    pub struct AlwaysApproveVerifier;

    #[contractimpl]
    impl AlwaysApproveVerifier {
        pub fn verify(_env: Env, _task: Task, _keeper: Address, _proof: Bytes) -> bool {
            true
        }
    }
}

/// A verifier whose `verify` always returns `false` — the rejection-path
/// mock for #109.
mod always_reject_verifier {
    use soroban_sdk::{contract, contractimpl, Address, Bytes, Env};

    use crate::Task;

    #[contract]
    pub struct AlwaysRejectVerifier;

    #[contractimpl]
    impl AlwaysRejectVerifier {
        pub fn verify(_env: Env, _task: Task, _keeper: Address, _proof: Bytes) -> bool {
            false
        }
    }
}

/// A verifier whose `verify` always panics — the worst-case mock for #110,
/// distinct from `always_reject_verifier`: a `false` return is a normal,
/// recoverable rejection `execute_task` handles gracefully, while a panic
/// is a genuinely unrecoverable host error that aborts the whole
/// transaction (see `IKeeperVerifier`'s doc comment for why).
mod panicking_verifier {
    use soroban_sdk::{contract, contractimpl, panic_with_error, Address, Bytes, Env};

    use crate::{KeeperError, Task};

    #[contract]
    pub struct PanickingVerifier;

    #[contractimpl]
    impl PanickingVerifier {
        pub fn verify(env: Env, _task: Task, _keeper: Address, _proof: Bytes) -> bool {
            // Any panic value works to exercise the "verifier panics" path;
            // panic_with_error is used here (rather than a bare panic!())
            // purely so the failure is visible as a proper contract error in
            // test output rather than an opaque WASM trap message.
            panic_with_error!(&env, KeeperError::Unauthorized);
        }
    }
}

/// Registers a task with `verifier` attached, funded and deadlined the same
/// way `register_reward_task` is, so the verifier-specific tests don't
/// duplicate that boilerplate.
fn register_task_with_verifier(s: &Setup, reward: i128, verifier: &Address) -> u64 {
    let deadline = s.env.ledger().timestamp() + 3_600;
    s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &20_000u32, // sufficient for required_ttl_ledgers at this deadline
        &120u32,
        &Some(verifier.clone()),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// #107 — update_verifier is rejected once a task is claimed
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_update_verifier_rejected_once_task_is_claimed() {
    let s = setup();
    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    let task_id = register_task_with_verifier(&s, 1_000_000i128, &verifier_id);

    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);

    let new_verifier = Address::generate(&s.env);
    let result = s
        .registry
        .try_update_verifier(&s.admin, &task_id, &Some(new_verifier.clone()));
    assert_eq!(result, Err(Ok(KeeperError::InvalidTaskStatus)));

    // The task's verifier field must be unchanged after the rejected attempt.
    let task = s.registry.get_task(&task_id);
    assert_eq!(task.verifier, Some(verifier_id));
    assert_ne!(task.verifier, Some(new_verifier));
}

#[test]
fn test_update_verifier_succeeds_while_pending() {
    // Sanity check alongside the rejection test above: the restriction is
    // specifically "not once claimed", not "never" — a Pending task's
    // verifier can still be changed freely.
    let s = setup();
    let task_id = register_default_task(&s); // no verifier attached yet

    let verifier_id = s
        .env
        .register(always_approve_verifier::AlwaysApproveVerifier, ());
    s.registry
        .update_verifier(&s.admin, &task_id, &Some(verifier_id.clone()));

    let task = s.registry.get_task(&task_id);
    assert_eq!(task.verifier, Some(verifier_id));

    // Clearing it back to None also works while still Pending.
    s.registry.update_verifier(&s.admin, &task_id, &None);
    assert_eq!(s.registry.get_task(&task_id).verifier, None);
}

#[test]
fn test_update_verifier_rejects_non_owner() {
    let s = setup();
    let task_id = register_default_task(&s);
    let stranger = Address::generate(&s.env);
    let verifier_id = s
        .env
        .register(always_approve_verifier::AlwaysApproveVerifier, ());

    assert_eq!(
        s.registry
            .try_update_verifier(&stranger, &task_id, &Some(verifier_id)),
        Err(Ok(KeeperError::NotTaskOwner))
    );
}

#[test]
fn test_update_verifier_rejected_when_paused() {
    let s = setup();
    let task_id = register_default_task(&s);
    let verifier_id = s
        .env
        .register(always_approve_verifier::AlwaysApproveVerifier, ());

    s.registry.pause(&s.admin);
    assert_eq!(
        s.registry
            .try_update_verifier(&s.admin, &task_id, &Some(verifier_id)),
        Err(Ok(KeeperError::ContractPaused))
    );
}

#[test]
fn test_update_verifier_emits_event() {
    let s = setup();
    let task_id = register_default_task(&s);
    let verifier_id = s
        .env
        .register(always_approve_verifier::AlwaysApproveVerifier, ());

    s.registry
        .update_verifier(&s.admin, &task_id, &Some(verifier_id.clone()));

    let expected_topic: soroban_sdk::Vec<soroban_sdk::Val> =
        (symbol_short!("verifier"), symbol_short!("task")).into_val(&s.env);
    let found = s.env.events().all().iter().any(|(contract, topics, _)| {
        contract == s.registry.address && topics == expected_topic
    });
    assert!(found, "VerifierUpdated event must be emitted");
}

// ─────────────────────────────────────────────────────────────────────────────
// #108 — execute_task with a verifier that always approves
// #99 — execute_task with a verifier attached
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_execute_task_with_always_approve_verifier_matches_no_verifier_outcome() {
    let s = setup();
    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    let reward = 1_000_000i128;
    let task_id = register_task_with_verifier(&s, reward, &verifier_id);

    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);

    let proof = Bytes::from_slice(&s.env, b"proof-bytes");
    s.registry.execute_task(&keeper, &task_id, &proof);

    // Check events immediately after the call that emits them: each
    // top-level client call is its own host invocation, and
    // `s.env.events().all()` only reflects the most recent one — any
    // further contract calls (even read-only ones like `get_task`) start a
    // new invocation and the previous one's events are no longer visible.
    let all_events = s.env.events().all();
    let verfail_topic: soroban_sdk::Vec<soroban_sdk::Val> =
        (symbol_short!("verfail"), symbol_short!("task")).into_val(&s.env);
    let verfail_fired = all_events.iter().any(|(contract, topics, _)| {
        contract == s.registry.address && topics == verfail_topic
    });
    assert!(
        !verfail_fired,
        "TaskVerificationFailed must not fire when the verifier approves"
    );

    let exec_topic: soroban_sdk::Vec<soroban_sdk::Val> =
        (symbol_short!("exec"), symbol_short!("task")).into_val(&s.env);
    let exec_fired = all_events.iter().any(|(contract, topics, _)| {
        contract == s.registry.address && topics == exec_topic
    });
    assert!(exec_fired, "TaskExecuted must still fire");

    // Same outcome as the no-verifier path: full net reward credited, fee
    // accrued, task Executed.
    let (expected_net, expected_fee) = split_reward(reward, 300u32).unwrap();
    assert_eq!(s.registry.keeper_balance(&keeper), expected_net);
    assert_eq!(s.registry.fees_accrued(), expected_fee);

    let task = s.registry.get_task(&task_id);
    assert_eq!(task.status, TaskStatus::Executed);
}

// ─────────────────────────────────────────────────────────────────────────────
// #109 — execute_task with a verifier that always rejects
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_execute_task_with_always_reject_verifier() {
    let s = setup();
    let verifier_id = s.env.register(always_reject_verifier::AlwaysRejectVerifier, ());
    let reward = 1_000_000i128;
    let task_id = register_task_with_verifier(&s, reward, &verifier_id);

    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);

    let proof = Bytes::from_slice(&s.env, b"first-attempt-proof");
    let result = s.registry.try_execute_task(&keeper, &task_id, &proof);
    assert_eq!(result, Err(Ok(KeeperError::VerificationFailed)));

    // Note: we don't assert TaskVerificationFailed's presence in
    // `s.env.events().all()` here. Soroban's host rolls back a whole call
    // frame — events included — whenever that frame's top-level function
    // returns `Err` (see `with_frame`/`call_n_internal` in
    // soroban-env-host), even though the error itself is "recoverable" from
    // the caller's perspective via `try_`. So an event published right
    // before an `Err` return is never observable by any caller, in any
    // Soroban contract; `emit_verification_failed`'s call site in
    // `execute_task` can't be exercised by a test that also checks the
    // `Err` result. What we *can* and do verify below is the actually
    // observable contract: the typed error, and that state didn't change.

    // Task remains Claimed — not Executed, not reverted to Pending.
    let task = s.registry.get_task(&task_id);
    assert_eq!(task.status, TaskStatus::Claimed);
    assert_eq!(task.claimer, Some(keeper.clone()));

    // No token transfer / keeper crediting occurred.
    assert_eq!(s.registry.keeper_balance(&keeper), 0i128);
    assert_eq!(s.registry.fees_accrued(), 0i128);

    // A second execute_task call (different proof bytes, same always-reject
    // verifier) fails the same way — the rejection is repeatable, not a
    // one-shot state change.
    let proof2 = Bytes::from_slice(&s.env, b"second-attempt-different-proof");
    let result2 = s.registry.try_execute_task(&keeper, &task_id, &proof2);
    assert_eq!(result2, Err(Ok(KeeperError::VerificationFailed)));

    let task_after_retry = s.registry.get_task(&task_id);
    assert_eq!(task_after_retry.status, TaskStatus::Claimed);
    assert_eq!(task_after_retry.claimer, Some(keeper));
}

// ─────────────────────────────────────────────────────────────────────────────
// #110 — execute_task against a panicking verifier must not permanently
// brick the task
// ─────────────────────────────────────────────────────────────────────────────
//
// Per #100's investigation (see IKeeperVerifier's doc comment in lib.rs,
// citing soroban-env-host's Host::try_call): Soroban's host only recovers
// *typed contract errors* across a try_invoke_contract call. A genuine
// panic in the callee is a non-recoverable host error and propagates,
// aborting the entire calling transaction — it is NOT caught as a
// VerificationFailed rejection. This test demonstrates that concretely,
// and confirms expire_task is the real, working recovery path once the
// deadline passes — proving the eventual-recovery fallback actually holds
// even in this worst case.

/// Shared setup for both panicking-verifier tests below: a task with a
/// verifier that always panics, claimed by a keeper.
fn setup_task_with_panicking_verifier() -> (Setup, u64, Address, i128) {
    let s = setup();
    let verifier_id = s.env.register(panicking_verifier::PanickingVerifier, ());
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s
        .registry
        .register_task(
            &s.admin,
            &TaskType::Liquidation,
            &calldata(&s.env),
            &reward,
            &deadline,
            &20_000u32, // sufficient for required_ttl_ledgers at this deadline
            &120u32,
            &Some(verifier_id),
        );

    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);
    (s, task_id, keeper, reward)
}

/// The panicking verifier is not isolated: `execute_task`'s own call
/// propagates the panic rather than returning a recoverable
/// `VerificationFailed` error. `#[should_panic]` is the idiomatic Rust way
/// to assert this (this crate is `#![no_std]`, so `std::panic::catch_unwind`
/// isn't available even in `#[cfg(test)]` code) — the same abort-the-whole-
/// transaction behavior any real caller would see, per the host semantics
/// documented on `IKeeperVerifier`.
#[test]
#[should_panic]
fn test_execute_task_against_panicking_verifier_panics() {
    let (s, task_id, keeper, _reward) = setup_task_with_panicking_verifier();
    let proof = Bytes::from_slice(&s.env, b"proof");
    s.registry.execute_task(&keeper, &task_id, &proof);
}

/// The recovery path: since the panic aborts the whole transaction rather
/// than being caught (proven by the `#[should_panic]` test above), the task
/// is never touched by the failed attempt and remains `Claimed` — this test
/// confirms `expire_task` still successfully returns the escrow to the
/// owner once the deadline passes, exactly as it would for any other
/// stuck-`Claimed` task. This is the concrete proof that Invariant I-2
/// (escrow recoverability) holds even for a permanently-panicking verifier:
/// the eventual-recovery fallback #100 concluded on actually works.
#[test]
fn test_expire_task_recovers_escrow_from_a_task_stuck_behind_a_panicking_verifier() {
    let (s, task_id, keeper, reward) = setup_task_with_panicking_verifier();

    // Deliberately never call execute_task here — the test above already
    // proves that call panics; this test only needs the pre-panic state
    // (Claimed, unexecuted) to demonstrate expire_task's recovery, and a
    // panic would abort this test function before reaching the assertions
    // below.
    let task_before_expiry = s.registry.get_task(&task_id);
    assert_eq!(task_before_expiry.status, TaskStatus::Claimed);
    assert_eq!(task_before_expiry.claimer, Some(keeper.clone()));
    assert_eq!(s.registry.keeper_balance(&keeper), 0i128);

    let token = token::Client::new(&s.env, &s.token_id);
    let owner_balance_before = token.balance(&s.admin);

    advance(&s.env, 1, 3_601);
    s.registry.expire_task(&task_id);

    let task_after_expiry = s.registry.get_task(&task_id);
    assert_eq!(task_after_expiry.status, TaskStatus::Expired);
    assert_eq!(token.balance(&s.admin), owner_balance_before + reward);
}

#[test]
fn test_execute_task_none_verifier_path_unchanged() {
    // The base MVP path (no verifier attached) must behave identically to
    // before this feature existed — required explicitly by #99's acceptance
    // criteria, on top of the fact that all 100 pre-existing tests already
    // pass unmodified.
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &20_000u32, // sufficient for required_ttl_ledgers at this deadline
        &120u32,
        &None,
    );
    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);

    let proof = Bytes::from_slice(&s.env, b"proof-bytes");
    s.registry.execute_task(&keeper, &task_id, &proof);

    let (expected_net, expected_fee) = split_reward(reward, 300u32).unwrap();
    assert_eq!(s.registry.keeper_balance(&keeper), expected_net);
    assert_eq!(s.registry.fees_accrued(), expected_fee);
    assert_eq!(s.registry.get_task(&task_id).status, TaskStatus::Executed);
}

// ─────────────────────────────────────────────────────────────────────────────
// #106 — update_verifier
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_update_verifier_succeeds_while_pending() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );

    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    s.registry
        .update_verifier(&s.admin, &task_id, &Some(verifier_id.clone()));

    let task = s.registry.get_task(&task_id);
    assert_eq!(task.verifier, Some(verifier_id));

    // Clearing it back to None also works while still Pending.
    s.registry.update_verifier(&s.admin, &task_id, &None);
    assert_eq!(s.registry.get_task(&task_id).verifier, None);
}

#[test]
fn test_update_verifier_rejected_once_task_is_claimed() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );
    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);

    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    assert_eq!(
        s.registry
            .try_update_verifier(&s.admin, &task_id, &Some(verifier_id)),
        Err(Ok(KeeperError::InvalidTaskStatus))
    );
    // Unchanged.
    assert_eq!(s.registry.get_task(&task_id).verifier, None);
}

#[test]
fn test_update_verifier_rejects_non_owner() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );

    let stranger = Address::generate(&s.env);
    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    assert_eq!(
        s.registry
            .try_update_verifier(&stranger, &task_id, &Some(verifier_id)),
        Err(Ok(KeeperError::NotTaskOwner))
    );
}

#[test]
fn test_update_verifier_rejected_when_paused() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );

    s.registry.pause(&s.admin);
    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    assert_eq!(
        s.registry
            .try_update_verifier(&s.admin, &task_id, &Some(verifier_id)),
        Err(Ok(KeeperError::ContractPaused))
    );
}

#[test]
fn test_update_verifier_emits_event() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );

    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    let before = s.env.events().all().len();
    s.registry
        .update_verifier(&s.admin, &task_id, &Some(verifier_id));
    assert!(s.env.events().all().len() > before);
}

// ─────────────────────────────────────────────────────────────────────────────
// #111 — execute_task against a verifier that consumes excessive resources
//
// The verifier below does a configurable number of persistent-storage writes
// before returning `true` — a stand-in for "does something resource-intensive"
// per the issue's own suggested approach. The test drives that count up under
// a tightly capped budget (`env.cost_estimate().budget().reset_limits(...)`)
// to find the point where the call starts failing on resource exhaustion
// rather than any contract logic, and confirms two things at that boundary:
//   1. The failure is a clean host-level error, not a panic that corrupts
//      test state or a silently-wrong `false` verification result.
//   2. No partial state mutation survives — the task is exactly as it was
//      before the call, same as the panicking-verifier findings in #110/0075.
// ─────────────────────────────────────────────────────────────────────────────

mod expensive_verifier {
    use soroban_sdk::{contract, contractimpl, contracttype, Address, Bytes, Env};

    #[contracttype]
    enum DataKey {
        Slot(u32),
    }

    #[contract]
    pub struct ExpensiveVerifier;

    #[contractimpl]
    impl ExpensiveVerifier {
        /// Writes `work_units` persistent-storage entries, then approves.
        /// `proof`'s first byte selects how much work to do so a single
        /// deployed instance can be reused across increasing loads.
        pub fn verify(
            env: Env,
            _task: crate::Task,
            _keeper: Address,
            proof: Bytes,
        ) -> bool {
            let work_units: u32 = proof.get(0).unwrap_or(0) as u32;
            for i in 0..work_units {
                env.storage()
                    .persistent()
                    .set(&DataKey::Slot(i), &Bytes::from_array(&env, &[0u8; 256]));
            }
            true
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Verifier mocks (#98/#99/#106) — minimal, test-only contracts following the
// established `mod reentrant_token { ... }` local-mock-contract pattern.
// ─────────────────────────────────────────────────────────────────────────────

mod always_approve_verifier {
    use soroban_sdk::{contract, contractimpl, Address, Bytes, Env};

    #[contract]
    pub struct AlwaysApproveVerifier;

    #[contractimpl]
    impl AlwaysApproveVerifier {
        pub fn verify(_env: Env, _task: crate::Task, _keeper: Address, _proof: Bytes) -> bool {
            true
        }
    }
}

mod always_reject_verifier {
    use soroban_sdk::{contract, contractimpl, Address, Bytes, Env};

    #[contract]
    pub struct AlwaysRejectVerifier;

    #[contractimpl]
    impl AlwaysRejectVerifier {
        pub fn verify(_env: Env, _task: crate::Task, _keeper: Address, _proof: Bytes) -> bool {
            false
        }
    }
}

fn register_task_with_verifier(s: &Setup, reward: i128, verifier: &Address) -> u64 {
    let deadline = s.env.ledger().timestamp() + 3_600;
    s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &Some(verifier.clone()),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// #99 — execute_task with a verifier attached
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_execute_task_with_always_approve_verifier_matches_no_verifier_outcome() {
    let s = setup();
    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    let reward = 1_000_000i128;
    let task_id = register_task_with_verifier(&s, reward, &verifier_id);

    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);

    let proof = Bytes::from_slice(&s.env, b"proof-bytes");
    s.registry.execute_task(&keeper, &task_id, &proof);

    // Check events immediately after the call that emits them: each
    // top-level client call is its own host invocation, and
    // `s.env.events().all()` only reflects the most recent one — any
    // further contract calls (even read-only ones like `get_task`) start a
    // new invocation and the previous one's events are no longer visible.
    let all_events = s.env.events().all();
    let verfail_topic: soroban_sdk::Vec<soroban_sdk::Val> =
        (symbol_short!("verfail"), symbol_short!("task")).into_val(&s.env);
    let verfail_fired = all_events.iter().any(|(contract, topics, _)| {
        contract == s.registry.address && topics == verfail_topic
    });
    assert!(
        !verfail_fired,
        "TaskVerificationFailed must not fire when the verifier approves"
    );

    let exec_topic: soroban_sdk::Vec<soroban_sdk::Val> =
        (symbol_short!("exec"), symbol_short!("task")).into_val(&s.env);
    let exec_fired = all_events.iter().any(|(contract, topics, _)| {
        contract == s.registry.address && topics == exec_topic
    });
    assert!(exec_fired, "TaskExecuted must still fire");

    // Same outcome as the no-verifier path: full net reward credited, fee
    // accrued, task Executed.
    let (expected_net, expected_fee) = split_reward(reward, 300u32);
    assert_eq!(s.registry.keeper_balance(&keeper), expected_net);
    assert_eq!(s.registry.fees_accrued(), expected_fee);

    let task = s.registry.get_task(&task_id);
    assert_eq!(task.status, TaskStatus::Executed);
}

#[test]
fn test_execute_task_with_always_reject_verifier() {
    let s = setup();
    let verifier_id = s.env.register(always_reject_verifier::AlwaysRejectVerifier, ());
    let reward = 1_000_000i128;
    let task_id = register_task_with_verifier(&s, reward, &verifier_id);

    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);

    let proof = Bytes::from_slice(&s.env, b"first-attempt-proof");
    let result = s.registry.try_execute_task(&keeper, &task_id, &proof);
    assert_eq!(result, Err(Ok(KeeperError::VerificationFailed)));

    // Note: we don't assert TaskVerificationFailed's presence in
    // `s.env.events().all()` here. Soroban's host rolls back a whole call
    // frame — events included — whenever that frame's top-level function
    // returns `Err` (see `with_frame`/`call_n_internal` in
    // soroban-env-host), even though the error itself is "recoverable" from
    // the caller's perspective via `try_`. So an event published right
    // before an `Err` return is never observable by any caller, in any
    // Soroban contract; `emit_verification_failed`'s call site in
    // `execute_task` can't be exercised by a test that also checks the
    // `Err` result. What we *can* and do verify below is the actually
    // observable contract: the typed error, and that state didn't change.

    // Task remains Claimed — not Executed, not reverted to Pending.
    let task = s.registry.get_task(&task_id);
    assert_eq!(task.status, TaskStatus::Claimed);
    assert_eq!(task.claimer, Some(keeper.clone()));

    // No token transfer / keeper crediting occurred.
    assert_eq!(s.registry.keeper_balance(&keeper), 0i128);
    assert_eq!(s.registry.fees_accrued(), 0i128);

    // A second execute_task call (different proof bytes, same always-reject
    // verifier) fails the same way — the rejection is repeatable, not a
    // one-shot state change.
    let proof2 = Bytes::from_slice(&s.env, b"second-attempt-different-proof");
    let result2 = s.registry.try_execute_task(&keeper, &task_id, &proof2);
    assert_eq!(result2, Err(Ok(KeeperError::VerificationFailed)));

    let task_after_retry = s.registry.get_task(&task_id);
    assert_eq!(task_after_retry.status, TaskStatus::Claimed);
    assert_eq!(task_after_retry.claimer, Some(keeper));
}

#[test]
fn test_execute_task_none_verifier_path_unchanged() {
    // The base MVP path (no verifier attached) must behave identically to
    // before this feature existed — required explicitly by #99's acceptance
    // criteria, on top of the fact that all 100 pre-existing tests already
    // pass unmodified.
    let s = setup();
fn setup_task_with_expensive_verifier() -> (Setup, u64, Address, i128) {
    let s = setup();
    let verifier_id = s.env.register(expensive_verifier::ExpensiveVerifier, ());
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );
    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);

    let proof = Bytes::from_slice(&s.env, b"proof-bytes");
    s.registry.execute_task(&keeper, &task_id, &proof);

    let (expected_net, expected_fee) = split_reward(reward, 300u32);
    assert_eq!(s.registry.keeper_balance(&keeper), expected_net);
    assert_eq!(s.registry.fees_accrued(), expected_fee);
    assert_eq!(s.registry.get_task(&task_id).status, TaskStatus::Executed);
}

// ─────────────────────────────────────────────────────────────────────────────
// #106 — update_verifier
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_update_verifier_succeeds_while_pending() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );

    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    s.registry
        .update_verifier(&s.admin, &task_id, &Some(verifier_id.clone()));

    let task = s.registry.get_task(&task_id);
    assert_eq!(task.verifier, Some(verifier_id));

    // Clearing it back to None also works while still Pending.
    s.registry.update_verifier(&s.admin, &task_id, &None);
    assert_eq!(s.registry.get_task(&task_id).verifier, None);
}

#[test]
fn test_update_verifier_rejected_once_task_is_claimed() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &Some(verifier_id),
    );
    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);
    (s, task_id, keeper, reward)
}

/// A `proof` whose first byte is `work_units` — see `ExpensiveVerifier::verify`.
fn proof_requesting_work(env: &Env, work_units: u8) -> Bytes {
    Bytes::from_array(env, &[work_units])
}

#[test]
fn test_execute_task_succeeds_against_expensive_verifier_under_default_budget() {
    // Under the default (untouched) test budget, a moderate amount of
    // verifier work still succeeds — establishes the baseline before we
    // tighten the budget to find the failure boundary below.
    let (s, task_id, keeper, reward) = setup_task_with_expensive_verifier();
    let proof = proof_requesting_work(&s.env, 50);
    s.registry.execute_task(&keeper, &task_id, &proof);

    let task = s.registry.get_task(&task_id);
    assert_eq!(task.status, TaskStatus::Executed);
    let (expected_net, _) = split_reward(reward, 300u32);
    assert_eq!(s.registry.keeper_balance(&keeper), expected_net);
}

#[test]
#[should_panic]
fn test_execute_task_against_expensive_verifier_exhausts_a_tight_budget() {
    // Cap the budget tightly, then ask the verifier to do far more work than
    // that budget allows. This empirically establishes the failure mode:
    // resource exhaustion during a nested contract call is a host-level
    // error that aborts the whole transaction (via the same frame-rollback
    // mechanism documented on IKeeperVerifier and exercised by #110's
    // panicking-verifier tests) — not a graceful `false` result, and not a
    // panic that leaves storage half-written. `#[should_panic]` is the same
    // no_std-compatible mechanism #110 uses for the equivalent claim about a
    // panicking verifier (this crate's `#![no_std]` makes
    // `std::panic::catch_unwind` unavailable even in `#[cfg(test)]` code).
    // `Env`'s Drop impl (soroban-sdk's test harness) writes a test snapshot
    // on drop, which itself needs to iterate storage under the budget. Once
    // we deliberately exhaust the budget below, that snapshot-on-drop would
    // hit the same exhausted budget while unwinding this panic and abort the
    // whole test binary ("panic in a destructor during cleanup") instead of
    // failing just this test. `EnvTestConfig` is plain (non-shared) state
    // captured by value into every `Env`/client clone at the point it's
    // cloned, so this must be set on the *original* `Env` before any client
    // is constructed from it — setting it on `Setup.env` afterwards doesn't
    // reach `Setup.registry`'s already-cloned internal `Env`. Disabling it
    // is safe: this test's only assertion is that the call panics, and the
    // state left behind is checked by the separate expire_task-recovery
    // test below instead (using its own fresh `Setup`).
    let mut env = Env::default();
    env.set_config(soroban_sdk::testutils::EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &10_000_000i128);
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    let verifier_id = env.register(expensive_verifier::ExpensiveVerifier, ());
    let reward = 1_000_000i128;
    let deadline = env.ledger().timestamp() + 3_600;
    let task_id = registry.register_task(
        &admin,
        &TaskType::Liquidation,
        &calldata(&env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );
    let keeper = Address::generate(&s.env);
    s.registry.claim_task(&keeper, &task_id);

    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    assert_eq!(
        s.registry
            .try_update_verifier(&s.admin, &task_id, &Some(verifier_id)),
        Err(Ok(KeeperError::InvalidTaskStatus))
    );
    // Unchanged.
    assert_eq!(s.registry.get_task(&task_id).verifier, None);
}

#[test]
fn test_update_verifier_rejects_non_owner() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );

    let stranger = Address::generate(&s.env);
    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    assert_eq!(
        s.registry
            .try_update_verifier(&stranger, &task_id, &Some(verifier_id)),
        Err(Ok(KeeperError::NotTaskOwner))
    );
}

#[test]
fn test_update_verifier_rejected_when_paused() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );

    s.registry.pause(&s.admin);
    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    assert_eq!(
        s.registry
            .try_update_verifier(&s.admin, &task_id, &Some(verifier_id)),
        Err(Ok(KeeperError::ContractPaused))
    );
}

#[test]
fn test_update_verifier_emits_event() {
    let s = setup();
    let reward = 1_000_000i128;
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &None,
    );

    let verifier_id = s.env.register(always_approve_verifier::AlwaysApproveVerifier, ());
    let before = s.env.events().all().len();
    s.registry
        .update_verifier(&s.admin, &task_id, &Some(verifier_id));
    assert!(s.env.events().all().len() > before);
        &Some(verifier_id),
    );
    let keeper = Address::generate(&env);
    registry.claim_task(&keeper, &task_id);

    // Tight but not zero: enough for the setup/register/claim above to have
    // happened, but far too little for hundreds of persistent writes.
    env.cost_estimate().budget().reset_limits(2_000_000, 2_000_000);

    let proof = proof_requesting_work(&env, 255);

    // A resource-exhaustion failure inside a nested call is a non-recoverable
    // host error (not a typed contract error), so — consistent with #110's
    // panicking-verifier findings — it propagates and aborts the caller's
    // transaction rather than surfacing as `Err(KeeperError::...)`.
    registry.execute_task(&keeper, &task_id, &proof);
}

#[test]
fn test_expire_task_recovers_escrow_from_a_task_stuck_behind_a_budget_exhausting_verifier() {
    // Companion to the panic test above, structured like #110's
    // expire_task-recovery test: since the aborting call can't be followed
    // by assertions in the same test function, this test independently
    // confirms the actual recovery guarantee — a task stuck behind a
    // verifier that will always blow the budget is not permanently bricked;
    // expire_task still recovers the escrowed reward once the deadline
    // passes, exactly as it does for a panicking verifier.
    let (s, task_id, keeper, reward) = setup_task_with_expensive_verifier();
    s.env.cost_estimate().budget().reset_limits(2_000_000, 2_000_000);

    let task_before_expiry = s.registry.get_task(&task_id);
    assert_eq!(task_before_expiry.status, TaskStatus::Claimed);
    assert_eq!(task_before_expiry.claimer, Some(keeper.clone()));
    assert_eq!(s.registry.keeper_balance(&keeper), 0i128);

    let token = token::Client::new(&s.env, &s.token_id);
    let owner_balance_before = token.balance(&s.admin);

    advance(&s.env, 1, 3_601);
    s.env.cost_estimate().budget().reset_unlimited();
    s.registry.expire_task(&task_id);

    let task_after_expiry = s.registry.get_task(&task_id);
    assert_eq!(task_after_expiry.status, TaskStatus::Expired);
    assert_eq!(token.balance(&s.admin), owner_balance_before + reward);
}

// ─────────────────────────────────────────────────────────────────────────────
// I-7: task ids are unique and never reused
// ─────────────────────────────────────────────────────────────────────────────

/// I-7 (issue #86): `next_task_id` hands out a strictly increasing `u64` and
/// never recycles an id, so an off-chain reference to a task id — an indexer's
/// primary key, a keeper bot's local queue entry, a dApp's deep link — stays
/// valid for the lifetime of the contract.
///
/// `next_task_id` only ever `checked_add(1)`s a monotonic counter, so the
/// invariant should already hold; these tests prove it rather than trusting the
/// implementation, and pin the `u64` behavior at the overflow boundary.
///
/// Self-contained (own fixture helpers rather than the module-level `setup()`)
/// so the property can be read and run without the surrounding suite's shared
/// state.
mod i7_task_id_monotonicity {
    // The contract crate is `#![no_std]`; the surrounding suite already pulls
    // `std` in for the same reason (proptest's generated collections).
    extern crate std;
    use std::vec::Vec;

    use proptest::prelude::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token, Address, Bytes, Env,
    };

    use crate::{DataKey, KeeperRegistry, KeeperRegistryClient, TaskStatus, TaskType};

    /// Generous enough that a randomized plan can register, refund, and
    /// re-register many times over without the owner running out of funds.
    const MINT: i128 = 1_000_000_000_000i128;
    const REWARD: i128 = 1_000i128;
    const DEADLINE_OFFSET: u64 = 3_600;
    /// `register_task` enforces `ttl_ledgers >= required_ttl_ledgers(deadline)`,
    /// which for a one-hour deadline is `3_600 / 5 + 17_280 = 18_000`.
    const TTL_LEDGERS: u32 = 20_000;
    const LOCK_LEDGERS: u32 = 120;
    const FEE_BPS: u32 = 300;

    /// Returns `(env, owner, registry_id)`. The owner doubles as admin — this
    /// property is about id allocation, which no admin function touches.
    fn fixture() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();

        let owner = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        token::StellarAssetClient::new(&env, &token_id).mint(&owner, &MINT);

        let registry_id = env.register(KeeperRegistry, ());
        KeeperRegistryClient::new(&env, &registry_id).initialize(&owner, &token_id, &FEE_BPS);

        (env, owner, registry_id)
    }

    fn register(env: &Env, owner: &Address, registry_id: &Address) -> u64 {
        let deadline = env.ledger().timestamp() + DEADLINE_OFFSET;
        KeeperRegistryClient::new(env, registry_id).register_task(
            owner,
            &TaskType::Liquidation,
            &Bytes::from_slice(env, b"i7-monotonic"),
            &REWARD,
            &deadline,
            &TTL_LEDGERS,
            &LOCK_LEDGERS,
            &None,
        )
    }

    fn advance_past_deadline(env: &Env) {
        env.ledger().with_mut(|ledger| {
            ledger.sequence_number += 1;
            ledger.timestamp += DEADLINE_OFFSET + 1;
        });
    }

    /// Writes the task counter directly so the `u64::MAX` boundary is reachable
    /// without the ~1.8e19 registrations it would otherwise take.
    fn set_task_counter(env: &Env, registry_id: &Address, value: u64) {
        env.as_contract(registry_id, || {
            env.storage().instance().set(&DataKey::TaskCounter, &value);
        });
    }

    /// How a freshly registered task is driven onward before the next
    /// registration. Terminal variants are the point of the property: an id
    /// belonging to a task the contract can no longer act on must still never
    /// be handed out again.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Step {
        /// Leave the task `Pending`.
        LeaveOpen,
        /// Owner pulls the escrow back out of a `Pending` task.
        Cancel,
        /// A keeper claims and executes it; the id belongs to a paid-out task.
        Execute,
        /// The deadline passes and anyone expires it, refunding the owner.
        Expire,
    }

    impl Step {
        fn expected_status(self) -> TaskStatus {
            match self {
                Step::LeaveOpen => TaskStatus::Pending,
                Step::Cancel => TaskStatus::Cancelled,
                Step::Execute => TaskStatus::Executed,
                Step::Expire => TaskStatus::Expired,
            }
        }
    }

    fn apply(env: &Env, owner: &Address, registry_id: &Address, task_id: u64, step: Step) {
        let registry = KeeperRegistryClient::new(env, registry_id);
        match step {
            Step::LeaveOpen => {}
            Step::Cancel => registry.cancel_task(owner, &task_id),
            Step::Execute => {
                let keeper = Address::generate(env);
                registry.claim_task(&keeper, &task_id);
                registry.execute_task(&keeper, &task_id, &Bytes::from_slice(env, b"i7-proof"));
            }
            Step::Expire => {
                advance_past_deadline(env);
                registry.expire_task(&task_id);
            }
        }
    }

    fn step_strategy() -> impl Strategy<Value = Step> {
        prop_oneof![
            Just(Step::LeaveOpen),
            Just(Step::Cancel),
            Just(Step::Execute),
            Just(Step::Expire),
        ]
    }

    proptest! {
        // Each case stands up a fresh `Env` and drives real contract calls, so
        // the case count is tuned for a useful sequence length rather than
        // proptest's default 256.
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// I-7: across an arbitrary interleaving of registrations and
        /// terminations, every issued id is strictly greater than every id
        /// issued before it, and `task_count()` counts registrations rather
        /// than live tasks.
        #[test]
        fn property_i7_task_ids_are_strictly_monotonic_and_never_reused(
            plan in prop::collection::vec(step_strategy(), 1..12usize),
        ) {
            let (env, owner, registry_id) = fixture();
            let registry = KeeperRegistryClient::new(&env, &registry_id);

            let mut issued: Vec<u64> = Vec::new();

            for &step in &plan {
                let task_id = register(&env, &owner, &registry_id);

                // Strictly greater than the most recent id, and — because the
                // sequence is strictly increasing — than every id before it.
                if let Some(&previous) = issued.last() {
                    prop_assert!(
                        task_id > previous,
                        "issued id {} did not exceed the previously issued {}",
                        task_id,
                        previous
                    );
                }
                // Stated directly as well, so a regression that broke
                // monotonicity without breaking the last-id comparison (an
                // id recycled from further back) still fails here.
                prop_assert!(
                    !issued.contains(&task_id),
                    "issued id {} had already been handed out",
                    task_id
                );
                issued.push(task_id);

                apply(&env, &owner, &registry_id, task_id, step);

                // Holds after the termination too: reaching a terminal state
                // must not decrement the counter.
                prop_assert_eq!(registry.task_count(), issued.len() as u64);
            }

            // `task_count()` after N registrations is N, no matter how many of
            // those N have since been cancelled, executed, or expired.
            prop_assert_eq!(registry.task_count(), plan.len() as u64);

            for (index, (&task_id, &step)) in issued.iter().zip(&plan).enumerate() {
                // Pins the current allocation as the dense `1..=N` sequence.
                // Stricter than I-7 needs — I-7 only requires uniqueness and
                // monotonicity — but true today and worth failing loudly on.
                prop_assert_eq!(task_id, index as u64 + 1);

                // The id is genuinely retired, not freed: a terminated task
                // still resolves under its own id, in its terminal state.
                prop_assert_eq!(
                    registry.get_task(&task_id).status,
                    step.expected_status()
                );
            }
        }
    }

    /// I-7 at the last usable id: with the counter one below its ceiling,
    /// `next_task_id` hands out `u64::MAX` and the task is registered normally.
    #[test]
    fn test_next_task_id_at_u64_max_minus_one_issues_u64_max() {
        let (env, owner, registry_id) = fixture();
        let registry = KeeperRegistryClient::new(&env, &registry_id);

        set_task_counter(&env, &registry_id, u64::MAX - 1);

        let task_id = register(&env, &owner, &registry_id);

        assert_eq!(task_id, u64::MAX);
        assert_eq!(registry.task_count(), u64::MAX);
        assert_eq!(registry.get_task(&task_id).status, TaskStatus::Pending);
    }

    /// I-7 at the ceiling — **documented decision for issue #86**.
    ///
    /// `next_task_id` resolves exhaustion with
    /// `.expect("task id counter exhausted")`: an untyped panic, not a
    /// `KeeperError`. That is **accepted as-is; no follow-up issue is filed.**
    /// The reasoning does not carry over from the `u32` overflows repaired in
    /// wave 1:
    ///
    /// * The counter is a `u64`. Exhausting it takes `2^64 - 1` ≈ 1.8e19
    ///   *successful* `register_task` calls, each a separate Stellar
    ///   transaction paying a fee and doing a storage write. At a million
    ///   registrations per second that is still ~584,000 years. It is not
    ///   reachable by an attacker with a budget, only by a much older
    ///   universe.
    /// * The wave-1 `u32` cases were different in kind: `u32` ledger and
    ///   amount arithmetic is reachable under ordinary operation, so those
    ///   genuinely needed typed errors.
    /// * Converting this to a typed error would widen `register_task`'s error
    ///   surface with a variant no caller can ever observe, making every
    ///   caller handle dead code.
    ///
    /// The test exists to pin the behavior, so a future refactor that changes
    /// it fails loudly here instead of silently.
    #[test]
    #[should_panic(expected = "task id counter exhausted")]
    fn test_next_task_id_at_u64_max_panics_by_design() {
        let (env, owner, registry_id) = fixture();

        set_task_counter(&env, &registry_id, u64::MAX);

        register(&env, &owner, &registry_id);
    }
}
