//! Corpus seed generation for the `batch_register_tasks` fuzz target.
//!
//! Per issue 0067's convention, seeds are *derived from the real boundary
//! constants* rather than being copy-pasted numbers, so they cannot drift if
//! `MAX_BATCH_ENTRIES` or the lock/TTL bounds change.
//!
//! Lives in the fuzz crate's test target rather than `src/`: `fuzz/src/lib.rs`
//! is `#![no_std]` and writing corpus files needs `std::fs`. Issue 0067
//! sanctions "`fuzz/src/seed.rs` (or equivalent generator)".
//!
//! # Regenerating the corpus
//!
//! ```text
//! cargo test -p keeper-registry-fuzz --test corpus_seeds -- \
//!     --ignored write_batch_register_tasks_corpus
//! ```
//!
//! Then validate the fuzzer accepts every seed without generating new input:
//!
//! ```text
//! cargo +nightly fuzz run batch_register_tasks -- -runs=0
//! ```
//!
//! # Byte layout
//!
//! Mirrors the hand-written `Arbitrary` impl in
//! `fuzz_targets/batch_register_tasks.rs`, which is hand-written precisely so
//! this stays a direct, checkable mapping:
//!
//! ```text
//! byte 0        entry-count seed  (count = seed % (MAX_BATCH_ENTRIES * 2 + 1))
//! byte 1        max_total_reward seed (mod 4 selects the ceiling strategy)
//! bytes 2..     four bytes per entry: reward, deadline, ttl, lock seeds
//! ```

use keeper_registry::MAX_BATCH_ENTRIES;
use keeper_registry_fuzz::batch::{self, EntrySeed, MAX_GENERATED_ENTRIES};
use keeper_registry_fuzz::support::RegistryHarness;

/// The modulus the target applies to byte 0 when choosing an entry count.
const COUNT_MODULUS: u32 = MAX_BATCH_ENTRIES * 2 + 1;

/// Per-entry seed values chosen so each one lands on a documented boundary.
/// The `% n` arms these correspond to are documented in the fuzz target's
/// `biased_*` functions.
mod entry {
    /// reward seed 2 -> 1 (minimum accepted positive reward)
    pub const REWARD_MIN_VALID: u8 = 2;
    /// reward seed 0 -> 0 (rejected: non-positive)
    pub const REWARD_ZERO: u8 = 0;
    /// reward seed 3 -> i128::MAX (accepted alone, overflows when summed)
    pub const REWARD_MAX: u8 = 3;

    /// deadline seed 2 -> now + 3_600
    pub const DEADLINE_ONE_HOUR: u8 = 2;
    /// deadline seed 0 -> now (rejected: already passed)
    pub const DEADLINE_PASSED: u8 = 0;

    /// ttl seed 5 -> 20_000 (covers a 1-hour deadline)
    pub const TTL_VALID: u8 = 5;
    /// ttl seed 1 -> MIN_TTL_LEDGERS - 1 (rejected)
    pub const TTL_BELOW_MIN: u8 = 1;
    /// ttl seed 2 -> MIN_TTL_LEDGERS (passes the floor, fails the deadline
    /// coverage rule for any non-trivial deadline)
    pub const TTL_AT_MIN: u8 = 2;

    /// lock seed 2 -> MIN_LOCK_LEDGERS
    pub const LOCK_AT_MIN: u8 = 2;
    /// lock seed 1 -> MIN_LOCK_LEDGERS - 1 (rejected)
    pub const LOCK_BELOW_MIN: u8 = 1;
    /// lock seed 4 -> MAX_LOCK_LEDGERS + 1 (rejected)
    pub const LOCK_ABOVE_MAX: u8 = 4;
}

/// Ceiling strategies, from byte 1 `% 4`.
mod ceiling {
    /// 0 -> i128::MAX, never binding
    pub const PERMISSIVE: u8 = 0;
    /// 1 -> exactly the batch total (accepted; the `>` boundary)
    pub const EXACT: u8 = 1;
    /// 2 -> one under the batch total (rejected)
    pub const ONE_UNDER: u8 = 2;
}

/// A valid entry: minimum positive reward, 1-hour deadline, TTL that covers it,
/// lock exactly at the floor.
fn valid_entry() -> [u8; 4] {
    [
        entry::REWARD_MIN_VALID,
        entry::DEADLINE_ONE_HOUR,
        entry::TTL_VALID,
        entry::LOCK_AT_MIN,
    ]
}

/// Builds one seed file's bytes.
///
/// `count` is the number of entries to encode; byte 0 is set so the target
/// decodes exactly that many. `mutate` may replace individual entries to
/// introduce a boundary violation at a chosen position.
fn seed(count: u32, ceiling_seed: u8, mutate: impl Fn(u32) -> Option<[u8; 4]>) -> Vec<u8> {
    assert!(
        count < COUNT_MODULUS,
        "count {count} is not encodable in one byte under modulus {COUNT_MODULUS}"
    );
    let count_seed = u8::try_from(count).expect("count must fit in a u8 seed");

    let mut bytes = vec![count_seed, ceiling_seed];
    for i in 0..count {
        bytes.extend_from_slice(&mutate(i).unwrap_or_else(valid_entry));
    }
    bytes
}

