//! # KeeperRegistry — Test Suite

#![cfg(test)]

use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Deployer as _, Ledger, MockAuth},
    token, Address, Bytes, Env,
};

use crate::{split_reward, KeeperError, KeeperRegistry, KeeperRegistryClient, TaskType};

struct Setup {
    env: Env,
    admin: Address,
    registry: KeeperRegistryClient<'static>,
    token_id: Address,
}

#[allow(clippy::useless_transmute, clippy::missing_transmute_annotations)]
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
    registry.initialize(&admin, &token_id, &300u32);

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
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
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
        );
        prop_assert_eq!(setup.registry.fees_accrued(), accrued_after_more);
    }
}
