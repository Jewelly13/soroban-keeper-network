# Contributing to Soroban Keeper Network

Thank you for your interest in contributing. This guide covers the workflow and review expectations for changes to the contract, documentation, and keeper bot.

## Before you start

Please read:

- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- [SECURITY.md](SECURITY.md), if the change concerns a vulnerability

For changes involving token transfers, task status transitions, rewards, fees, pausing, storage TTL, or task ids, the **Money invariants** section of the architecture document is the canonical review checklist. Cite the relevant invariant identifiers (`I-1` through `I-7`) in tests and pull-request descriptions.

Do not disclose an exploitable vulnerability in a public issue. Follow the process in [SECURITY.md](SECURITY.md).

## Development environment

### Required tools

| Tool | Version | Install |
|------|---------|---------|
| Rust | stable (>= 1.78) | `rustup install stable` |
| wasm32 target | — | `rustup target add wasm32-unknown-unknown` |
| Soroban CLI | >= 22.x | `cargo install --locked stellar-cli --features opt` |
| Node.js | >= 18 LTS | Use your platform's package manager |

### Setup

```sh
git clone https://github.com/soroban-tooling/soroban-keeper-network.git
cd soroban-keeper-network
rustup target add wasm32-unknown-unknown
cargo test --workspace --locked
```

## Project structure

- `contracts/keeper-registry/` — Soroban registry contract and Rust tests.
- `examples/keeper-bot/` — example JavaScript keeper bot.
- `fuzz/` — fuzz targets and shared fuzzing support.
- `docs/` — architecture, deployment, and demonstration documentation.
- `.github/backlog/issues/` — planned work and issue-level specifications.

## Git workflow

1. Fork the repository and create a focused branch from the current development branch.
2. Keep each pull request limited to one issue or closely related change.
3. Write clear commits using the project commit convention.
4. Rebase or merge the current base branch before requesting review when practical.
5. Open a pull request using the repository template.

Do not commit build artifacts, generated WASM, credentials, private keys, or local configuration containing secrets.

## Branching and pull requests

Pull requests should explain:

- what changed and why;
- which issue is addressed;
- whether the public contract interface or deployment behavior changed;
- which architecture invariants are affected, if any;
- tests and validation commands run; and
- known limitations or follow-up work.

A change that moves funds or changes a terminal task transition must include tests for the affected invariant. Reviewers should specifically check conservation, payout uniqueness, authorization, pause behavior, and re-entrant token interactions where applicable.

Documentation-only changes should still preserve links and use the repository's existing terminology.

## Commit convention

Use a short conventional-commit prefix where possible:

- `feat:` for a new capability;
- `fix:` for a bug fix;
- `test:` for tests;
- `docs:` for documentation;
- `refactor:` for behavior-preserving code changes; and
- `chore:` for maintenance.

Keep the subject concise and use the body to explain non-obvious design decisions.

## Code style

Run `cargo fmt --all` for Rust changes. Keep contract entry points small and explicit. Preserve checks-effects-interactions ordering: validate first, write the state transition second, and perform external token calls last. Do not weaken authorization or pause behavior without documenting the resulting policy and updating the relevant architecture invariant.

For JavaScript changes, follow the existing ESLint configuration and use the repository's existing module and error-handling patterns.

## Testing requirements

Before submitting a pull request, run:

```sh
make fmt-check
make test
make wasm
```

The combined command is:

```sh
make ci
```

Add regression tests for bug fixes. For contract changes involving money movement, assert both state and token balances. Where relevant, test:

- repeated terminal calls;
- re-entrant token callbacks;
- fee rounding and maximum fee boundaries;
- paused and unpaused withdrawal;
- admin attempts to exceed accrued fees; and
- task storage lifetime relative to the deadline.

Property-based and fuzz tests should reference the invariant they encode, such as `I-1` for solvency or `I-3` for single payout.

## Review checklist for fund movement

Before requesting review for a fund-moving change, answer these questions in the pull request:

- Does every token movement have a matching accounting-state change?
- Can the same task reward be transferred or credited twice?
- Does every open escrow retain a reachable cancellation, expiry, or execution path?
- Can an admin function touch task escrow or keeper credits?
- Is the fee floored and bounded by `fee_bps`?
- Can a keeper withdraw the full credited amount while paused?
- Are task ids still unique, increasing, and never reused?
- Can storage eviction occur before an escrow can be resolved?

The complete statements, rationale, enforcement points, and break scenarios are in [docs/ARCHITECTURE.md — Money invariants](docs/ARCHITECTURE.md#money-invariants).

## PR template and review process

A pull request should have passing required CI checks, focused commits, and a description that allows a reviewer to reproduce the result. Maintainers may request additional tests, documentation, or a design discussion when a change affects public behavior or a security property.

Do not merge around a failing required check. If a check is flaky or an upstream tool is unavailable, explain it in the pull request and coordinate with a maintainer.

## Coordination — issues and discussions

Search existing issues before opening a new one. Use an issue for a focused bug, feature, test gap, or documentation task. Include reproduction steps and expected behavior for bugs. Security reports belong in the private reporting channel described in [SECURITY.md](SECURITY.md).

## Release process

Releases and deployments are maintainer-managed. Do not update canonical deployment addresses or publish a contract artifact without coordinating with maintainers. Public interface changes must be documented in the changelog and deployment documentation.

## Security reporting

Please do not report security vulnerabilities in a public issue or pull request. Follow [SECURITY.md](SECURITY.md) for the private reporting process.

## Code of Conduct

This project follows the [Contributor Covenant v2.1](CODE_OF_CONDUCT.md). By participating, you agree to uphold a respectful, harassment-free community.