/// Every seed, paired with the filename it should be written under.
///
/// Each name says which boundary it pins, so a failing corpus entry is
/// self-describing.
pub fn corpus() -> Vec<(String, Vec<u8>)> {
    let max = MAX_BATCH_ENTRIES;
    let all_valid = |_| None;

    vec![
        // ── Batch-shape boundaries ───────────────────────────────────────────
        (
            "empty_batch".into(),
            seed(0, ceiling::PERMISSIVE, all_valid),
        ),
        (
            "single_entry".into(),
            seed(1, ceiling::PERMISSIVE, all_valid),
        ),
        (
            "at_max_batch_entries".into(),
            seed(max, ceiling::PERMISSIVE, all_valid),
        ),
        (
            "one_over_max_batch_entries".into(),
            seed(max + 1, ceiling::PERMISSIVE, all_valid),
        ),
        (
            "well_over_max_batch_entries".into(),
            seed(max * 2, ceiling::PERMISSIVE, all_valid),
        ),
        // ── Ceiling boundaries ───────────────────────────────────────────────
        (
            "ceiling_exactly_at_total".into(),
            seed(4, ceiling::EXACT, all_valid),
        ),
        (
            "ceiling_one_under_total".into(),
            seed(4, ceiling::ONE_UNDER, all_valid),
        ),
        // ── Per-entry violations, at each position in the batch ──────────────
        //
        // Position matters: a validation loop that transferred as it went would
        // only be caught when the bad entry is not first.
        (
            "invalid_reward_first".into(),
            seed(4, ceiling::PERMISSIVE, |i| {
                (i == 0).then(|| {
                    [
                        entry::REWARD_ZERO,
                        entry::DEADLINE_ONE_HOUR,
                        entry::TTL_VALID,
                        entry::LOCK_AT_MIN,
                    ]
                })
            }),
        ),
        (
            "invalid_reward_last".into(),
            seed(4, ceiling::PERMISSIVE, |i| {
                (i == 3).then(|| {
                    [
                        entry::REWARD_ZERO,
                        entry::DEADLINE_ONE_HOUR,
                        entry::TTL_VALID,
                        entry::LOCK_AT_MIN,
                    ]
                })
            }),
        ),
        (
            "deadline_passed_mid_batch".into(),
            seed(4, ceiling::PERMISSIVE, |i| {
                (i == 2).then(|| {
                    [
                        entry::REWARD_MIN_VALID,
                        entry::DEADLINE_PASSED,
                        entry::TTL_VALID,
                        entry::LOCK_AT_MIN,
                    ]
                })
            }),
        ),
        (
            "lock_below_min_mid_batch".into(),
            seed(4, ceiling::PERMISSIVE, |i| {
                (i == 1).then(|| {
                    [
                        entry::REWARD_MIN_VALID,
                        entry::DEADLINE_ONE_HOUR,
                        entry::TTL_VALID,
                        entry::LOCK_BELOW_MIN,
                    ]
                })
            }),
        ),
        (
            "lock_above_max_mid_batch".into(),
            seed(4, ceiling::PERMISSIVE, |i| {
                (i == 1).then(|| {
                    [
                        entry::REWARD_MIN_VALID,
                        entry::DEADLINE_ONE_HOUR,
                        entry::TTL_VALID,
                        entry::LOCK_ABOVE_MAX,
                    ]
                })
            }),
        ),
        (
            "ttl_below_min_mid_batch".into(),
            seed(4, ceiling::PERMISSIVE, |i| {
                (i == 2).then(|| {
                    [
                        entry::REWARD_MIN_VALID,
                        entry::DEADLINE_ONE_HOUR,
                        entry::TTL_BELOW_MIN,
                        entry::LOCK_AT_MIN,
                    ]
                })
            }),
        ),
        (
            "ttl_at_min_but_short_of_deadline".into(),
            seed(2, ceiling::PERMISSIVE, |i| {
                (i == 0).then(|| {
                    [
                        entry::REWARD_MIN_VALID,
                        entry::DEADLINE_ONE_HOUR,
                        entry::TTL_AT_MIN,
                        entry::LOCK_AT_MIN,
                    ]
                })
            }),
        ),
        // Two i128::MAX rewards overflow the running total before the ceiling
        // comparison is reached.
        (
            "reward_sum_overflows".into(),
            seed(2, ceiling::PERMISSIVE, |_| {
                Some([
                    entry::REWARD_MAX,
                    entry::DEADLINE_ONE_HOUR,
                    entry::TTL_VALID,
                    entry::LOCK_AT_MIN,
                ])
            }),
        ),
    ]
}

use std::path::PathBuf;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus/batch_register_tasks")
}

