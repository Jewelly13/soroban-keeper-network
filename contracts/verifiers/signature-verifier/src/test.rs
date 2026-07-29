#![cfg(test)]

use ed25519_dalek::{Signer, SigningKey};
use keeper_registry::{Task, TaskStatus, TaskType};
use rand::rngs::OsRng;
use soroban_sdk::{testutils::Address as _, Address, Bytes, BytesN, Env};

use crate::{signed_message, SignatureVerifier, SignatureVerifierClient};

fn keypair() -> SigningKey {
    SigningKey::generate(&mut OsRng)
}

fn public_key_bytes(env: &Env, signing_key: &SigningKey) -> BytesN<32> {
    BytesN::from_array(env, &signing_key.verifying_key().to_bytes())
}

fn sign(env: &Env, signing_key: &SigningKey, message: &Bytes) -> BytesN<64> {
    let mut buf = [0u8; 4096];
    let len = message.len() as usize;
    message.copy_into_slice(&mut buf[..len]);
    let sig = signing_key.sign(&buf[..len]);
    BytesN::from_array(env, &sig.to_bytes())
}

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

struct Setup {
    env: Env,
    contract: SignatureVerifierClient<'static>,
    signing_key: SigningKey,
}

// The transmutes below intentionally re-bind the env/client to a 'static
// lifetime — the standard Soroban test-harness pattern for a shared Setup.
#[allow(clippy::useless_transmute, clippy::missing_transmute_annotations)]
fn setup() -> Setup {
    let env = Env::default();
    let signing_key = keypair();
    let public_key = public_key_bytes(&env, &signing_key);

    let contract_id = env.register(SignatureVerifier, ());
    let contract = SignatureVerifierClient::new(&env, &contract_id);
    contract.initialize(&public_key);

    let env = unsafe { core::mem::transmute::<Env, Env>(env) };
    Setup {
        env,
        contract: unsafe { core::mem::transmute(contract) },
        signing_key,
    }
}

#[test]
fn test_initialize_and_read_signer() {
    let s = setup();
    let expected = public_key_bytes(&s.env, &s.signing_key);
    assert_eq!(s.contract.signer(), expected);
}

#[test]
fn test_initialize_twice_fails() {
    let s = setup();
    let another_key = public_key_bytes(&s.env, &keypair());
    assert_eq!(
        s.contract.try_initialize(&another_key),
        Err(Ok(crate::SignatureVerifierError::AlreadyInitialized))
    );
}

#[test]
fn test_valid_signature_over_correct_task_verifies() {
    let s = setup();
    let owner = Address::generate(&s.env);
    let task = sample_task(&s.env, owner.clone());
    let message = signed_message(&s.env, &task);
    let signature = sign(&s.env, &s.signing_key, &message);
    let proof = Bytes::from(signature.clone());

    let keeper = Address::generate(&s.env);
    let approved = s.contract.verify(&task, &keeper, &proof);
    assert!(approved);
}

#[test]
#[should_panic(expected = "InvalidInput")]
fn test_signature_valid_for_different_task_is_rejected() {
    // Replay protection: a signature produced for one task's identity
    // (owner + calldata + deadline + reward) does not verify for a
    // different task, even though both tasks share the same verifier
    // instance and signer.
    //
    // This is `#[should_panic]`, not a `false`-return assertion, because
    // `soroban_sdk::crypto::Crypto::ed25519_verify` panics on a
    // cryptographically invalid (but correctly-sized) signature rather than
    // returning a boolean — there's no non-panicking variant in this SDK
    // version. Per the module doc comment's "panic-on-invalid-signature
    // caveat", that panic is not isolated when this happens inside a real
    // `execute_task` call (only the length check ahead of it can return a
    // clean `false`; see `test_wrong_length_proof_rejected_without_panicking`
    // for that half). This test still proves the important thing: a
    // mismatched signature is never treated as *valid* — whether the
    // rejection surfaces as `false` or as an abort, a keeper can never be
    // credited against a task the signature wasn't actually produced for.
    let s = setup();
    let owner = Address::generate(&s.env);
    let task_a = sample_task(&s.env, owner.clone());

    let mut task_b = sample_task(&s.env, owner);
    task_b.reward = 2_000_000i128; // different identity from task_a

    let message_a = signed_message(&s.env, &task_a);
    let signature_over_a = sign(&s.env, &s.signing_key, &message_a);
    let proof = Bytes::from(signature_over_a);

    let keeper = Address::generate(&s.env);
    s.contract.verify(&task_b, &keeper, &proof);
}

