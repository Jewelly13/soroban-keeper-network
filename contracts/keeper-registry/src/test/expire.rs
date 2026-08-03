//! `expire_task`, including the re-entrancy regression.

use soroban_sdk::{testutils::Address as _, token, Address, Env};

use super::common::*;
use crate::{KeeperError, KeeperRegistry, KeeperRegistryClient, TaskStatus, TaskType};

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
        &DEFAULT_TTL_LEDGERS,
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