/// Writes the seed corpus. Ignored by default so an ordinary `cargo test`
/// never rewrites committed files; run explicitly to regenerate.
#[test]
#[ignore = "regenerates committed corpus files; run explicitly"]
fn write_batch_register_tasks_corpus() {
    let dir = corpus_dir();
    std::fs::create_dir_all(&dir).expect("create corpus dir");
    for (name, bytes) in corpus() {
        std::fs::write(dir.join(&name), &bytes).expect("write seed");
    }
    println!("wrote {} seeds to {}", corpus().len(), dir.display());
}

/// Guards issue 0067's acceptance criterion: at least 5 seeds, all derived
/// from real constants, and all present on disk matching what the generator
/// produces. Fails if someone changes a constant without regenerating.
#[test]
fn committed_corpus_matches_generator() {
    let seeds = corpus();
    assert!(
        seeds.len() >= 5,
        "issue 0067 requires at least 5 seed entries, found {}",
        seeds.len()
    );

    let dir = corpus_dir();
    for (name, expected) in seeds {
        let path = dir.join(&name);
        let actual = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "missing corpus seed {}: {e}. Regenerate with: cargo test \
                 -p keeper-registry-fuzz --features arbitrary -- --ignored \
                 write_batch_register_tasks_corpus",
                path.display()
            )
        });
        assert_eq!(
            actual, expected,
            "corpus seed {name} is stale -- a boundary constant changed. \
             Regenerate with: cargo test -p keeper-registry-fuzz --features \
             arbitrary -- --ignored write_batch_register_tasks_corpus"
        );
    }
}

/// Runs every committed seed through the fuzz target's actual body.
///
/// This is what makes the target verifiable on platforms where a libFuzzer
/// runtime cannot link (MSVC stable, for one): `cargo +nightly fuzz run` is the
/// only way to exercise `fuzz_target!` itself, but the logic inside it lives in
/// `keeper_registry_fuzz::batch` and can be driven directly.
///
/// Any assertion inside `run_case` — a trap, an untyped error, a wrong
/// rejection, or a partial transfer — fails here exactly as it would as a
/// libFuzzer crash.
#[test]
fn every_corpus_seed_passes_the_target_assertions() {
    for (name, bytes) in corpus() {
        let mut entries = [EntrySeed::default(); MAX_GENERATED_ENTRIES as usize];
        let (count, ceiling_seed) = batch::decode(&bytes, &mut entries);

        let harness = RegistryHarness::new();
        batch::run_case(
            &harness.env,
            &harness.client(),
            &harness.token_client(),
            &harness.contract_id,
            &harness.user,
            &entries[..count],
            ceiling_seed,
        );

        println!("seed {name}: {count} entries, ceiling seed {ceiling_seed} -- ok");
    }
}

/// A single unaffordable entry: reward i128::MAX passes every parameter rule
/// and does not overflow the sum, so validation accepts it -- but the escrow
/// transfer cannot succeed against any real balance.
#[test]
fn single_unaffordable_entry() {
    let bytes = vec![1u8, 0u8, 3u8, 2u8, 5u8, 2u8];
    let mut entries = [EntrySeed::default(); MAX_GENERATED_ENTRIES as usize];
    let (count, ceiling_seed) = batch::decode(&bytes, &mut entries);
    let harness = RegistryHarness::new();
    batch::run_case(
        &harness.env,
        &harness.client(),
        &harness.token_client(),
        &harness.contract_id,
        &harness.user,
        &entries[..count],
        ceiling_seed,
    );
}

/// Truncation must decode as a shorter batch rather than being discarded --
/// libFuzzer mutates by truncating constantly, so a decoder that errored on
/// short input would throw away most of the corpus.
#[test]
fn truncated_seeds_decode_and_run() {
    let (_, full) = corpus()
        .into_iter()
        .find(|(name, _)| name == "at_max_batch_entries")
        .expect("seed must exist");

    for cut in [0usize, 1, 2, 3, 7, full.len() / 2] {
        let bytes = &full[..cut.min(full.len())];
        let mut entries = [EntrySeed::default(); MAX_GENERATED_ENTRIES as usize];
        let (count, ceiling_seed) = batch::decode(bytes, &mut entries);

        let harness = RegistryHarness::new();
        batch::run_case(
            &harness.env,
            &harness.client(),
            &harness.token_client(),
            &harness.contract_id,
            &harness.user,
            &entries[..count],
            ceiling_seed,
        );
    }
}

/// The count byte must actually decode to the count the seed intends. This
/// is the mapping that would silently break if the target's
/// `MAX_GENERATED_ENTRIES` changed without this file following.
#[test]
fn count_byte_decodes_to_intended_entry_count() {
    for count in [0u32, 1, MAX_BATCH_ENTRIES, MAX_BATCH_ENTRIES + 1] {
        let bytes = seed(count, ceiling::PERMISSIVE, |_| None);
        let decoded = u32::from(bytes[0]) % COUNT_MODULUS;
        assert_eq!(decoded, count, "count byte round-trip failed");
        assert_eq!(
            bytes.len(),
            2 + 4 * count as usize,
            "seed length must match entry count"
        );
    }
}
