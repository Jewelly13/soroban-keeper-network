# Soroban Keeper Network

> **The decentralized automation & upkeep layer for the Stellar/Soroban ecosystem.**
> Chainlink Keepers — but native to Soroban.

[![CI](https://github.com/soroban-tooling/soroban-keeper-network/actions/workflows/ci.yml/badge.svg)](https://github.com/soroban-tooling/soroban-keeper-network/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Built on Soroban](https://img.shields.io/badge/built%20on-Soroban-blueviolet)](https://soroban.stellar.org)

## Documentation

| Doc | What's inside |
|-----|---------------|
| [Live demo](docs/DEMO.md) | Deployed testnet contract and full on-chain transaction trace |
| [Architecture](docs/ARCHITECTURE.md) | Components, task lifecycle, storage, money invariants, and trust model |
| [Deploying & running](docs/DEPLOYING.md) | Testnet deployment walkthrough and keeper-bot operator guide |
| [Deployments](DEPLOYMENTS.md) | Canonical record of on-chain addresses |
| [Security policy](SECURITY.md) | How to report a vulnerability |
| [Live demo](docs/DEMO.md) | Deployed testnet contract + full on-chain transaction trace |
| [Architecture](docs/ARCHITECTURE.md) | Components, task lifecycle, storage, money invariants, trust model |
| [Fuzzing & property testing](docs/FUZZING.md) | Running/adding fuzz targets, the shared invariant module, crash-to-regression convention |
| [Verifier design (E04)](docs/VERIFIER_DESIGN.md) | Proposed `IKeeperVerifier` interface for optional on-chain proof verification |
| [Deploying & running](docs/DEPLOYING.md) | Testnet deploy walkthrough and keeper-bot operator guide |
| [Deployments](DEPLOYMENTS.md) | Canonical record of on-chain addresses |
| [Contributing](CONTRIBUTING.md) | How to pick up an issue and open your first PR |
| [Changelog](CHANGELOG.md) | Notable changes |

**Quick start:** `make help` lists every common command (build, test, fmt, lint, wasm, optimize, bot).

---

## Problem & Solution

### The Problem

Every DeFi protocol running on Soroban has **time-sensitive operations** that must be triggered by an external agent:

- **Liquidations** — health factor drops below threshold → position must be liquidated
- **Oracle price pushes** — off-chain price must be written on-chain every N seconds
- **Funding rate updates** — perpetuals markets need periodic rate settlements
- **LP rebalancing** — concentrated liquidity positions fall outside active range
- **TTL extensions** — Soroban's storage expiry model means contract data expires unless refreshed

Today, each protocol runs its own centralised bot, creating:

| Pain | Impact |
|------|--------|
| Single point of failure | Missed liquidations → bad debt, insolvency |
| High ops burden | Every team re-invents the same infrastructure |
| No economic incentives | Bots run at a loss; sustainability risk |
| Opaque | No on-chain record of who executed what and when |

### The Solution — Soroban Keeper Network

A **shared, permissionless, on-chain coordination layer** where:

- **dApps** register automation tasks with an XLM reward bounty.
- **Anyone** can run a keeper bot to claim and execute tasks, earning rewards.
- **The registry contract** enforces fairness, handles escrow, and emits events.
- **No trust required** — keepers are economically incentivised, not whitelisted.

```
┌─────────────────────────────────────────────────────────┐
│                    dApp / Protocol                      │
│  (lending protocol, DEX, perps, oracle aggregator...)   │
└────────────────┬────────────────────────────────────────┘
                 │  register_task(reward, calldata, deadline)
                 ▼
┌─────────────────────────────────────────────────────────┐
│              KeeperRegistry Contract                    │
│  ┌──────────────┐  ┌─────────────┐  ┌───────────────┐  │
│  │ Task Storage │  │  Fee Logic  │  │  Auth / Pause │  │
│  └──────────────┘  └─────────────┘  └───────────────┘  │
└────────────────┬────────────────────────────────────────┘
                 │  events: TaskRegistered, TaskClaimed, TaskExecuted
                 ▼
┌──────────────────────────────────────────────────────────────┐
│                 Off-Chain Keeper Bots (permissionless)        │
│  Bot A   Bot B   Bot C   ... (anyone can run one)            │
│  ┌────────────────────────────────────────────────────────┐  │
│  │ 1. Listen to events                                    │  │
│  │ 2. claim_task(task_id)                                 │  │
│  │ 3. Execute underlying action (liquidate, push price…)  │  │
│  │ 4. execute_task(task_id, proof)                        │  │
│  │ 5. withdraw_rewards()                                  │  │
│  └────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘
```

---

## Key Features

### MVP (v1 — This Repo)

- [x] **Task Registry** — any Soroban contract or EOA registers tasks with XLM reward
- [x] **Permissionless claiming** — first keeper to claim wins lock rights
- [x] **Lock period** — prevents spam claims while giving the claimer time to execute
- [x] **Re-claim after lock expiry** — unresponsive keepers lose their lock
- [x] **Execution proof** — keepers submit a tx hash / state witness for transparency
- [x] **Reward escrow** — XLM held in contract until task is executed or expired
- [x] **Auto-expiry** — permissionless `expire_task` refunds owner after deadline
- [x] **Task cancellation** — owner can cancel a Pending task and receive refund
- [x] **Protocol fee** — configurable basis-point fee taken from rewards
- [x] **Upgradeable** — admin can upgrade WASM via Soroban's native pattern
- [x] **Pause/unpause** — emergency circuit breaker
- [x] **Full event log** — `TaskRegistered`, `TaskClaimed`, `TaskExecuted`, `TaskExpired`, `TaskCancelled`

### Phase 2 (Roadmap)

- [ ] **On-chain execution verifier interface** — target contracts implement `IKeeperVerifier` and the registry calls them to verify execution succeeded
- [ ] **Batch task registration** — register multiple tasks in one transaction
- [ ] **EIP-like task conditions** — on-chain `checkUpkeep` callback before claiming
- [ ] **Keeper reputation scores** — slash stake for missed executions
- [ ] **Keeper staking** — stake XLM or governance token for priority and dispute resolution
- [ ] **Governance token ($KPRS)** — vote on fee parameters, upgrades, whitelists
- [ ] **Treasury contract** — protocol fees flow to stakers
- [ ] **Subgraph / indexer** — TheGraph-style event indexing for analytics

### Phase 3 (Vision)

- [ ] **Cross-contract task composition** — chain multiple operations as a single task
- [ ] **Decentralized oracle integration** — task conditions driven by Reflector/Band
- [ ] **SDK libraries** — TypeScript + Rust SDKs so dApps integrate in < 1 hour
- [ ] **Keeper DAO** — fully on-chain governance of protocol parameters
- [ ] **Stellar Community Fund grant round** — sustained ecosystem funding

---

## Architecture Diagram

```
┌────────────────────────────────────────────────────────────────────────────┐
│                        Soroban Keeper Network                              │
├────────────────────────────────────────────────────────────────────────────┤
│                                                                            │
│  ┌─────────────────────────────────────────────────────────────────────┐  │
│  │                    KeeperRegistry Contract                          │  │
│  │                                                                     │  │
│  │  Instance Storage (hot, short-TTL)                                  │  │
│  │  ┌──────────┬─────────┬────────┬─────────────┬─────────────┬──────┐ │  │
│  │  │  Admin   │ FeeBps  │ Paused │ TaskCounter  │ RewardToken │ Fees │ │  │
│  │  └──────────┴─────────┴────────┴─────────────┴─────────────┴──────┘ │  │
│  │                                                                     │  │
│  │  Persistent Storage (task lifetime)                                 │  │
│  │  ┌────────────────────────────────────────────────────────────┐    │  │
│  │  │  Task(id) → { owner, type, calldata, reward, deadline,     │    │  │
│  │  │               status, claimer, claim_ledger, lock_ledgers } │    │  │
│  │  └────────────────────────────────────────────────────────────┘    │  │
│  │  ┌────────────────────────────────────────────────────────────┐    │  │
│  │  │  KeeperReward(address) → i128  (claimable balance)         │    │  │
│  │  └────────────────────────────────────────────────────────────┘    │  │
│  │                                                                     │  │
│  │  External (Token)                                                   │  │
│  │  ┌────────────────────────────────────────────────────────────┐    │  │
│  │  │  SAC / XLM token contract (transfer, balance)              │    │  │
│  │  └────────────────────────────────────────────────────────────┘    │  │
│  └─────────────────────────────────────────────────────────────────────┘  │
│                                                                            │
│  ┌────────────────┐    ┌─────────────────────────┐    ┌───────────────┐   │
│  │  dApp Contract │───▶│  register_task (XLM dep) │───▶│  TaskRegistered│  │
│  └────────────────┘    └─────────────────────────┘    │     Event     │   │
│                                                        └───────┬───────┘   │
│  ┌────────────────┐    ┌─────────────────────────┐            │           │
│  │  Keeper Bot A  │───▶│  claim_task             │◀───────────┘           │
│  └────────────────┘    └─────────────────────────┘                        │
│         │              ┌─────────────────────────┐    ┌───────────────┐   │
│         └─────────────▶│  execute_task + proof   │───▶│ TaskExecuted  │   │
│                        └─────────────────────────┘    │     Event     │   │
│                                                        └───────────────┘   │
└────────────────────────────────────────────────────────────────────────────┘
```

---

## Product Requirements Document (PRD)

### User Stories

#### dApp Developers / Protocol Owners

| As a... | I want to... | So that... |
|---------|-------------|-----------|
| Lending protocol | Register a liquidation task when a position is undercollateralised | My protocol remains solvent without running my own bot |
| Oracle provider | Register periodic price-push tasks with a time deadline | Prices stay fresh without centralised infrastructure |
| Perp DEX | Register funding rate settlement tasks every 8 hours | Settlement never misses even if my team is offline |
| AMM | Register LP rebalancing tasks with custom calldata | Liquidity is always in range without manual intervention |
| Any Soroban contract | Cancel a task if the underlying condition resolves itself | I don't pay keepers for work that's no longer needed |

#### Keeper Operators

| As a... | I want to... | So that... |
|---------|-------------|-----------|
| Keeper | Listen to on-chain events and claim profitable tasks | I earn XLM rewards for providing upkeep |
| Keeper | See the reward amount before claiming | I can calculate profitability vs gas |
| Keeper | Re-claim a task if the original claimer vanished | No task is permanently stuck |
| Keeper | Withdraw my accumulated balance in one transaction | I minimise transaction overhead |

#### Protocol/Admin

| As a... | I want to... | So that... |
|---------|-------------|-----------|
| Admin | Pause the registry in emergencies | No new tasks can be registered during an incident |
| Admin | Upgrade the WASM hash | Bug fixes and new features can be deployed without redeployment |
| Admin | Adjust fee basis points | Protocol economics can be tuned by governance |
| Admin | Sweep accumulated fees to treasury | Revenue flows to stakeholders |

---

### Functional Requirements

#### FR-1: Task Registration
- `register_task` MUST escrow the full reward amount from the caller.
- Task ID MUST be monotonically increasing and globally unique.
- `deadline` MUST be strictly in the future at registration time.
- `ttl_ledgers` MUST cover `deadline` plus a safety margin (rejected with
  `TtlTooShort` otherwise) so the storage entry cannot expire before the
  escrow it guards is resolved.
- `calldata` MUST NOT exceed `MAX_CALLDATA_LEN` (1024 bytes), rejected with
  `CalldataTooLarge` otherwise. Empty `calldata` is accepted.
- `reward` MUST be greater than zero.
- MUST emit `TaskRegistered` event with `(task_id, owner, reward, deadline)`.

#### FR-2: Task Claiming
- `claim_task` MUST be callable by any address (permissionless).
- MUST reject if task is not in `Pending` or `Claimed` (with expired lock) state.
- MUST reject if `deadline` has passed.
- MUST record the `claimer` address and `claim_ledger`.
- A second keeper MUST be able to claim after `lock_ledgers` have elapsed.
- MUST emit `TaskClaimed` event.

#### FR-3: Task Execution
- `execute_task` MUST only be callable by the current `claimer`.
- MUST reject if task deadline has passed.
- If the task has a `verifier` attached, MUST call `verifier.verify(task,
  keeper, proof)` and reject with `VerificationFailed` (emitting
  `TaskVerificationFailed`) without crediting, transferring, or mutating
  task status if it returns `false`. A task with no verifier is unaffected.
- MUST credit `(reward * (10000 - fee_bps) / 10000)` to the keeper's balance.
- Protocol fee MUST remain in the contract (swept separately by admin).
- MUST emit `TaskExecuted` with net reward and proof bytes.
- Task status MUST transition to `Executed` (immutable after this point).

#### FR-3a: Verifier Update
- `update_verifier` MUST only be callable by the task owner.
- MUST only be callable when task is in `Pending` state — once claimed, a
  keeper has begun acting on the terms it saw at claim time; see
  `IKeeperVerifier`'s doc comment for the griefing-protection rationale.
  `update_verifier`'s doc comment in
  `contracts/keeper-registry/src/lib.rs` for the bait-and-switch rationale.
- MUST emit `VerifierUpdated`.

#### FR-4: Task Cancellation
- `cancel_task` MUST only be callable by the task owner.
- MUST only be callable when task is in `Pending` state.
- MUST refund the full reward to the owner.
- MUST emit `TaskCancelled`.

#### FR-5: Task Expiry
- `expire_task` MUST be callable by anyone.
- MUST only succeed when `ledger.timestamp >= task.deadline`.
- MUST refund the full reward to the task owner.
- MUST emit `TaskExpired`.

#### FR-6: Reward Withdrawal
- `withdraw_rewards` MUST transfer the keeper's full credited balance.
- MUST zero the balance before transfer (CEI pattern).
- MUST emit `RewardsWithdrawn`.
- MUST revert if balance is zero.

#### FR-7: Admin Controls
- `pause`/`unpause` MUST gate `register_task`, `claim_task`, `execute_task`,
  `increase_reward`, and `update_verifier` — all five open new escrow,
  reward, or task-outcome exposure.
- `pause`/`unpause` MUST NOT gate `cancel_task`, `expire_task`, or
  `withdraw_rewards` — these only let already-escrowed value flow back to
  whoever already owns it, which must always stay available so an admin
  pause can never become a fund freeze. Read-only views are likewise never
  gated.
- `extend_deadline` is currently **not** gated by pause in the deployed code
  (own bug, tracked separately from this requirement) — it changes no funds
  either way, but the intent was likely for it to follow
  register/claim/execute. See the `pause`/`unpause` doc comment in
  `contracts/keeper-registry/src/lib.rs` and the
  `test_pause_policy_matrix_entry_point_by_entry_point` test in
  `contracts/keeper-registry/src/test.rs` for the authoritative, verified
  matrix.
- `set_fee_bps` MUST reject values > 10 000.
- `transfer_admin` MUST require auth from BOTH current admin AND new admin.
- `upgrade` MUST use `deployer().update_current_contract_wasm`.

---

### Non-Functional Requirements

#### Security
- All state-mutating functions require `address.require_auth()`.
- No re-entrancy vectors: token transfers happen after all state mutations (CEI pattern).
- No unchecked arithmetic — Rust's `checked_*` methods or overflow-checks = true.
- Admin cannot drain escrowed task rewards; only sweeps protocol fees.
- Upgrade requires admin auth — no anonymous upgrades.

#### Gas Efficiency
- Instance storage for hot/shared data (admin, counter, flags).
- Persistent storage for per-task data with explicit TTL management.
- No unbounded iteration — no `Vec<task_id>` scanned in O(n); queries are by key.
- Events are the query primitive for off-chain indexers.

## What it does

Task owners register jobs with a token reward and execution conditions. Keepers discover eligible jobs, claim them, perform the off-chain work, and submit the execution transaction. Successful execution credits the keeper with the reward after the protocol fee; cancellation and expiry return escrow to the owner.

The repository contains the Soroban registry contract and an example JavaScript keeper bot.

## Security considerations

The contract's security is defined by the money invariants in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#money-invariants). That section is the canonical specification for solvency, escrow recoverability, single payout, fee bounding, escrow isolation, withdrawal liveness, and monotonic task ids. It also identifies the contract functions that enforce each property, concrete changes that would break them, and known gaps tracked in the issue backlog.

Implementation mechanisms such as authorization, checked arithmetic, status guards, and checks-effects-interactions are means of preserving those properties; they are not a substitute for reviewing the properties themselves. Any change involving token transfers, task terminal states, fee accounting, pausing, storage TTL, or task-id allocation must be checked against the architecture invariants.

Known open issues include the relationship between task TTL and deadline ([#0005](https://github.com/soroban-tooling/soroban-keeper-network/issues/5)) and the historical CEI ordering concerns in cancellation and expiry ([#0002](https://github.com/soroban-tooling/soroban-keeper-network/issues/2), [#0003](https://github.com/soroban-tooling/soroban-keeper-network/issues/3)).
`Task.deadline` is a unix timestamp **in seconds**; `Task.ttl_ledgers` is a
Persistent storage TTL **in ledgers** — the two are different units with no
fixed conversion. `register_task` and `extend_deadline` require
`ttl_ledgers >= (deadline - now) / SECONDS_PER_LEDGER + TTL_SAFETY_MARGIN_LEDGERS`
(5 seconds/ledger, ~1 day margin), rejecting the call with `TtlTooShort`
otherwise. This guarantees a task's storage entry can never be evicted while
its escrowed reward is still held — see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#ttl--deadline-invariant).
`Task.calldata` is capped at `MAX_CALLDATA_LEN` = 1024 bytes, enforced at
`register_task`. `save_task` re-writes the whole `Task` struct (including
`calldata`) on every lifecycle mutation — `claim_task`, `execute_task`, the
permissionless `expire_task`, `increase_reward`, `extend_deadline` — and those
calls are frequently made by a keeper or third party, not the task owner. An
unbounded `calldata` would let an owner push arbitrarily large re-serialisation
and storage cost onto whoever touches the task next. 1024 bytes comfortably
covers a realistic encoded contract call — a target `Address` (~40 bytes XDR),
a function `Symbol` (up to 32 bytes), and several scalar or address arguments —
with headroom for XDR/Vec overhead. Empty `calldata` is accepted, since some
task types (e.g. a `TtlExtension` against a well-known key) need no extra
encoded parameters.

Report suspected vulnerabilities according to [SECURITY.md](SECURITY.md), rather than opening a public issue with exploit details.

## Repository layout

```text
contracts/keeper-registry/  Soroban keeper registry contract
examples/keeper-bot/         Example keeper bot
a fuzz/                        Fuzzing targets and shared support code
docs/                        Architecture, deployment, and demo documentation
```

## Development
| Event | Topics | Data |
|-------|--------|------|
| `TaskRegistered` | `("reg", "task")` | `(task_id, owner, reward, deadline)` |
| `TaskClaimed` | `("claim", "task")` | `(task_id, keeper, ledger_seq)` |
| `TaskExecuted` | `("exec", "task")` | `(task_id, keeper, net_reward, proof)` |
| `TaskExpired` | `("exp", "task")` | `(task_id,)` |
| `TaskCancelled` | `("cancel", "task")` | `(task_id, owner)` |
| `RewardsWithdrawn` | `("withdraw", "reward")` | `(keeper, amount)` |
| `TaskVerificationFailed` | `("verfail", "task")` | `(task_id, keeper)` |
| `VerifierUpdated` | `("verifier", "task")` | `(task_id, verifier)` |
| `Initialized` | `("init", "admin")` | `(admin, reward_token, fee_bps)` — emitted at most once |
| `MinRewardUpdated` | `("minrwd", "admin")` | `(old_min, new_min)` |
| `FeesSweep` | `("sweep", "admin")` | `(treasury, amount, remaining)` |

#### Task Lifecycle State Machine

```
              register_task()
NONE ─────────────────────────────────▶ PENDING
                                           │
               ┌──────────────────────────┘│
               │ claim_task()              │ cancel_task()
               ▼                          ▼
            CLAIMED                    CANCELLED
               │
       ┌───────┴──────────┐
       │ execute_task()   │ expire_task() (deadline passed)
       ▼                  ▼
    EXECUTED           EXPIRED

    (re-claim possible if lock_ledgers elapsed without execute)
```

---

### Integration Guide

#### How Other Soroban Contracts Call This

**Step 1 — Approve the reward amount** (ERC-20 / SEP-41 style):

```rust
// In your dApp contract, approve the registry to transfer reward tokens
token_client.approve(
    &env.current_contract_address(), // from: your contract
    &registry_contract_id,           // spender: the registry
    &reward_amount,
    &(env.ledger().sequence() + 1000), // expiry ledger
);
```

**Step 2 — Register the task**:

```rust
// Cross-contract call to register a task
let registry = KeeperRegistryClient::new(&env, &registry_contract_id);
let task_id = registry.register_task(
    &env.current_contract_address(), // owner
    &TaskType::Liquidation,
    &calldata,                        // encoded liquidation params
    &reward_amount,                   // XLM in stroops
    &(env.ledger().timestamp() + 3600), // deadline: 1 hour from now (seconds)
    &18_000u32,                       // TTL: ledgers, must cover the deadline
                                       // plus a ~1-day safety margin — see
                                       // "Storage Model" above — or this call
                                       // fails with TtlTooShort
    &120u32,                          // lock: ~10 minutes
    &None,                            // verifier: None = trust the proof (see below)
);
```

**Step 3 — Optional on-chain proof verification**:

A task owner may attach a verifier contract — any address implementing
`IKeeperVerifier` — either at registration (above) or afterward via
`update_verifier` while the task is still `Pending`:

```rust
pub trait IKeeperVerifier {
    /// Returns true if `proof` is a valid attestation that `keeper`
    /// performed the off-chain action `task` describes.
    fn verify(env: Env, task: Task, keeper: Address, proof: Bytes) -> bool;
}
```

When a task has a verifier attached, `execute_task` calls it before
crediting the keeper's reward. A `false` result rejects the call with
`VerificationFailed` (and fires a `TaskVerificationFailed` event) without
transferring anything or changing the task's status, so the keeper may
retry with a different proof. A task with no verifier behaves exactly as
before this feature existed — this is a strictly opt-in, additive path.

See `IKeeperVerifier`'s doc comment in `contracts/keeper-registry/src/lib.rs`
for the trust model and the documented failure semantics of a verifier that
panics rather than returning `false`.
for the trust model and cross-contract panic-isolation semantics.

---

### Tokenomics

#### Phase 1 — XLM Rewards

- Task owners deposit XLM (or any SAC-wrapped token) as the reward.
- Keepers earn `reward * (1 - fee_bps/10000)` per task.
- Protocol fee (`fee_bps`) is configurable by admin (default 3%).
- Fees accumulate in the contract; admin sweeps to a treasury address.

#### Phase 2 — Governance Token ($KPRS)

| Attribute | Value |
|-----------|-------|
| Name | Keeper Token |
| Symbol | KPRS |
| Total Supply | 100,000,000 |
| Distribution | 40% Keepers (emissions over 4 years), 20% Team (4-year vest), 20% Ecosystem fund, 10% Early supporters, 10% Treasury |
| Utility | Vote on fee params, propose upgrades, stake for priority queue |
| Emissions | Proportional to tasks executed and stake weight |

---

## Deployment & Usage

### Prerequisites

- Rust stable (1.78 or newer)
- `wasm32-unknown-unknown` Rust target
- Soroban/Stellar CLI 22.x or newer
- Node.js 18 LTS or newer for the example bot

### Common commands

```sh
make build       # Build the workspace
make test        # Run the contract test suite
make fmt-check   # Check formatting
make wasm        # Build the release WASM contract
make ci          # Run the required CI checks locally
```

The contract can also be tested directly with `cargo test --workspace --locked`.
### Local Development

```bash
git clone https://github.com/soroban-tooling/soroban-keeper-network
cd soroban-keeper-network

# Run all tests
cargo test --all --features testutils

# Build WASM
cargo build --release --target wasm32-unknown-unknown --package keeper-registry
```

### Testnet Deployment

```bash
# Fund a testnet account
stellar keys generate --global deployer
stellar keys fund deployer --network testnet

export DEPLOYER_SECRET_KEY=$(stellar keys show --secret deployer)
export ADMIN_ADDRESS=$(stellar keys address deployer)

# Deploy
./scripts/deploy.sh testnet
```

### Running the Keeper Bot

```bash
cd examples/keeper-bot
npm install
cp .env.example .env
# Edit .env with your secret key and contract ID
npm run start:testnet
```

---

## Security Considerations & Audit Plan

### Known Design Decisions

1. **On-chain execution verification is optional** — By default (`verifier: None`) the registry trusts the claimer to submit proof, same as the original MVP. A task owner can attach an `IKeeperVerifier` contract via `register_task`/`update_verifier` to gate crediting on a custom on-chain check instead. See [`docs/VERIFIER_SECURITY.md`](docs/VERIFIER_SECURITY.md) for the security considerations of attaching a third-party verifier.
1. **On-chain execution verification is optional** — By default (`verifier: None`) the registry trusts the claimer to submit proof, same as the original MVP. A task owner can attach an `IKeeperVerifier` contract via `register_task`/`update_verifier` to gate crediting on a custom on-chain check instead.
2. **Fee sweep is manual** — Protocol fees are batched and swept by admin. In Phase 2 this flows automatically to a staking/treasury contract.
3. **No slashing (MVP)** — Unresponsive keepers lose their lock but face no economic penalty. Phase 2 introduces staking + slashing.

### Security Properties

- **No re-entrancy** — State transitions happen before token transfers (CEI pattern throughout); this also holds for the verifier call in `execute_task`, which runs before any crediting and cannot re-enter the registry (see [`docs/VERIFIER_SECURITY.md`](docs/VERIFIER_SECURITY.md)).
- **Auth on all mutations** — Every write function calls `address.require_auth()`.
- **Overflow protection** — `overflow-checks = true` in release profile + `checked_*` arithmetic.
- **Bounded storage** — No dynamic `Vec` in storage; all reads are O(1) by key.
- **Upgrade is admin-gated** — WASM upgrade requires admin auth; new WASM must be pre-uploaded.
- **Third-party verifiers cannot move funds** — a verifier can only approve/reject; see [`docs/VERIFIER_SECURITY.md`](docs/VERIFIER_SECURITY.md) for the full threat-model walkthrough (proof-size griefing, resource-budget cost, panic isolation, fund-safety).

### Audit Plan

| Phase | Scope | Target |
|-------|-------|--------|
| Pre-audit | Internal review + fuzzing | Q3 2026 |
| Formal audit | `keeper-registry` contract | Q4 2026 |
| Ongoing | Automated invariant testing with `cargo-fuzz` | Continuous |

Security issues should be reported per [SECURITY.md](SECURITY.md).

---

## Stellar Community Fund / SDF Grant Readiness

This project is designed to qualify for:

- **Stellar Community Fund (SCF)** — Open source infrastructure grant
- **SDF Build program** — Soroban DeFi tooling
- **Meridian hackathon** — Infrastructure track

**Grant readiness checklist:**
- [x] Open source (Apache-2.0)
- [x] On Soroban / Stellar ecosystem
- [x] Novel infrastructure (no equivalent exists)
- [x] Composable — designed to be used by other protocols
- [x] Fully documented + testable
- [x] Roadmap beyond MVP

---

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. In particular, changes that move funds or alter task lifecycle behavior must be reviewed against the numbered invariants in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#money-invariants), and tests should name the invariant they protect.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for the full text.
