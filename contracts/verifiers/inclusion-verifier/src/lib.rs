//! # Inclusion Verifier — reference `IKeeperVerifier` implementation
//!
//! Confirms a target contract called back into this verifier during the
//! *current* transaction, as evidence the keeper actually performed the
//! off-chain-coordinated action before submitting `execute_task`.
//!
//! ## What this is NOT — read before using
//! The issue this contract implements (#104/0079) originally asked for
//! proof that a target contract *emitted a specific event*, i.e. a full
//! retroactive inclusion proof: "prove event E was emitted by contract C
//! at some point in the past." **That is not something Soroban contracts
//! can do today.** Investigated directly against the SDK this repo
//! targets (`soroban-sdk` 22.0.11):
//!
//! - `soroban_sdk::Env::events()` returns an [`soroban_sdk::Events`] whose
//!   only method in a *production* build (i.e. without the `testutils`
//!   feature) is `publish` — there is no method to read events at all,
//!   not even the calling contract's own, let alone another contract's,
//!   past or present. The read-back method (`Events::all`, used
//!   extensively in this repo's own tests) only exists behind
//!   `#[cfg(any(test, feature = "testutils"))]` — it is test-only
//!   instrumentation, not a capability available to a deployed contract.
//! - There is no cross-contract storage-read API either: `env.storage()`
//!   is always scoped to the *currently executing* contract's own storage.
//!   A contract cannot read another contract's storage directly; the only
//!   way to observe another contract's state is to call one of that
//!   contract's own read functions.
//!
//! So a genuine "did contract C emit event E, ever" on-chain check is not
//! implementable against this SDK. What **is** implementable, and what
//! this contract does instead: a materially weaker, narrower guarantee —
//! **did the target contract call back into this verifier during the same
//! transaction as `execute_task`.** This requires the target contract to
//! cooperate (it must be written to call [`InclusionVerifier::record_call`]
//! itself); it says nothing about targets that were never integrated with
//! this pattern, and nothing about calls from a *prior* transaction.
//!
//! ## How it works
//! 1. Before submitting `execute_task`, the keeper's transaction first
//!    calls the target contract (e.g. a lending pool's `liquidate`), and
//!    that target contract — written to cooperate with this pattern —
//!    calls [`InclusionVerifier::record_call`] on this verifier, passing
//!    the same `(task, keeper)` pair `execute_task` will eventually pass
//!    to `verify`. This contract records a marker keyed by
//!    `(task identity, keeper, current ledger sequence)`.
//! 2. `execute_task` then calls `verify(task, keeper, proof)` (per
//!    `IKeeperVerifier`). This contract checks for a marker matching
//!    `(task identity, keeper)` recorded **at the current ledger
//!    sequence** — i.e. written earlier in the same transaction, not a
//!    leftover from some prior one.
//! 3. Markers are stored in *temporary* storage with a short TTL (not
//!    persistent), so they don't accumulate indefinitely and a marker
//!    that's somehow still present from an much-earlier ledger (temporary
//!    storage can in principle survive if repeatedly bumped, though
//!    nothing in this contract does that) is still rejected by the
//!    ledger-sequence check regardless.
//!
//! ## Task-identity caveat
//! Same limitation as `signature-verifier`: `IKeeperVerifier::verify`
//! receives a [`keeper_registry::Task`], not a `task_id` — the registry's
//! task identifier is only the storage key, never passed to the verifier.
//! The "task identity" this contract keys markers by is therefore
//! `(owner, calldata, deadline, reward)`, the same practical
//! approximation `signature-verifier` uses, with the same residual gap:
//! two distinct tasks sharing all four of those fields are
//! indistinguishable to this verifier.
#![no_std]

use keeper_registry::{IKeeperVerifier, Task, KEEPER_VERIFIER_INTERFACE_VERSION};
use soroban_sdk::{contract, contractimpl, contracttype, xdr::ToXdr, Address, Bytes, Env};

/// How long a recorded inclusion marker survives in temporary storage
/// before it's eligible for eviction. Generous relative to a single
/// transaction's lifetime (which is what actually matters — see the
/// ledger-sequence check in `verify`) purely so a marker recorded and
/// then checked within the same transaction is never evicted mid-flight
/// by the ledger's own TTL bookkeeping.
const MARKER_TTL_LEDGERS: u32 = 10;

#[contracttype]
struct MarkerKey {
    task_identity: Bytes,
    keeper: Address,
}

#[contracttype]
struct Marker {
    ledger_sequence: u32,
}

#[contract]
pub struct InclusionVerifier;

/// Builds the key this contract binds a marker/verification check to:
/// the task's identity (owner, calldata, deadline, reward — see the
/// module doc comment's task-identity caveat) plus the keeper.
fn task_identity_bytes(e: &Env, task: &Task) -> Bytes {
    let mut id = Bytes::new(e);
    id.append(&task.owner.clone().to_xdr(e));
    id.append(&task.calldata);
    id.extend_from_array(&task.deadline.to_be_bytes());
    id.extend_from_array(&task.reward.to_be_bytes());
    id
}

#[contractimpl]
impl InclusionVerifier {
    /// Called by a cooperating target contract, in the same transaction
    /// and before `execute_task`, to record that it was actually invoked
    /// for `task`/`keeper`. Permissionless — any address can call this for
    /// any `(task, keeper)` pair, which is intentional and safe: recording
    /// a marker only ever makes a *future* `verify` call in the *same
    /// transaction* return `true` instead of `false`, and returning `true`
    /// only gates whether `execute_task`'s own crediting logic runs (see
    /// `IKeeperVerifier`'s doc comment in `keeper_registry` for why a
    /// verifier can't move funds itself regardless of what it returns) —
    /// a griefer recording a bogus marker for someone else's task doesn't
    /// let them claim anything they couldn't already attempt, and a
    /// legitimate keeper's own transaction recording its own marker is
    /// the intended, only-useful case.
    pub fn record_call(e: Env, task: Task, keeper: Address) {
        let key = MarkerKey {
            task_identity: task_identity_bytes(&e, &task),
            keeper,
        };
        let marker = Marker {
            ledger_sequence: e.ledger().sequence(),
        };
        e.storage().temporary().set(&key, &marker);
        e.storage()
            .temporary()
            .extend_ttl(&key, MARKER_TTL_LEDGERS, MARKER_TTL_LEDGERS);
    }
}

#[contractimpl]
impl IKeeperVerifier for InclusionVerifier {
    fn interface_version(_env: Env) -> u32 {
        KEEPER_VERIFIER_INTERFACE_VERSION
    }

    fn verify(env: Env, task: Task, keeper: Address, _proof: Bytes) -> bool {
        let key = MarkerKey {
            task_identity: task_identity_bytes(&env, &task),
            keeper,
        };
        match env.storage().temporary().get::<_, Marker>(&key) {
            Some(marker) => marker.ledger_sequence == env.ledger().sequence(),
            None => false,
        }
    }
}

#[cfg(test)]
mod test;
