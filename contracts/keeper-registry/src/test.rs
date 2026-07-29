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
