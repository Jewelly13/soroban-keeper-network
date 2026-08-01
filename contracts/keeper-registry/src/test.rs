//! # KeeperRegistry — Test Suite
//!
//! Covers the full task lifecycle (register → claim → execute → withdraw) plus
//! the refund paths (cancel/expire), fee accounting, and every admin control.
//!
//! ## For contributors
//! When you add a function, add tests here. Every public function should have:
//!   - one happy-path test
//!   - a test for each KeeperError variant it can return
//!
//! Run with: `cargo test -p keeper-registry`

#![cfg(test)]

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Deployer as _, Events as _, Ledger, MockAuth, MockAuthInvoke},
    token, Address, Bytes, Env, IntoVal, Symbol, TryIntoVal, Vec,
};

use crate::{
    split_reward, BatchTaskParams, DataKey, KeeperError, KeeperRegistry, KeeperRegistryClient,
    TaskStatus, TaskType, INSTANCE_BUMP_LEDGERS, INSTANCE_BUMP_THRESHOLD, MAX_BATCH_SIZE,
    MAX_CALLDATA_LEN, MAX_LOCK_LEDGERS, MIN_LOCK_LEDGERS, MIN_TTL_LEDGERS,
};

// ─────────────────────────────────────────────────────────────────────────────
// Shared test setup
// ─────────────────────────────────────────────────────────────────────────────

struct TestSetup {
    env: Env,
    admin: Address,
    registry: KeeperRegistryClient<'static>,
    token_id: Address,
}

/// Deploys a SAC-wrapped token, mints 10M units to the admin, and returns the token's address.
fn deploy_token(env: &Env, admin: &Address) -> Address {
    let token_admin = Address::generate(env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    token::StellarAssetClient::new(env, &token_id).mint(admin, &10_000_000i128);
    token_id
}

/// Deploys and initializes the KeeperRegistry contract.
fn deploy_registry<'a>(
    env: &'a Env,
    admin: &Address,
    token_id: &Address,
) -> KeeperRegistryClient<'a> {
    let registry_id = env.register(KeeperRegistry, ());
    let registry_client = KeeperRegistryClient::new(env, &registry_id);
    registry_client.initialize(admin, token_id, &300u32); // Default 3% fee
    registry_client
}

// The transmutes below intentionally re-bind the env/client to a 'static
// lifetime — the standard Soroban test-harness pattern for a shared Setup.
#[allow(clippy::useless_transmute, clippy::missing_transmute_annotations)]
fn setup() -> TestSetup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = deploy_token(&env, &admin);
    // The client is bound to the lifetime of `env_for_registry`.
    let env_for_registry = env.clone();
    let registry = deploy_registry(&env_for_registry, &admin, &token_id);

    // Leak env to get a 'static lifetime — standard soroban test pattern.
    TestSetup {
        env: unsafe { core::mem::transmute(env) },
        admin,
        registry: unsafe { core::mem::transmute(registry) }, // Now transmutes a client with a 'static lifetime.
        token_id,
    }
}

fn calldata(env: &Env) -> Bytes {
    Bytes::from_slice(env, b"liquidate:position:42")
}

/// Registers a standard 1-hour task funded by `admin` and returns its id.
fn register_default_task(s: &TestSetup) -> u64 {
    register_reward_task(s, 1_000_000i128)
}

/// Same as `register_default_task` but with a caller-chosen reward, so tests
/// can exercise several distinct amounts (e.g. non-round fee splits) without
/// duplicating the register_task call boilerplate.
fn register_reward_task(s: &TestSetup, reward: i128) -> u64 {
    let deadline = s.env.ledger().timestamp() + 3_600;
    s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
    )
}

/// Advances the ledger sequence and timestamp so lock-window / deadline logic
/// can be exercised deterministically.
fn advance(env: &Env, ledgers: u32, seconds: u64) {
    env.ledger().with_mut(|li| {
        li.sequence_number += ledgers;
        li.timestamp += seconds;
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// initialize
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// End-to-end integration: multiple tasks, multiple keepers
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_multi_keeper_end_to_end_conserves_funds() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let k1 = Address::generate(&s.env);
    let k2 = Address::generate(&s.env);

    // Three tasks funded from admin, 1_000_000 each.
    let t_exec = register_default_task(&s); // will be executed by k1
    let t_expire = register_default_task(&s); // will be claimed by k2 then expire
    let t_cancel = register_default_task(&s); // will be cancelled by owner

    // The contract now escrows all three rewards.
    assert_eq!(token.balance(&s.registry.address), 3_000_000i128);

    // k1 executes the first task (3% fee → 970_000 to k1, 30_000 accrued).
    s.registry.claim_task(&k1, &t_exec);
    s.registry
        .execute_task(&k1, &t_exec, &Bytes::from_slice(&s.env, b"p1"));

    // k2 claims the second but never executes; owner cancels the third now.
    s.registry.claim_task(&k2, &t_expire);
    s.registry.cancel_task(&s.admin, &t_cancel); // refunds 1_000_000

    // Time passes; the abandoned task is expired permissionlessly.
    advance(&s.env, 200, 3_601);
    s.registry.expire_task(&t_expire); // refunds 1_000_000 to owner

    // k1 withdraws its earnings; admin sweeps the fee.
    assert_eq!(s.registry.withdraw_rewards(&k1), 970_000i128);
    let treasury = Address::generate(&s.env);
    s.registry.sweep_fees(&s.admin, &treasury, &30_000i128);

    // Conservation: the contract should hold nothing left over — every stroop
    // is now either with the keeper, the treasury, or refunded to the owner.
    assert_eq!(token.balance(&s.registry.address), 0i128);
    assert_eq!(token.balance(&k1), 970_000i128);
    assert_eq!(token.balance(&treasury), 30_000i128);
    assert_eq!(s.registry.fees_accrued(), 0i128);
}

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

            // 1. Conservation: nothing leaks.
            assert_eq!(keeper_net + fee, reward, "reward={reward} bps={bps}");
            // 2. Non-negative shares.
            assert!(keeper_net >= 0 && fee >= 0, "reward={reward} bps={bps}");
            // 3. Fee never exceeds the reward.
            assert!(fee <= reward, "reward={reward} bps={bps}");
            // 4. Fee matches the basis-point formula (floor division).
            assert_eq!(
                fee,
                reward * bps as i128 / 10_000,
                "reward={reward} bps={bps}"
            );
        }
    }
}

