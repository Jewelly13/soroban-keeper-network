# Continuous integration

CI runs on every pull request and on pushes to `main` and `develop`.

## Required jobs

The `format`, `test`, and `build-wasm` jobs are required. They check Rust formatting, run the workspace test suite, and verify that the contract builds to WASM. The aggregate `ci-required` job is the branch-protection gate for these checks.

## Advisory jobs

The following jobs report findings without blocking a pull request:

- **Clippy** checks Rust lint warnings.
- **Dependency audit** reports known dependency advisories.
- **WASM size report** records the optimized contract size.
- **Keeper bot** runs the example bot tests and lint.
- **ShellCheck** runs `shellcheck -x -S style` against `scripts/*.sh` and `.github/backlog/push.sh`.

ShellCheck is advisory because style-level findings can require context-specific decisions. The shell scripts nevertheless use strict error handling, validate required inputs, quote expansions, and validate deployment networks before invoking deployment tooling.

The ShellCheck job runs on every pull request through the workflow's `pull_request` trigger, including pull requests that only change shell scripts.