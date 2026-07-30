//! Shared logic for the `batch_register_tasks` fuzz target (issue 0110).
//!
//! Lives in the library rather than the fuzz target so that the boundary-value
//! generators, the independent outcome prediction, and the assertions can be
//! exercised by an ordinary `cargo test` over the seed corpus — on platforms
//! where a libFuzzer runtime cannot link, that is the only way to know the
//! target is actually correct rather than merely compiling.
//!
//! The fuzz target is then a thin wrapper: decode bytes, call [`run_case`].

use keeper_registry::{
    KeeperError, KeeperRegistryClient, TaskParams, TaskType, MAX_BATCH_ENTRIES, MAX_LOCK_LEDGERS,
    MIN_LOCK_LEDGERS, MIN_TTL_LEDGERS,
};
use soroban_sdk::{token, Address, Bytes, Env, Vec as SorobanVec};

/// Largest batch the target will build. Comfortably past [`MAX_BATCH_ENTRIES`]
/// so the `BatchTooLarge` boundary is crossed often, while keeping each
/// testcase cheap enough to run at fuzzing speed.
pub const MAX_GENERATED_ENTRIES: u32 = MAX_BATCH_ENTRIES * 2;

/// Per-entry seeds. One byte each keeps the corpus format trivial to write by
/// hand; see `fuzz/tests/corpus_seeds.rs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct EntrySeed {
    pub reward: u8,
    pub deadline: u8,
    pub ttl: u8,
    pub lock: u8,
}

// ─────────────────────────────────────────────────────────────────────────────
// Boundary-biased parameter generation
// ─────────────────────────────────────────────────────────────────────────────
//
// Same approach as `register_task_bounds.rs`: concentrate generated values on
// just-inside, exactly-on, and just-outside the documented limits, since that
// is where validation bugs live. A uniformly random u32 essentially never lands
// on `MIN_TTL_LEDGERS - 1`.

pub fn biased_reward(seed: u8) -> i128 {
    match seed % 6 {
        0 => 0,                    // rejected: non-positive
        1 => -1,                   // rejected: negative
        2 => 1,                    // accepted: smallest positive
        3 => i128::MAX,            // accepted alone; overflows when summed
        4 => 1_000,                // ordinary
        _ => i128::from(seed) + 1, // varied, always positive
    }
}

pub fn biased_lock(seed: u8) -> u32 {
    match seed % 7 {
        0 => 0,
        1 => MIN_LOCK_LEDGERS - 1,
        2 => MIN_LOCK_LEDGERS,
        3 => MAX_LOCK_LEDGERS,
        4 => MAX_LOCK_LEDGERS + 1,
        5 => u32::MAX,
        _ => MIN_LOCK_LEDGERS + u32::from(seed),
    }
}

pub fn biased_ttl(seed: u8) -> u32 {
    match seed % 7 {
        0 => 0,
        1 => MIN_TTL_LEDGERS - 1,
        2 => MIN_TTL_LEDGERS,
        3 => MIN_TTL_LEDGERS + 1,
        4 => u32::MAX,
        5 => 20_000, // covers a 1-hour deadline; the common valid case
        _ => MIN_TTL_LEDGERS + u32::from(seed) * 100,
    }
}

/// Seconds past `now`. Zero means "already passed", which must be rejected.
pub fn biased_deadline_offset(seed: u8) -> u64 {
    match seed % 5 {
        0 => 0, // rejected: deadline == now
        1 => 1,
        2 => 3_600,
        3 => 86_400,
        _ => u64::from(seed) * 60,
    }
}

