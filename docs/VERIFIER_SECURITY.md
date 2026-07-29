# Verifier Design — Security Considerations

Attaching a verifier to a task means the registry executes arbitrary code
chosen by the task owner, on a path that gates whether a keeper gets paid.
That's a meaningfully different trust and threat model than the base MVP
(where `execute_task` trusts the claimer's proof unconditionally), and it
deserves its own dedicated write-up rather than being folded into a general
integration guide aimed at "how do I use this" rather than "what could go
wrong."

This document only covers the security properties and threats. It doesn't
cover the `IKeeperVerifier` interface itself or how to write a custom
verifier — see the trait's doc comment in
[`contracts/keeper-registry/src/lib.rs`](../contracts/keeper-registry/src/lib.rs)
for that.

## Threat model

A verifier is an arbitrary, task-owner-chosen contract. From a keeper's
perspective, claiming a verifier-gated task means trusting that contract not
to be malicious or broken in three specific ways: it lies about proof
validity, it costs more to call than the keeper bargained for, or it panics
instead of returning cleanly. Each is addressed below.

## 1. Proof-size griefing

**Threat:** a task owner (or the fixed proof format itself) forces every
keeper who attempts the task to submit an unbounded amount of proof data,
inflating the transaction cost of every execution attempt — including failed
ones a keeper pays for before finding out the verifier rejects them.

**Mitigation:** `execute_task` rejects any `proof` longer than
`MAX_PROOF_LEN` (256 bytes) before the verifier is ever invoked:

```rust
if proof.len() > MAX_PROOF_LEN {
    return Err(KeeperError::ProofTooLarge);
}
```

This check runs unconditionally — whether or not a verifier is attached —
and runs *before* `load_task`/the verifier call, so an oversized proof is
rejected as cheaply as possible, before any storage read or cross-contract
call. A verifier cannot opt out of this bound; the cap is enforced by the
registry itself, not by the verifier's own logic.

**Scope note:** this bounds the *proof*, not the verifier's own internal
work (e.g. how many storage reads or how much compute `verify()` does
per byte of proof). That's a distinct, resource-budget concern — see
[#2](#2-resource-budget-cost-transfer) below.

## 2. Resource-budget cost transfer

**Threat:** a verifier does something expensive — a large loop, many storage
reads — that consumes most or all of the transaction's resource budget,
either failing the keeper's `execute_task` call outright or leaving little
budget margin for anything else in the same transaction.

**Finding:** a verifier that exhausts the budget produces a **non-recoverable
host error** (`Error(Budget, ExceededLimit)`), confirmed empirically by
`test_execute_task_against_expensive_verifier_exhausts_a_tight_budget` in
`contracts/keeper-registry/src/test.rs`. Per `soroban-env-host`'s
`HostError::is_recoverable`, budget/storage `ExceededLimit` errors are
explicitly excluded from the set of recoverable (typed-`Err`) outcomes —
they always propagate and abort the whole transaction, the same as a panic.
No partial state mutation survives: nothing written by `execute_task` up to
that point (including anything the verifier itself wrote before running out
of budget) is persisted, confirmed by
`test_expire_task_recovers_escrow_from_a_task_stuck_behind_a_budget_exhausting_verifier`
in the same file, which shows `expire_task` still recovers the escrowed
reward for a task stuck behind such a verifier once its deadline passes.

**Practical implication:** a keeper claiming a verifier-gated task is
trusting that the task owner didn't attach something abusively expensive.
This isn't a fund-safety issue (see [#4](#4-can-a-verifier-move-funds) — the
worst case is a wasted `claim_task` call and a stuck-until-expiry task, not
loss of the keeper's or owner's principal), but it is a real availability
cost: a keeper who claims such a task has locked it (via `lock_ledgers`)
without being able to complete it, and must wait for `expire_task` to free
it up. There is currently no on-chain way for a keeper to preview a
verifier's cost before claiming; see the integration guide (once it exists)
for the off-chain mitigation of inspecting a verifier's source/reputation
before claiming tasks that use it.

## 3. Panic isolation

**Threat:** a verifier panics instead of returning `false` for a rejected
proof — does that corrupt registry state, or get silently swallowed?

**Finding:** per `soroban-env-host`'s `Host::try_call`, only *typed contract
errors* (`ScErrorType::Contract`) are recovered as a graceful `Err` across a
cross-contract call boundary. A genuine panic in the verifier is a
non-recoverable host error and is **not isolated** — it propagates and
aborts the entire calling transaction, exactly like the budget-exhaustion
case above. This is documented on `IKeeperVerifier`'s trait definition in
`lib.rs` and exercised by
`test_execute_task_against_panicking_verifier_panics` /
`test_expire_task_recovers_escrow_from_a_task_stuck_behind_a_panicking_verifier`.

**Practical implication:** identical to the budget case — the task remains
`Claimed` and stuck until `expire_task` recovers it after the deadline. A
well-behaved verifier should prefer returning `false` over panicking for any
"proof didn't check out" outcome that isn't genuinely exceptional, since a
panic costs the keeper a wasted `claim_task`/`execute_task` attempt with no
more information than a rejection would have given them.

## 4. Can a verifier move funds?

**Threat:** could a malicious verifier be used to *steal* funds, as opposed
to merely griefing availability (cases 1-3 above)?

**Finding: no.** Walking the call sequence in `execute_task`:

```rust
if let Some(verifier) = task.verifier.clone() {
    let approved: bool =
        IKeeperVerifierClient::new(&e, &verifier).verify(&task, &keeper, &proof);
    if !approved {
        emit_verification_failed(&e, task_id, &keeper);
        return Err(KeeperError::VerificationFailed);
    }
}

bump_instance(&e);
let (keeper_net, fee) = split_reward(task.reward, fee_bps(&e));
credit_keeper(&e, &keeper, keeper_net);
accrue_fee(&e, fee);
```

The verifier call happens strictly *before* `credit_keeper`/`accrue_fee` —
the only two places in the entire contract that move escrowed reward. The
verifier:

- Receives `(task, keeper, proof)` by value — read-only data, no capability
  or authorization object that would let it act on the registry's behalf.
- Has no reference to the reward token contract, the registry's own
  address, or any stored balance. It cannot call `credit_keeper`,
  `accrue_fee`, `withdraw_rewards`, or any other registry function itself —
  those are plain functions on `KeeperRegistry`, not something reachable
  through the `IKeeperVerifier` interface the registry calls it through.
- Is called via a standard cross-contract call, which per Soroban's default
  reentry mode (`ContractReentryMode::Prohibited`, confirmed in
  `soroban-env-host`'s `call_n_internal`) cannot re-enter the calling
  contract (`KeeperRegistry`) at all — even if the verifier's code tried to
  call back into the registry, the host rejects it as a prohibited
  re-entry before any registry function executes.
- Can only return a `bool`. Returning `true` when it should return `false`
  lets an *already-claimed* task's *already-designated* keeper receive the
  reward the task owner already escrowed for that task — the same reward
  that keeper would have received under the no-verifier MVP trust model.
  It cannot redirect the payout to a different address, change the reward
  amount, or credit itself.

So the worst a malicious or buggy verifier can do is: (a) wrongly approve a
bad proof (equivalent to the base MVP's trust-the-claimer behavior — not a
new capability, since `None` already means this), (b) wrongly reject a good
proof (denies the keeper *this task's* reward; the escrow still recovers to
the owner via `expire_task`), or (c) grief availability per cases 1-3 above.
None of these let it move funds it wasn't already the gate for, to a
destination other than the task's own already-claimed keeper.

## Summary

| Threat | Mitigated by | Residual risk |
|---|---|---|
| Proof-size griefing | `MAX_PROOF_LEN` bound, checked before any verifier call | None — enforced unconditionally by the registry |
| Resource-budget cost transfer | None on-chain; budget exhaustion aborts cleanly (no state corruption) | Keeper wastes a claim/lock on an expensive verifier; recovers via `expire_task` |
| Panic (non-recoverable failure) | None on-chain; propagates and aborts cleanly (no state corruption) | Same as above |
| Fund theft via a malicious verifier | Call-ordering (`verify` before any crediting), no reentry, `bool`-only return | None found |

---

Linked from the main [README's Security Considerations](../README.md#security-considerations--audit-plan) section.