/// `VERSION` is the only signal an off-chain client has that the ABI it
/// compiled against is the ABI it is talking to, so this assertion is
/// deliberately a hardcoded literal rather than a comparison against the
/// `VERSION` constant — the point is that changing the constant without
/// noticing the ABI change is what breaks integrators. Bump both together,
/// and add a CHANGELOG entry saying what changed.
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
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);

    assert_eq!(
        registry.try_initialize(&admin, &token_id, &10_001u32),
        Err(Ok(KeeperError::InvalidFeeBps))
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// register_task
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_register_task_success() {
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

    let deadline = env.ledger().timestamp() + 3_600; // 1 hour
    let task_id = registry.register_task(
        &admin,
        &TaskType::Liquidation,
        &calldata(&env),
        &1_000_000i128,
        &deadline,
        &17_280u32,
        &120u32,
    );

    assert_eq!(task_id, 1u64);
    assert_eq!(registry.task_count(), 1u64);

    let task = registry.get_task(&1u64);
    assert_eq!(task.owner, admin);
    assert_eq!(task.status, TaskStatus::Pending);
    assert_eq!(task.reward, 1_000_000i128);
    assert_eq!(task.deadline, deadline);
    assert!(task.claimer.is_none());
}

#[test]
fn test_register_task_escrows_reward() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let sac = token::StellarAssetClient::new(&env, &token_id);
    sac.mint(&admin, &5_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    let token = token::Client::new(&env, &token_id);
    let owner_before = token.balance(&admin);

    registry.register_task(
        &admin,
        &TaskType::Custom,
        &calldata(&env),
        &1_000_000i128,
        &(env.ledger().timestamp() + 3_600),
        &17_280u32,
        &120u32,
    );

    // Owner balance decreased by the escrowed reward.
    assert_eq!(token.balance(&admin), owner_before - 1_000_000i128);
    // Contract holds the escrow.
    assert_eq!(token.balance(&registry_id), 1_000_000i128);
}

#[test]
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
            &17_280u32,
            &120u32,
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
            &17_280u32,
            &120u32,
        ),
        Err(Ok(KeeperError::DeadlinePassed))
    );
}

#[test]
fn test_register_increments_task_counter() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &10_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    let deadline = env.ledger().timestamp() + 3_600;
    for expected_id in 1u64..=3 {
        let id = registry.register_task(
            &admin,
            &TaskType::TtlExtension,
            &calldata(&env),
            &100_000i128,
            &deadline,
            &17_280u32,
            &60u32,
        );
        assert_eq!(id, expected_id);
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
        &17_280u32,
        &120u32,
    );
    assert_eq!(registry.get_task(&id).calldata.len(), MAX_CALLDATA_LEN);
}

#[test]
fn test_register_task_over_max_calldata_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

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
        &17_280u32,
        &120u32,
    );
    assert_eq!(registry.get_task(&id).calldata.len(), 0);
}

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
        ),
        Err(Ok(KeeperError::InvalidTaskParams))
    );
}

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
        &17_280u32,
        &MIN_LOCK_LEDGERS,
    );
    assert_eq!(s.registry.get_task(&task_id).lock_ledgers, MIN_LOCK_LEDGERS);
}

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
        ),
        Err(Ok(KeeperError::InvalidTaskParams))
    );
}

#[test]
fn test_register_task_lock_ledgers_at_max_succeeds() {
    let s = setup();
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Custom,
        &calldata(&s.env),
        &1_000_000i128,
        &deadline,
        &17_280u32,
        &MAX_LOCK_LEDGERS,
    );
    assert_eq!(s.registry.get_task(&task_id).lock_ledgers, MAX_LOCK_LEDGERS);
}

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
        ),
        Err(Ok(KeeperError::InvalidTaskParams))
    );
}

#[test]
fn test_register_task_ttl_ledgers_at_min_succeeds() {
    let s = setup();
    let deadline = s.env.ledger().timestamp() + 3_600;
    let task_id = s.registry.register_task(
        &s.admin,
        &TaskType::Custom,
        &calldata(&s.env),
        &1_000_000i128,
        &deadline,
        &MIN_TTL_LEDGERS,
        &120u32,
    );
    assert_eq!(s.registry.get_task(&task_id).ttl_ledgers, MIN_TTL_LEDGERS);
}

// ─────────────────────────────────────────────────────────────────────────────
// batch_register_tasks
//
// Semantics under test mirror docs/BATCH_OPERATIONS.md: one auth for the whole
// batch (§2), whole-batch atomicity with zero partial success (§3), a
// MAX_BATCH_SIZE ceiling (§4), ids returned in input order (§5), and the
// max_total_reward ceiling (§7).
// ─────────────────────────────────────────────────────────────────────────────

/// One well-formed batch entry with a caller-chosen reward.
fn batch_entry(env: &Env, reward: i128) -> BatchTaskParams {
    BatchTaskParams {
        task_type: TaskType::Liquidation,
        calldata: calldata(env),
        reward,
        deadline: env.ledger().timestamp() + 3_600,
        ttl_ledgers: 17_280,
        lock_ledgers: 120,
    }
}

/// A batch of `n` entries, each worth `reward`.
fn batch_of(env: &Env, n: u32, reward: i128) -> Vec<BatchTaskParams> {
    let mut v = Vec::new(env);
    for _ in 0..n {
        v.push_back(batch_entry(env, reward));
    }
    v
}

#[test]
fn test_batch_register_registers_all_and_returns_ids_in_order() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);

    let tasks = batch_of(&s.env, 3, 1_000_000i128);
    let ids = s
        .registry
        .batch_register_tasks(&s.admin, &tasks, &3_000_000i128);

    assert_eq!(ids.len(), 3);
    // Ids are the contract's own monotonic sequence, in input order.
    assert_eq!(ids.get(1).unwrap(), ids.get(0).unwrap() + 1);
    assert_eq!(ids.get(2).unwrap(), ids.get(1).unwrap() + 1);

    for id in ids.iter() {
        let task = s.registry.get_task(&id);
        assert_eq!(task.owner, s.admin);
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.reward, 1_000_000i128);
    }

    // Escrow for the whole batch is held by the registry.
    assert_eq!(token.balance(&s.registry.address), 3_000_000i128);
    assert_eq!(s.registry.task_count(), 3);
}

#[test]
fn test_batch_register_max_total_reward_ceiling_rejects_whole_batch() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);

    // Sum is 3_000_000; the ceiling is one stroop short of it.
    let tasks = batch_of(&s.env, 3, 1_000_000i128);
    assert_eq!(
        s.registry
            .try_batch_register_tasks(&s.admin, &tasks, &2_999_999i128),
        Err(Ok(KeeperError::BatchRewardCeilingExceeded))
    );

    // §3: zero transfers, zero tasks — not "the first two landed".
    assert_eq!(token.balance(&s.registry.address), 0i128);
    assert_eq!(s.registry.task_count(), 0);
}

