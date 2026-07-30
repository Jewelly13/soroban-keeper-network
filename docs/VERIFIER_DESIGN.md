# Verifier Design

## Status

Decision record for verifier failure handling. This document unblocks implementation of `execute_task` verifier calls.

## Interface

A verifier is an optional contract attached to a task. The verifier exposes the following Soroban contract entry points:

```rust
/// Must equal `KEEPER_VERIFIER_INTERFACE_VERSION` (currently `1`).
pub fn interface_version(env: Env) -> u32;

pub fn verify(env: Env, task: Task, keeper: Address, proof: Bytes) -> bool
```

The verifier returns `true` when the proof is acceptable and `false` otherwise. The task, keeper, and proof are passed by value at the contract ABI boundary; generated Soroban clients may use references in their Rust-facing method signatures.

### Interface versioning

The registry contract exposes its own `VERSION` constant for ABI detection. The verifier interface needs the same discipline: a contract written against v1 of `IKeeperVerifier` must not silently misbehave if the registry later calls it with a v2 calling convention.

- `KEEPER_VERIFIER_INTERFACE_VERSION` (in `keeper-registry`) is the sole supported convention version.
- Every verifier returns that value from `interface_version`.
- `execute_task` reads `interface_version` **before** calling `verify`. On mismatch it returns `KeeperError::IncompatibleVerifierInterface` without calling `verify`, without crediting the keeper, and without changing task status.
- When `verify`'s parameters or semantics change incompatibly, bump `KEEPER_VERIFIER_INTERFACE_VERSION` and update reference verifiers together. Older deployed verifiers will fail closed with the typed error until redeployed.

A task without an attached verifier retains the existing behavior: `execute_task` does not perform an external call and proceeds with the normal reward accounting after its existing checks.

## Investigation: panics in cross-contract calls

Soroban does not use Rust unwinding to isolate a callee. A panic in a contract is a VM/host trap, not a Rust panic that a contract can catch with `std::panic::catch_unwind`. Contract code is compiled with `no_std`, and Soroban does not provide a general-purpose panic-catching mechanism.

Soroban exposes a fallible invocation API:

