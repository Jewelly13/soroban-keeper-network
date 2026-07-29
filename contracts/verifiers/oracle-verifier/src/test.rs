#![cfg(test)]

use keeper_registry::{Task, TaskStatus, TaskType};
use soroban_sdk::{testutils::Address as _, testutils::Ledger, Address, Bytes, Env};

use crate::{OracleVerifier, OracleVerifierClient, OracleVerifierError};

mod mock_oracle {
    use soroban_sdk::{contract, contractimpl, contracttype, Env};

    #[contracttype]
    enum DataKey {
        Price,
        LastUpdated,
    }

    #[contract]
    pub struct MockOracle;

    #[contractimpl]
    impl MockOracle {
        pub fn set_price(env: Env, price: i128, last_updated: u64) {
            env.storage().instance().set(&DataKey::Price, &price);
            env.storage()
                .instance()
                .set(&DataKey::LastUpdated, &last_updated);
        }

        pub fn price(env: Env) -> i128 {
            env.storage().instance().get(&DataKey::Price).unwrap_or(0)
        }

        pub fn last_updated(env: Env) -> u64 {
            env.storage()
                .instance()
                .get(&DataKey::LastUpdated)
                .unwrap_or(0)
        }
    }
}

fn sample_task(env: &Env, owner: Address) -> Task {
    Task {
        owner,
        task_type: TaskType::OraclePricePush,
        calldata: Bytes::from_slice(env, b"push-price:BTC/USD"),
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

fn encode_proof(env: &Env, price: i128, timestamp: u64) -> Bytes {
    let mut proof = Bytes::new(env);
    proof.extend_from_array(&price.to_be_bytes());
    proof.extend_from_array(&timestamp.to_be_bytes());
    proof
}

struct Setup {
    env: Env,
    contract: OracleVerifierClient<'static>,
    oracle: mock_oracle::MockOracleClient<'static>,
}

#[allow(clippy::useless_transmute, clippy::missing_transmute_annotations)]
fn setup(tolerance_bps: u32, staleness_threshold_secs: u64) -> Setup {
    let env = Env::default();
    env.ledger().with_mut(|li| li.timestamp = 1_000_000);

    let oracle_id = env.register(mock_oracle::MockOracle, ());
    let oracle = mock_oracle::MockOracleClient::new(&env, &oracle_id);

    let contract_id = env.register(OracleVerifier, ());
    let contract = OracleVerifierClient::new(&env, &contract_id);
    contract.initialize(&oracle_id, &tolerance_bps, &staleness_threshold_secs);

    let env = unsafe { core::mem::transmute::<Env, Env>(env) };
    Setup {
        env,
        contract: unsafe { core::mem::transmute::<OracleVerifierClient, OracleVerifierClient>(contract) },
        oracle: unsafe {
            core::mem::transmute::<mock_oracle::MockOracleClient, mock_oracle::MockOracleClient>(
                oracle,
            )
        },
    }
}

#[test]
fn test_initialize_rejects_tolerance_above_10000_bps() {
    let env = Env::default();
    let oracle_id = env.register(mock_oracle::MockOracle, ());
    let contract_id = env.register(OracleVerifier, ());
    let contract = OracleVerifierClient::new(&env, &contract_id);
    assert_eq!(
        contract.try_initialize(&oracle_id, &10_001u32, &60u64),
        Err(Ok(OracleVerifierError::InvalidToleranceBps))
    );
}

#[test]
fn test_exact_price_match_verifies() {
    let s = setup(50, 60); // 0.5% tolerance, 60s staleness
    s.oracle.set_price(&100_000i128, &s.env.ledger().timestamp());

    let owner = Address::generate(&s.env);
    let keeper = Address::generate(&s.env);
    let task = sample_task(&s.env, owner);
    let proof = encode_proof(&s.env, 100_000i128, s.env.ledger().timestamp());

    assert!(s.contract.verify(&task, &keeper, &proof));
}

#[test]
fn test_price_within_tolerance_verifies() {
    let s = setup(100, 60); // 1% tolerance
    s.oracle.set_price(&100_000i128, &s.env.ledger().timestamp());

    let owner = Address::generate(&s.env);
    let keeper = Address::generate(&s.env);
    let task = sample_task(&s.env, owner);
    // 0.5% off — within the 1% tolerance.
    let proof = encode_proof(&s.env, 100_500i128, s.env.ledger().timestamp());

    assert!(s.contract.verify(&task, &keeper, &proof));
}

#[test]
fn test_price_outside_tolerance_rejected() {
    let s = setup(50, 60); // 0.5% tolerance
    s.oracle.set_price(&100_000i128, &s.env.ledger().timestamp());

    let owner = Address::generate(&s.env);
    let keeper = Address::generate(&s.env);
    let task = sample_task(&s.env, owner);
    // 2% off — outside the 0.5% tolerance.
    let proof = encode_proof(&s.env, 102_000i128, s.env.ledger().timestamp());

    assert!(!s.contract.verify(&task, &keeper, &proof));
}

#[test]
fn test_stale_oracle_data_rejected() {
    let s = setup(50, 60); // 60s staleness threshold
    let stale_timestamp = s.env.ledger().timestamp() - 120; // 2 minutes old
    s.oracle.set_price(&100_000i128, &stale_timestamp);

    let owner = Address::generate(&s.env);
    let keeper = Address::generate(&s.env);
    let task = sample_task(&s.env, owner);
    let proof = encode_proof(&s.env, 100_000i128, stale_timestamp);

    assert!(
        !s.contract.verify(&task, &keeper, &proof),
        "a proof checked against stale oracle data must be rejected"
    );
}

#[test]
fn test_fresh_oracle_data_at_the_staleness_boundary_verifies() {
    let s = setup(50, 60);
    let boundary_timestamp = s.env.ledger().timestamp() - 60; // exactly at threshold
    s.oracle.set_price(&100_000i128, &boundary_timestamp);

    let owner = Address::generate(&s.env);
    let keeper = Address::generate(&s.env);
    let task = sample_task(&s.env, owner);
    let proof = encode_proof(&s.env, 100_000i128, boundary_timestamp);

    assert!(s.contract.verify(&task, &keeper, &proof));
}

#[test]
fn test_malformed_proof_rejected_without_panicking() {
    let s = setup(50, 60);
    s.oracle.set_price(&100_000i128, &s.env.ledger().timestamp());

    let owner = Address::generate(&s.env);
    let keeper = Address::generate(&s.env);
    let task = sample_task(&s.env, owner);

    // Wrong length (not the required 24 bytes).
    let short_proof = Bytes::from_slice(&s.env, &[0u8; 10]);
    assert!(!s.contract.verify(&task, &keeper, &short_proof));

    let empty_proof = Bytes::new(&s.env);
    assert!(!s.contract.verify(&task, &keeper, &empty_proof));
}

#[test]
fn test_unconfigured_verifier_fails_closed() {
    let env = Env::default();
    let contract_id = env.register(OracleVerifier, ());
    let contract = OracleVerifierClient::new(&env, &contract_id);

    let owner = Address::generate(&env);
    let keeper = Address::generate(&env);
    let task = sample_task(&env, owner);
    let proof = encode_proof(&env, 100_000i128, env.ledger().timestamp());

    assert!(!contract.verify(&task, &keeper, &proof));
}

// ─────────────────────────────────────────────────────────────────────────────
// End-to-end: register a task on the real KeeperRegistry with this verifier
// attached, execute with a proof matching the mock oracle's live state.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_end_to_end_execute_task_with_oracle_verifier_credits_reward() {
    use keeper_registry::{KeeperRegistry, KeeperRegistryClient};
    use soroban_sdk::token;

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = 1_000_000);

    let oracle_id = env.register(mock_oracle::MockOracle, ());
    let oracle = mock_oracle::MockOracleClient::new(&env, &oracle_id);
    oracle.set_price(&100_000i128, &env.ledger().timestamp());

    let verifier_id = env.register(OracleVerifier, ());
    OracleVerifierClient::new(&env, &verifier_id).initialize(&oracle_id, &50u32, &60u64);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &10_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    let calldata = Bytes::from_slice(&env, b"push-price:BTC/USD");
    let reward = 1_000_000i128;
    let deadline = env.ledger().timestamp() + 3_600;
    let task_id = registry.register_task(
        &admin,
        &TaskType::OraclePricePush,
        &calldata,
        &reward,
        &deadline,
        &20_000u32,
        &120u32,
        &Some(verifier_id),
    );

    let keeper = Address::generate(&env);
    registry.claim_task(&keeper, &task_id);

    let proof = encode_proof(&env, 100_000i128, env.ledger().timestamp());
    registry.execute_task(&keeper, &task_id, &proof);

    let (expected_net, _) = (
        reward - (reward * 300 / 10_000),
        reward * 300 / 10_000,
    );
    assert_eq!(registry.keeper_balance(&keeper), expected_net);
    assert_eq!(registry.get_task(&task_id).status, TaskStatus::Executed);
}

