# Batch Operations Design & Integration Guide (E05)

**Status: implemented.** `batch_register_tasks` now exists in
`contracts/keeper-registry/src/lib.rs`, together with the `MAX_BATCH_SIZE`
guard and the `max_total_reward` ceiling this document specified. This
document originally answered issue 0097's design questions and issue 0108's
integration-guide ask by pinning the interface on paper — the same pattern
`docs/VERIFIER_DESIGN.md` used for the E04 verifier field — and it is now
also the reference for the shipped behaviour.

One caveat carried over from the design phase: the batch-size ceiling
(`MAX_BATCH_SIZE`, currently **50**) is a *conservative, not yet empirically
measured* bound. Measuring the real ceiling against Soroban's per-transaction
budget is issue 0104's job (see §4); until it lands, treat 50 as a safety
guard, not a tuned optimum. Callers should read it from the contract via the
`max_batch_size()` view rather than hardcoding it, so a later revision does
not silently invalidate their chunking logic.

## 1. Why batch registration

`register_task` pays a fixed per-call overhead — owner auth, one storage
write, one instance TTL bump, one event — for every single task. A dApp with
many similar tasks to register at once (e.g. a lending protocol opening N
liquidation watches after a market listing) pays that overhead N times over
N separate transactions. `batch_register_tasks` amortizes it: one auth, one
transaction, N storage writes.

## 2. Auth model

**The batch shares one owner and one auth**, exactly like a single
`register_task` call: `owner.require_auth()` is checked once for the whole
batch, not once per entry. This matches the existing single-task UX and
avoids requiring N separate signatures for what the caller intends as one
logical action.

The risk this creates: an owner reviews and signs a transaction containing
batch contents at simulation time, but (in principle) the batch's *effect* —
total escrow pulled — could differ from what was reviewed if the entries
were mutated between simulation and submission. Soroban authorization is
scoped to the specific invocation's arguments (the signed auth entry commits
to the exact `tasks` vector and its rewards), so unlike an ERC-20-style
unlimited approval, this is **not** the same class of risk as an approve/
transferFrom mismatch — the signature is over the actual argument bytes, so
the batch itself cannot be silently swapped for a different one without
invalidating the signature. `max_total_reward` (§6, from issue 0103) exists
as an explicit, human-readable ceiling on top of that: even correctly-signed
batch contents get an extra sum check the caller doesn't have to reason
through the full argument encoding to verify at a glance.

## 3. Partial-failure semantics

**Whole-batch atomicity — no partial success.** A Soroban contract
invocation is atomic by construction: if the function returns `Err`, the
host reverts every storage write and token transfer the call made, and none
of it lands on-chain. There is no host-level primitive for "commit entries
1–7, skip entry 8" within a single function call — partial success would
have to be hand-rolled by catching per-entry failures and choosing to
continue, which this design rejects for two reasons:

- It would mean a batch could pull escrow from the owner for M ≤ N tasks
  while silently dropping the rest, requiring the caller's off-chain code to
  reconcile which task ids were actually created — the same "did my batch
  partially land" bookkeeping problem `execute_task`'s all-or-nothing
  design already avoids for a single task.
- `register_task`'s existing semantics are all-or-nothing per call; batching
  should not introduce a second, inconsistent failure model alongside it.

If any entry has an invalid `reward` (`<= 0` or below `MinReward`), an
invalid `deadline`, oversized `calldata`, or out-of-range `lock_ledgers`/
`ttl_ledgers` — or the batch's total exceeds `max_total_reward` — **the
entire call is rejected and zero transfers occur**, with a typed error
identifying which validation failed. The caller corrects the offending entry
and resubmits the whole batch.

## 4. Resource ceiling

**Enforced at 50 entries (`MAX_BATCH_SIZE`), not yet empirically tuned.** The
contract rejects a batch over that size with `KeeperError::BatchTooLarge`
before doing any work, so an oversized batch fails with an actionable typed
error rather than an opaque host-level resource-exhaustion failure. Measuring
the *real* ceiling against Soroban's per-transaction CPU/memory budget remains
issue 0104's job; 50 is deliberately conservative until then. What informs it:

- Entry count is only half the story. Each entry writes one `Task`, whose
  `calldata` alone may be up to `MAX_CALLDATA_LEN` (1024) bytes — 50 entries
  is already ~50 KB of ledger writes before the rest of the `Task` struct, the
  per-entry token transfer, and the per-entry event are counted. A batch of
  small-`calldata` entries has far more headroom than a batch of
  maximum-sized ones, and a caller who packs both large payloads and many
  entries can still exhaust the budget *below* the 50-entry cap.
- Read the cap from the contract (`max_batch_size()`) rather than hardcoding
  50, so issue 0104's revision does not silently break your chunking.

