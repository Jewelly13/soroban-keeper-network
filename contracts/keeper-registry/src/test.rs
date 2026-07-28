//! # KeeperRegistry — Solvency Invariant Tests
//!
//! These tests verify that every token held by the registry is accounted for
//! by task escrow, keeper credits, or accrued protocol fees.

#![cfg(test)]

use soroban_sdk::{
    testutils::{Address as _, Deployer as _, Ledger, MockAuth},
    token, Address, Bytes, Env,
};

use crate::{KeeperRegistry, KeeperRegistryClient, TaskType};

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
    Bytes::from_slice(env, b"solvency-test")
}

fn register_task(s: &Setup, reward: i128) -> u64 {
    let deadline = s.env.ledger().timestamp() + 3_600;
    s.registry.register_task(
        &s.admin,
        &TaskType::Custom,
        &calldata(&s.env),
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
    )
}

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
    );

    s.registry.cancel_task(&s.admin, &cancelled);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[executed, expired],
        std::slice::from_ref(&keeper),
    );

    s.registry.claim_task(&keeper, &executed);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[executed, expired],
        std::slice::from_ref(&keeper),
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
    );

    advance(&s.env, 200, 3_601);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[expired],
        std::slice::from_ref(&keeper),
    );

    s.registry.expire_task(&expired);
    assert_solvent(
        &s.env,
        &s.registry,
        &token,
        &s.registry.address,
        &[],
        std::slice::from_ref(&keeper),
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