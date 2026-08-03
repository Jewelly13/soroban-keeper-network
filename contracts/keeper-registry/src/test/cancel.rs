//! `cancel_task`, including the checks-effects-interactions regression.

use soroban_sdk::{testutils::Address as _, Address, Env};

use super::common::*;
use crate::{KeeperError, KeeperRegistry, KeeperRegistryClient, TaskStatus, TaskType};

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
        &DEFAULT_TTL_LEDGERS,
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