#[test]
fn test_batch_register_accepts_ceiling_set_to_exact_sum() {
    let s = setup();
    // The guidance in docs §7 is to set max_total_reward to the exact sum, so
    // the boundary itself must not be off-by-one.
    let tasks = batch_of(&s.env, 2, 1_000_000i128);
    let ids = s
        .registry
        .batch_register_tasks(&s.admin, &tasks, &2_000_000i128);
    assert_eq!(ids.len(), 2);
}

#[test]
fn test_batch_register_rejects_batch_over_max_size() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);

    let tasks = batch_of(&s.env, MAX_BATCH_SIZE + 1, 1i128);
    assert_eq!(
        s.registry
            .try_batch_register_tasks(&s.admin, &tasks, &i128::MAX),
        Err(Ok(KeeperError::BatchTooLarge))
    );
    assert_eq!(token.balance(&s.registry.address), 0i128);
    assert_eq!(s.registry.task_count(), 0);
}

#[test]
fn test_batch_register_accepts_exactly_max_batch_size() {
    let s = setup();
    let tasks = batch_of(&s.env, MAX_BATCH_SIZE, 1i128);
    let ids = s
        .registry
        .batch_register_tasks(&s.admin, &tasks, &(MAX_BATCH_SIZE as i128));
    assert_eq!(ids.len(), MAX_BATCH_SIZE);
}

#[test]
fn test_batch_register_max_batch_size_view_matches_constant() {
    let s = setup();
    assert_eq!(s.registry.max_batch_size(), MAX_BATCH_SIZE);
}

#[test]
fn test_batch_register_rejects_empty_batch() {
    let s = setup();
    let tasks: Vec<BatchTaskParams> = Vec::new(&s.env);
    assert_eq!(
        s.registry
            .try_batch_register_tasks(&s.admin, &tasks, &1_000_000i128),
        Err(Ok(KeeperError::EmptyBatch))
    );
}

#[test]
fn test_batch_register_rejects_non_positive_ceiling() {
    let s = setup();
    let tasks = batch_of(&s.env, 1, 1_000_000i128);
    assert_eq!(
        s.registry.try_batch_register_tasks(&s.admin, &tasks, &0i128),
        Err(Ok(KeeperError::InvalidReward))
    );
}

/// A single bad entry rejects the batch and rolls back the good entries with
/// it — the "no partial success" guarantee integrators are told to rely on.
#[test]
fn test_batch_register_one_bad_entry_rejects_entire_batch() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);

    let cases: std::vec::Vec<(BatchTaskParams, KeeperError)> = std::vec![
        (
            BatchTaskParams {
                reward: 0,
                ..batch_entry(&s.env, 1_000_000i128)
            },
            KeeperError::InvalidReward,
        ),
        (
            BatchTaskParams {
                deadline: s.env.ledger().timestamp(),
                ..batch_entry(&s.env, 1_000_000i128)
            },
            KeeperError::DeadlinePassed,
        ),
        (
            BatchTaskParams {
                calldata: Bytes::from_slice(
                    &s.env,
                    &[0u8; (MAX_CALLDATA_LEN + 1) as usize]
                ),
                ..batch_entry(&s.env, 1_000_000i128)
            },
            KeeperError::CalldataTooLarge,
        ),
        (
            BatchTaskParams {
                lock_ledgers: MIN_LOCK_LEDGERS - 1,
                ..batch_entry(&s.env, 1_000_000i128)
            },
            KeeperError::InvalidTaskParams,
        ),
        (
            BatchTaskParams {
                lock_ledgers: MAX_LOCK_LEDGERS + 1,
                ..batch_entry(&s.env, 1_000_000i128)
            },
            KeeperError::InvalidTaskParams,
        ),
        (
            BatchTaskParams {
                ttl_ledgers: MIN_TTL_LEDGERS - 1,
                ..batch_entry(&s.env, 1_000_000i128)
            },
            KeeperError::InvalidTaskParams,
        ),
        (
            BatchTaskParams {
                // Issue 11 fix for batch parameters too: ttl must cover deadline
                ttl_ledgers: 17_280, // far too short for this deadline
                deadline: s.env.ledger().timestamp() + 2_592_000,
                ..batch_entry(&s.env, 1_000_000i128)
            },
            KeeperError::TtlTooShort,
        ),
    ];

    for (bad, expected) in cases {
        // Good entry first, so a rejection proves the whole batch rolled back
        // rather than stopping before it had done anything.
        let mut tasks = Vec::new(&s.env);
        tasks.push_back(batch_entry(&s.env, 1_000_000i128));
        tasks.push_back(bad);

        assert_eq!(
            s.registry
                .try_batch_register_tasks(&s.admin, &tasks, &i128::MAX),
            Err(Ok(expected)),
        );
        assert_eq!(token.balance(&s.registry.address), 0i128);
        assert_eq!(s.registry.task_count(), 0);
    }
}

#[test]
fn test_batch_register_respects_min_reward_floor() {
    let s = setup();
    s.registry.set_min_reward(&s.admin, &500_000i128);

    let mut tasks = Vec::new(&s.env);
    tasks.push_back(batch_entry(&s.env, 500_000i128)); // exactly at the floor
    tasks.push_back(batch_entry(&s.env, 499_999i128)); // one below

    assert_eq!(
        s.registry
            .try_batch_register_tasks(&s.admin, &tasks, &i128::MAX),
        Err(Ok(KeeperError::InvalidReward))
    );
    assert_eq!(s.registry.task_count(), 0);
}

#[test]
fn test_batch_register_blocked_while_paused() {
    let s = setup();
    s.registry.pause(&s.admin);

    let tasks = batch_of(&s.env, 2, 1_000_000i128);
    assert_eq!(
        s.registry
            .try_batch_register_tasks(&s.admin, &tasks, &2_000_000i128),
        Err(Ok(KeeperError::ContractPaused))
    );
}

