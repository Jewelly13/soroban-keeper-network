# Verifier Integration Guide

This guide is for dApp authors registering a task and deciding whether to
attach a verifier — and, if so, which one, or how to write their own. It
doesn't cover the underlying design rationale (why the interface looks the
way it does); see `IKeeperVerifier`'s doc comment in
`contracts/keeper-registry/src/lib.rs` for that.

## When to attach a verifier versus the base trust model

By default (`verifier: None`), `execute_task` trusts the claiming keeper's
`proof` unconditionally — the registry has no way to tell a genuine proof
from garbage. This is the base MVP trust model, and it's a real gap named in
the README's Known Design Decisions: a malicious keeper could claim a task
and submit a fabricated proof, and the registry would credit it exactly as
if the work had been done.

Attaching a verifier closes that gap by having an on-chain contract check
`proof` before any reward is credited. The tradeoff:

- **Extra resource cost.** A verifier call is a nested cross-contract call,
  and per the resource-budget findings elsewhere in this repo's test suite
  (`test_execute_task_against_expensive_verifier_exhausts_a_tight_budget` in
  `contracts/keeper-registry/src/test.rs`), a verifier that does non-trivial
  work — storage reads, cryptographic checks — consumes real budget on top
  of `execute_task`'s own cost. A keeper claiming a verifier-gated task is
  trusting the owner didn't attach something abusively expensive.
- **A stuck-task risk if the verifier misbehaves.** A verifier that panics
  (rather than returning `false`) is not isolated by Soroban's host — the
  whole `execute_task` transaction aborts. The task stays `Claimed`, and the
  only recovery is `expire_task` once the deadline passes. This is the same
  eventual-recovery guarantee every other stuck-task scenario in this
  contract relies on, not a new failure mode — but it does mean a
  verifier-gated task can take longer to resolve if something goes wrong.

**Use `None`** if your task's proof is inherently self-verifying off-chain
(e.g. you're going to check it yourself before acting on it, or the
consequence of a bad proof is low). **Attach a verifier** if you need the
registry itself to refuse crediting on a bad proof — for example, gating
payment on cryptographic attestation from a source you trust, without
needing to trust the claiming keeper at all.

## Using the reference verifiers

One reference verifier currently exists in this repo:

### Signature verifier (`contracts/verifiers/signature-verifier/`)

Verifies that `proof` is a valid ed25519 signature over the task's identity
`(owner, calldata, deadline, reward)`, produced by a key you configure. Use
this when you have (or can run) an off-chain signer — an oracle, an
attestation service, or your own backend — that you trust to co-sign a
keeper's completion proof before the registry pays out.

**Constructor / init parameters:**

```rust
pub fn initialize(e: Env, signer: BytesN<32>) -> Result<(), SignatureVerifierError>
```

One instance is configured with a single ed25519 public key at
`initialize` time. `initialize` can only be called once per deployed
instance (`SignatureVerifierError::AlreadyInitialized` on a second call).

**Registration example** — deploy an instance, initialize it with your
signer's public key, then attach it when registering a task:

```rust
// 1. Deploy and initialize the verifier once (reusable across many tasks
//    signed by the same key).
let verifier_id = env.deployer().with_current_contract(salt).deploy_v2(
    signature_verifier_wasm_hash,
    (),
);
SignatureVerifierClient::new(&env, &verifier_id).initialize(&my_signer_public_key);

// 2. Attach it when registering a task.
let registry = KeeperRegistryClient::new(&env, &registry_contract_id);
let task_id = registry.register_task(
    &env.current_contract_address(), // owner
    &TaskType::Liquidation,
    &calldata,
    &reward_amount,
    &(env.ledger().timestamp() + 3600),
    &17_280u32,
    &120u32,
    &Some(verifier_id), // verifier: gate crediting on a valid signature
);
```

**Off-chain proof generation** — the keeper's `proof` must be a raw 64-byte
ed25519 signature over the exact message `signed_message` constructs (owner
address XDR bytes, then calldata, then deadline and reward as big-endian
bytes, concatenated). The reference keeper bot
(`examples/keeper-bot/index.js`) implements this — see its
`buildSignatureVerifierMessage`/`signProofForTask` functions for a worked,
runnable example of constructing the message and signing it off-chain. The
on-chain equivalent, if you're building the message from within another
contract instead, is `signature_verifier::signed_message(&env, &task)`.