/// Ceiling strategies, from the ceiling seed `% 4`. Selecting deliberately
/// rather than randomly is what makes the `>` boundary get hit at all.
pub fn ceiling_for(seed: u8, total: i128, overflowed: bool) -> i128 {
    match seed % 4 {
        0 => i128::MAX,
        1 if !overflowed => total,
        2 if !overflowed => total.saturating_sub(1),
        _ => i128::from(seed) * 1_000,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Independent outcome prediction
// ─────────────────────────────────────────────────────────────────────────────

/// Recomputes, outside the contract, what the batch call must return.
///
/// Deliberately mirrors the *documented* rules rather than calling the
/// contract's own helpers: if this and the contract disagree, one of them is
/// wrong, which is the entire point. Order matters and follows
/// `batch_register_tasks`: size cap, then per-entry validation in input order,
/// then the reward ceiling.
pub fn predict(now: u64, entries: &[EntrySeed], ceiling: i128) -> Option<KeeperError> {
    if entries.len() as u32 > MAX_BATCH_ENTRIES {
        return Some(KeeperError::BatchTooLarge);
    }

    let mut total: i128 = 0;
    for seed in entries {
        let reward = biased_reward(seed.reward);
        let deadline = now + biased_deadline_offset(seed.deadline);
        let ttl = biased_ttl(seed.ttl);
        let lock = biased_lock(seed.lock);

        if reward <= 0 {
            return Some(KeeperError::InvalidReward);
        }
        if deadline <= now {
            return Some(KeeperError::DeadlinePassed);
        }
        // Calldata is fixed and short in this target, so CalldataTooLarge is
        // unreachable here; `register_task_bounds` covers that dimension.
        if !(MIN_LOCK_LEDGERS..=MAX_LOCK_LEDGERS).contains(&lock) {
            return Some(KeeperError::InvalidTaskParams);
        }
        if ttl < MIN_TTL_LEDGERS {
            return Some(KeeperError::InvalidTaskParams);
        }
        // Mirrors `required_ttl_ledgers`: SECONDS_PER_LEDGER = 5 and a
        // TTL_SAFETY_MARGIN_LEDGERS of 17_280.
        let required = deadline.saturating_sub(now) / 5 + 17_280;
        if u64::from(ttl) < required {
            return Some(KeeperError::TtlTooShort);
        }

        match total.checked_add(reward) {
            Some(sum) => total = sum,
            None => return Some(KeeperError::ArithmeticOverflow),
        }
    }

    if total > ceiling {
        return Some(KeeperError::BatchRewardCeilingExceeded);
    }
    None
}

/// Builds the `TaskParams` vector for `entries`, returning it alongside the
/// batch total and whether that total overflowed `i128`.
pub fn build_params(
    env: &Env,
    entries: &[EntrySeed],
    now: u64,
) -> (SorobanVec<TaskParams>, i128, bool) {
    let mut params = SorobanVec::new(env);
    let mut total: i128 = 0;
    let mut overflowed = false;

    for seed in entries {
        let reward = biased_reward(seed.reward);
        params.push_back(TaskParams {
            task_type: TaskType::Liquidation,
            calldata: Bytes::from_slice(env, b"fuzz-batch"),
            reward,
            deadline: now + biased_deadline_offset(seed.deadline),
            ttl_ledgers: biased_ttl(seed.ttl),
            lock_ledgers: biased_lock(seed.lock),
            verifier: None,
        });
        match total.checked_add(reward) {
            Some(sum) => total = sum,
            None => overflowed = true,
        }
    }

    (params, total, overflowed)
}

/// Drives one generated case against a live registry and asserts every property
/// issue 0110 asks for.
///
/// Panics — which is what libFuzzer reports as a crash — on any violation.
#[allow(clippy::too_many_arguments)]
pub fn run_case(
    env: &Env,
    client: &KeeperRegistryClient<'_>,
    token: &token::Client<'_>,
    contract_id: &Address,
    owner: &Address,
    entries: &[EntrySeed],
    ceiling_seed: u8,
) {
    let now = env.ledger().timestamp();
    let (params, expected_total, overflowed) = build_params(env, entries, now);
    let ceiling = ceiling_for(ceiling_seed, expected_total, overflowed);

    let owner_before = token.balance(owner);
    let registry_before = token.balance(contract_id);
    let count_before = client.task_count();

    let predicted = predict(now, entries, ceiling);

    // Affordability is the reward token's business, not this entry point's.
    //
    // A batch can pass every parameter rule and every ceiling check and still
    // fail, because the escrow transfer runs out of the owner's balance -- a
    // single entry with `reward = i128::MAX` does exactly that. The failure
    // then comes from the token contract, and its error code is decoded
    // against `KeeperError`'s discriminants on the way back, so it can even
    // surface as a nonsensical variant (the SAC's insufficient-balance error
    // shares discriminant 10 with `InvalidFeeBps`).
    //
    // Asserting on that would be testing the token, and would drown out real
    // findings. Skip the case instead: `batch_register_tasks`'s own validation
    // has already been fully exercised by the time we get here.
    if predicted.is_none() && expected_total > owner_before {
        return;
    }

    let result = client.try_batch_register_tasks(owner, &params, &ceiling);

    // `try_*` splits the two failure kinds across the outer Result: `Ok` holds
    // the decoded return value, `Err` holds either the contract's own typed
    // error or an `InvokeError` for a host-level trap. Unwrapping the inner
    // `Err` as a `KeeperError` is what asserts "never traps".
    match (result, predicted) {
        (Ok(ids), None) => {
            let ids = ids.expect("a successful batch must decode its return value");
            assert_eq!(
                ids.len(),
                entries.len() as u32,
                "a successful batch must return one id per entry"
            );
            assert_eq!(
                client.task_count(),
                count_before + entries.len() as u64,
                "task counter must advance by exactly the batch size"
            );
            assert_eq!(
                token.balance(contract_id) - registry_before,
                expected_total,
                "registry must escrow exactly the batch total"
            );
            assert_eq!(
                owner_before - token.balance(owner),
                expected_total,
                "owner must be debited exactly the batch total"
            );

            // Ids are returned in input order and are all distinct.
            for i in 0..ids.len() {
                assert_eq!(
                    ids.get(i).unwrap(),
                    count_before + u64::from(i) + 1,
                    "ids must be allocated in input order"
                );
            }
        }
        (Err(actual), Some(expected)) => {
            let actual = actual.expect("rejection must be a typed KeeperError, not a host trap");
            assert_eq!(
                actual,
                expected,
                "wrong rejection for a batch of {} entries",
                entries.len()
            );

            // All-or-nothing: not one stroop moved, in either direction, and no
            // task was created.
            assert_eq!(
                token.balance(owner),
                owner_before,
                "owner balance changed on a rejected batch"
            );
            assert_eq!(
                token.balance(contract_id),
                registry_before,
                "registry balance changed on a rejected batch"
            );
            assert_eq!(
                client.task_count(),
                count_before,
                "task counter advanced on a rejected batch"
            );
        }
        (Ok(_), Some(expected)) => {
            panic!("batch was accepted but should have been rejected with {expected:?}")
        }
        (Err(actual), None) => {
            panic!("batch was rejected with {actual:?} but should have been accepted")
        }
    }
}

/// Decodes the corpus byte layout documented in
/// `fuzz/tests/corpus_seeds.rs`, returning the entry seeds and ceiling seed.
///
/// Shared with the fuzz target's `Arbitrary` impl so the corpus format has
/// exactly one definition. Missing trailing bytes decode as zero, so a
/// truncated input is a valid shorter batch rather than a rejected testcase —
/// libFuzzer mutates by truncation constantly.
pub fn decode(bytes: &[u8], out: &mut [EntrySeed; MAX_GENERATED_ENTRIES as usize]) -> (usize, u8) {
    let count_seed = bytes.first().copied().unwrap_or(0);
    let ceiling_seed = bytes.get(1).copied().unwrap_or(0);
    let count = (u32::from(count_seed) % (MAX_GENERATED_ENTRIES + 1)) as usize;

    for (i, slot) in out.iter_mut().enumerate().take(count) {
        let base = 2 + i * 4;
        *slot = EntrySeed {
            reward: bytes.get(base).copied().unwrap_or(0),
            deadline: bytes.get(base + 1).copied().unwrap_or(0),
            ttl: bytes.get(base + 2).copied().unwrap_or(0),
            lock: bytes.get(base + 3).copied().unwrap_or(0),
        };
    }

    (count, ceiling_seed)
}
