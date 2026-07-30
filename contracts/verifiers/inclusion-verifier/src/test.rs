#![cfg(test)]

use keeper_registry::{Task, TaskStatus, TaskType};
use soroban_sdk::{testutils::Address as _, testutils::Ledger, Address, Bytes, Env};

use crate::{InclusionVerifier, InclusionVerifierClient};

fn sample_task(env: &Env, owner: Address) -> Task {
    Task {
        owner,
        task_type: TaskType::Liquidation,
        calldata: Bytes::from_slice(env, b"liquidate:position:42"),
        reward: 1_000_000i128,
        deadline: 1_800_000_000u64,
        ttl_ledgers: 17_280u32,
        status: TaskStatus::Claimed,
        claimer: None,
        claim_ledger: None,
        lock_ledgers: 120u32,
        verifier: None,
    }
}

// The transmutes below intentionally re-bind the env/client to a 'static
// lifetime — the standard Soroban test-harness pattern used elsewhere in
// this workspace (e.g. keeper-registry's own test.rs).
#[allow(clippy::useless_transmute, clippy::missing_transmute_annotations)]
fn setup() -> (Env, InclusionVerifierClient<'static>) {
    let env = Env::default();
    let contract_id = env.register(InclusionVerifier, ());
    let contract = InclusionVerifierClient::new(&env, &contract_id);
    let env = unsafe { core::mem::transmute::<Env, Env>(env) };
    (
        env,
        unsafe { core::mem::transmute::<InclusionVerifierClient, InclusionVerifierClient>(contract) },
    )
}

#[test]
fn test_verify_fails_without_a_recorded_call() {
    let (env, contract) = setup();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let task = sample_task(&env, owner);
    let proof = Bytes::new(&env);

    let approved = contract.verify(&task, &keeper, &proof);
    assert!(!approved);
}

#[test]
fn test_verify_succeeds_after_record_call_in_the_same_ledger() {
    let (env, contract) = setup();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let task = sample_task(&env, owner);
    let proof = Bytes::new(&env);

    contract.record_call(&task, &keeper);
    let approved = contract.verify(&task, &keeper, &proof);
    assert!(approved);
}

#[test]
fn test_verify_fails_for_a_different_keeper() {
    let (env, contract) = setup();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let other_keeper = Address::generate(&env);
    let task = sample_task(&env, owner);
    let proof = Bytes::new(&env);

    contract.record_call(&task, &keeper);
    let approved = contract.verify(&task, &other_keeper, &proof);
    assert!(
        !approved,
        "a marker recorded for one keeper must not verify for another"
    );
}

#[test]
fn test_verify_fails_for_a_different_task_identity() {
    let (env, contract) = setup();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let task_a = sample_task(&env, owner.clone());
    let mut task_b = sample_task(&env, owner);
    task_b.reward = 2_000_000i128;
    let proof = Bytes::new(&env);

    contract.record_call(&task_a, &keeper);
    let approved = contract.verify(&task_b, &keeper, &proof);
    assert!(
        !approved,
        "a marker recorded for task_a must not verify for task_b"
    );
}

#[test]
fn test_verify_fails_for_a_marker_from_an_earlier_ledger() {
    // The "same transaction" guarantee is enforced via the ledger sequence
    // at record time: a marker recorded in an earlier ledger (i.e. an
    // earlier, separate transaction) must not satisfy a later verify call,
    // even though the marker itself hasn't expired out of temporary
    // storage yet.
    let (env, contract) = setup();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let task = sample_task(&env, owner);
    let proof = Bytes::new(&env);

    contract.record_call(&task, &keeper);

    env.ledger().with_mut(|li| li.sequence_number += 1);

    let approved = contract.verify(&task, &keeper, &proof);
    assert!(
        !approved,
        "a marker recorded in a prior ledger must not verify in a later one"
    );
}

#[test]
fn test_two_different_tasks_can_both_be_recorded_and_verified_independently() {
    let (env, contract) = setup();
    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let task_a = sample_task(&env, owner.clone());
    let mut task_b = sample_task(&env, owner);
    task_b.reward = 2_000_000i128;
    let proof = Bytes::new(&env);

    contract.record_call(&task_a, &keeper);
    contract.record_call(&task_b, &keeper);

    assert!(contract.verify(&task_a, &keeper, &proof));
    assert!(contract.verify(&task_b, &keeper, &proof));
}

