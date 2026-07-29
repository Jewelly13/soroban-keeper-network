use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Bytes, Env, IntoVal,
};

fn setup(fee_bps: u32) -> (Env, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let registry_id = env.register(KeeperRegistry, ());
    let token_id = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let registry = KeeperRegistryClient::new(&env, &registry_id);

    registry.initialize(&admin, &token_id, &fee_bps);

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
}