#[test]
fn test_end_to_end_execute_task_rejected_with_price_outside_tolerance() {
    use keeper_registry::{KeeperError, KeeperRegistry, KeeperRegistryClient};
    use soroban_sdk::token;

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = 1_000_000);

    let oracle_id = env.register(mock_oracle::MockOracle, ());
    let oracle = mock_oracle::MockOracleClient::new(&env, &oracle_id);
    oracle.set_price(&100_000i128, &env.ledger().timestamp());

    let verifier_id = env.register(OracleVerifier, ());
    OracleVerifierClient::new(&env, &verifier_id).initialize(&oracle_id, &50u32, &60u64);

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &10_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&admin, &token_id, &300u32);

    let calldata = Bytes::from_slice(&env, b"push-price:BTC/USD");
    let reward = 1_000_000i128;
    let deadline = env.ledger().timestamp() + 3_600;
    let task_id = registry.register_task(
        &admin,
        &TaskType::OraclePricePush,
        &calldata,
        &reward,
        &deadline,
        &20_000u32,
        &120u32,
        &Some(verifier_id),
    );

    let keeper = Address::generate(&env);
    registry.claim_task(&keeper, &task_id);

    // Claims a price 5% off — well outside the 0.5% tolerance.
    let proof = encode_proof(&env, 105_000i128, env.ledger().timestamp());
    let result = registry.try_execute_task(&keeper, &task_id, &proof);
    assert_eq!(result, Err(Ok(KeeperError::VerificationFailed)));
}