The rest of this section is the original design-phase reasoning, retained
because it is what issue 0104 should follow when it measures:

- `register_task`'s own per-call cost (auth, one storage write of a
  `Task`-sized value, one TTL bump, one event) is the per-entry unit cost a
  batch of N entries roughly multiplies by N, plus the batch call's own
  fixed overhead (one auth check, iterating the input `Vec`, one aggregate
  `max_total_reward` sum check up front).
- Issue 0107 adds pinned CPU-instruction ceiling tests for `claim_task` and
  `execute_task` using `env.cost_estimate().budget()`
  (`contracts/keeper-registry/src/test.rs`). The same measurement technique
  applies directly to `register_task` and, once implemented,
  `batch_register_tasks` — issue 0104 should measure the real batch call at
  increasing N until it approaches Soroban's per-transaction instruction
  budget, and pin the largest N that stays comfortably under it as the
  practical ceiling, exactly as issue 0104's acceptance criteria describe.
- Until that measurement exists, treat any batch size guidance as
  provisional. The worked example in §7 below illustrates the *reasoning*
  a dApp should apply, using a placeholder ceiling — substitute the real
  number issue 0104 measures and documents.

**The contract enforces an explicit `MAX_BATCH_SIZE` constant**, to be resized
from that measurement with headroom, so a caller that submits too large a
batch gets a clear typed error (`KeeperError::BatchTooLarge`) rather than an
opaque host-level resource-exhaustion failure. This is what issue 0104's
regression test pins against.

## 5. Return shape

`register_task` returns a single `u64`. A batch call returns a `Vec<u64>` in
the same order as the input `tasks` vector, so the caller can zip its own
input list against the result to know which task id corresponds to which
entry it submitted.

## 6. Function signature (as shipped)

```rust
/// One entry in a `batch_register_tasks` call — the same fields
/// `register_task` takes, minus `owner` (shared across the whole batch).
#[contracttype]
#[derive(Clone)]
pub struct BatchTaskParams {
    pub task_type: TaskType,
    pub calldata: Bytes,
    pub reward: i128,
    pub deadline: u64,
    pub ttl_ledgers: u32,
    pub lock_ledgers: u32,
}

/// Registers every entry in `tasks` under a single owner auth. Rejects the
/// entire call (zero transfers, zero tasks registered) if any entry fails
/// `register_task`'s existing per-entry validation, if `tasks` exceeds
/// `MAX_BATCH_SIZE`, or if the sum of all entries' `reward` exceeds
/// `max_total_reward`. Returns task ids in the same order as `tasks`.
pub fn batch_register_tasks(
    e: Env,
    owner: Address,
    tasks: Vec<BatchTaskParams>,
    max_total_reward: i128,
) -> Result<Vec<u64>, KeeperError>;

/// Read-only: the deployed contract's own `MAX_BATCH_SIZE`. Chunk against
/// this rather than a hardcoded constant.
pub fn max_batch_size(e: Env) -> u32;
```

### Error variants introduced

| Variant | Discriminant | Raised when |
|---|---|---|
| `BatchTooLarge` | 20 | `tasks.len() > MAX_BATCH_SIZE` |
| `EmptyBatch` | 21 | `tasks` is empty — rejected rather than treated as a no-op, so a caller whose off-chain filter produced nothing finds out instead of paying for a transaction that registered nothing |
| `BatchRewardCeilingExceeded` | 22 | `sum(tasks[].reward) > max_total_reward` |

A batch also returns any of `register_task`'s existing per-entry errors
(`InvalidReward`, `DeadlinePassed`, `CalldataTooLarge`, `InvalidTaskParams`),
plus `ContractPaused` and `InvalidReward` for a non-positive
`max_total_reward`. Per-entry validation is shared with `register_task`
through one internal helper, so the two paths cannot drift into accepting
different task shapes.

**Validation happens before any transfer.** The implementation sweeps every
entry and totals the rewards up front, then checks the ceiling, and only then
performs the escrow transfers. A batch that will be rejected therefore never
pays for even one cross-contract token transfer first.

## 7. `max_total_reward` and batch-size tradeoffs

These are the two levers a dApp integrator actually controls, and they pull
in opposite directions:

- **`max_total_reward` too tight** (set to exactly the batch's current sum,
  or below it): protects the owner precisely, but any legitimate late
  addition to the batch — or simply computing the sum with a rounding bug —
  makes the whole call fail, including the valid entries within it (§3: no
  partial success). Guidance: set it to the actual sum of the batch you are
  submitting, not a round-number guess; since the whole call is atomic, there
  is no benefit to padding it — padding only widens the window in which a
  batch could authorize more escrow than the caller reviewed, which is the
  exact risk this parameter exists to close (§2).
