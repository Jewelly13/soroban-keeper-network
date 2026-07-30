# Architecture

## Overview

Soroban Keeper Network is a task registry for permissionless automation on Stellar. Task owners escrow a reward, keepers claim and execute eligible tasks, and the registry routes the reward either back to the owner or to the keeper who completed the task.

The registry is deliberately small: task state, reward accounting, authorization, and token movement are all handled by the `keeper-registry` contract. The configured reward token is held by the registry while a task is open. Keeper rewards are first credited in registry storage and are paid out when the keeper calls `withdraw_rewards`.

## Components

### Keeper registry contract

The registry owns the task lifecycle and stores:

- the administrator and pause state;
- the configured reward token;
- the next task id;
- fee configuration and the accrued-fee accumulator;
- task records; and
- credited keeper balances.

### Reward token

The registry transfers the configured token when a task is registered, topped up, cancelled, expired, executed, or when a keeper withdraws rewards. The registry is therefore written against a token contract boundary and must preserve its safety properties even when token transfers are treated as external interactions.

### Owners and keepers

An owner creates and funds a task. A keeper may claim an eligible task, execute it with the required proof or calldata, and receive the net reward as a credited balance. A keeper withdraws that balance independently of the task lifecycle.

## Task lifecycle

A task begins in `Pending` with its reward held by the registry.

1. The owner registers the task and transfers the reward into the registry.
2. An eligible keeper may claim it, moving it to `Claimed`.
3. The owner may cancel a pending task and recover the escrow.
4. After the deadline, anyone may expire a pending or claimed task and return the escrow to the owner.
5. A claimant may execute a claimed task. The task becomes terminal, the protocol fee is accumulated, and the keeper's net reward is credited.
6. The keeper may withdraw the credited balance, including while the registry is paused.

Terminal task states are `Executed`, `Cancelled`, and `Expired`. Terminal tasks cannot be claimed, executed, cancelled, or otherwise used as a source of another payout.

## Storage and accounting

Task ids are allocated from the `next_task_id` counter. Task records are stored in persistent storage and include the owner, reward, deadline, lock configuration, status, and other execution parameters. Keeper balances and the fee accumulator are separate accounting buckets from task escrow.

The reward amount is split on execution using the configured basis-point fee. The fee is accumulated for the administrator and the keeper's net amount is credited to the keeper. Credits remain obligations of the registry until withdrawn.