#[test]
fn test_wrong_length_proof_rejected_without_panicking() {
    let s = setup();
    let owner = Address::generate(&s.env);
    let task = sample_task(&s.env, owner);
    let keeper = Address::generate(&s.env);

    // 63 bytes: one short of the required 64-byte ed25519 signature.
    let short_proof = Bytes::from_slice(&s.env, &[7u8; 63]);
    let approved = s.contract.verify(&task, &keeper, &short_proof);
    assert!(!approved);

    // Empty proof.
    let empty_proof = Bytes::new(&s.env);
    let approved = s.contract.verify(&task, &keeper, &empty_proof);
    assert!(!approved);
}

#[test]
fn test_unconfigured_verifier_fails_closed() {
    // A SignatureVerifier instance that was never initialize()'d must
    // reject every proof rather than panicking or approving.
    let env = Env::default();
    let contract_id = env.register(SignatureVerifier, ());
    let contract = SignatureVerifierClient::new(&env, &contract_id);

    let owner = Address::generate(&env);
    let task = sample_task(&env, owner);
    let keeper = Address::generate(&env);
    let proof = Bytes::from_slice(&env, &[0u8; 64]);

    let approved = contract.verify(&task, &keeper, &proof);
    assert!(!approved);
}

// ─────────────────────────────────────────────────────────────────────────────
// End-to-end: register a task on the real KeeperRegistry with this verifier
// attached, execute with a real generated signature, confirm crediting.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_end_to_end_execute_task_with_signature_verifier_credits_reward() {
    use keeper_registry::{KeeperError, KeeperRegistry, KeeperRegistryClient};
    use soroban_sdk::token;

    let env = Env::default();
    env.mock_all_auths();

    let signing_key = keypair();
    let public_key = public_key_bytes(&env, &signing_key);
    let verifier_id = env.register(SignatureVerifier, ());
    SignatureVerifierClient::new(&env, &verifier_id).initialize(&public_key);

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
        &17_280u32,
        &120u32,
        &Some(verifier_id.clone()),
    );

    let keeper = Address::generate(&env);
    registry.claim_task(&keeper, &task_id);

    // The signed message must match exactly what `execute_task` will hand
    // the verifier: the registry's own stored `Task` for this task_id.
    let task = registry.get_task(&task_id);
    let message = signed_message(&env, &task);
    let signature = sign(&env, &signing_key, &message);
    let proof = Bytes::from(signature.clone());

    registry.execute_task(&keeper, &task_id, &proof);

    let (expected_net, _) = (
        reward - (reward * 300 / 10_000),
        reward * 300 / 10_000,
    );
    assert_eq!(registry.keeper_balance(&keeper), expected_net);
    assert_eq!(registry.get_task(&task_id).status, keeper_registry::TaskStatus::Executed);

    // A wrong-length proof (not a real signature at all) is rejected
    // cleanly through the full execute_task path — confirms the wiring
    // isn't just accepting anything. (A mismatched-but-correctly-sized
    // signature is covered separately by
    // `test_signature_valid_for_different_task_is_rejected`, which is
    // `#[should_panic]` — see this crate's module doc comment for why that
    // case can't be a clean `Err` when it happens through the real
    // execute_task path.)
    let task_id_2 = registry.register_task(
        &admin,
        &keeper_registry::TaskType::Liquidation,
        &calldata,
        &reward,
        &deadline,
        &17_280u32,
        &120u32,
        &Some(verifier_id),
    );
    let keeper2 = Address::generate(&env);
    registry.claim_task(&keeper2, &task_id_2);
    let wrong_length_proof = Bytes::from_slice(&env, &[9u8; 10]);
    let result = registry.try_execute_task(&keeper2, &task_id_2, &wrong_length_proof);
    assert_eq!(result, Err(Ok(KeeperError::VerificationFailed)));
}