- **`max_total_reward` too loose** (e.g. an arbitrary large constant "to be
  safe"): defeats the purpose entirely — it stops being a meaningful ceiling
  the owner reviewed and becomes a rubber stamp. Always compute it from the
  batch you are actually about to submit.
- **Batch size too large**: more amortization of the fixed per-call
  overhead, but higher risk of hitting Soroban's per-transaction resource
  budget (§4) — and if it does, the *entire* batch fails atomically, so an
  oversized batch doesn't even partially help. Guidance: chunk large
  worklists into batches at or under the measured/documented ceiling (§4),
  not in one unbounded call.
- **Batch size too small**: safely under budget, but reduces to close to
  `register_task`'s per-call overhead again, eroding the reason to batch at
  all. Guidance: batch as close to the measured ceiling as comfortably fits,
  not the smallest number that "feels safe."

## 8. Worked example — a dApp with N pending tasks

Scenario: a lending protocol has identified 40 undercollateralized positions
after a price move and needs a liquidation task registered for each one,
in one submission rather than 40 separate transactions.

**Sizing against the ceiling (§4):** sizing is a single division against the
enforced `MAX_BATCH_SIZE` (currently 50 — read it from `max_batch_size()`
rather than hardcoding, since issue 0104 may revise it). This dApp's 40
positions fit in one batch. If the same dApp instead had 500 positions to
register, it would split into `ceil(500 / 50) = 10` batches of at most 50
entries each, computing a separate `max_total_reward` per batch (the sum of
*that batch's* rewards only — never the sum across batches, since each is an
independent atomic call per §3).

### Raw contract-call context (Rust cross-contract call)

```rust
// In the calling dApp contract. `registry` is the KeeperRegistry contract
// address; `positions` is this contract's own list of undercollateralized
// position ids, already computed off-chain or by a prior on-chain step.
let registry = KeeperRegistryClient::new(&env, &registry_contract_id);

let mut tasks: Vec<BatchTaskParams> = Vec::new(&env);
let mut total_reward: i128 = 0;
for position_id in positions.iter() {
    let reward = liquidation_bounty_for(&env, position_id); // this dApp's own sizing logic
    total_reward = total_reward.checked_add(reward).expect("reward sum overflow");
    tasks.push_back(BatchTaskParams {
        task_type: TaskType::Liquidation,
        calldata: encode_liquidation_calldata(&env, position_id),
        reward,
        deadline: env.ledger().timestamp() + 3600,
        ttl_ledgers: 17_280,
        lock_ledgers: 120,
    });
}

// max_total_reward is the exact sum just computed above — not padded (§7).
match registry.try_batch_register_tasks(&env.current_contract_address(), &tasks, &total_reward) {
    Ok(Ok(task_ids)) => {
        // task_ids[i] corresponds to positions[i] — zip them to record locally.
    }
    Ok(Err(KeeperError::BatchTooLarge)) => {
        // Split `positions` into smaller chunks (§4/§7) and resubmit each
        // chunk as its own batch call — do not assume any entries landed.
    }
    Ok(Err(_)) | Err(_) => {
        // Any other rejection: zero tasks were registered, zero escrow
        // moved (§3). Fix the flagged entry and resubmit the whole batch.
    }
}
```

### Off-chain context (Node.js)

A runnable owner-side script implementing exactly this flow — reading a task
list from JSON or CSV, validating entries locally, chunking against
`max_batch_size()`, and reporting the returned ids — lives in
[`examples/batch-register/`](../examples/batch-register/README.md). Its README
covers the file format and walks through the `max_total_reward` choice in §7
terms.

### Raw contract-call context (Soroban CLI)

```bash
stellar contract invoke \
  --id "$REGISTRY_CONTRACT_ID" \
  --source owner \
  --network testnet \
  -- \
  batch_register_tasks \
  --owner "$OWNER_ADDRESS" \
  --tasks '[{"task_type":"Liquidation","calldata":"...","reward":"1000000","deadline":"1780000000","ttl_ledgers":17280,"lock_ledgers":120}, ...]' \
  --max_total_reward "40000000"
```

### Handling a rejected batch

Because failure is all-or-nothing (§3), the caller's error-handling only has
two cases to reason about, not "how many of my 40 tasks actually landed":

1. **Validation error on a specific entry** (bad reward, oversized calldata,
   etc.) or **sum exceeds `max_total_reward`**: zero tasks were registered.
   Fix the flagged entry (or recompute the sum) and resubmit the identical
   batch — no cleanup of partially-created state is ever needed.
2. **`BatchTooLarge`** (or an opaque resource-exhaustion failure, which the
   `MAX_BATCH_SIZE` guard in §4 is meant to prevent from ever surfacing this
   way): split the worklist into smaller batches at or under the documented
   ceiling and submit them as separate calls.
