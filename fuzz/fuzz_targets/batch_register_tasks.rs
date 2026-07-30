//! Fuzz target for `batch_register_tasks` entry count and per-entry parameter
//! mix (issue 0110).
//!
//! Explores two dimensions at once:
//!
//! 1. **Batch shape** — the number of entries, from the degenerate empty batch
//!    and single-entry cases up to well past `MAX_BATCH_ENTRIES`, so both the
//!    typed-error cap and the resource ceiling behind it are exercised.
//! 2. **Per-entry parameter mix** — reward, deadline, TTL, and lock values are
//!    generated independently per entry and biased toward the documented
//!    boundaries, so a batch can mix valid and invalid entries in any position
//!    rather than being uniformly good or uniformly bad.
//!
//! What it asserts (all in [`keeper_registry_fuzz::batch::run_case`]):
//!
//! - The contract never traps, whatever the batch shape.
//! - Every rejection is a typed `KeeperError`, never a host error.
//! - The rejection is the *right* one: the outcome is predicted independently
//!   from the generated parameters and compared.
//! - All-or-nothing holds under fuzzing, not just in the hand-written tests:
//!   after any rejection the owner's balance, the registry's balance, and the
//!   task counter are all unchanged; after any success exactly `n` tasks exist
//!   and exactly the batch total was escrowed.
//!
//! The body lives in the library so an ordinary `cargo test` over the seed
//! corpus can exercise it on platforms where a libFuzzer runtime cannot link.
//! See `fuzz/tests/corpus_seeds.rs`.
//!
//! ## Input encoding
//!
//! `Arbitrary` is implemented by hand rather than derived, so the byte layout
//! is fixed and documented and corpus seeds can be written directly against it:
//!
//! ```text
//! byte 0        entry-count seed
//! byte 1        max_total_reward seed
//! bytes 2..     four bytes per entry: reward, deadline, ttl, lock seeds
//! ```
//!
//! Missing trailing bytes decode as zero rather than erroring, so a truncated
//! input is a valid short batch instead of a discarded testcase.

#![no_main]

use arbitrary::{Arbitrary, Result as ArbitraryResult, Unstructured};
use keeper_registry_fuzz::batch::{self, EntrySeed, MAX_GENERATED_ENTRIES};
use keeper_registry_fuzz::support::RegistryHarness;
use libfuzzer_sys::fuzz_target;

#[derive(Debug)]
struct BatchInput {
    entries: [EntrySeed; MAX_GENERATED_ENTRIES as usize],
    count: usize,
    ceiling_seed: u8,
}

fn decode_input(bytes: &[u8]) -> BatchInput {
    // Decoding goes through the same `batch::decode` the corpus generator and
    // the corpus test use, so the byte layout has exactly one definition.
    let mut entries = [EntrySeed::default(); MAX_GENERATED_ENTRIES as usize];
    let (count, ceiling_seed) = batch::decode(bytes, &mut entries);
    BatchInput {
        entries,
        count,
        ceiling_seed,
    }
}

impl<'a> Arbitrary<'a> for BatchInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbitraryResult<Self> {
        // Consume everything available. `decode` treats missing trailing bytes
        // as zero, so this never fails on short input.
        let len = u.len();
        Ok(decode_input(u.bytes(len)?))
    }

    fn arbitrary_take_rest(u: Unstructured<'a>) -> ArbitraryResult<Self> {
        Ok(decode_input(u.take_rest()))
    }
}

fuzz_target!(|input: BatchInput| {
    let harness = RegistryHarness::new();
    batch::run_case(
        &harness.env,
        &harness.client(),
        &harness.token_client(),
        &harness.contract_id,
        &harness.user,
        &input.entries[..input.count],
        input.ceiling_seed,
    );
});