**Changing or clearing a verifier later:** if a task hasn't been claimed
yet, its verifier can be updated (or cleared back to `None`) via
`update_verifier`:

```rust
pub fn update_verifier(
    e: Env,
    owner: Address,
    task_id: u64,
    new_verifier: Option<Address>,
) -> Result<(), KeeperError>
```

This only works while the task is `Pending` — once a keeper has claimed it,
they've committed to a specific proof requirement, and changing that out
from under them would be a bait-and-switch. If you need to change a
`Claimed` task's verifier, there's no path except waiting for the claim to
lapse or the task to be cancelled/expired.

## Writing a custom verifier

Any contract implementing `IKeeperVerifier` can be attached as a `verifier`:

```rust
#[soroban_sdk::contractclient(name = "IKeeperVerifierClient")]
pub trait IKeeperVerifier {
    /// Returns `true` if `proof` is a valid attestation that `keeper`
    /// performed the off-chain action `task` describes.
    fn verify(env: Env, task: Task, keeper: Address, proof: Bytes) -> bool;
}
```

Minimal worked example — a verifier that only ever approves (useful for
local testing, not for anything real):

```rust
#[contract]
pub struct AlwaysApproveVerifier;

#[contractimpl]
impl IKeeperVerifier for AlwaysApproveVerifier {
    fn verify(_env: Env, _task: Task, _keeper: Address, _proof: Bytes) -> bool {
        true
    }
}
```

A few things to get right when writing your own:

- **Return `false`, don't panic, for a normal "proof didn't check out"
  outcome.** A panic isn't isolated — see the "Failure-handling and budget
  implications" section below. Reserve panics for conditions that are
  genuinely exceptional (a misconfigured/uninitialized verifier, say),
  and even then, consider whether failing closed with `false` is safer —
  the reference signature verifier does exactly this for an uninitialized
  instance (see `SignatureVerifier::verify`'s `None => return false` arm).
- **Bind your check to the task's actual identity if replay matters.** As
  the signature verifier's own doc comment explains, `verify` receives a
  `Task`, not a `task_id` — the registry's task identifier is only the
  storage key, never passed to the verifier. If your proof format needs
  to be replay-safe across tasks, bind it to as much of the `Task`'s
  content as practically distinguishes your tasks (owner, calldata,
  deadline, reward — the fields the signature verifier binds to). This
  isn't a hard guarantee against two tasks that happen to share all of
  those fields; there is currently no way to get a hard guarantee without
  a breaking change to `IKeeperVerifier::verify`'s signature.
- **You cannot move funds from `verify`.** It's called strictly before any
  crediting in `execute_task`, receives no reference to the reward token
  or the registry's own functions, and runs under Soroban's default
  reentry-prohibited mode, so it can't call back into the registry either.
  The only thing your verifier controls is whether `execute_task`'s own
  crediting logic runs — see `IKeeperVerifier`'s doc comment in `lib.rs`
  for the full trust-model writeup.

## Failure-handling and budget implications, for integrators

Two findings from this repo's own investigation into verifier failure
modes, restated here for someone attaching a verifier rather than building
the registry itself:

- **A verifier that panics aborts the whole transaction.** Per Soroban's
  host (`soroban-env-host`'s `Host::try_call`), only typed contract errors
  are recoverable across a cross-contract call boundary — a genuine panic
  in your verifier propagates and aborts `execute_task` entirely. The task
  remains `Claimed`; recovery is `expire_task` once the deadline passes.
  If you're evaluating a third-party verifier (not writing your own), this
  means a buggy or malicious verifier can delay a task's resolution until
  expiry, but cannot corrupt registry state or move funds it isn't already
  the gate for.
- **A verifier that's expensive costs the claiming keeper extra budget.**
  There's currently no on-chain way to preview a verifier's cost before
  claiming a task that uses it. If you're publishing a verifier for others
  to use, document its approximate resource cost so integrators and
  keepers can judge it; if you're a keeper deciding whether to claim a
  verifier-gated task, treat an unfamiliar verifier's cost as unknown until
  you've inspected its source or tested against it.
