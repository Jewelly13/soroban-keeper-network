# Continuous integration

CI runs on every pull request and on pushes to `main` and `develop`.

## Required jobs

The `format`, `test`, and `build-wasm` jobs are required. They check Rust formatting, run the workspace test suite, and verify that the contract builds to WASM. The aggregate `ci-required` job is the branch-protection gate for these checks.

## Advisory jobs

The following jobs report findings without blocking a pull request:

- **Clippy** checks Rust lint warnings.
- **Dependency audit** reports known dependency advisories.
- **WASM size report** records the optimized contract size.
- **Fuzz (`fuzz-pr`)** runs every registered `cargo-fuzz` target
  (`fuzz/fuzz_targets/`) for a short, fixed budget (60 seconds each) on any
  PR touching `contracts/keeper-registry/` or `fuzz/`. See
  [FUZZING.md](FUZZING.md) for what the targets themselves cover.
- **Fuzz nightly** (`.github/workflows/fuzz-nightly.yml`) runs the same
  targets on a daily schedule for a much longer budget (15 minutes each),
  restoring and saving a corpus via `actions/cache` so coverage accumulates
  across nights instead of restarting from empty every run. It is a separate
  workflow (not part of `ci.yml`) because it runs on `schedule`, not
  `pull_request`.
  - Both fuzz jobs share `scripts/run-fuzz-targets.sh`, which reports total
    runs, corpus size, and — on a crash — a `::error::` annotation plus the
    decoded failing input (via `cargo fuzz fmt`) in the job summary, so a
    real crash is hard to miss even though neither job blocks a merge. A
    target that fails to *build* (see FUZZING.md's target-status table) is
    reported distinctly from a crash, so a known pre-existing gap doesn't
    read as a new regression on every PR.
- **Resource cost report** runs one representative call through every
  state-changing contract entry point via the test harness's
  `Env::cost_estimate().budget()` (see `resource_report` in
  `contracts/keeper-registry/src/test.rs`) and publishes a CPU
  instructions / memory bytes table to the job summary, diffed against the
  checked-in baseline at `contracts/keeper-registry/resource-baseline.json`
  by `scripts/report-resource-cost.sh`. A per-entry-point change of 10% or
  more is flagged so a reviewer can spot a regression at a glance. Update
  the baseline file in the same PR as an intentional cost change.
- **Keeper bot** runs the example bot tests and lint.

ShellCheck linting of `scripts/*.sh` is tracked separately (backlog 0049)
and is not wired into CI yet.
