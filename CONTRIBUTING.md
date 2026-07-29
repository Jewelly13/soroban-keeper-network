### Coverage

CI runs `cargo-llvm-cov` as an **advisory** job (`Coverage (advisory)` in
`.github/workflows/ci.yml`) — it reports a number in the job summary but
never blocks a PR. There is no coverage threshold and none is planned; use
the report to spot untested branches, not to chase a percentage.

**Please read this entire document before opening a PR or issue.**

---

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Development Environment Setup](#development-environment-setup)
- [Project Structure](#project-structure)
- [Git Workflow](#git-workflow)
- [Branching & PR Rules](#branching--pr-rules)
- [Commit Convention](#commit-convention)
- [Code Style](#code-style)
- [Testing Requirements](#testing-requirements)
- [PR Template & Review Process](#pr-template--review-process)
- [Coordination — Issues & Discussions](#coordination--issues--discussions)
- [Release Process](#release-process)
- [Security Reporting](#security-reporting)

---

## Code of Conduct

This project follows the [Contributor Covenant v2.1](CODE_OF_CONDUCT.md). By participating you agree to uphold a respectful, harassment-free environment. Report violations to **conduct@soroban-keeper.network** (email TBD once domain is registered).

---

## Development Environment Setup

### Required Tools

| Tool | Version | Install |
|------|---------|---------|
| Rust | stable (≥ 1.78) | `rustup install stable` |
| wasm32 target | — | `rustup target add wasm32-unknown-unknown` |
| Soroban CLI | ≥ 22.x | `cargo install --locked stellar-cli --features opt` |
| Node.js | ≥ 18 LTS | Install from [nodejs.org](https://nodejs.org/) |
| npm | bundled with Node.js | — |

### Clone and configure the repository

```bash
git clone https://github.com/soroban-tooling/soroban-keeper-network.git
cd soroban-keeper-network
rustup target add wasm32-unknown-unknown
```

Install the keeper-bot dependencies when working on the example bot:

```bash
cd examples/keeper-bot
npm ci
cd ../..
```

### Editor configuration

The repository does not require a particular editor. For a convenient Rust setup, use an editor with rust-analyzer support and enable format-on-save if desired. Keep editor-specific settings local rather than committing generated workspace configuration.

---

## Project Structure

```text
contracts/keeper-registry/  Soroban keeper registry contract
examples/keeper-bot/        Example off-chain keeper bot
fuzz/                       Fuzzing harness and targets
docs/                       Architecture, deployment, and demo documentation
.github/                    CI, issue templates, and pull-request template
```

Rust contract changes normally belong in `contracts/keeper-registry/src/`. Contract tests are in `contracts/keeper-registry/src/test.rs`.

---

## Git Workflow

We use a trunk-based workflow with short-lived topic branches. Keep branches focused and delete them after merging.

1. Start from the current default branch.
2. Create a descriptive topic branch.
3. Make small, focused commits.
4. Run the relevant local checks.
5. Push the branch and open a pull request.
6. Respond to review feedback with follow-up commits or a clean fixup before merge.

Use branch names such as:

```text
feature/task-batching
fix/reward-accounting
docs/update-deployment-guide
chore/update-dependencies
refactor/storage-helper
```

Avoid committing generated build artifacts, secrets, local configuration, or unrelated formatting changes.

---

## Branching & PR Rules

- Keep each pull request limited to one concern.
- Explain the problem, the approach, and any trade-offs in the pull request description.
- Include tests for behavior changes and regression fixes.
- Update documentation or the changelog when the user-visible behavior or project workflow changes.
- Do not rewrite shared branches or force-push over another contributor's work.
- Resolve merge conflicts carefully and rerun the relevant checks afterward.
- Never include private keys, tokens, passwords, `.env` files, or other sensitive data in code or commits.

Pull requests should be opened against the repository's current default integration branch. If the target branch changes, follow the branch policy stated by the maintainers and in the pull request template.

---

## Commit Convention

Use an imperative subject line and keep it concise. Conventional Commit prefixes are encouraged:

```text
feat: add task query helper
fix: reject expired task claims
docs: clarify deployment prerequisites
test: cover reward accumulation
chore: update CI action
refactor: simplify task loading
```

Keep unrelated changes in separate commits. A commit body is useful when the reason for a non-obvious change cannot be understood from the code. Do not commit temporary debugging output or commented-out experiments.

---

## Code Style

### Rust

Run rustfmt before submitting Rust changes:

```bash
cargo fmt --all
```

Prefer clear, explicit code and typed errors over panics. Do not use `unwrap()` in production contract code. An `expect()` is acceptable only for a genuinely unreachable invariant, and the surrounding code or comment must explain why it is unreachable.

Use checked arithmetic for values influenced by callers or contract state. Validate inputs at the public boundary and return the appropriate contract error rather than allowing an avoidable panic.

### JavaScript

Keep the keeper bot dependency-free beyond its declared package dependencies. Follow the existing module style, validate external input, and handle expected transaction failures explicitly. Run the bot test suite before submitting bot changes.

### Documentation

Use concise headings, fenced code blocks for commands, and links to canonical files instead of copying their contents into another document. When a repository file is the source of truth, link to it rather than maintaining a second example that can drift.

---

## Testing Requirements

For Rust changes, run the checks that apply to the change:

```bash
make ci
```

This runs the required formatting, test, and WASM build checks. `make check` also runs the stricter local clippy check.

For keeper-bot changes:

```bash
cd examples/keeper-bot
npm test
```

New behavior should include a focused test. Bug fixes should include a regression test where practical. Tests should assert the observable behavior and relevant error, event, balance, or storage state rather than merely checking that a call does not panic.

Before opening a pull request, check the final diff for accidental files, debug output, secrets, and unrelated changes.

---

## PR Template & Review Process

Opening a pull request populates [`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md) automatically. Fill in every section. The checklist identifies the required CI checks and the additional local standards expected for a high-quality contribution.

### Review Process

1. A maintainer checks that the pull request is scoped, described clearly, and associated with the relevant issue.
2. CI runs automatically and reports required checks and advisory checks.
3. Reviewers examine correctness, security, test coverage, compatibility, and documentation.
4. The author responds to feedback and keeps the branch up to date when requested.
5. A maintainer merges the pull request once required checks pass and review concerns are resolved.

CI's required checks are formatting, tests, and the WASM build. Clippy is advisory in CI, but contributors should still run the stricter local check and explain relevant warnings rather than ignoring them.

### Review Turnaround

Maintainers aim to acknowledge new pull requests within a few working days. Review time can be longer for contract, security, deployment, or architectural changes. A respectful comment on the pull request is appropriate if an expected review window has passed.

Keeping changes small, explaining design decisions, and providing tests makes review faster.

---

## Coordination — Issues & Discussions

Search existing issues before opening a new one. For substantial changes, open or comment on an issue before implementation so the intended behavior and design can be discussed first.

A useful issue includes:

- The current behavior and the expected behavior.
- Reproduction steps or a minimal example.
- The impact and why the change matters.
- Relevant files, logs, or transaction details.
- Any compatibility, migration, or security considerations.

Use issues for actionable bugs and proposals. Use pull-request discussions for implementation details once a change is underway.

---

## Release Process

Releases are prepared by maintainers. Before a release, verify the required CI checks, update user-facing documentation and `CHANGELOG.md` where appropriate, confirm deployment records, and review the generated WASM artifact.

Do not publish deployments or change canonical contract addresses without maintainer approval. Record released addresses and network details in `DEPLOYMENTS.md`.

---

## Security Reporting

Do not disclose suspected vulnerabilities in a public issue or pull request. Follow the instructions in [SECURITY.md](SECURITY.md) for private reporting.

Include enough detail to reproduce the issue safely, including affected versions, relevant transaction or input data, impact, and any suggested mitigation. Do not include private keys or other sensitive credentials in a report.
To run it locally:

```bash
# One-time install
cargo install cargo-llvm-cov --locked
rustup component add llvm-tools-preview

# Browsable HTML report (writes to target/llvm-cov/html/index.html)
cargo llvm-cov --workspace --html

# Quick text summary
cargo llvm-cov --workspace --summary-only

# Or, equivalent to the HTML command above:
make coverage
```

`src/test.rs` is excluded from the report (`--ignore-filename-regex
'test\.rs$'`) — it's the test module itself, so counting it inflates the
number without saying anything about how well the contract code in `lib.rs`
is actually exercised.

### Fuzzing & crash-to-regression convention

A crash found by the fuzz harness (`fuzz/fuzz_targets/`) and merely "fixed"
is a bug that can silently come back — a future refactor can reintroduce
the same shape of mistake, and the fuzzer might not rediscover it for a
long time since it searches randomly rather than systematically. **Every
crash the fuzzer finds must become a permanent, checked-in regression**,
not just a patched line of contract code:

1. Minimize the crashing input (`cargo fuzz tmin <target> <path-to-crash>`)
   and commit it under `fuzz/corpus/<target>/regressions/`, so the fuzzer's
   own corpus keeps re-testing it on every future run.
2. Add a corresponding `#[test]` in `contracts/keeper-registry/src/test.rs`
   that reproduces the exact scenario **in human-readable form** — the
   actual sequence of contract calls that triggered the crash, not "replay
   these fuzzer bytes." A raw fuzzer input replay is not reviewable by a
   human and doesn't explain *why* the input was dangerous.
3. If the crash revealed a gap in one of the money invariants (`I-1`
   through `I-7` in `docs/ARCHITECTURE.md`), consider whether it should
   also become a case in the corresponding property test rather than only
   a one-off regression.

Any PR that fixes a bug found by fuzzing must include both the minimized
corpus entry and the human-readable regression test in the same commit as
the fix — see the PR template's checkbox for this.

See [`docs/FUZZING.md`](docs/FUZZING.md) for how to run an existing fuzz
target, add a new one, and use the shared `invariants` module.