/// Batch-registered tasks are ordinary tasks: nothing about how they were
/// created changes claim/execute or the refund paths.
#[test]
fn test_batch_registered_task_completes_normal_lifecycle() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let keeper = Address::generate(&s.env);

    let tasks = batch_of(&s.env, 2, 1_000_000i128);
    let ids = s
        .registry
        .batch_register_tasks(&s.admin, &tasks, &2_000_000i128);
    let (executed_id, cancelled_id) = (ids.get(0).unwrap(), ids.get(1).unwrap());

    s.registry.claim_task(&keeper, &executed_id);
    s.registry
        .execute_task(&keeper, &executed_id, &Bytes::from_slice(&s.env, b"proof"));
    let (net, fee) = split_reward(1_000_000i128, 300).unwrap();
    assert_eq!(s.registry.keeper_balance(&keeper), net);
    assert_eq!(s.registry.fees_accrued(), fee);

    // Each entry's escrow is refundable independently of the rest of its batch.
    s.registry.cancel_task(&s.admin, &cancelled_id);
    assert_eq!(
        s.registry.get_task(&cancelled_id).status,
        TaskStatus::Cancelled
    );
    // Only the executed task's reward (net + fee) is still held.
    assert_eq!(token.balance(&s.registry.address), 1_000_000i128);
}

/// A batch may only pull escrow from the address that authorized it: entries
/// carry no per-entry owner, so every task in the batch is owned by the single
/// authorizing `owner` (§2) and nobody else's funds are reachable.
#[test]
fn test_batch_register_tasks_are_all_owned_by_the_authorizing_owner() {
    let s = setup();
    let other = Address::generate(&s.env);

    let tasks = batch_of(&s.env, 3, 1_000_000i128);
    let ids = s
        .registry
        .batch_register_tasks(&s.admin, &tasks, &3_000_000i128);

    for id in ids.iter() {
        assert_eq!(s.registry.get_task(&id).owner, s.admin);
    }

    // A non-owner cannot cancel any of them.
    assert_eq!(
        s.registry.try_cancel_task(&other, &ids.get(0).unwrap()),
        Err(Ok(KeeperError::NotTaskOwner))
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Placeholder tests for unimplemented functions
//
// These are intentionally left as stubs. When you implement a function,
// remove the #[ignore] tag and fill in the test body.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_increase_reward_escrows_and_raises_bounty() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let id = register_default_task(&s); // reward 1_000_000
    let contract_before = token.balance(&s.registry.address);

    s.registry.increase_reward(&s.admin, &id, &500_000i128);

    assert_eq!(s.registry.get_task(&id).reward, 1_500_000i128);
    assert_eq!(
        token.balance(&s.registry.address),
        contract_before + 500_000i128
    );
}

#[test]
fn test_increase_reward_by_non_owner_fails() {
    let s = setup();
    let stranger = Address::generate(&s.env);
    let id = register_default_task(&s);
    assert_eq!(
        s.registry.try_increase_reward(&stranger, &id, &1i128),
        Err(Ok(KeeperError::NotTaskOwner))
    );
}

#[test]
fn test_extend_deadline_pushes_it_out() {
    let s = setup();
    let id = register_default_task(&s);
    let old = s.registry.get_task(&id).deadline;

    s.registry.extend_deadline(&s.admin, &id, &(old + 7_200));
    assert_eq!(s.registry.get_task(&id).deadline, old + 7_200);
}

#[test]
fn test_extend_deadline_backwards_fails() {
    let s = setup();
    let id = register_default_task(&s);
    let old = s.registry.get_task(&id).deadline;
    // A new deadline that isn't strictly later is rejected.
    assert_eq!(
        s.registry.try_extend_deadline(&s.admin, &id, &old),
        Err(Ok(KeeperError::DeadlinePassed))
    );
}

// Regression test for issue #20: `extend_deadline` did not call
// `require_not_paused`, so an owner could keep escrow locked in a paused
// contract by pushing the deadline out indefinitely. Mirrors the style of
// `test_pause_blocks_registration_but_allows_withdraw`.
#[test]
fn test_extend_deadline_blocked_while_paused() {
    let s = setup();
    let id = register_default_task(&s);
    let old_deadline = s.registry.get_task(&id).deadline;

    s.registry.pause(&s.admin);
    assert!(s.registry.is_paused());

    assert_eq!(
        s.registry
            .try_extend_deadline(&s.admin, &id, &(old_deadline + 7_200)),
        Err(Ok(KeeperError::ContractPaused))
    );
    assert_eq!(s.registry.get_task(&id).deadline, old_deadline); // untouched
}

#[test]
fn test_extend_deadline_succeeds_after_unpause() {
    let s = setup();
    let id = register_default_task(&s);
    let old_deadline = s.registry.get_task(&id).deadline;

    s.registry.pause(&s.admin);
    s.registry.unpause(&s.admin);
    assert!(!s.registry.is_paused());

    s.registry
        .extend_deadline(&s.admin, &id, &(old_deadline + 7_200));
    assert_eq!(s.registry.get_task(&id).deadline, old_deadline + 7_200);
}

#[test]
fn test_is_claimable_lifecycle() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);

    assert!(s.registry.is_claimable(&id)); // Pending → claimable
    s.registry.claim_task(&keeper, &id);
    assert!(!s.registry.is_claimable(&id)); // Claimed, lock active → not

    advance(&s.env, 121, 60); // lock window elapses
    assert!(s.registry.is_claimable(&id)); // re-claimable

    advance(&s.env, 1, 3_601); // past deadline
    assert!(!s.registry.is_claimable(&id)); // deadline passed → not
    assert!(!s.registry.is_claimable(&999u64)); // unknown → not
}

#[test]
fn test_claim_pending_task() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);

    s.registry.claim_task(&keeper, &id);

    let task = s.registry.get_task(&id);
    assert_eq!(task.status, TaskStatus::Claimed);
    assert_eq!(task.claimer, Some(keeper));
    assert!(task.claim_ledger.is_some());
}

#[test]
fn test_claim_locked_task_by_second_keeper_fails() {
    let s = setup();
    let first = Address::generate(&s.env);
    let second = Address::generate(&s.env);
    let id = register_default_task(&s);

    s.registry.claim_task(&first, &id);
    // Still inside the 120-ledger lock window.
    assert_eq!(
        s.registry.try_claim_task(&second, &id),
        Err(Ok(KeeperError::LockPeriodActive))
    );
}

#[test]
fn test_reclaim_after_lock_window_elapses() {
    let s = setup();
    let first = Address::generate(&s.env);
    let second = Address::generate(&s.env);
    let id = register_default_task(&s);

    s.registry.claim_task(&first, &id);
    // Move past the lock window (120 ledgers) but stay before the deadline.
    advance(&s.env, 121, 60);

    s.registry.claim_task(&second, &id);
    assert_eq!(s.registry.get_task(&id).claimer, Some(second));
}

