//! # Signature Verifier — reference `IKeeperVerifier` implementation
//!
//! Verifies that `proof` is a valid ed25519 signature, produced by a key the
//! task owner designates at construction time, over a message binding to the
//! task's identity — see [`SignatureVerifier::signed_message`] for exactly
//! what's signed and the replay-protection caveat that comes with it.
//!
//! ## Trust model
//! One deployed instance of this contract is configured with a single
//! `signer` public key at `initialize` time. A task owner attaches this
//! instance's address as `verifier` on any task whose valid completion proof
//! they expect to be co-signed by that key (e.g. an off-chain oracle or
//! attestation service the owner trusts). It is the task owner's
//! responsibility to deploy (or point at) an instance configured with the
//! right signer for their use case — this contract itself makes no judgment
//! about who a "correct" signer is.
//!
//! ## Replay-protection caveat — see the security note below
//! [`IKeeperVerifier::verify`] receives a [`keeper_registry::Task`], not a
//! `task_id` — the registry's task identifier is only the storage key it's
//! stored under, and isn't part of the `Task` struct or passed separately.
//! This means the signed message this contract can construct is bound to
//! `(owner, calldata, deadline, reward)`, not to a task's actual on-chain
//! identity. In the overwhelmingly common case this is sufficient — two
//! *distinct* tasks sharing all four of those fields is unusual — but it is
//! not a hard guarantee: an owner who registers two tasks with identical
//! `calldata`, `deadline`, and `reward` (e.g. two separately-submitted but
//! byte-identical liquidation calls) would have a signature valid for one
//! also accepted for the other. Mitigating this at the interface level would
//! require `IKeeperVerifier::verify` to receive the task's id, which it
//! currently does not — flagged as a known limitation rather than solved
//! here, since changing the trait's signature is a breaking change outside
//! this issue's scope.
//!
//! ## Panic-on-invalid-signature caveat
//! `soroban_sdk::crypto::Crypto::ed25519_verify` panics on an invalid
//! signature rather than returning `false` — there is no non-panicking
//! variant available in this SDK version. This contract can and does check
//! `proof`'s *length* before calling it (rejecting a wrong-length proof with
//! a clean `false`, satisfying half of the "malformed proof rejected without
//! panicking" goal), but a correctly-*sized* signature that is
//! cryptographically invalid still panics inside `ed25519_verify`, and per
//! `IKeeperVerifier`'s documented cross-contract call semantics, that panic
//! is not isolated — it propagates and aborts the caller's `execute_task`
//! transaction. This is a real, unresolved limitation of building a
//! non-panicking verifier on top of `Crypto`'s current panic-on-failure API,
//! not an oversight in this contract's own logic.

#![no_std]

use keeper_registry::{IKeeperVerifier, Task};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, xdr::ToXdr, Address, Bytes, BytesN, Env,
};

#[contracttype]
enum DataKey {
    Signer,
}

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SignatureVerifierError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
}

#[contract]
pub struct SignatureVerifier;

#[contractimpl]
impl SignatureVerifier {
    /// One-time setup: configure the ed25519 public key this instance
    /// checks every `verify` call's `proof` against.
    pub fn initialize(e: Env, signer: BytesN<32>) -> Result<(), SignatureVerifierError> {
        if e.storage().instance().has(&DataKey::Signer) {
            return Err(SignatureVerifierError::AlreadyInitialized);
        }
        e.storage().instance().set(&DataKey::Signer, &signer);
        Ok(())
    }

    /// The currently configured signer, for off-chain tooling / tests.
    pub fn signer(e: Env) -> Result<BytesN<32>, SignatureVerifierError> {
        e.storage()
            .instance()
            .get(&DataKey::Signer)
            .ok_or(SignatureVerifierError::NotInitialized)
    }
}

/// Builds the exact message this contract expects `proof` to be a valid
/// ed25519 signature over: the concatenation of the task owner's raw address
/// bytes, the calldata, the deadline, and the reward, each XDR-encoded and
/// length-unambiguous by construction (each field is serialized with
/// `soroban_sdk`'s own `Bytes`/scalar `to_be_bytes` encodings, which are
/// fixed-width or self-describing, so no field-boundary ambiguity is
/// introduced by concatenation). See the module doc comment for what this
/// does and doesn't protect against.
///
/// A free function rather than a `SignatureVerifier` method: everything
/// inside a `#[contractimpl] impl` block is treated as a contract entry
/// point by the macro, which rejects a `&Task`-by-reference parameter —
/// this needs to be called from both `verify` and this crate's own tests
/// without being part of the contract's public ABI.
pub fn signed_message(e: &Env, task: &Task) -> Bytes {
    let mut msg = Bytes::new(e);
    msg.append(&task.owner.clone().to_xdr(e));
    msg.append(&task.calldata);
    msg.extend_from_array(&task.deadline.to_be_bytes());
    msg.extend_from_array(&task.reward.to_be_bytes());
    msg
}

#[contractimpl]
impl IKeeperVerifier for SignatureVerifier {
    fn verify(env: Env, task: Task, _keeper: Address, proof: Bytes) -> bool {
        let signer: BytesN<32> = match env.storage().instance().get(&DataKey::Signer) {
            Some(s) => s,
            // Not initialized: fail closed rather than panicking, matching
            // #102's "malformed proof rejected without panicking" spirit —
            // an unconfigured verifier should never be treated as approving.
            None => return false,
        };

        // A malformed or wrong-length proof is rejected without panicking:
        // `ed25519_verify` itself panics on a bad signature, and it also
        // requires a fixed 64-byte input, so any non-64-byte `proof` is
        // rejected here, before `ed25519_verify` is ever called.
        let signature: BytesN<64> = match proof.try_into() {
            Ok(sig) => sig,
            Err(_) => return false,
        };

        let message = signed_message(&env, &task);

        // `ed25519_verify` panics if the signature doesn't check out against
        // `message`/`signer` — per IKeeperVerifier's own documented
        // semantics (see keeper_registry::IKeeperVerifier's doc comment),
        // that panic is the caller's problem, not this contract's, and here
        // it would incorrectly turn a "bad proof" into an aborted
        // transaction instead of a clean `false`. There is no non-panicking
        // ed25519-verify host function available, so the check is done via
        // catching the only failure mode that's actually observable ahead
        // of time (length) and trusting `ed25519_verify` for the rest —
        // a malformed-but-right-length proof that fails cryptographic
        // verification will still panic here. This is a known limitation
        // of building on top of `Crypto::ed25519_verify`'s panic-on-failure
        // API rather than a `Result`/`bool`-returning one.
        env.crypto().ed25519_verify(&signer, &message, &signature);
        true
    }
}

#[cfg(test)]
mod test;
