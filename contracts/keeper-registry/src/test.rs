//! # KeeperRegistry — Test Suite

#![cfg(test)]

use soroban_sdk::{
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
    Bytes::from_slice(env, b"liquidate:position:42")
}

fn register_task(s: &Setup, reward: i128) -> u64 {
    s.registry.register_task(
        &s.admin,
        &TaskType::Liquidation,
        &calldata(&s.env),
        &reward,
        &(s.env.ledger().timestamp() + 3_600),
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

#[derive(Clone, Copy)]
enum TerminalStatus {
    Executed,
    Cancelled,
    Expired,
}

fn task_in_status(s: &Setup, status: TerminalStatus) -> (u64, Address) {
    let task_id = register_task(s, 1_000_000);
    let keeper = Address::generate(&s.env);

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
fn test_increase_reward_accepts_claimed_task() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let task_id = register_task(&s, 1_000_000);

    s.registry.claim_task(&keeper, &task_id);
    s.registry.increase_reward(&s.admin, &task_id, &500_000);

    assert_eq!(s.registry.get_task(&task_id).status, TaskStatus::Claimed);
    assert_eq!(s.registry.get_task(&task_id).reward, 1_500_000);
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
}