- [`Env::try_invoke_contract` in soroban-sdk 22.0.1](https://docs.rs/soroban-sdk/22.0.1/soroban_sdk/struct.Env.html#method.try_invoke_contract)
- [`Env::invoke_contract` in soroban-sdk 22.0.1](https://docs.rs/soroban-sdk/22.0.1/soroban_sdk/struct.Env.html#method.invoke_contract)

`try_invoke_contract` is intended for handling an invocation that returns an error value. It does not turn an arbitrary callee VM panic/trap into a recoverable verifier result. A verifier panic therefore propagates as a failed host invocation and aborts the calling transaction; registry state changes from that transaction are rolled back.

This distinction is important:

- A verifier returning `false` is an ordinary successful invocation and must be handled as a typed verification rejection.
- A verifier returning a contract error may be handled through Soroban's fallible invocation API where supported by the chosen interface.
- A verifier panicking or otherwise trapping is not safely catchable by `execute_task`; the transaction fails and no payout state is committed.
- Budget exhaustion and other transaction-level host failures also abort the transaction and must not be treated as successful verification.

## Decision

`execute_task` must treat verifier results as follows:

1. `None` verifier: preserve the current execution path exactly.
2. Verifier returns `true`: continue with the existing reward split, keeper credit, task transition, and event emission.
3. Verifier returns `false`: return the typed verification-failed error specified by issue 0080. The task remains `Claimed`; no reward is credited or transferred.
4. Verifier panics or traps: allow the invocation failure to abort the transaction. No reward is credited or transferred, and the task remains unchanged because the transaction is rolled back.
5. A transaction-level failure such as exhausted budget may also abort the transaction. It must not be represented as a successful verification or cause partial reward accounting.

The implementation must not use `catch_unwind` or depend on Rust panic behavior. It may use `Env::try_invoke_contract` only for invocation errors that Soroban exposes as recoverable results; it must not assume that this API catches a callee VM panic.

## Recovery and denial of service

A panicking verifier can prevent successful execution attempts, but it cannot permanently consume the escrow merely by causing verification attempts to fail:

- A failed verifier invocation does not make the task `Executed`.
- The failed transaction does not credit or transfer the reward.
- The task remains `Claimed` after the failed transaction.
- Once the deadline passes, the existing permissionless `expire_task` path can refund the owner.

Thus a panicking verifier causes a liveness delay until the deadline, but it does not permanently strand the escrow under the current lifecycle rules. No separate maximum-failed-attempts or force-cancel mechanism is introduced by this decision record. Any such mechanism would be a separate protocol change and follow-up issue.

This conclusion depends on the task record remaining available until expiry. The existing storage-TTL/deadline invariant is tracked by issue 0005 (`ttl_ledgers` must cover the task deadline and expiry window). That issue must be resolved for the general escrow-recoverability invariant to hold for arbitrarily long-lived tasks.

## Consequences for issue 0074

The implementation in issue 0074 should call the attached verifier using the interface defined above. A returned `false` must map to the typed verification-failed error and must leave the task `Claimed` without crediting or transferring the reward. A callee panic is expected to abort that transaction; it must not be falsely treated as approval or as a successful payout.

The no-verifier path must not invoke an external contract and must remain backward compatible with tasks created before verifier support.
# Verifier Design (E04)

This is the design document for the `IKeeperVerifier` interface — the
decision record the other issues in E04 (0072–0096) implement against. No
contract code changes are made by this document; it exists so the
interface is agreed on paper before several PRs build on top of it.

## Context

The MVP (wave 1) trusts the claiming keeper to submit an honest `proof` —
`execute_task` records it but never checks it against anything (see the
README's Known Design Decisions section). E04 replaces "trust the keeper"
with an **optional**, per-task, on-chain verification callback: a task
owner can attach a verifier contract at registration time, and
`execute_task` calls it before crediting the keeper.

## 1. Interface shape

```rust
/// Implemented by any contract a task owner wants to use as a per-task
/// proof verifier. Registered per-task via `register_task`'s optional
/// `verifier` parameter (see §4, Attachment timing).
pub trait IKeeperVerifier {
    /// Calling-convention version. Must equal
    /// `KEEPER_VERIFIER_INTERFACE_VERSION` or `execute_task` rejects before
    /// `verify`.
    fn interface_version(env: Env) -> u32;

    /// Returns `true` if `proof` is a valid witness that `keeper` correctly
    /// executed `task_id`'s off-chain work, `false` otherwise.
    ///
    /// Must not panic on a merely-invalid proof — return `false`. A panic
    /// is reserved for the verifier being fundamentally broken (see §2),
    /// and `execute_task` treats it as equivalent to `false` regardless.
    fn verify(env: Env, task_id: u64, keeper: Address, proof: Bytes) -> bool;
}
```

**`keeper` is part of the signature.** A verifier that only receives
`task_id` and `proof` cannot bind the proof to the specific keeper
claiming credit — anyone who observes a valid `(task_id, proof)` pair
on-chain (proofs are logged in the `exec` event, per wave 1's issue #4)
could resubmit it under their own claim on a *different* task the same
verifier is attached to, if the proof format doesn't happen to encode
enough context itself. Requiring the registry to pass `keeper` explicitly
means every reference verifier the registry ships (§7–9) can and should
check the proof against that specific address, rather than relying on
every third-party verifier author to remember to do so unprompted.

**No return-value error detail.** `verify` returns a plain `bool`, not a
`Result<bool, E>` with a typed reason. A verifier that wants to
communicate *why* a proof failed (for off-chain debugging, e.g. by a
keeper bot deciding whether a proof is worth resubmitting) should emit its
own event before returning `false` — the registry's `VerificationFailed`
event (§8) intentionally does not attempt to relay a verifier-specific
reason code, since that would couple the registry's ABI to every
verifier's own error taxonomy.

## 2. Failure semantics

**If the verifier call panics, `execute_task` catches it and returns a
typed error rather than reverting the whole transaction.**

Soroban's host provides exactly this primitive:
`Env::try_invoke_contract` catches a callee panic (and any other callee
error) and surfaces it as a `Result` to the caller, as opposed to
`Env::invoke_contract`, which propagates the callee's failure straight
through and aborts the calling transaction too. `execute_task` uses
`try_invoke_contract`, mapping any panic *or* an explicit `false` return
to the same outcome: the execution attempt fails with
`KeeperError::VerificationFailed`, task state is unchanged (no partial
credit, no status transition — enforced the same way every other
rejection path in `execute_task` already works, per I-3/I-5 in
`docs/ARCHITECTURE.md`), and the keeper is free to retry (their claim
lock is untouched) or the task can still expire/be cancelled normally by
its other paths.

This was a real design choice, not a foregone conclusion: propagating the
panic (aborting the whole transaction) is *also* safe from a funds
perspective — no state changes were persisted, since Soroban transactions
are atomic — but it would mean a single misbehaving or buggy verifier
contract could make `execute_task` unusable for every task attached to
it, with no way to recover the escrow except waiting for the deadline and
falling back to `expire_task`. Returning a typed error instead gives the
keeper (or the task owner, via `cancel_task` once the lock lapses, or
anyone via `expire_task` once the deadline passes) every existing recovery
path immediately, rather than only the slowest one.

## 3. Resource budget

**No documented budget ceiling is reserved for the verifier call; the
whole transaction's resource footprint (set by whoever submits it) is
the only limit.**

Soroban does not give a contract an in-band way to sub-allocate a
resource budget to a specific cross-contract call and enforce it — the
`Budget` type in `soroban-sdk`'s `testutils` is a test-only
measurement/reset tool, not a runtime limiting mechanism a contract can
invoke against a callee. The actual ceiling on a cross-contract call's
CPU/memory cost is the calling transaction's own resource footprint,
declared by whoever submits it (a keeper bot, in this case) before
simulation/submission.

Practically, this means: a keeper choosing to execute a task with an
expensive verifier attached pays for that cost in their own transaction's
resource footprint, and an excessively expensive verifier simply makes the
transaction fail at the network's resource-limit boundary (the same
failure mode as any other transaction that tries to do too much) — not a
distinct, registry-specific error. `docs/FUZZING.md`'s target-status table
and this repo's keeper-bot example (`examples/keeper-bot`) should document
the practical implication: a keeper bot integrating with a
verifier-gated task should simulate the transaction first (standard
Soroban RPC `simulateTransaction`) to estimate the real cost before
committing to a fee, exactly as it should for any other execution — this
isn't a new burden E04 introduces, just one that becomes newly relevant
once a verifier call is in the path.

## 4. Attachment timing

**A verifier is chosen at `register_task` time and is immutable once a
keeper has claimed the task (per 0082); the owner may still change it
while the task is `Pending`.**

Rationale: a keeper decides whether a task is worth claiming partly based
on how hard/expensive it'll be to produce a satisfying proof — that
decision is made against whatever verifier is attached *at claim time*.
Letting the owner swap the verifier out from under an already-claimed
task (after the keeper has done off-chain work matching the old verifier)
would let an owner grief a keeper by attaching an impossible-to-satisfy
verifier post-claim, with no way for the keeper to recover except waiting
out the lock window. Locking the verifier at claim time closes that; still
allowing changes pre-claim keeps it consistent with every other
owner-adjustable field on a `Pending` task (`increase_reward`,
`extend_deadline` already work this way).

## 5. Trust model

**Permissionless: any address may be used as a verifier, consistent with
the registry's existing trust model** (keepers are permissionless;
correctness is enforced by contract logic, not a whitelist — see
`docs/ARCHITECTURE.md`'s Trust model section).

A registry-level admin-curated allow-list (630's fork, tracked separately
as issue 0092) is explicitly *not* part of this baseline design — adding
one is a strictly separate, optional extension an operator could layer on
top (e.g. a wrapper contract that only forwards to allow-listed
verifiers), not a change to `IKeeperVerifier` or `execute_task` itself.
Baking an allow-list into the core registry would mean every dApp using
the registry inherits whichever admin's curation policy, which cuts
against the "admin can never gate ordinary task/keeper activity" property
I-5 already establishes for fee sweeping — extending that same principle,
an admin should not get to gate *which verifiers are usable* either,
without an explicit, separately-designed extension opting into that
tradeoff.

## 6. Backward compatibility

**A task with no verifier attached behaves identically to every existing
task today** — `execute_task` performs the verifier call only when
`Task.verifier` is `Some(_)`; when it's `None`, execution proceeds exactly
as it does on `main` right now, with no additional call, no additional
gas cost, and no behavior change. Existing tasks registered before this
epic ships have no `verifier` field populated (backward-compatible
storage migration: `Task.verifier: Option<Address>` defaults to `None`
for any task read that predates this field, the same pattern the existing
`Task` struct already handles for schema evolution elsewhere in this
contract). Any dApp integration written against the current ABI continues
to work with zero changes required — attaching a verifier is opt-in per
task, not a new required parameter with no default.

## Summary of decisions

| Question | Decision |
|---|---|
| Interface shape | `fn interface_version(env) -> u32` plus `fn verify(env, task_id, keeper, proof) -> bool` — `keeper` included to bind the proof to the specific claim |
| Interface versioning | Verifier reports `KEEPER_VERIFIER_INTERFACE_VERSION`; mismatch → `IncompatibleVerifierInterface` before `verify` |
| Failure semantics | `execute_task` uses `try_invoke_contract`; a panicking or `false`-returning verifier both map to `KeeperError::VerificationFailed`, never a transaction-wide revert |
| Resource budget | No in-contract ceiling reserved; the calling transaction's own resource footprint is the only limit — keeper bots should simulate first |
| Attachment timing | Chosen at `register_task`, owner-changeable while `Pending`, immutable once claimed |
| Trust model | Permissionless — any address may be a verifier; an admin allow-list is an optional, separate extension (0092), not baseline |
| Backward compatibility | `Task.verifier: Option<Address>`, `None` behaves identically to today, zero-cost when absent |

## Status

Proposed. Per this issue's own acceptance criteria, 0072–0096 should wait
for a maintainer to review and lock these decisions (or request changes)
before building against them — this document is the basis for that
review, not a substitute for it.

## Implementation note (added while investigating #104/0079)

The interface actually shipped (`contracts/keeper-registry/src/lib.rs`,
via #97/#98/#99) diverges from two of this proposal's decisions above.
Recorded here rather than silently edited into the proposal sections
above, so the divergence itself is visible instead of erasing the
original design record:

- **§1, Interface shape**: the shipped `IKeeperVerifier::verify` takes
  `task: Task` (the full struct), not `task_id: u64` as proposed here.
  This has a real consequence: `Task` carries no `task_id` field (the
  task's identifier is only the storage key it's stored under, never
  passed to the verifier), so a verifier that wants to bind its check to
  a specific task's *identity* can only do so via `Task`'s other fields
  (`owner`, `calldata`, `deadline`, `reward`, ...) — not a guaranteed-
  unique task identifier. Two distinct tasks that happen to share all of
  those fields are indistinguishable to any verifier built against the
  shipped interface. See `contracts/verifiers/signature-verifier/src/
  lib.rs`'s module doc comment for a concrete verifier hitting this limit
  in practice. Fixing it would mean adding `task_id: u64` as a parameter
  to `IKeeperVerifier::verify` — a breaking change to an interface
  reference verifiers already depend on, flagged here as a real, open
  gap rather than fixed opportunistically by this doc update.
- **§2, Failure semantics**: the shipped `execute_task` calls the
  verifier via `IKeeperVerifierClient::new(&e, &verifier).verify(...)`,
  which compiles to `Env::invoke_contract` (the *panicking* variant), not
  `Env::try_invoke_contract` as this proposal specified. Concretely: a
  verifier that panics is confirmed **not** isolated — it aborts the
  entire `execute_task` transaction, exactly the failure mode this
  proposal's §2 argued against and chose `try_invoke_contract` to avoid.
  This is directly observable in the shipped code's own doc comments
  (`IKeeperVerifier`'s "Cross-contract call semantics" section in
  `lib.rs`) and exercised by
  `test_execute_task_against_panicking_verifier_panics` in
  `contracts/keeper-registry/src/test.rs`, which asserts the panic via
  `#[should_panic]` specifically *because* it propagates. The recovery
  path this proposal described as unnecessary (`expire_task` once the
  deadline passes) is, in the shipped implementation, the *only*
  recovery path for a task stuck behind a panicking verifier — matching
  what §2's rejected alternative predicted, not what was decided.

Both of these are genuine implementation-vs-proposal divergences, not
errors in this document — they're recorded here as ground truth for
anyone building against `IKeeperVerifier` today, since the interface
that shipped is the one that matters, whatever this proposal originally
decided.

## Feasibility study: composing multiple verifiers (AND/OR)

**Issue:** #194 (backlog 0125).  
**Question:** Should the registry support multiple verifier addresses per
task (AND/OR composition), or stay single-verifier and leave composition
to the ecosystem?

### Options considered

#### A. First-class multi-verifier on the registry

Extend `Task` with `verifiers: Vec<Address>` (or a small fixed array) plus
a composition operator (`And` / `Or`). `execute_task` would invoke each
address and combine the bools.

Costs:

- Storage and ABI growth on every task, including the common single-verifier
  and no-verifier cases.
- New validation (empty list? max length? operator required when `len > 1`?).
- Proof encoding becomes ambiguous: one shared `proof` blob for N verifiers,
  or N proofs? Either choice couples the registry ABI to composition.
- Failure semantics multiply: which verifier failed? Does one panic abort the
  batch of checks? Partial short-circuit for `Or`?
- Resource budgeting (already "whole transaction only," per §3) becomes
  harder to reason about once the registry itself nests an unbounded number
  of external calls. Nested composite contracts have the same problem, but
  the cost is then opt-in at the edge rather than paid by every registry
  reader and the core execution path.

Benefits: slightly nicer UX for owners who want AND/OR without deploying
extra contracts.

#### B. Single verifier + ecosystem composite contracts (recommended)

Keep `Task.verifier: Option<Address>` exactly as shipped. A dApp that needs
"oracle attestation AND signature" deploys a thin composite contract that
implements `IKeeperVerifier`, holds the child verifier addresses (and any
AND/OR policy) in its own storage, and in `verify` calls each child and
combines the results. The registry never learns about composition.

Benefits:

- Matches the permissionless design elsewhere: complexity stays at the edge;
  the registry stays a small coordination layer (`docs/ARCHITECTURE.md`).
- No registry ABI / storage migration; existing tasks and bots unchanged.
- Proof layout is defined by the composite author (e.g. concatenated
  length-prefixed child proofs, or a single shared proof both children
  understand) without forcing one scheme on the registry.
- AND, OR, M-of-N, ordered short-circuit, and weighted policies are all
  expressible without further registry issues.
- Gas nesting is a property of the contracts the owner chose to attach —
  the same trust decision they already make when picking any verifier.

Costs: the owner (or ecosystem) must deploy a composite contract. That is
the same class of cost as deploying any custom verifier, and is appropriate
for the minority of tasks that need multi-check policies.

### Gas-budgeting concern

Issue 0076 / §3 concluded there is no in-contract sub-budget for a verifier
call. Nested composites can make a single `execute_task` arbitrarily
expensive. That is already true of any malicious or heavy single verifier;
registry-level multi-verifier does not fix it and would still rely on the
keeper simulating the transaction first. Composition at the edge does not
make this worse in a way first-class multi-verifier would uniquely solve.

### Recommendation

**Keep the registry single-verifier-per-task. Composition is an
ecosystem-level pattern, not a registry feature.**

Do not file a first-class multi-verifier design/implementation issue unless
a concrete product requirement appears that cannot be met by a composite
contract (none identified in this study).

### Follow-up: worked example composite verifier

Add a fourth reference implementation under `contracts/verifiers/` (scope
alongside 0077–0079's reference set), for example
`contracts/verifiers/composite-verifier/`:

- `initialize(left: Address, right: Address, mode: And | Or)`
- `interface_version` / `verify` implementing `IKeeperVerifier`
- `verify` calls `IKeeperVerifierClient` on `left` then `right` (short-circuit
  on `And` failure / `Or` success), forwarding the same `(task, keeper, proof)`
  or a documented split of `proof`
- Tests: both approve; left rejects under And; either approves under Or;
  end-to-end `execute_task` against the composite

That example documents the pattern without growing registry surface area.
Until it lands, the sketch above is sufficient guidance for dApp authors.

### Decision table addendum

| Question | Decision |
|---|---|
| Multi-verifier on one task | **Declined at registry layer** — use a composite `IKeeperVerifier` |
| AND/OR operators in registry | **Declined** — encode in the composite contract |
| Worked composite example | **Follow-up** reference crate (not blocking this decision) |
