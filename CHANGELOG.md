# Changelog

All notable changes to the Soroban Keeper Network are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed — restore work silently reverted by an unrelated merge

- `split_reward`'s return type (`Result<(i128, i128), KeeperError>`) and its
  three call sites, a missing closing brace in
  `contracts/keeper-registry/src/test.rs`, and `docs/CI.md` had all been
  silently reverted/deleted by an unrelated commit (`fee3b2d`, ostensibly a
  keeper-bot lint fix), leaving `keeper-registry` unable to compile at all.
  Restored to the state an earlier fix (`038f6c7`) had already established.

### Added — batch claim/execute feasibility study

- [docs/BATCH_OPERATIONS.md](docs/BATCH_OPERATIONS.md): naive all-or-nothing
  batch claiming is strictly worse than independent claims under Soroban's
  transaction atomicity; recommends `claim_first_available` instead
  (backlog issue 0101, already scoped) and defers batch execute pending
  epic E04 (backlog issue 0201, filed alongside this study).

### Added — advisory CI: fuzz jobs, resource cost report

- `fuzz-pr` (`ci.yml`): runs every registered `cargo-fuzz` target for 60s on
  PRs touching `contracts/keeper-registry/` or `fuzz/`.
- `fuzz-nightly` (`.github/workflows/fuzz-nightly.yml`): the same targets
  for 15 minutes each, on a daily schedule, with a persistent cached corpus.
- `resource-cost` (`ci.yml`): reports CPU instructions and memory bytes per
  state-changing entry point via `soroban-sdk`'s budget testutils, diffed
  against a checked-in baseline.
- Both documented in [docs/CI.md](docs/CI.md) (restored — see Fixed above).

### Added — partial verifier resource cost catalog

- [docs/VERIFIERS.md](docs/VERIFIERS.md): baseline (no-verifier) measurement
  methodology in place; per-verifier deltas blocked pending epic E04's
  reference verifiers, which do not exist in this repo yet.

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
