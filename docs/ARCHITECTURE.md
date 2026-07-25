# Architecture

This document describes how the Soroban Keeper Network fits together and the
invariants the `keeper-registry` contract enforces.

## Components

| Component | Location | Role |
|-----------|----------|------|
| `KeeperRegistry` contract | `contracts/keeper-registry` | On-chain coordination: task registry, escrow, fee accounting, admin controls |
| Keeper bot (example) | `examples/keeper-bot` | Off-chain worker that claims, executes, and settles tasks |
| Deploy / optimize scripts | `scripts/` | Build, optimize, and deploy the contract |

## Task lifecycle

```
                 register_task              claim_task            execute_task
   dApp/owner ───────────────▶  PENDING ───────────────▶ CLAIMED ───────────────▶ EXECUTED
                                   │                         │
                       cancel_task │                         │ (deadline passes, unexecuted)
                                   ▼                         ▼
                               CANCELLED                  expire_task ──▶ EXPIRED
```

- **PENDING** — funded and waiting. Owner may `cancel_task` (refund),
  `increase_reward` (top up), or `extend_deadline`.
- **CLAIMED** — a keeper holds an exclusive lock for `lock_ledgers`. After the
  window elapses, any keeper may re-claim (prevents squatting).
- **EXECUTED** — the keeper submitted proof; its net reward is credited to an
  internal balance and later withdrawn.
- **CANCELLED / EXPIRED** — terminal refund states.

## Storage layout

| Scope | Key | Value |
|-------|-----|-------|
| Instance | `Admin`, `FeeBps`, `Paused`, `TaskCounter`, `RewardToken`, `FeesAccrued`, `MinReward` | Global config + counters |
| Persistent | `Task(id)` | Full `Task` record |
| Persistent | `KeeperReward(addr)` | A keeper's withdrawable balance |

## Money invariants

The contract holds exactly the funds it owes. At any time:

```
contract_token_balance == Σ(escrow of PENDING/CLAIMED tasks)
                        + Σ(KeeperReward balances)
                        + FeesAccrued
```

Enforced by:

- **Escrow on register / top-up**, released exactly once on execute (split into
  keeper credit + accrued fee), cancel, or expire.
- **Checks-Effects-Interactions** in `withdraw_rewards` and `sweep_fees`: the
  stored balance is zeroed *before* the token transfer, so a re-entrant reward
  token cannot double-spend.
- **`sweep_fees` bounded by `FeesAccrued`**, so admin can never touch task
  escrow or keeper balances.

The `test_multi_keeper_end_to_end_conserves_funds` and
`test_split_reward_invariants` tests guard these invariants.

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

## Events

Every state transition emits an event so off-chain keepers and indexers can
react without polling storage: `reg`, `claim`, `exec`, `exp`, `cancel`,
`topup`, `extend` (task topics) and `paused`, `fee`, `admin`, `wdraw`
(governance / settlement topics).

## Trust model

- **Keepers are permissionless** — anyone can claim and execute; correctness is
  enforced by the contract, not a whitelist.
- **Admin** controls fee rate, pause, min-reward, upgrade, and fee sweeping —
  but can never seize task escrow or keeper earnings.
- **Owners** fund their own tasks and can always recover funds via cancel
  (pending) or the permissionless expiry path (after deadline).
