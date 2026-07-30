# Changelog

All notable changes to the Soroban Keeper Network are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — bounded batch task reads (#25)

- New read-only views `get_tasks(ids)` and `get_tasks_range(from, count)` let
  an indexer or keeper bot inspect many tasks in one call instead of one RPC
  round trip per task. Both are bounded by `MAX_BATCH_READ` (50) and return
  the new `BatchTooLarge` error when exceeded, rather than silently truncating
  a page — a clipped result is indistinguishable from the end of a range.
- The result is positionally aligned with the request: entry `i` is
  `Some(task)` if the id at position `i` exists and `None` if it does not, so
  one missing id does not fail the whole call. `Vec<Option<Task>>` is used
  rather than a compacted `Vec<Task>` because `Task` carries no `task_id`,
  which would make the mapping from result back to requested id unrecoverable.
- No storage iteration is introduced: every read is still O(1) by key against
  `DataKey::Task(id)`, and the caller supplies the bounded key set.
- `get_tasks_range` rejects a window whose last id would exceed `u64::MAX` with
  `ArithmeticOverflow` rather than wrapping around to low-numbered tasks. A
  window ending exactly on `u64::MAX` is still accepted.
- `Task` now derives `PartialEq`/`Eq`, matching `TaskType` and `TaskStatus`, so
  batched results can be compared. Additive only — no XDR or behaviour change.
- `VERSION` is deliberately unchanged: these are purely additive read-only
  views and no existing function's behaviour is affected.

### Documented — protocol fee rounding guarantee (#26)

- `split_reward`'s rounding direction is now a stated guarantee rather than an
  undocumented artifact of integer division: the fee is
  `floor(reward * fee_bps / 10_000)` and the keeper receives the remainder, so
  the protocol can never collect more than the nominal rate and the error is
  bounded by one stroop per execution, always in the keeper's favour.
- The `min_reward` / `fee_bps` dust threshold is documented in the README
  tokenomics section: the fee is non-zero only once
  `min_reward >= ceil(10_000 / fee_bps)`. Below that the protocol earns
  nothing on a task while still bearing its storage cost — a relationship
  between two parameters that were previously set independently.
- Boundary tests pin the behaviour at `reward = 1`, the first reward yielding a
  non-zero fee, `fee_bps = 0`, and `fee_bps = 10_000`. No behaviour change.

### Added — optional on-chain proof verifier (VERSION bumped to 3)

- `register_task` now takes a required eighth parameter,
  `verifier: Option<Address>`. `None` behaves exactly as before this change;
  `Some(addr)` attaches an `IKeeperVerifier`-implementing contract that
  `execute_task` calls before crediting the keeper, rejecting with the new
  `VerificationFailed` error (and a `TaskVerificationFailed` event) if it
  returns `false`. This is a breaking ABI change — every existing
  `register_task` call site must add the new argument.
- New `update_verifier` entry point lets the task owner change or clear a
  task's verifier while it is still `Pending`.
- New events: `TaskVerificationFailed` (`("verfail", "task")`) and
  `VerifierUpdated` (`("verifier", "task")`).
- `VERSION` bumped from 2 to 3.

### Fixed — task parameter validation

- `register_task` now rejects `lock_ledgers` outside `[MIN_LOCK_LEDGERS,
  MAX_LOCK_LEDGERS]` and `ttl_ledgers` below `MIN_TTL_LEDGERS`, returning the
  new `InvalidTaskParams` error. Previously a `lock_ledgers` of `0` let any
  keeper instantly re-claim a task from another keeper, an oversized
  `lock_ledgers` let one unresponsive keeper hold a task hostage until the
  deadline, and a `ttl_ledgers` of `0` risked stranding escrowed funds.

### Added — calldata size bound (VERSION bumped to 2)

- `register_task` now rejects `calldata` larger than `MAX_CALLDATA_LEN`
  (1024 bytes) with a new `CalldataTooLarge` error. Previously `calldata` was
  unbounded, so a task owner could register a payload that every later
  lifecycle call (`claim_task`, `execute_task`, the permissionless
  `expire_task`) would have to re-read and re-write in full, pushing the
  storage and re-serialisation cost onto keepers and passers-by rather than
  the owner who chose the payload size.
- Empty `calldata` is intentionally still accepted; documented in the README.
- Adding `CalldataTooLarge` changes the contract's error ABI — `VERSION`
  bumped from 1 to 2.

### Added — live testnet deployment

- Deployed `KeeperRegistry` to Stellar testnet
  (`CDJOYHBS7C2PVJS47BTRDLGBNG2YOE43VX6Y3EWIZPPPKOPRNYQQ54U4`) and ran a full
  register → claim → execute → withdraw cycle on-chain.
- Added [docs/DEMO.md](docs/DEMO.md) (transaction-by-transaction trace) and
  [DEPLOYMENTS.md](DEPLOYMENTS.md) (canonical address record); surfaced the live
  deployment in the README.

### Added — contract capabilities & views

- `increase_reward` — owners can top up a task bounty (Pending/Claimed).
- `extend_deadline` — owners can push out a task's deadline.
- `set_min_reward` + `min_reward` view — admin-set anti-dust floor for new tasks.
- `is_claimable` view — cheap keeper-side eligibility check.
- `version` view + `VERSION` constant for ABI detection.
- Governance events on pause/unpause, fee change, and admin transfer, plus
  `topup`/`extend` task events.

### Added — tests

- `split_reward` accounting-invariant sweep (conservation, bounds, formula).
- Multi-keeper end-to-end conservation test across execute/expire/cancel.
- Test count grown from 38 to 52.

### Added — contributor infrastructure

- CONTRIBUTING-facing repo setup: `.editorconfig`, `rustfmt.toml`, `.gitignore`,
  Code of Conduct, issue templates (bug / feature / good-first-issue) + chooser,
  PR template, `CODEOWNERS`, a Wave-Program label taxonomy, and a `Makefile`.
- `docs/ARCHITECTURE.md` and `docs/DEPLOYING.md`; README documentation index.
- `scripts/optimize.sh` build/optimize helper.

### Changed

- CI: concurrency control (cancels superseded runs) and `--locked` builds.
- Repository references updated to the `soroban-tooling` org.

### Fixed

- Cleared all compiler and `clippy -D warnings` findings and applied `rustfmt`
  so the CI lint/format gates pass. Removed the ignored child-manifest
  `[profile.release]`.

### Added — MVP contract feature-complete

The `KeeperRegistry` contract's core lifecycle is now fully implemented and
tested (38 unit tests, full happy-path and error-path coverage):

- **`claim_task`** — permissionless first-come-first-served claiming, with
  re-claim allowed only after the prior claimer's lock window elapses.
- **`execute_task`** — execution-proof submission, reward split between keeper
  and protocol fee, and CEI-safe keeper crediting.
- **`cancel_task`** — owner reclaims escrow of a still-Pending task.
- **`expire_task`** — permissionless deadline enforcement; anyone can refund a
  stuck task's escrow to its owner after the deadline.
- **`withdraw_rewards`** — keeper pulls its accrued balance (balance zeroed
  before transfer to prevent re-entrant double-spend).
- **`sweep_fees`** + `FeesAccrued` accumulator — admin moves accrued protocol
  fees to a treasury; can never touch task escrow or keeper balances.
- **Admin controls** — `pause`/`unpause` (funds-recovery paths stay open during
  a pause), `set_fee_bps` (bounded, future-effective), `transfer_admin` (dual
  auth to prevent lock-out), and `upgrade`.
- **Views** — `fees_accrued`, alongside the existing task/keeper/state views.

### Added — keeper-bot

- Retry with exponential back-off + jitter on transient RPC errors, skipping
  retries on permanent contract errors.
- Graceful shutdown (SIGINT/SIGTERM) that drains the in-flight round so a task
  is never left claimed-but-unexecuted.
- Optional permissionless expiry of past-deadline tasks to refund owners.

### Fixed

- Pinned `ed25519-dalek` to 2.2.0 and committed `Cargo.lock` so the test build
  is reproducible (`soroban-env-host` was resolving an incompatible 3.0.0).