Persistent task storage has a caller-supplied ledger TTL. The deadline is a timestamp in seconds, while TTL is measured in ledgers; these are different units. A task must remain addressable for every operation that can release its escrow. The current implementation has a known gap because TTL and deadline are not required to remain compatible; see [issue #0005](https://github.com/soroban-tooling/soroban-keeper-network/issues/5).

## Money invariants

These are the contract's required money-safety properties. They are intentionally stated as assertions over observable state and transfers so that property tests and fuzzing can encode them directly. A change that moves funds, changes task terminal transitions, changes fee accounting, or changes storage lifetime must be reviewed against this section.

### I-1: Registry solvency is conserved

**Statement.** At every successful transaction boundary, the registry's balance of the configured reward token equals the sum of:

- rewards of all non-terminal tasks whose escrow is still held by the registry;
- all credited but not yet withdrawn keeper balances; and
- the `FeesAccrued` amount not yet swept by the administrator.

Equivalently, every token entering the registry is represented by exactly one open-task escrow, keeper credit, or accrued-fee unit, and every token leaving one of those buckets is represented by the corresponding state reduction. Rounding in the fee split is included in this equality.

**Why.** A deficit means one user can withdraw funds belonging to another user, while an unexplained surplus means an owner's or keeper's funds have been stranded or mis-accounted. Solvency is the conservation property on which the other invariants depend.

**Enforced by.** `register_task` and `increase_reward` transfer funds into the registry and increase task escrow; `cancel_task` and `expire_task` resolve escrow to the owner; `execute_task` marks the task terminal, increments `FeesAccrued`, and credits the keeper; `withdraw_rewards` decreases the keeper credit only when it transfers the same amount; and `sweep_fees` is limited by the fee accumulator. Checked arithmetic and the terminal-state guards prevent accounting entries from being created or removed without the associated transition.

**Breaks if.** A payout transfers tokens without first consuming the corresponding escrow or keeper credit; a fee is added to `FeesAccrued` but not retained in the registry; `sweep_fees` can exceed the accumulator; or a storage eviction removes an open task record while its escrow remains in the registry. The TTL/deadline gap in [issue #0005](https://github.com/soroban-tooling/soroban-keeper-network/issues/5) is a known recoverability risk that can become a solvency/accounting failure.

### I-2: Every escrow is recoverable

**Statement.** For every task whose reward has been transferred into the registry and whose status is non-terminal, there is a reachable valid sequence that resolves the escrow: the owner can cancel while the task is `Pending`, anyone can expire it after its deadline while it is `Pending` or `Claimed`, or an eligible claimant can execute it and withdraw the resulting keeper credit. No valid state transition may leave the reward in the registry with no callable release path.

**Why.** Escrow that cannot reach an owner refund or keeper payout is permanently stranded. This violates the owner-facing promise that cancellation and expiry recover funds and the keeper-facing promise that successful execution can be withdrawn.

**Enforced by.** The status and authorization checks in `cancel_task`, the deadline and status checks in `expire_task`, the claim and execution checks in `claim_task` and `execute_task`, and the independent credit withdrawal path in `withdraw_rewards`. Task records are retained through the period in which these paths may be needed.

**Breaks if.** The task record expires before its deadline, the deadline is extended beyond storage coverage, a terminal transition is accepted without resolving the reward, or pausing also gates `withdraw_rewards`. The TTL-versus-deadline defect is tracked in [issue #0005](https://github.com/soroban-tooling/soroban-keeper-network/issues/5).

### I-3: Each task has exactly one payout

**Statement.** For any task, exactly one terminal transition consumes its escrow, and the total amount attributable to that reward is paid or credited exactly once: the full reward to the owner on cancellation or expiry, or the reward split into one protocol fee and one keeper credit on execution. A second terminal transition for the same task must fail and must move no tokens.

**Why.** Paying twice makes the registry insolvent at the expense of other tasks, keeper balances, or fees. Paying zero times strands the owner's escrow.

**Enforced by.** `cancel_task`, `expire_task`, and `execute_task` reject terminal task statuses. Each terminal transition writes the terminal status and saves the task before performing its token transfer or credit-producing interaction, following checks-effects-interactions. The execution path credits the keeper rather than providing an additional task payout path.

**Breaks if.** A transfer occurs before the terminal status is persisted, a terminal status is added to an accepting `match` arm, a new payout path skips the status guard, or a re-entrant token transfer can observe the old task status. The historical CEI defects are tracked in [issue #0002](https://github.com/soroban-tooling/soroban-keeper-network/issues/2) and [issue #0003](https://github.com/soroban-tooling/soroban-keeper-network/issues/3); those issues must be resolved before this property can honestly be described as present in deployed code.

### I-4: Fees are bounded and rounded down

**Statement.** For every executed task with reward `R` and configured `fee_bps` `F`, the protocol fee is `floor(R * F / 10_000)`, so `0 <= fee <= R * F / 10_000` and the keeper credit is `R - fee`. The administrator may withdraw at most the current `FeesAccrued` amount, and every successful sweep decreases that accumulator by exactly the swept amount.

**Why.** The fee is a protocol charge, not an additional claim on escrow. Bounding it protects task owners and keepers from overcharging; bounding sweeps protects all users from the administrator withdrawing escrow or keeper obligations. Flooring means the protocol may receive marginally less than the nominal percentage, never more.

**Enforced by.** The fee-basis-point validation and checked split arithmetic in `execute_task`, the fee accumulation performed with the execution accounting, and the `FeesAccrued` limit checked by `sweep_fees`.

**Breaks if.** Fee arithmetic rounds up, uses a denominator other than 10,000, permits `fee_bps` above its configured maximum, or lets `sweep_fees` transfer an amount greater than the accumulator. Any change to fee defaults or rounding must also be checked against [issue #0020](https://github.com/soroban-tooling/soroban-keeper-network/issues/20).

### I-5: Escrow and keeper credits are isolated from administration

**Statement.** Administrative operations cannot decrease an open task's escrow or a keeper's credited balance. The only administrative token outflow is a `sweep_fees` transfer bounded by `FeesAccrued`; configuration, pause, admin transfer, and upgrade operations do not move reward tokens.

**Why.** The administrator is entitled to accrued protocol fees, not to user escrow or earned keeper rewards. Mixing these buckets would make an apparently authorized operation an insolvency mechanism.

**Enforced by.** Separate storage for task rewards, keeper balances, and `FeesAccrued`; authorization on administrative mutations; and the accumulator bound in `sweep_fees`. Ordinary task transitions, rather than admin functions, consume escrow and create keeper credits.

**Breaks if.** `sweep_fees` uses the registry's raw token balance as its limit, an admin method transfers the entire balance, or an administrative mutation rewrites task or keeper accounting. The relevant property-test plan is tracked in [issue #0058](https://github.com/soroban-tooling/soroban-keeper-network/issues/58).

### I-6: Credited keeper balances remain withdrawable

**Statement.** If a keeper has a credited balance `B > 0`, a valid call to `withdraw_rewards` can transfer exactly `B` to that keeper and leave its credited balance at zero. This remains true while the contract is paused and after any sequence of other valid pause and unpause operations.

**Why.** Pausing is an emergency control over new activity, not permission to confiscate or indefinitely freeze earned rewards. Withdrawal liveness is the promise that makes the pause mechanism acceptable to keepers.

**Enforced by.** `withdraw_rewards` reads and clears only the caller's keeper credit and is not gated by the pause switch. The transfer amount is the credited amount, and the credit is zeroed as part of the withdrawal effects.

**Breaks if.** A pause guard is added to `withdraw_rewards`, withdrawal clears a credit without transferring it, withdrawal transfers a different amount, or an admin operation can reduce keeper credits. The generalized property-test requirement is tracked in [issue #0059](https://github.com/soroban-tooling/soroban-keeper-network/issues/59).

### I-7: Task ids are monotonic and never reused

**Statement.** Every successful registration returns a task id strictly greater than every previously allocated id. The counter is never decremented or recycled when a task is cancelled, expired, or executed; therefore an external reference to a task id remains unique and stable for the lifetime of the contract.

**Why.** Indexers, keeper bots, and users use task ids as durable references. Reusing an id could make an old reference point at a different owner's escrow or execution policy.

**Enforced by.** `register_task` allocates from `next_task_id` and increments the counter once for each successful registration. Terminal transitions modify task status but do not return ids to an allocation pool.

**Breaks if.** Registration derives ids from the number of currently open tasks, decrements the counter after deletion, or silently wraps the counter on overflow. The monotonicity property and the explicit overflow boundary are covered by [issue #0060](https://github.com/soroban-tooling/soroban-keeper-network/issues/60).
That top-level statement is decomposed into seven named invariants,
`I-1` through `I-7`. Each is referenced by this identifier elsewhere in the
repo (property tests, the shared invariant-checker module, fuzz targets) so
a single name always means the same check.

- **I-1 — Solvency.** The registry's token balance always equals open task
  escrow plus credited keeper balances plus accrued fees (the equation
  above, taken as a whole).
- **I-2 — Escrow recoverability.** Every escrowed reward has at least one
  reachable path back out: to the owner via `cancel_task` or `expire_task`,
  or to a keeper via `execute_task` then `withdraw_rewards`. No state
  strands funds permanently.
- **I-3 — Single payout.** Each task's reward is paid out exactly once —
  never zero times, never twice. (Wave 1 fixed two concrete CEI-ordering
  violations of this, issues 0002/0003.)
- **I-4 — Fee bounding.** The protocol never takes more than `fee_bps` of a
  reward, and the admin can never sweep more than has accrued. The fee is
  floored by integer division, so the protocol may take marginally *less*
  than the nominal rate — never more.
- **I-5 — Escrow isolation.** Admin functions can never touch task escrow
  or credited keeper balances. `sweep_fees` is bounded by the
  `FeesAccrued` accumulator specifically to enforce this.
- **I-6 — Withdrawal liveness.** A keeper's credited balance is always
  withdrawable, including while the contract is paused — this is the
  promise that makes pausing acceptable to keepers.
- **I-7 — Monotonic task ids.** Task ids are unique and never reused, so an
  external reference to a task id (an off-chain indexer, a keeper bot's
  local state, a dApp's UI) is stable forever. `next_task_id` increments a
  `u64` counter and never decrements it.

Enforced by:

- **Escrow on register / top-up**, released exactly once on execute (split into
  keeper credit + accrued fee), cancel, or expire. (I-1, I-2, I-3)
- **Checks-Effects-Interactions** in `withdraw_rewards` and `sweep_fees`: the
  stored balance is zeroed *before* the token transfer, so a re-entrant reward
  token cannot double-spend. (I-3, I-6)
- **`sweep_fees` bounded by `FeesAccrued`**, so admin can never touch task
  escrow or keeper balances. (I-4, I-5)
- **`next_task_id`** is a monotonically incrementing `u64` counter with no
  decrement or reset path. (I-7)

The `test_multi_keeper_end_to_end_conserves_funds` and
`test_split_reward_invariants` tests guard these invariants with fixed
scenarios. `contracts/keeper-registry/src/invariants.rs` exposes one
`assert_*` function per `I-N` invariant, shared between the `proptest`-based
property tests in `test.rs` and the fuzz targets under `fuzz/fuzz_targets/`,
so both call the same assertion logic instead of maintaining parallel
copies that can drift apart.

## TTL / deadline invariant

`Task.deadline` (a unix timestamp, seconds) and `Task.ttl_ledgers` (a Persistent
storage TTL, ledgers) are different units with no fixed conversion. If a task's
storage entry could expire before its deadline, the entry — and the escrow
functions that depend on `load_task` (`cancel_task`, `expire_task`,
`execute_task`) — become permanently unreachable once the entry is evicted,
stranding the escrowed reward with no recovery path.

The contract enforces, by construction, that a task's storage always outlives
its deadline:

```
ttl_ledgers >= (deadline - now) / SECONDS_PER_LEDGER + TTL_SAFETY_MARGIN_LEDGERS
```

- `SECONDS_PER_LEDGER = 5` — a conservative estimate of Stellar's ledger close
  time, used only to convert the deadline into a ledger count. Over-estimating
  the ledger rate over-provisions TTL, which is the safe direction to be wrong.
- `TTL_SAFETY_MARGIN_LEDGERS = 17_280` (~1 day) — extra ledgers kept beyond the
  deadline so `expire_task` remains callable for a while after the deadline
  passes.

`register_task` rejects a `ttl_ledgers` that doesn't satisfy this with
`TtlTooShort`, and `extend_deadline` applies the same check against the task's
existing `ttl_ledgers` before accepting a new, later deadline — so an owner
cannot push the deadline out from under the TTL. `save_task` also re-extends
the entry's TTL on every mutation (claim, execute, top-up, deadline change),
so an active task's storage lifetime keeps moving forward rather than only
being set once at registration.

## Batch registration

`batch_register_tasks(owner, tasks: Vec<TaskParams>, max_total_reward)` registers
many tasks for one owner in a single transaction, amortizing the per-transaction
overhead a dApp would otherwise pay once per task.

**Auth.** Every entry shares one `owner`, who authorizes once. That is the point
of the entry point, but it means the owner signs for a total they cannot read off
the signature payload. `max_total_reward` is the mitigation: the owner commits to
a ceiling upfront and the call is rejected with `BatchRewardCeilingExceeded` if
the entries sum above it, before any transfer happens.

**All-or-nothing.** Soroban transactions are atomic, so partial success is not
expressible — returning `Err` discards every state change and transfer made
during the call. Validation nonetheless runs over all entries before any
transfer, so common rejections never touch the token contract at all rather than
relying on rollback. An empty batch is an accepted no-op.

**Validation parity.** Both entry points run the same `validate_task_params` and
`validate_ttl_covers_deadline` helpers, so a batch can never accept an entry
`register_task` would reject. `batch_rejects_each_invalid_entry_exactly_as_register_task_does`
asserts that equivalence case by case.

**Size cap.** `MAX_BATCH_ENTRIES` is 32, rejected with `BatchTooLarge`. CPU is
not what makes 32 the cap — a full batch measures ~6.5M instructions, 6.5% of the
100M transaction budget, scaling linearly at ~202k per entry. The binding
constraint is the per-transaction ledger write-entry footprint
(`txMaxWriteLedgerEntries`), one persistent entry per task, which is a live
network config the test host does not enforce. See the constant's doc comment.

**Conservation.** `tests/model.rs` extends the I-1 solvency property to arbitrary
interleavings of single and batch registration, and adds a batch-specific
property: after any rejected batch — invalid entry, ceiling exceeded, or
oversized — the owner's balance and the registry's escrow are both provably
unchanged.
## Instance TTL and traffic assumptions

Instance storage holds the admin, reward token, pause flag, fee, and task
counter. Every entry point reads it, so it must not lapse on a contract that is
still in use. `bump_instance` renews it from every state-mutating call using
`extend_ttl(INSTANCE_BUMP_THRESHOLD, INSTANCE_BUMP_LEDGERS)`.

Issue 0112 asked for those two constants — originally round numbers from rough
ledger-time math — to be re-derived against real traffic. They were, and they
are unchanged. The behaviour is pinned by
`contracts/keeper-registry/tests/instance_ttl_tuning.rs`:

```
cargo test -p keeper-registry --test instance_ttl_tuning -- --nocapture
```

### Ledger rate: measured, not assumed

Sampling 300,000 consecutive testnet ledgers (3,577,512 → 3,877,512,
2026-07-13 to 2026-07-30) gives **5.009 s/ledger**, against the
`SECONDS_PER_LEDGER = 5` the constants assume — accurate to 0.2%, and
conservative in the safe direction. So:

| Constant                  | Ledgers | Wall-clock at measured rate |
|---------------------------|---------|-----------------------------|
| `INSTANCE_BUMP_LEDGERS`   | 100,000 | 5.80 days of lifetime       |
| `INSTANCE_BUMP_THRESHOLD` | 50,000  | renewal opens at 2.90 days left |

### Real traffic data: none exists

The testnet deployment recorded in [DEPLOYMENTS.md](../DEPLOYMENTS.md) was
created 2026-07-14 and has 4 events, all within 7 minutes of deployment, and
nothing since. There is no call-frequency distribution to fit. The constants are
therefore justified against an explicit assumption rather than observed
frequency, and that assumption is stated below so it can be challenged when data
does exist.

### Cost is not the binding constraint

Measured on `set_fee_bps`, the cheapest mutating entry point:

| State                             | CPU    | Memory       |
|-----------------------------------|--------|--------------|
| Renewal short-circuited (safe band) | 82,446 | 13,588 bytes |
| Renewal performed (danger band)     | 85,744 | 15,252 bytes |
| **Delta**                           | +3,298 | +1,664 bytes |

The threshold caps renewal at once per 50,000 ledgers of *elapsed time*
regardless of call volume, so the amortized cost is negligible at any traffic
level — ~0.003% of a 100M-instruction transaction budget, collected at most once
every 2.9 days. Moving the threshold in either direction buys nothing
measurable, which is why it was left alone.

### The assumption that does bind

A registry only needs to survive while it holds escrow, and **a registry holding
escrow cannot go silent**. `expire_task` is permissionless and mutating, and
becomes callable the moment any task's deadline passes — so a registry with
anything at stake always has a call available to renew it, whether from the
owner recovering funds or a keeper bot doing it as a courtesy while scanning.

The registry that *can* archive is one with no open tasks. That case strands
nothing: instance state is not lost, only made inaccessible until a
RestoreFootprint brings it back with its values intact. This is the same
tradeoff `bump_instance` documents for declining to renew on read-only views,
which are simulated by clients for free and must stay side-effect-free.

### If this needs revisiting

Should real traffic data appear and 5.8 days of idle tolerance prove too short,
the lever is `INSTANCE_BUMP_LEDGERS`, not the threshold. Rent is charged per
ledger extended, so a larger window costs proportionally more per renewal but
renews proportionally less often — roughly rent-neutral, while strictly
improving idle tolerance. Both values sit far below the network's
`max_entry_ttl`, and persistent entries clamp rather than fail at that ceiling,
so there is headroom.
### `save_task` TTL-extension cost

`save_task` calls `extend_ttl` unconditionally, including on writes that leave
`ttl_ledgers` untouched (`claim_task`, `execute_task`, `increase_reward`).
Issue 0111 asked whether that unconditional call is paying an avoidable cost
and whether a read-and-compare guard should be added. It was measured, and the
answer is no on three independent grounds. The measurements live in
`contracts/keeper-registry/tests/ttl_extension_cost.rs` and are reproducible
with:

```
cargo test -p keeper-registry --test ttl_extension_cost -- --nocapture
```

**1. The guard is not implementable.** Reading an entry's current TTL is the
load-bearing half of "read and compare", and contract code cannot do it.
`Storage::get_ttl` exists only on the `soroban_sdk::testutils::storage` traits,
compiled under `testutils` and absent from the contract-facing API. The
underlying host function is likewise absent from the WASM host interface in
`soroban-env-common`'s `env.json`; the only TTL reader exposed to contracts is
`get_max_live_until_ledger`, which returns the network-wide maximum rather than
this entry's remaining lifetime.

**2. The host already short-circuits.** `Storage::extend_ttl` in
`soroban-env-host` guards its storage-map insert behind
`new_live_until > old_live_until && old_live_until - ledger_seq <= threshold`,
so a redundant extension skips the expensive half host-side. Measured on
soroban-sdk 22, against a 25,826-instruction baseline persistent write:

| Call                                      | CPU delta | Memory delta |
|-------------------------------------------|-----------|--------------|
| `extend_ttl`, redundant (short-circuited)  | +2,437    | +376 bytes   |
| `extend_ttl`, effective (insert performed) | +3,642    | +664 bytes   |

The redundant call costs ~2.4k instructions — roughly 0.002% of Soroban's
100M-instruction transaction budget. That figure is also the ceiling on what
any guard could save, since a guard can at best elide the call entirely.

**3. The redundant case barely arises.** The premise that a write leaving
`ttl_ledgers` unchanged performs a redundant extension conflates two different
things. `save_task` passes `extend_to = task.ttl_ledgers`, a TTL measured
*from the current ledger*, not an absolute expiry — so an unchanged
`ttl_ledgers` still means a genuinely later `live_until_ledger` whenever any
ledger has closed since the previous write. An extension is redundant only for
two writes landing in the *same* ledger, and a task's lifecycle calls are
necessarily separate transactions. On the common path the extension is doing
real work, which is exactly what the TTL/deadline invariant above depends on.

No code change was made. `every_lifecycle_write_restores_the_full_ttl_window`
is the regression test guarding conclusion 3: it walks a task through
registration, top-up, deadline extension, claim, and execution with ledgers
closing in between, and asserts the entry's TTL is restored to the full window
after each write. A future guard that skipped extension too eagerly would fail
it rather than silently reintroducing the early-archival risk.

## Review checklist

For a change that touches fund movement or task lifecycle, reviewers should verify:

- Which accounting bucket changes, and is the matching token movement present?
- Can a terminal transition be entered twice, including through a token callback?
- Does every open escrow still have a reachable release path after the change?
- Can an administrator affect task escrow or keeper credits?
- Is the fee no greater than the configured floored percentage?
- Can a keeper withdraw an existing credit while paused?
- Are task ids still strictly increasing?
- If deadlines or storage TTLs change, does the task remain addressable until its escrow is resolved?

These invariants are the specification for the property-based and fuzzing work. Tests should cite the invariant identifier they enforce.