// ─────────────────────────────────────────────────────────────────────────────
// End-to-end: a mock "target contract" that cooperates with the inclusion
// pattern, a real KeeperRegistry task attached to this verifier, and the
// full register -> claim -> (target call, which records inclusion) ->
// execute_task flow, demonstrating the one pattern Soroban actually
// supports (per this crate's module doc comment) end to end.
// ─────────────────────────────────────────────────────────────────────────────

mod mock_target_contract {
    use keeper_registry::Task;
    use soroban_sdk::{contract, contractimpl, Address, Env};

    /// Stands in for a real protocol contract (a lending pool's
    /// `liquidate`, say) that has been written to cooperate with the
    /// inclusion-verifier pattern: as part of doing its own real work, it
    /// also calls back into the configured verifier to record that this
    /// call happened.
    #[contract]
    pub struct MockTargetContract;

    #[contractimpl]
    impl MockTargetContract {
        /// The keeper calls this (standing in for whatever real off-chain-
        /// coordinated action the task represents) before calling
        /// `execute_task`.
        pub fn perform_action(
            env: Env,
            verifier: Address,
            task: Task,
            keeper: Address,
        ) {
            crate::InclusionVerifierClient::new(&env, &verifier)
                .record_call(&task, &keeper);
        }
    }
}

#[test]
fn test_end_to_end_execute_task_with_inclusion_verifier_credits_reward() {
    use keeper_registry::{KeeperRegistry, KeeperRegistryClient};
    use soroban_sdk::token;

    let env = Env::default();
    env.mock_all_auths();

    let verifier_id = env.register(InclusionVerifier, ());
    let target_id = env.register(mock_target_contract::MockTargetContract, ());
    let target_client = mock_target_contract::MockTargetContractClient::new(&env, &target_id);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &10_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    let calldata = Bytes::from_slice(&env, b"liquidate:position:42");
    let reward = 1_000_000i128;
    let deadline = env.ledger().timestamp() + 3_600;
    let task_id = registry.register_task(
        &admin,
        &keeper_registry::TaskType::Liquidation,
        &calldata,
        &reward,
        &deadline,
        &20_000u32,
        &120u32,
        &Some(verifier_id.clone()),
    );

    let keeper = Address::generate(&env);
    registry.claim_task(&keeper, &task_id);

    // Off-chain, the keeper would call the real target contract to perform
    // the coordinated action; here that's simulated by calling the mock
    // target directly, which records the inclusion marker as a side effect
    // of doing so — exactly the cooperating-contract pattern this
    // verifier's module doc comment describes.
    let task = registry.get_task(&task_id);
    target_client.perform_action(&verifier_id, &task, &keeper);

    let proof = Bytes::new(&env);
    registry.execute_task(&keeper, &task_id, &proof);

    let (expected_net, _) = (
        reward - (reward * 300 / 10_000),
        reward * 300 / 10_000,
    );
    assert_eq!(registry.keeper_balance(&keeper), expected_net);
    assert_eq!(
        registry.get_task(&task_id).status,
        keeper_registry::TaskStatus::Executed
    );
}

#[test]
fn test_end_to_end_execute_task_rejected_without_the_target_call() {
    use keeper_registry::{KeeperError, KeeperRegistry, KeeperRegistryClient};
    use soroban_sdk::token;

    let env = Env::default();
    env.mock_all_auths();

    let verifier_id = env.register(InclusionVerifier, ());

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &10_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    let calldata = Bytes::from_slice(&env, b"liquidate:position:42");
    let reward = 1_000_000i128;
    let deadline = env.ledger().timestamp() + 3_600;
    let task_id = registry.register_task(
        &admin,
        &keeper_registry::TaskType::Liquidation,
        &calldata,
        &reward,
        &deadline,
        &20_000u32,
        &120u32,
        &Some(verifier_id.clone()),
    );

    let keeper = Address::generate(&env);
    registry.claim_task(&keeper, &task_id);

    // Skips calling the target contract entirely — no marker recorded.
    let proof = Bytes::new(&env);
    let result = registry.try_execute_task(&keeper, &task_id, &proof);
    assert_eq!(result, Err(Ok(KeeperError::VerificationFailed)));
}
