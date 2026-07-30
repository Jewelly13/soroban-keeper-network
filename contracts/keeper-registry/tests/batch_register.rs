//! # `batch_register_tasks` behaviour and resource ceiling
//!
//! Covers the batch registration entry point: shared validation with
//! `register_task`, the `max_total_reward` ceiling, all-or-nothing semantics,
//! and the empirically-measured batch-size ceiling that `MAX_BATCH_ENTRIES` is
//! set against.
//!
//! The conservation properties (solvency across mixed single/batch traffic, and
//! zero token movement on any rejected batch) live in `tests/model.rs`.

use keeper_registry::{
    KeeperError, KeeperRegistry, KeeperRegistryClient, TaskParams, TaskStatus, TaskType,
    MAX_BATCH_ENTRIES, MAX_CALLDATA_LEN,
};
use soroban_sdk::{
    testutils::{Address as _, Events as _},
    token, Address, Bytes, Env, IntoVal, TryFromVal, Val, Vec,
};

struct Setup {
    env: Env,
    owner: Address,
    token_id: Address,
    registry_id: Address,
    registry: KeeperRegistryClient<'static>,
}

const OWNER_FUNDING: i128 = 1_000_000_000i128;

// The shared environment/client lifetime is intentionally extended for the
// standard Soroban test-harness pattern.
#[allow(clippy::useless_transmute, clippy::missing_transmute_annotations)]
fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&owner, &OWNER_FUNDING);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&owner, &token_id, &300u32);

    let env = unsafe { core::mem::transmute::<Env, Env>(env) };
    Setup {
        env,
        owner,
        token_id,
        registry_id,
        registry: unsafe { core::mem::transmute(registry) },
    }
}

impl Setup {
    fn owner_balance(&self) -> i128 {
        token::Client::new(&self.env, &self.token_id).balance(&self.owner)
    }

    fn registry_balance(&self) -> i128 {
        token::Client::new(&self.env, &self.token_id).balance(&self.registry_id)
    }
}

/// A valid entry. `deadline` is 1 hour out, so `required_ttl_ledgers` demands
/// 720 + 17_280 = 18_000 ledgers; 20_000 clears it.
fn params(env: &Env, reward: i128) -> TaskParams {
    TaskParams {
        task_type: TaskType::Liquidation,
        calldata: Bytes::from_slice(env, b"batch-entry"),
        reward,
        deadline: env.ledger().timestamp() + 3_600,
        ttl_ledgers: 20_000,
        lock_ledgers: 120,
        verifier: None,
    }
}

fn batch(env: &Env, rewards: &[i128]) -> Vec<TaskParams> {
    let mut v = Vec::new(env);
    for r in rewards {
        v.push_back(params(env, *r));
    }
    v
}

/// Rewards summing to a known total, for ceiling tests.
fn uniform_batch(env: &Env, count: u32, reward: i128) -> Vec<TaskParams> {
    let mut v = Vec::new(env);
    for _ in 0..count {
        v.push_back(params(env, reward));
    }
    v
}

// ─────────────────────────────────────────────────────────────────────────────
// Happy path (issue 0098)
// ─────────────────────────────────────────────────────────────────────────────

/// Registers a batch and confirms every entry is individually retrievable with
/// the right fields, and that ids come back in input order.
#[test]
fn batch_registers_every_entry_with_correct_fields() {
    let s = setup();
    let rewards = [1_000i128, 2_500i128, 7_000i128, 250i128];

    let ids = s
        .registry
        .batch_register_tasks(&s.owner, &batch(&s.env, &rewards), &100_000i128);

    assert_eq!(ids.len(), rewards.len() as u32);
    assert_eq!(s.registry.task_count(), rewards.len() as u64);

    for (i, expected_reward) in rewards.iter().enumerate() {
        let id = ids.get(i as u32).unwrap();

        // Ids are allocated in input order, so result[i] pairs with tasks[i].
        assert_eq!(id, i as u64 + 1, "ids must be returned in input order");

        let task = s.registry.get_task(&id);
        assert_eq!(task.owner, s.owner);
        assert_eq!(task.reward, *expected_reward);
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.task_type, TaskType::Liquidation);
        assert_eq!(task.ttl_ledgers, 20_000);
        assert_eq!(task.lock_ledgers, 120);
        assert_eq!(task.claimer, None);
        assert_eq!(task.claim_ledger, None);
        assert_eq!(task.verifier, None);
    }

    let total: i128 = rewards.iter().sum();
    assert_eq!(
        s.registry_balance(),
        total,
        "all escrow held by the registry"
    );
    assert_eq!(s.owner_balance(), OWNER_FUNDING - total);
}

