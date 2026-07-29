use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Bytes, Env,
};

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
        &TASK_TTL_LEDGERS,
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
    }
}
