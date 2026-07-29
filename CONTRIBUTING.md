### Coverage

CI runs `cargo-llvm-cov` as an **advisory** job (`Coverage (advisory)` in
`.github/workflows/ci.yml`) — it reports a number in the job summary but
never blocks a PR. There is no coverage threshold and none is planned; use
the report to spot untested branches, not to chase a percentage.

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