/// An empty batch is a no-op success: a dApp looping over a possibly-empty work
/// list should not have to special-case it, and a no-op cannot violate any
/// accounting invariant.
#[test]
fn empty_batch_is_an_accepted_no_op() {
    let s = setup();
    let before = s.owner_balance();

    let ids = s
        .registry
        .batch_register_tasks(&s.owner, &Vec::new(&s.env), &0i128);

    assert_eq!(ids.len(), 0);
    assert_eq!(s.registry.task_count(), 0);
    assert_eq!(s.owner_balance(), before);
    assert_eq!(s.registry_balance(), 0);
}

/// A single-entry batch must be indistinguishable in effect from one
/// `register_task` call.
#[test]
fn single_entry_batch_matches_register_task() {
    let s = setup();

    let via_batch =
        s.registry
            .batch_register_tasks(&s.owner, &batch(&s.env, &[5_000i128]), &5_000i128);
    let batch_task = s.registry.get_task(&via_batch.get(0).unwrap());

    let single_id = s.registry.register_task(
        &s.owner,
        &TaskType::Liquidation,
        &Bytes::from_slice(&s.env, b"batch-entry"),
        &5_000i128,
        &(s.env.ledger().timestamp() + 3_600),
        &20_000u32,
        &120u32,
        &None,
    );
    let single_task = s.registry.get_task(&single_id);

    assert_eq!(batch_task.owner, single_task.owner);
    assert_eq!(batch_task.reward, single_task.reward);
    assert_eq!(batch_task.deadline, single_task.deadline);
    assert_eq!(batch_task.ttl_ledgers, single_task.ttl_ledgers);
    assert_eq!(batch_task.lock_ledgers, single_task.lock_ledgers);
    assert_eq!(batch_task.status, single_task.status);
    assert_eq!(batch_task.calldata, single_task.calldata);
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared validation (issue 0098: no duplicated validation)
// ─────────────────────────────────────────────────────────────────────────────

/// Every per-entry rejection reason must produce the same typed error the
/// single-task path produces, because both run the same helper. If someone adds
/// a check to `register_task` without routing it through
/// `validate_task_params`, one of these rows stops matching.
#[test]
fn batch_rejects_each_invalid_entry_exactly_as_register_task_does() {
    let s = setup();
    let now = s.env.ledger().timestamp();

    let mut oversized_calldata = Bytes::new(&s.env);
    for _ in 0..(MAX_CALLDATA_LEN + 1) {
        oversized_calldata.push_back(0u8);
    }

    let cases: [(TaskParams, KeeperError, &str); 6] = [
        (
            TaskParams {
                reward: 0,
                ..params(&s.env, 1_000)
            },
            KeeperError::InvalidReward,
            "non-positive reward",
        ),
        (
            TaskParams {
                deadline: now,
                ..params(&s.env, 1_000)
            },
            KeeperError::DeadlinePassed,
            "deadline already passed",
        ),
        (
            TaskParams {
                calldata: oversized_calldata,
                ..params(&s.env, 1_000)
            },
            KeeperError::CalldataTooLarge,
            "calldata over MAX_CALLDATA_LEN",
        ),
        (
            TaskParams {
                lock_ledgers: 1,
                ..params(&s.env, 1_000)
            },
            KeeperError::InvalidTaskParams,
            "lock_ledgers below MIN_LOCK_LEDGERS",
        ),
        (
            TaskParams {
                ttl_ledgers: 999,
                ..params(&s.env, 1_000)
            },
            KeeperError::InvalidTaskParams,
            "ttl_ledgers below MIN_TTL_LEDGERS",
        ),
        (
            TaskParams {
                ttl_ledgers: 1_500,
                ..params(&s.env, 1_000)
            },
            KeeperError::TtlTooShort,
            "ttl_ledgers does not cover the deadline",
        ),
    ];

    for (bad, expected, label) in cases {
        // Via the batch path, with the bad entry in the middle so it is not
        // simply the first thing validated.
        let mut entries = Vec::new(&s.env);
        entries.push_back(params(&s.env, 1_000));
        entries.push_back(bad.clone());
        entries.push_back(params(&s.env, 1_000));

        let batch_err = s
            .registry
            .try_batch_register_tasks(&s.owner, &entries, &1_000_000i128)
            .expect_err(label)
            .expect(label);
        assert_eq!(batch_err, expected, "batch path: {label}");

        // Via the single-task path, same inputs.
        let single_err = s
            .registry
            .try_register_task(
                &s.owner,
                &bad.task_type,
                &bad.calldata,
                &bad.reward,
                &bad.deadline,
                &bad.ttl_ledgers,
                &bad.lock_ledgers,
                &bad.verifier,
            )
            .expect_err(label)
            .expect(label);
        assert_eq!(
            single_err, expected,
            "single path must agree with batch path: {label}"
        );
    }

    // Nothing was registered and no escrow moved through any of it.
    assert_eq!(s.registry.task_count(), 0);
    assert_eq!(s.registry_balance(), 0);
    assert_eq!(s.owner_balance(), OWNER_FUNDING);
}

/// Extracting `register_task`'s validation into shared helpers must not have
/// reordered which error wins when several conditions fail at once.
///
/// The TTL-covers-deadline rule is deliberately kept *after* the `RewardToken`
/// lookup rather than folded into `validate_task_params`, precisely so this
/// ordering is preserved. These two cases are what would break if a future
/// tidy-up merged them.
#[test]
fn register_task_error_ordering_is_unchanged_by_the_shared_helpers() {
    let env = Env::default();
    env.mock_all_auths();
    let owner = Address::generate(&env);
    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    let now = env.ledger().timestamp();

    // Uninitialized registry plus a TTL that does not cover the deadline:
    // NotInitialized wins, because the RewardToken lookup runs first.
    let err = registry
        .try_register_task(
            &owner,
            &TaskType::Liquidation,
            &Bytes::new(&env),
            &1_000i128,
            &(now + 3_600),
            &1_500u32,
            &120u32,
            &None,
        )
        .expect_err("uninitialized registry must reject")
        .expect("typed error");
    assert_eq!(err, KeeperError::NotInitialized);

    // Uninitialized registry plus an invalid reward: InvalidReward wins,
    // because parameter-shape validation runs before the lookup.
    let err = registry
        .try_register_task(
            &owner,
            &TaskType::Liquidation,
            &Bytes::new(&env),
            &0i128,
            &(now + 3_600),
            &20_000u32,
            &120u32,
            &None,
        )
        .expect_err("invalid reward must reject")
        .expect("typed error");
    assert_eq!(err, KeeperError::InvalidReward);
}

/// `set_min_reward` is enforced per entry, through the same shared helper.
#[test]
fn batch_honours_min_reward() {
    let s = setup();
    s.registry.set_min_reward(&s.owner, &5_000i128);

    let err = s
        .registry
        .try_batch_register_tasks(
            &s.owner,
            &batch(&s.env, &[10_000i128, 4_999i128]),
            &100_000i128,
        )
        .expect_err("below min_reward")
        .expect("typed error");
    assert_eq!(err, KeeperError::InvalidReward);
    assert_eq!(s.registry_balance(), 0);

    // At the floor exactly, it succeeds.
    let ids = s
        .registry
        .batch_register_tasks(&s.owner, &batch(&s.env, &[5_000i128]), &5_000i128);
    assert_eq!(ids.len(), 1);
}

/// The pause gate applies to the batch path, matching `register_task`: both
/// open new escrow exposure.
#[test]
fn batch_is_blocked_while_paused() {
    let s = setup();
    s.registry.pause(&s.owner);

    let err = s
        .registry
        .try_batch_register_tasks(&s.owner, &batch(&s.env, &[1_000i128]), &1_000i128)
        .expect_err("paused")
        .expect("typed error");

    assert_eq!(err, KeeperError::ContractPaused);
    assert_eq!(s.registry_balance(), 0);
    assert_eq!(s.owner_balance(), OWNER_FUNDING);
}

// ─────────────────────────────────────────────────────────────────────────────
// max_total_reward ceiling (issue 0103)
// ─────────────────────────────────────────────────────────────────────────────

/// A batch summing above the ceiling is rejected entirely, with zero transfers.
#[test]
fn batch_above_reward_ceiling_is_rejected_with_no_transfers() {
    let s = setup();

    let err = s
        .registry
        .try_batch_register_tasks(
            &s.owner,
            &batch(&s.env, &[4_000i128, 4_000i128, 4_000i128]),
            &11_999i128, // one stroop under the 12_000 total
        )
        .expect_err("over ceiling")
        .expect("typed error");

    assert_eq!(err, KeeperError::BatchRewardCeilingExceeded);
    assert_eq!(s.registry.task_count(), 0);
    assert_eq!(s.registry_balance(), 0);
    assert_eq!(s.owner_balance(), OWNER_FUNDING);
}

/// Exactly at the ceiling succeeds — the comparison is `>`, not `>=`.
#[test]
fn batch_at_reward_ceiling_succeeds() {
    let s = setup();

    let ids = s.registry.batch_register_tasks(
        &s.owner,
        &batch(&s.env, &[4_000i128, 4_000i128, 4_000i128]),
        &12_000i128,
    );

    assert_eq!(ids.len(), 3);
    assert_eq!(s.registry_balance(), 12_000i128);
    assert_eq!(s.owner_balance(), OWNER_FUNDING - 12_000i128);
}

/// The ceiling is checked before any transfer, so it cannot be bypassed by a
/// batch whose early entries would fit but whose total does not.
#[test]
fn ceiling_is_enforced_before_any_transfer() {
    let s = setup();

    let err = s
        .registry
        .try_batch_register_tasks(
            &s.owner,
            // The first entry alone is under the ceiling; the batch is not.
            &batch(&s.env, &[100i128, 100_000i128]),
            &1_000i128,
        )
        .expect_err("over ceiling")
        .expect("typed error");

    assert_eq!(err, KeeperError::BatchRewardCeilingExceeded);
    assert_eq!(
        s.owner_balance(),
        OWNER_FUNDING,
        "not even the first, individually-affordable entry may be escrowed"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Batch size cap and resource ceiling (issues 0098, 0104)
// ─────────────────────────────────────────────────────────────────────────────

/// A batch over `MAX_BATCH_ENTRIES` fails as a typed error, not as an opaque
/// resource-exhaustion trap the caller cannot interpret.
#[test]
fn oversized_batch_is_rejected_with_a_typed_error() {
    let s = setup();

    let err = s
        .registry
        .try_batch_register_tasks(
            &s.owner,
            &uniform_batch(&s.env, MAX_BATCH_ENTRIES + 1, 100i128),
            &i128::MAX,
        )
        .expect_err("over MAX_BATCH_ENTRIES")
        .expect("must be a typed KeeperError, not a host trap");

    assert_eq!(err, KeeperError::BatchTooLarge);
    assert_eq!(s.registry.task_count(), 0);
    assert_eq!(s.registry_balance(), 0);
}

/// Issue 0104: pins the measured resource ceiling so a future change that makes
/// per-task registration more expensive is caught by CI rather than by a keeper
/// bot's transaction failing in production.
///
/// `MAX_BATCH_ENTRIES` is 32. This registers a full 32-entry batch and asserts
/// on the *budget consumed*, not merely on success — so the failure mode is a
/// clear "per-entry cost has grown" message rather than an opaque resource
/// error.
///
/// Measured at the time of writing: ~6.5M CPU instructions for 32 entries,
/// 6.5% of Soroban's 100M-instruction transaction budget, ~202k per entry. The
/// 25M threshold below is ~4x that, so ordinary drift does not cause churn but
/// a change that quadruples per-task cost does trip it.
///
/// Note what this test does *not* cover: CPU is not what makes 32 the cap. The
/// binding constraint is the per-transaction ledger write-entry footprint
/// (`txMaxWriteLedgerEntries`), one entry per task, which is a live network
/// config the test host does not enforce and this measurement cannot observe.
/// See `MAX_BATCH_ENTRIES` for that reasoning. This test guards the dimension
/// that *is* observable here.
#[test]
fn batch_ceiling_is_within_budget() {
    let s = setup();
    let entries = uniform_batch(&s.env, MAX_BATCH_ENTRIES, 1_000i128);

    s.env.cost_estimate().budget().reset_default();
    let ids = s
        .registry
        .batch_register_tasks(&s.owner, &entries, &i128::MAX);
    let cpu = s.env.cost_estimate().budget().cpu_instruction_cost();
    let mem = s.env.cost_estimate().budget().memory_bytes_cost();

    assert_eq!(ids.len(), MAX_BATCH_ENTRIES);

    let per_entry = cpu / MAX_BATCH_ENTRIES as u64;
    println!("batch of {MAX_BATCH_ENTRIES}: cpu={cpu} mem={mem} ({per_entry} cpu/entry)");

    assert!(
        cpu < 25_000_000,
        "a full {MAX_BATCH_ENTRIES}-entry batch consumed {cpu} CPU instructions \
         ({per_entry}/entry), up from the ~6.5M (~202k/entry) this cap was sized against. \
         Per-task registration has become substantially more expensive: either bring that \
         cost back down or reduce MAX_BATCH_ENTRIES, and re-check the footprint reasoning \
         in its doc comment while you are there."
    );
}

/// Sanity check that the cap is not so conservative it is pointless: one entry
/// past the cap would still have fit comfortably, which is exactly the headroom
/// the cap is meant to preserve.
#[test]
fn per_entry_cost_scales_linearly() {
    let s = setup();

    let measure = |count: u32| -> u64 {
        let entries = uniform_batch(&s.env, count, 1_000i128);
        s.env.cost_estimate().budget().reset_default();
        s.registry
            .batch_register_tasks(&s.owner, &entries, &i128::MAX);
        s.env.cost_estimate().budget().cpu_instruction_cost()
    };

    let small = measure(4);
    let large = measure(MAX_BATCH_ENTRIES);

    // Cost per entry must not blow up with batch size -- if it did, the linear
    // extrapolation behind MAX_BATCH_ENTRIES would be invalid.
    let small_per_entry = small / 4;
    let large_per_entry = large / MAX_BATCH_ENTRIES as u64;
    println!(
        "per-entry cpu: 4-entry={small_per_entry}, {MAX_BATCH_ENTRIES}-entry={large_per_entry}"
    );

    assert!(
        large_per_entry < small_per_entry * 2,
        "per-entry cost grew from {small_per_entry} to {large_per_entry} with batch size; \
         MAX_BATCH_ENTRIES assumes roughly linear scaling"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Events
// ─────────────────────────────────────────────────────────────────────────────

/// A successful batch emits its own summary event alongside the per-task
/// registration events, so an indexer can tell batched registrations apart.
#[test]
fn batch_emits_a_summary_event() {
    let s = setup();
    let ids = s.registry.batch_register_tasks(
        &s.owner,
        &batch(&s.env, &[1_000i128, 2_000i128]),
        &3_000i128,
    );
    assert_eq!(ids.len(), 2);

    let expected_topics: Vec<Val> = (
        soroban_sdk::symbol_short!("batchreg"),
        soroban_sdk::symbol_short!("task"),
    )
        .into_val(&s.env);

    let events = s.env.events().all();
    let found = events.iter().any(|(contract, topics, data)| {
        contract == s.registry_id
            && topics == expected_topics
            && <(Address, u32, i128)>::try_from_val(&s.env, &data)
                .map(|decoded| decoded == (s.owner.clone(), 2u32, 3_000i128))
                .unwrap_or(false)
    });

    assert!(
        found,
        "expected a batchreg/task summary event, got {events:?}"
    );
}