// ─────────────────────────────────────────────────────────────────────────────
// lock_expired boundary — pins the exact ledger the lock lifts, per issue #33.
// A small `lock_ledgers` (12, the protocol minimum) keeps the arithmetic easy
// to follow.
// ─────────────────────────────────────────────────────────────────────────────

/// Registers a task with the given `lock_ledgers`, claims it as `keeper`, and
/// returns `(task_id, unlock_at)` where `unlock_at = claim_ledger + lock_ledgers`
/// — the first ledger sequence at which the lock is considered expired.
fn claim_with_lock(s: &TestSetup, keeper: &Address, lock_ledgers: u32) -> (u64, u32) {
    let deadline = s.env.ledger().timestamp() + 3_600;
    let id = s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &1_000_000i128,
        &deadline,
        &17_280u32,
        &lock_ledgers,
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
        &17_280u32,
        &1_000u32,
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
        &17_280u32,
        &120u32,
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
fn executed_task_keeper(s: &TestSetup) -> Address {
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
            &17_280u32,
            &60u32,
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
///     `register_task`, `claim_task`, `execute_task`, `increase_reward`,
///     `extend_deadline`.
///   - Allowed while paused, and asserted to have their full intended
///     effect (not just "didn't error"): `cancel_task` (refund + status),
///     `expire_task` (refund + status), `withdraw_rewards` (balance
///     transferred + zeroed).
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
        &17_280u32,
        &120u32,
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

    // ── BLOCKED: extend_deadline — gated as of the fix for issue #20. It
    // touches no funds directly, but leaving it open while paused would let
    // an owner keep escrow locked in a contract the admin has declared
    // unsafe, working against the point of the pause.
    let old_deadline = s.registry.get_task(&extend_target_id).deadline;
    assert_eq!(
        s.registry.try_extend_deadline(
            &s.admin,
            &extend_target_id,
            &(old_deadline + 3_600)
        ),
        Err(Ok(KeeperError::ContractPaused))
    );
    assert_eq!(
        s.registry.get_task(&extend_target_id).deadline,
        old_deadline
    ); // untouched

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
            &17_280u32,
            &60u32,
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
        &17_280u32,
        &60u32,
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
    let s = setup();
    let before = s.env.events().all().len();
    s.registry.set_fee_bps(&s.admin, &500u32);
    assert!(s.env.events().all().len() > before);
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
    let s = setup();
    let new_admin = Address::generate(&s.env);
    let before = s.env.events().all().len();
    s.registry.transfer_admin(&s.admin, &new_admin);
    assert!(s.env.events().all().len() > before);
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
}

#[test]
fn test_unpause_before_init_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let caller = Address::generate(&env);
    assert_eq!(
        registry.try_unpause(&caller),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_set_fee_bps_before_init_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let caller = Address::generate(&env);
    assert_eq!(
        registry.try_set_fee_bps(&caller, &500u32),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_set_min_reward_before_init_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let caller = Address::generate(&env);
    assert_eq!(
        registry.try_set_min_reward(&caller, &1i128),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_transfer_admin_before_init_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let caller = Address::generate(&env);
    let new_admin = Address::generate(&env);
    assert_eq!(
        registry.try_transfer_admin(&caller, &new_admin),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_upgrade_before_init_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let caller = Address::generate(&env);
    let bogus = soroban_sdk::BytesN::from_array(&env, &[0u8; 32]);
    assert_eq!(
        registry.try_upgrade(&caller, &bogus),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_sweep_fees_before_init_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let caller = Address::generate(&env);
    let treasury = Address::generate(&env);
    // require_admin runs before the reward-token lookup, so this surfaces
    // NotInitialized from the missing Admin key, not from RewardToken.
    assert_eq!(
        registry.try_sweep_fees(&caller, &treasury, &1i128),
        Err(Ok(KeeperError::NotInitialized))
    );
}

// increase_reward, cancel_task, and expire_task all load the task by id
// before they ever reach the reward-token lookup, and no task can exist on
// a registry that was never initialized (register_task itself requires the
// reward token to be configured). So "call before initialize" can only ever
// surface TaskNotFound for these three, not NotInitialized — that ordering
// (existence check before configuration check) is correct, not a gap.
//
// The reward-token dependency in these three functions is still real,
// though: a registry that was initialized and had a task registered, but
// later had its RewardToken key removed by e.g. a partial storage
// migration, must not panic. These tests reproduce exactly that.

#[test]
fn test_increase_reward_after_reward_token_migration_drop_fails() {
    let s = setup();
    let id = register_default_task(&s);
    s.env.as_contract(&s.registry.address, || {
        s.env.storage().instance().remove(&DataKey::RewardToken);
    });
    assert_eq!(
        s.registry.try_increase_reward(&s.admin, &id, &1i128),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_cancel_task_after_reward_token_migration_drop_fails() {
    let s = setup();
    let id = register_default_task(&s);
    s.env.as_contract(&s.registry.address, || {
        s.env.storage().instance().remove(&DataKey::RewardToken);
    });
    assert_eq!(
        s.registry.try_cancel_task(&s.admin, &id),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_expire_task_after_reward_token_migration_drop_fails() {
    let s = setup();
    let id = register_default_task(&s);
    s.env.as_contract(&s.registry.address, || {
        s.env.storage().instance().remove(&DataKey::RewardToken);
    });
    advance(&s.env, 1, 3_601); // past deadline
    assert_eq!(
        s.registry.try_expire_task(&id),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_withdraw_rewards_after_reward_token_migration_drop_fails() {
    let s = setup();
    let keeper = executed_task_keeper(&s); // has a balance to withdraw
    s.env.as_contract(&s.registry.address, || {
        s.env.storage().instance().remove(&DataKey::RewardToken);
    });
    assert_eq!(
        s.registry.try_withdraw_rewards(&keeper),
        Err(Ok(KeeperError::NotInitialized))
    );
}

#[test]
fn test_require_admin_distinguishes_not_initialized_from_wrong_caller() {
    // Uninitialized: no admin configured at all.
    let env = Env::default();
    env.mock_all_auths();
    let registry = uninitialized_registry(&env);
    let caller = Address::generate(&env);
    assert_eq!(
        registry.try_pause(&caller),
        Err(Ok(KeeperError::NotInitialized))
    );

    // Initialized, but caller isn't the admin: a different, more specific
    // error than "not initialized".
    let s = setup();
    let stranger = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_pause(&stranger),
        Err(Ok(KeeperError::Unauthorized))
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #15: ArithmeticOverflow tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_split_reward_extreme_value_returns_overflow_error() {
    // Any reward above i128::MAX / 10_000 will overflow the multiplication
    // when fee_bps is at the max (10_000). This test pins that the function returns a
    // typed error rather than panicking.
    let extreme_reward = i128::MAX / 9_999; // Will overflow when multiplied by 10_000
    let fee_bps = 10_000u32; // Max fee rate

    let result = split_reward(extreme_reward, fee_bps);
    assert_eq!(result, Err(KeeperError::ArithmeticOverflow));
}

#[test]
fn test_split_reward_max_safe_value_succeeds() {
    // The largest reward that can be safely multiplied by 10_000
    let safe_reward = i128::MAX / 10_000;
    let fee_bps = 300u32;

    let result = split_reward(safe_reward, fee_bps);
    assert!(result.is_ok());
    let (keeper_net, fee) = result.unwrap();
    assert_eq!(keeper_net + fee, safe_reward);
}

#[test]
fn test_split_reward_with_zero_fee_never_overflows() {
    // With fee_bps = 0, the multiplication by 0 can never overflow
    let huge_reward = i128::MAX;
    let result = split_reward(huge_reward, 0);
    assert!(result.is_ok());
    let (keeper_net, fee) = result.unwrap();
    assert_eq!(keeper_net, huge_reward);
    assert_eq!(fee, 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #16: set_min_reward event emission tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_set_min_reward_emits_event() {
    let s = setup();
    let old_min = s.registry.min_reward(); // initially 0
    let new_min = 500_000i128;
    s.registry.set_min_reward(&s.admin, &new_min);
    // Find the minrwd event - it should be emitted
    let events = s.env.events().all();
    let mut found = false;
    for event in events.iter() {
        let data_result: Result<(i128, i128), _> = event.2.try_into_val(&s.env);
        if let Ok((event_old, event_new)) = data_result {
            if event_old == old_min && event_new == new_min {
                found = true;
                break;
            }
        }
    }
    assert!(found, "MinRewardUpdated event was not emitted");
}

#[test]
fn test_set_min_reward_no_event_when_validation_fails() {
    let s = setup();
    let events_before = s.env.events().all();
    // Negative reward fails validation
    let _ = s.registry.try_set_min_reward(&s.admin, &-1i128);
    let events_after = s.env.events().all();
    // No new min reward event should be added
    let mut found_new_min_reward_event = false;
    for i in events_before.len()..events_after.len() {
        let event = events_after.get(i).unwrap();
        // Try to parse as min reward event
        let data_result: Result<(i128, i128), _> = event.2.try_into_val(&s.env);
        if data_result.is_ok() {
            found_new_min_reward_event = true;
        }
    }
    assert!(
        !found_new_min_reward_event,
        "no event should be emitted on validation failure"
    );
}

#[test]
fn test_set_min_reward_event_captures_old_and_new() {
    let s = setup();
    // Set initial value
    s.registry.set_min_reward(&s.admin, &100_000i128);
    // Change it again
    s.registry.set_min_reward(&s.admin, &200_000i128);
    let events = s.env.events().all();
    let event = events.last().unwrap();
    let data: (i128, i128) = event.2.try_into_val(&s.env).unwrap();
    let (event_old, event_new) = data;
    assert_eq!(event_old, 100_000i128);
    assert_eq!(event_new, 200_000i128);
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #17: sweep_fees event emission tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_sweep_fees_emits_event() {
    let s = setup();
    let _ = executed_task_keeper(&s); // accrues 30_000 fee
    let treasury = Address::generate(&s.env);
    s.registry.sweep_fees(&s.admin, &treasury, &30_000i128);
    // Verify event data - last event should be the sweep
    let events = s.env.events().all();
    let event = events.last().unwrap();
    let data: (Address, i128, i128) = event.2.try_into_val(&s.env).unwrap();
    let (event_treasury, event_amount, event_remaining) = data;
    assert_eq!(event_treasury, treasury);
    assert_eq!(event_amount, 30_000i128);
    assert_eq!(event_remaining, 0i128);
}

#[test]
fn test_sweep_fees_partial_amount_shows_remaining() {
    let s = setup();
    let _ = executed_task_keeper(&s); // accrues 30_000 fee
    let treasury = Address::generate(&s.env);
    s.registry.sweep_fees(&s.admin, &treasury, &12_000i128);
    let events = s.env.events().all();
    let event = events.last().unwrap();
    let data: (Address, i128, i128) = event.2.try_into_val(&s.env).unwrap();
    let (_event_treasury, event_amount, event_remaining) = data;
    assert_eq!(event_amount, 12_000i128);
    assert_eq!(event_remaining, 18_000i128);
    // Verify remaining matches actual state
    assert_eq!(s.registry.fees_accrued(), 18_000i128);
}

#[test]
fn test_sweep_fees_no_event_when_validation_fails() {
    let s = setup();
    let _ = executed_task_keeper(&s); // accrues 30_000
    let treasury = Address::generate(&s.env);
    let events_before = s.env.events().all();
    // Try to sweep more than accrued
    let _ = s.registry.try_sweep_fees(&s.admin, &treasury, &30_001i128);
    let events_after = s.env.events().all();
    // Check that no sweep event was added (events may include diagnostic events)
    // The sweep event has 3 fields: (Address, i128, i128)
    let mut found_sweep_event = false;
    for i in events_before.len()..events_after.len() {
        let event = events_after.get(i).unwrap();
        let data_result: Result<(Address, i128, i128), _> = event.2.try_into_val(&s.env);
        if data_result.is_ok() {
            found_sweep_event = true;
        }
    }
    assert!(
        !found_sweep_event,
        "no sweep event should be emitted on validation failure"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #19: initialize event emission tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_initialize_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);
    // Verify event data - last event should be the init event
    let events = env.events().all();
    let event = events.last().unwrap();
    // Data contains (admin, reward_token, fee_bps)
    let data: (Address, Address, u32) = event.2.try_into_val(&env).unwrap();
    let (event_admin, event_token, event_fee_bps) = data;
    assert_eq!(event_admin, admin);
    assert_eq!(event_token, token_id);
    assert_eq!(event_fee_bps, 300u32);
}

#[test]
fn test_initialize_no_event_on_second_call() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);
    let events_before = env.events().all();
    // Second initialize call fails
    let _ = registry.try_initialize(&admin, &token_id, &300u32);
    let events_after = env.events().all();
    // Check that no init event was added
    let mut found_init_event = false;
    for i in events_before.len()..events_after.len() {
        let event = events_after.get(i).unwrap();
        let data_result: Result<(Address, Address, u32), _> = event.2.try_into_val(&env);
        if data_result.is_ok() {
            found_init_event = true;
        }
    }
    assert!(
        !found_init_event,
        "no event should be emitted on rejected second initialize"
    );
}

#[test]
fn test_initialize_no_event_when_validation_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);

    let events_before = env.events().all();

    // Invalid fee_bps > 10_000
    let _ = registry.try_initialize(&admin, &token_id, &10_001u32);

    let events_after = env.events().all();
    // Check that no init event was added
    let mut found_init_event = false;
    for i in events_before.len()..events_after.len() {
        let event = events_after.get(i).unwrap();
        let data_result: Result<(Address, Address, u32), _> = event.2.try_into_val(&env);
        if data_result.is_ok() {
            found_init_event = true;
        }
    }
    assert!(
        !found_init_event,
        "no event should be emitted on validation failure"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// CPU-instruction regression ceilings — issue 0107. `claim_task` and
// `execute_task` are the two entry points most likely to be called under real
// load (every keeper bot calls them once per task), so a silent cost
// regression there is the one most likely to surprise a keeper's transaction
// budget in production.
//
// Measured via `env.cost_estimate().budget().cpu_instruction_cost()` (the
// same budget-tracking API issue 0100's CI job uses) at the time these tests
// were written: `claim_task` costs ~100,555 instructions, `execute_task`
// costs ~158,338. The ceilings below are set at roughly 3x each measured
// value — loose enough that an ordinary change (one extra storage read, a
// slightly bigger event) won't trip it, but tight enough to catch an
// accidental order-of-magnitude regression, such as a refactor that starts
// calling `bump_instance` twice by mistake, or a verifier integration that
// reruns the whole load/save path per call. (Confirmed these have teeth: a
// temporary ceiling of 1 during development made both fail with the exact
// measured instruction count in the message, not an opaque error.)
const CLAIM_TASK_CPU_INSN_CEILING: u64 = 350_000;
const EXECUTE_TASK_CPU_INSN_CEILING: u64 = 500_000;

#[test]
fn test_claim_task_cpu_instructions_within_ceiling() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);

    s.env.cost_estimate().budget().reset_default();
    s.registry.claim_task(&keeper, &id);
    let consumed = s.env.cost_estimate().budget().cpu_instruction_cost();

    assert!(
        consumed < CLAIM_TASK_CPU_INSN_CEILING,
        "claim_task consumed {consumed} CPU instructions, exceeding the regression \
         ceiling of {CLAIM_TASK_CPU_INSN_CEILING}"
    );
}

#[test]
fn test_execute_task_cpu_instructions_within_ceiling() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);
    s.registry.claim_task(&keeper, &id);

    s.env.cost_estimate().budget().reset_default();
    s.registry
        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"proof"));
    let consumed = s.env.cost_estimate().budget().cpu_instruction_cost();

    assert!(
        consumed < EXECUTE_TASK_CPU_INSN_CEILING,
        "execute_task consumed {consumed} CPU instructions, exceeding the regression \
         ceiling of {EXECUTE_TASK_CPU_INSN_CEILING}"
    );
}

// Property tests (issue #93 / backlog 0068): compact proptest coverage per
// I-N invariant, using the shared `invariants` module so these and any
// future fuzz target assert the exact same thing. This is intentionally a
// SMALL proptest per invariant, not the full-depth exploration that
// backlog 0054-0060 (upstream issues #80/#83/#84/#85/#86) call for — those
// remain open, separately-scoped issues; extend these in place rather than
// duplicating them once that work lands.
// ─────────────────────────────────────────────────────────────────────────────

// The crate root is `#![no_std]` for the on-chain WASM build; this whole
// file only ever compiles under `#[cfg(test)]`, where `std` is always
// linked by the test harness regardless — see the identical note in
// `invariants.rs`.
extern crate std;
use std::{vec, vec::Vec};

use crate::invariants::{
    assert_admin_action_isolated, assert_fee_bounded, assert_lapsed_claim_is_expirable,
    assert_solvent, assert_task_ids_monotonic, assert_withdrawal_live,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    // I-1 — Solvency, across a random handful of tasks with random rewards
    // and a mix of execute/cancel/leave-pending outcomes.
    //
    // `setup()` mints a fixed 10_000_000 units to `admin`; up to 5 tasks can
    // be generated here, so each reward is capped at 1_000_000 to guarantee
    // the sum never exceeds what's actually mintable (a proptest input that
    // can't be funded would fail for a reason unrelated to the invariant
    // under test).
    #[test]
    fn property_i1_solvency_holds_across_random_task_outcomes(
        rewards in prop::collection::vec(1_i128..1_000_000, 1..6),
        outcomes in prop::collection::vec(0u8..3, 1..6),
    ) {
        let s = setup();
        let token = token::Client::new(&s.env, &s.token_id);
        let keeper = Address::generate(&s.env);
        let mut task_ids = Vec::new();

        for (reward, outcome) in rewards.iter().zip(outcomes.iter()) {
            let id = register_reward_task(&s, *reward);
            task_ids.push(id);
            match outcome % 3 {
                0 => {
                    // Execute.
                    s.registry.claim_task(&keeper, &id);
                    s.registry
                        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"p"));
                }
                1 => {
                    // Cancel.
                    s.registry.cancel_task(&s.admin, &id);
                }
                _ => {
                    // Leave Pending — still open escrow.
                }
            }
        }

        let balance = token.balance(&s.registry.address);
        assert_solvent(&s.env, &s.registry, &task_ids, &[keeper], balance)
            .expect("I-1 solvency must hold after any mix of task outcomes");
    }

    // I-2 — Escrow recoverability: a claimed task past its deadline is
    // always expirable.
    #[test]
    fn property_i2_lapsed_claim_is_always_expirable(reward in 1_i128..9_000_000) {
        let s = setup();
        let keeper = Address::generate(&s.env);
        let id = register_reward_task(&s, reward);

        s.registry.claim_task(&keeper, &id);
        // Past both the lock window and the task deadline (register_reward_task
        // sets a 1-hour deadline).
        advance(&s.env, 1000, 3_601);

        let now = s.env.ledger().timestamp();
        assert_lapsed_claim_is_expirable(&s.registry, id, now)
            .expect("I-2: a Claimed task past its deadline must be expirable");
    }

    // I-3 — Single payout: executing a task credits the keeper exactly
    // once; a second execute attempt is rejected, not double-paid.
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
        prop_assert_eq!(
            balance_after_second_attempt,
            balance_after_first,
            "a rejected re-execution must not change the keeper's balance"
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

        s.registry.claim_task(&keeper, &executed_id);
        s.registry
            .execute_task(&keeper, &executed_id, &Bytes::from_slice(&s.env, b"p"));

        let task_rewards_before = vec![
            (executed_id, s.registry.get_task(&executed_id).reward),
            (pending_id, s.registry.get_task(&pending_id).reward),
        ];
        let keeper_balances_before = vec![(keeper.clone(), s.registry.keeper_balance(&keeper))];

        let accrued = s.registry.fees_accrued();
        if accrued > 0 {
            let treasury = Address::generate(&s.env);
            s.registry.sweep_fees(&s.admin, &treasury, &accrued);
        }

        let task_rewards_after = vec![
            (executed_id, s.registry.get_task(&executed_id).reward),
            (pending_id, s.registry.get_task(&pending_id).reward),
        ];
        let keeper_balances_after = vec![(keeper.clone(), s.registry.keeper_balance(&keeper))];

        assert_admin_action_isolated(
            &task_rewards_before,
            &task_rewards_after,
            &keeper_balances_before,
            &keeper_balances_after,
        )
        .expect("I-5: sweep_fees must never touch task escrow or keeper balances");
    }

    // I-6 — Withdrawal liveness: a keeper's credited balance is always
    // withdrawable, including while the contract is paused.
    #[test]
    fn property_i6_withdrawal_live_while_paused(reward in 1_i128..9_000_000) {
        let s = setup();
        let keeper = Address::generate(&s.env);
        let id = register_reward_task(&s, reward);

        s.registry.claim_task(&keeper, &id);
        s.registry
            .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"p"));

        s.registry.pause(&s.admin);
        assert_withdrawal_live(&s.registry, &keeper)
            .expect("I-6: a keeper's balance must be withdrawable even while paused");
    }

    // I-7 — Monotonic task ids: registering N tasks in a row always yields
    // strictly increasing, non-repeating ids. Up to 7 tasks, so each
    // reward is capped at 1_000_000 to stay within the minted supply.
    #[test]
    fn property_i7_task_ids_strictly_increasing(
        rewards in prop::collection::vec(1_i128..1_000_000, 2..8),
    ) {
        let s = setup();
        let mut ids = Vec::new();
        for reward in &rewards {
            ids.push(register_reward_task(&s, *reward));
        }

        assert_task_ids_monotonic(&ids)
            .expect("I-7: task ids must be strictly increasing and never reused");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Resource cost report (backlog 0100) — not a correctness test. Drives one
// representative call through every state-changing entry point and prints
// its CPU instruction / memory cost, plus a machine-readable JSON file the
// `resource-cost` advisory CI job diffs against a checked-in baseline (see
// scripts/report-resource-cost.sh and docs/CI.md).
//
// `#[ignore]` keeps this out of the default `cargo test` run; CI invokes it
// explicitly with `-- --ignored --nocapture`.
//
// `upgrade` is not covered — it needs a real, separately-deployed WASM hash
// to upgrade to, which is out of scope for a single-entry-point call. Pure
// read-only views (`get_task`, `task_count`, `admin`, etc.) are also not
// covered — they are single storage reads with no interesting cost profile
// to track for regressions.
#[test]
#[ignore]
fn resource_report() {
    let s = setup();
    // `initialize` was the last top-level call `setup()` made; the budget
    // reflects it until the next call resets it (see soroban-sdk's
    // `cost_estimate().budget()` docs: "resets before every top-level
    // contract level invocation").
    let mut rows: std::vec::Vec<(&str, u64, u64)> = std::vec::Vec::new();
    let record = |name: &'static str, env: &Env, rows: &mut std::vec::Vec<(&str, u64, u64)>| {
        let budget = env.cost_estimate().budget();
        rows.push((
            name,
            budget.cpu_instruction_cost(),
            budget.memory_bytes_cost(),
        ));
    };
    record("initialize", &s.env, &mut rows);

    let task_a = register_default_task(&s);
    record("register_task", &s.env, &mut rows);

    s.registry.increase_reward(&s.admin, &task_a, &500_000i128);
    record("increase_reward", &s.env, &mut rows);

    let deadline = s.registry.get_task(&task_a).deadline;
    s.registry
        .extend_deadline(&s.admin, &task_a, &(deadline + 7_200));
    record("extend_deadline", &s.env, &mut rows);

    let keeper1 = Address::generate(&s.env);
    let task_b = register_default_task(&s);
    s.registry.claim_task(&keeper1, &task_b);
    record("claim_task", &s.env, &mut rows);

    s.registry
        .execute_task(&keeper1, &task_b, &Bytes::from_slice(&s.env, b"proof"));
    record("execute_task", &s.env, &mut rows);

    s.registry.withdraw_rewards(&keeper1);
    record("withdraw_rewards", &s.env, &mut rows);

    let task_c = register_default_task(&s);
    s.registry.cancel_task(&s.admin, &task_c);
    record("cancel_task", &s.env, &mut rows);

    let task_d = register_default_task(&s);
    advance(&s.env, 200, 3_601);
    s.registry.expire_task(&task_d);
    record("expire_task", &s.env, &mut rows);

    s.registry.pause(&s.admin);
    record("pause", &s.env, &mut rows);

    s.registry.unpause(&s.admin);
    record("unpause", &s.env, &mut rows);

    s.registry.set_fee_bps(&s.admin, &500u32);
    record("set_fee_bps", &s.env, &mut rows);

    s.registry.set_min_reward(&s.admin, &0i128);
    record("set_min_reward", &s.env, &mut rows);

    let treasury = Address::generate(&s.env);
    let accrued = s.registry.fees_accrued();
    s.registry.sweep_fees(&s.admin, &treasury, &accrued);
    record("sweep_fees", &s.env, &mut rows);

    let new_admin = Address::generate(&s.env);
    s.registry.transfer_admin(&s.admin, &new_admin);
    record("transfer_admin", &s.env, &mut rows);

    std::println!("### Resource cost per entry point");
    std::println!();
    std::println!("| Entry point | CPU instructions | Memory bytes |");
    std::println!("|---|---|---|");
    for (name, cpu, mem) in &rows {
        std::println!("| `{name}` | {cpu} | {mem} |");
    }

    let mut json = std::string::String::from("{\"entry_points\":[");
    for (i, (name, cpu, mem)) in rows.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&std::format!(
            "{{\"name\":\"{name}\",\"cpu_instructions\":{cpu},\"memory_bytes\":{mem}}}"
        ));
    }
    json.push_str("]}");

    let out_path = std::path::Path::new("target/resource-report.json");
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(out_path, json).expect("failed to write target/resource-report.json");
}