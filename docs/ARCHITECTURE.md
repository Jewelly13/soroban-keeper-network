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
