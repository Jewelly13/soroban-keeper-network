# Batch Operations (E05)

Design record and epic retrospective for batch entry points on the keeper
registry. This document is the decision surface for epic E05: what was built,
what was studied, and what was explicitly declined so those negative results
stay discoverable.

## Status

Epic E05 ships **batch registration** (`batch_register_tasks`) and
`claim_first_available` as the only multi-task claim helper. Naive batch claim,
batch execute, batch cancel, and collapsed escrow transfers were studied and
**declined** (see [Epic E05 retrospective](#epic-e05-retrospective)).

## `batch_register_tasks` design

### Motivation

A dApp registering many similar tasks currently pays full per-call overhead
(`require_auth`, instance bumps, events) once per `register_task`. A batch
entry point amortizes that fixed cost inside one transaction while preserving
the same per-task validation and escrow accounting as the single-task path.

### Auth model

All entries in one call share a single `owner`. That owner authorizes once for
the whole batch. To prevent a crafted payload from escrowing more than the
owner intended (for example if transaction contents change between review and
submission), the call takes an explicit `max_total_reward: i128` ceiling. Before
any token transfer, the sum of entry rewards is checked against that ceiling;
exceeding it rejects the entire call with no transfers.

### Partial-failure semantics

Soroban transactions are atomic. Partial success inside one contract call is
not a meaningful alternative without inventing a custom per-entry result model
that still commits storage and transfers for the successes — which would
complicate solvency (I-1) and the "no silent partial escrow" guarantee.

**Decision: all-or-nothing.** Every entry is validated first; only if the whole
batch is valid do transfers and storage writes proceed. Any invalid entry (or
ceiling breach) reverts the call with a typed error and zero token movement.

### Return shape

```rust
pub struct TaskParams {
    pub task_type: TaskType,
    pub calldata: Bytes,
    pub reward: i128,
    pub deadline: u64,
    pub ttl_ledgers: u32,
    pub lock_ledgers: u32,
    pub verifier: Option<Address>,
}

pub fn batch_register_tasks(
    e: Env,
    owner: Address,
    tasks: Vec<TaskParams>,
    max_total_reward: i128,
) -> Result<Vec<u64>, KeeperError>;
```

Returned task ids are in **input order** so the caller can correlate results
without a secondary lookup.

### Guardrails

| Guardrail | Purpose |
|---|---|
| Shared validation helper with `register_task` | Single and batch paths cannot drift |
| `max_total_reward` ceiling | Caps total escrow the owner commits to in one auth |
| Per-entry parameter bounds | Same reward / lock / TTL / calldata rules as single register |
| Optional `verifier` per entry | Mix of verified and trust-the-proof tasks in one batch |
| Empty batch rejected | Avoids a no-op that still burns fees for nothing useful |
| Practical size ceiling | Stay under Soroban per-transaction CPU/memory budget |

### Resource ceiling

Each entry costs roughly one `register_task` worth of storage writes, TTL
extension, event emission, and one SAC transfer. Empirically, batches on the
order of **tens of tasks** (guidance target: pin a regression test around a
measured practical ceiling once implementation lands; treat ~32 as a starting
integrator heuristic, not a hard host limit) remain comfortable; larger
batches should be split client-side. The permanent ceiling is owned by the
resource-limit regression test (issue 0104), not hard-coded into the ABI.

### Escrow transfers

`batch_register_tasks` performs **one token transfer per entry**, matching
`register_task`. Collapsing to a single `sum(rewards)` transfer was studied
and declined — see the retrospective.

---

## `claim_first_available`

When a keeper can execute any of several pending tasks, a naive
`batch_claim(tasks)` is worse than N single claims under contention (see
0099). The supported alternative:

```rust
pub fn claim_first_available(
    e: Env,
    keeper: Address,
    candidates: Vec<u64>,
) -> Result<u64, KeeperError>;
```

Tries candidates in order using the same rules as `claim_task`, returns the
first successfully claimed id, or a typed error if none were claimable. This
expresses "give me any one of these" without all-or-nothing multi-claim
atomicity.

---

## Epic E05 retrospective

Closing summary of what shipped versus what was studied and declined. A reader
should not need to open the linked issues to know the decision and the rough
why; the links hold the longer reasoning trail.

### What shipped

| Feature | Guardrails / notes |
|---|---|
| `batch_register_tasks` | Single-owner auth; `max_total_reward` ceiling; all-or-nothing validation; ids in input order; shared validation with `register_task`; optional per-entry `verifier` |
| `claim_first_available` | First-success claim over a candidate list; no multi-claim all-or-nothing |

### What was studied and declined

#### Batch claim and batch execute (issue 0099 / #124)

**Decision: do not build naive `batch_claim` or `batch_execute`. Prefer
`claim_first_available` for multi-candidate claiming; keep execute single-task.**

**Why.**

- **Batch claim.** `claim_task` is permissionless FCFS. A multi-task claim in
  one Soroban transaction is atomic: if any candidate was taken by another
  keeper moments earlier, the entire batch reverts — including claims that
  would have succeeded alone. Under real contention that makes batching
  strictly worse for the keeper. Benefit concentrates in low-contention
  settings where the fee savings are smallest.
- **`claim_first_available`.** Tries candidates until one succeeds, so a
  single lost race does not discard other opportunities. That is the useful
  multi-task claim shape and is what shipped (issue 0101 / #170 family).
- **Batch execute.** The keeper already holds exclusive locks, so competing
  claims are not the failure mode. A verifier rejection (or panic) on one
  task still forces all-or-nothing revert and blocks crediting for every
  other already-proven task in the batch. Keepers can submit N independent
  `execute_task` transactions and isolate failures; registry-level batch
  execute adds complexity without a clear win once verifiers exist.

Detail: [issue #124](https://github.com/soroban-tooling/soroban-keeper-network/issues/124)
(backlog 0099).

#### Batch cancel for an owner's own tasks (issue 0114 / #183)

**Decision: do not build `batch_cancel_tasks` in this epic.**

**Why.**

- Demand is real in principle (a dApp winding down a stale watch set), but the
  owner already controls every target task and can call `cancel_task` N times
  without cross-keeper races. The UX pain is client-side fee amortization, not
  a missing protocol primitive.
- Implementation is not free: each cancel refunds via a token transfer. A
  batch of N refunds is N external calls in one transaction. That multiplies
  the surface area of the CEI / reentrancy analysis that wave 1 already had to
  get right for single-task cancel (issues 0002 / 0057): every refund must
  still mark the task terminal before transfer, and a reentrant token cannot
  observe a non-terminal twin. Looping the proven single-path helper is safe
  but does not remove that review burden; collapsing refunds would change
  accounting.
- Relative to calling `cancel_task` N times (or a thin off-chain multicall
  wrapper), on-contract batch cancel does not pay for its complexity in this
  epic. Revisit only if integrator demand shows repeated large cancel bursts
  that cannot be handled client-side.

Detail: [issue #183](https://github.com/soroban-tooling/soroban-keeper-network/issues/183)
(backlog 0114).

#### Collapsing escrow transfers in `batch_register_tasks` (issue 0115 / #184)

**Decision: keep one token transfer per batch entry; do not collapse to a
single `sum(rewards)` transfer.**

**Why.**

- Each task's reward must remain independently refundable via `cancel_task` /
  `expire_task`. A collapsed deposit still requires per-task reward fields in
  storage, so the accounting model does not simplify — it gains a second
  representation (bulk deposit vs per-task escrow) that must stay consistent
  with solvency invariant I-1.
- Failure midway through a collapsed design is harder to reason about: either
  the bulk transfer happens before all task rows exist (temporary surplus that
  must never be sweepable as fees), or rows exist before the bulk transfer
  (temporary deficit). Per-entry transfer-then-record mirrors `register_task`
  and keeps the existing conservation proof structure.
- Resource savings of N-1 SAC calls are real but modest next to per-entry
  storage, TTL, and event costs that dominate a large batch. Complexity cost
  outweighs the gas win for the current model.

Detail: [issue #184](https://github.com/soroban-tooling/soroban-keeper-network/issues/184)
(backlog 0115).

### Summary table

| Topic | Outcome | One-line why |
|---|---|---|
| `batch_register_tasks` | **Built** | Amortizes owner-side registration; all-or-nothing + reward ceiling |
| `claim_first_available` | **Built** | Multi-candidate claim without all-or-nothing races |
| Batch claim (all-or-nothing) | **Declined** | Contention makes atomic multi-claim worse than singles |
| Batch execute | **Declined** | One verifier failure would block unrelated payouts |
| Batch cancel | **Declined** | N× `cancel_task` is enough; CEI cost not justified |
| Collapsed escrow transfer | **Declined** | Breaks simple per-task escrow / I-1 story for little gas gain |

### Issue index

| Backlog | GitHub | Topic |
|---|---|---|
| 0097 | #122 | Batch register API design |
| 0098 | #123 | Batch register implementation |
| 0099 | #124 | Batch claim / execute feasibility |
| 0101 | #170+ | `claim_first_available` |
| 0114 | #183 | Batch cancel feasibility |
| 0115 | #184 | Transfer collapsing investigation |
| 0118 | #187 | This retrospective |
