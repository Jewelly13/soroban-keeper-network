//! # Conservation properties across single and batch registration (issue 0109)
//!
//! Epic E03 built the solvency (issue 0054) and single-payout (issue 0056)
//! properties against the pre-batch contract. This module extends them to cover
//! `batch_register_tasks`, so epic E05's work is proven compatible with E03's.
//!
//! Two families of property live here:
//!
//! 1. **I-1 solvency**, from `docs/ARCHITECTURE.md`: at every transaction
//!    boundary the registry's token balance equals open-task escrow + credited
//!    keeper balances + accrued fees. [`property_solvency_holds_under_mixed_registration`]
//!    drives arbitrary interleavings of single and batch registration through
//!    the full task lifecycle and re-checks the identity after every step.
//!
//! 2. **Batch all-or-nothing**, the guarantee specific to batching:
//!    [`property_rejected_batch_moves_no_tokens`] asserts that after *any*
//!    rejected batch, the owner's balance and the registry's escrow are both
//!    provably unchanged — not merely "no task was created", but "no partial
//!    transfer happened either". Soroban transactions are atomic, so this
//!    should hold by construction; the property exists because "should hold by
//!    construction" is exactly the kind of claim that quietly stops being true.
//!
//! I-3 (each task has exactly one payout) is covered for batch-created tasks by
//! [`property_batch_created_task_pays_out_exactly_once`], which confirms a task
//! reached via the batch path is no more claimable twice than one reached via
//! `register_task`.

use keeper_registry::{
    KeeperError, KeeperRegistry, KeeperRegistryClient, TaskParams, TaskStatus, TaskType,
    MAX_BATCH_ENTRIES,
};
use proptest::prelude::*;
use soroban_sdk::{testutils::Address as _, token, Address, Bytes, Env, Vec};

const OWNER_FUNDING: i128 = 10_000_000_000i128;
const FEE_BPS: u32 = 300;

struct Setup {
    env: Env,
    owner: Address,
    token_id: Address,
    registry_id: Address,
    registry: KeeperRegistryClient<'static>,
}

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
    registry.initialize(&owner, &token_id, &FEE_BPS);

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

    /// Sum of rewards still escrowed: every task in a non-terminal state.
    fn open_escrow(&self) -> i128 {
        let mut total = 0i128;
        for id in 1..=self.registry.task_count() {
            let task = self.registry.get_task(&id);
            if matches!(task.status, TaskStatus::Pending | TaskStatus::Claimed) {
                total += task.reward;
            }
        }
        total
    }

    /// I-1: registry balance == open escrow + keeper credits + accrued fees.
    ///
    /// `keepers` must list every address that could hold a credit; the property
    /// tests below use a fixed, known set.
    fn assert_solvent(&self, keepers: &[Address], context: &str) {
        let credits: i128 = keepers
            .iter()
            .map(|k| self.registry.keeper_balance(k))
            .sum();
        let accounted = self.open_escrow() + credits + self.registry.fees_accrued();
        assert_eq!(
            self.registry_balance(),
            accounted,
            "I-1 solvency violated after {context}: registry holds {} but accounts for \
             {} (escrow {} + credits {} + fees {})",
            self.registry_balance(),
            accounted,
            self.open_escrow(),
            credits,
            self.registry.fees_accrued(),
        );
    }
}

/// A valid entry: deadline 1 hour out needs 720 + 17_280 = 18_000 TTL ledgers.
fn params(env: &Env, reward: i128) -> TaskParams {
    TaskParams {
        task_type: TaskType::Liquidation,
        calldata: Bytes::from_slice(env, b"model"),
        reward,
        deadline: env.ledger().timestamp() + 3_600,
        ttl_ledgers: 20_000,
        lock_ledgers: 120,
        verifier: None,
    }
}

fn register_single(s: &Setup, reward: i128) -> u64 {
    s.registry.register_task(
        &s.owner,
        &TaskType::Liquidation,
        &Bytes::from_slice(&s.env, b"model"),
        &reward,
        &(s.env.ledger().timestamp() + 3_600),
        &20_000u32,
        &120u32,
        &None,
    )
}

fn register_batch(s: &Setup, rewards: &[i128]) -> soroban_sdk::Vec<u64> {
    let mut entries = Vec::new(&s.env);
    for r in rewards {
        entries.push_back(params(&s.env, *r));
    }
    let ceiling: i128 = rewards.iter().sum();
    s.registry
        .batch_register_tasks(&s.owner, &entries, &ceiling)
}

/// How a task created in either way is resolved, so the lifecycle — not just
/// registration — is exercised.
#[derive(Debug, Clone, Copy)]
enum Resolution {
    Leave,
    Cancel,
    Execute,
}

// ─────────────────────────────────────────────────────────────────────────────
// I-1: solvency across mixed single and batch registration
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// I-1 must hold when task creation is an arbitrary interleaving of
    /// `register_task` and `batch_register_tasks` calls, and tasks are resolved
    /// through different terminal transitions.
    ///
    /// `ops` drives the interleaving: `None` is a single registration, `Some(n)`
    /// is a batch of `n` entries. Rewards vary per task so a bug that happened
    /// to balance out with uniform amounts is still caught.
    #[test]
    fn property_solvency_holds_under_mixed_registration(
        ops in prop::collection::vec(
            prop::option::of(1usize..=5usize),
            1usize..=6usize,
        ),
        base_reward in 1_000i128..=50_000i128,
        resolutions in prop::collection::vec(0u8..3u8, 1usize..=24usize),
    ) {
        let s = setup();
        let keeper = Address::generate(&s.env);
        let keepers = [keeper.clone()];

        s.assert_solvent(&keepers, "initialization");

        // ── Creation phase: interleave single and batch registrations ────────
        let mut created: std::vec::Vec<u64> = std::vec::Vec::new();
        let mut nth = 0i128;

        for op in &ops {
            match op {
                None => {
                    nth += 1;
                    let id = register_single(&s, base_reward + nth);
                    created.push(id);
                    s.assert_solvent(&keepers, "register_task");
                }
                Some(n) => {
                    let rewards: std::vec::Vec<i128> = (0..*n)
                        .map(|i| {
                            nth += 1;
                            base_reward + nth + i as i128
                        })
                        .collect();
                    let ids = register_batch(&s, &rewards);

                    // Ordering guarantee: result[i] corresponds to entry i.
                    prop_assert_eq!(ids.len(), rewards.len() as u32);
                    for (i, reward) in rewards.iter().enumerate() {
                        let id = ids.get(i as u32).unwrap();
                        prop_assert_eq!(s.registry.get_task(&id).reward, *reward);
                        created.push(id);
                    }
                    s.assert_solvent(&keepers, "batch_register_tasks");
                }
            }
        }

        // Every created task is distinct -- ids are never reused across the two
        // registration paths (I-7).
        let mut sorted = created.clone();
        sorted.sort_unstable();
        sorted.dedup();
        prop_assert_eq!(sorted.len(), created.len(), "task ids must be unique");

        // ── Resolution phase: drive tasks to terminal states ─────────────────
        for (i, id) in created.iter().enumerate() {
            let resolution = match resolutions.get(i % resolutions.len()) {
                Some(0) => Resolution::Cancel,
                Some(1) => Resolution::Execute,
                _ => Resolution::Leave,
            };

            match resolution {
                Resolution::Leave => {}
                Resolution::Cancel => {
                    s.registry.cancel_task(&s.owner, id);
                    s.assert_solvent(&keepers, "cancel_task");
                }
                Resolution::Execute => {
                    s.registry.claim_task(&keeper, id);
                    s.assert_solvent(&keepers, "claim_task");
                    s.registry.execute_task(
                        &keeper,
                        id,
                        &Bytes::from_slice(&s.env, b"proof"),
                    );
                    s.assert_solvent(&keepers, "execute_task");
                }
            }
        }

        // Withdrawal must also preserve the identity.
        if s.registry.keeper_balance(&keeper) > 0 {
            s.registry.withdraw_rewards(&keeper);
            s.assert_solvent(&keepers, "withdraw_rewards");
        }

        // Nothing was conjured across the whole run. Every token the owner
        // started with is now in exactly one of three places: still with the
        // owner, still held by the registry (as open escrow, unwithdrawn keeper
        // credit, or accrued fees), or withdrawn into the keeper's own account.
        let keeper_tokens = token::Client::new(&s.env, &s.token_id).balance(&keeper);
        prop_assert_eq!(
            s.owner_balance() + s.registry_balance() + keeper_tokens,
            OWNER_FUNDING,
            "value was created or destroyed across the run"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Batch all-or-nothing: zero token movement on any rejection
// ─────────────────────────────────────────────────────────────────────────────

/// The rejection reasons a batch can be turned away for. Each must produce the
/// same outcome: a typed error, no tasks, and not one stroop moved.
#[derive(Debug, Clone, Copy)]
enum Rejection {
    /// An entry with a non-positive reward.
    InvalidReward,
    /// An entry whose deadline has already passed.
    DeadlinePassed,
    /// An entry whose lock window is out of bounds.
    InvalidLock,
    /// An entry whose TTL does not cover its deadline.
    TtlTooShort,
    /// Entries summing above `max_total_reward`.
    CeilingExceeded,
    /// More entries than `MAX_BATCH_ENTRIES`.
    TooLarge,
}

impl Rejection {
    fn all() -> [Rejection; 6] {
        [
            Rejection::InvalidReward,
            Rejection::DeadlinePassed,
            Rejection::InvalidLock,
            Rejection::TtlTooShort,
            Rejection::CeilingExceeded,
            Rejection::TooLarge,
        ]
    }

    fn expected(&self) -> KeeperError {
        match self {
            Rejection::InvalidReward => KeeperError::InvalidReward,
            Rejection::DeadlinePassed => KeeperError::DeadlinePassed,
            Rejection::InvalidLock => KeeperError::InvalidTaskParams,
            Rejection::TtlTooShort => KeeperError::TtlTooShort,
            Rejection::CeilingExceeded => KeeperError::BatchRewardCeilingExceeded,
            Rejection::TooLarge => KeeperError::BatchTooLarge,
        }
    }
}

/// Builds a batch that will be rejected for `reason`, placing the offending
/// entry at `bad_index` among `good_count` otherwise-valid entries.
///
/// Returns the batch and the ceiling to pass alongside it.
fn rejecting_batch(
    s: &Setup,
    reason: Rejection,
    good_count: usize,
    bad_index: usize,
    reward: i128,
) -> (soroban_sdk::Vec<TaskParams>, i128) {
    let now = s.env.ledger().timestamp();
    let mut entries = Vec::new(&s.env);

    if let Rejection::TooLarge = reason {
        for _ in 0..(MAX_BATCH_ENTRIES + 1) {
            entries.push_back(params(&s.env, reward));
        }
        return (entries, i128::MAX);
    }

    if let Rejection::CeilingExceeded = reason {
        for _ in 0..good_count.max(2) {
            entries.push_back(params(&s.env, reward));
        }
        let total: i128 = reward * good_count.max(2) as i128;
        // One stroop under the total: valid entries, invalid sum.
        return (entries, total - 1);
    }

    let bad = match reason {
        Rejection::InvalidReward => TaskParams {
            reward: 0,
            ..params(&s.env, reward)
        },
        Rejection::DeadlinePassed => TaskParams {
            deadline: now,
            ..params(&s.env, reward)
        },
        Rejection::InvalidLock => TaskParams {
            lock_ledgers: 0,
            ..params(&s.env, reward)
        },
        Rejection::TtlTooShort => TaskParams {
            ttl_ledgers: 1_500,
            ..params(&s.env, reward)
        },
        Rejection::CeilingExceeded | Rejection::TooLarge => unreachable!("handled above"),
    };

    let index = bad_index.min(good_count);
    for i in 0..=good_count {
        if i == index {
            entries.push_back(bad.clone());
        } else {
            entries.push_back(params(&s.env, reward));
        }
    }

    (entries, i128::MAX)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// The batch-specific all-or-nothing guarantee: after a rejected batch, the
    /// owner's token balance and the registry's escrow are both unchanged.
    ///
    /// This is stronger than "no task was created". A batch that transferred
    /// escrow for its first two entries before rejecting the third would still
    /// create no tasks if the transfers were rolled back — but if they were
    /// not, the registry would be holding tokens no task accounts for, which is
    /// exactly the I-1 violation this property rules out.
    ///
    /// Varying `bad_index` matters: it puts the offending entry first, last,
    /// and in the middle, so a validation loop that transferred as it went
    /// would be caught rather than passing because the bad entry happened to
    /// come first.
    #[test]
    fn property_rejected_batch_moves_no_tokens(
        good_count in 1usize..=5usize,
        bad_index in 0usize..=5usize,
        reward in 1_000i128..=100_000i128,
        prior_tasks in 0usize..=2usize,
    ) {
        for reason in Rejection::all() {
            let s = setup();
            let keeper = Address::generate(&s.env);
            let keepers = [keeper];

            // Some pre-existing escrow, so the property is checked against a
            // non-empty registry rather than only a pristine one.
            for i in 0..prior_tasks {
                register_single(&s, reward + i as i128);
            }

            let owner_before = s.owner_balance();
            let registry_before = s.registry_balance();
            let count_before = s.registry.task_count();
            let escrow_before = s.open_escrow();

            let (entries, ceiling) =
                rejecting_batch(&s, reason, good_count, bad_index, reward);

            let err = s
                .registry
                .try_batch_register_tasks(&s.owner, &entries, &ceiling);

            // Every rejection is a typed KeeperError, never a host trap.
            let err = err
                .expect_err("batch should have been rejected")
                .expect("rejection must be a typed KeeperError");
            prop_assert_eq!(err, reason.expected(), "wrong error for {:?}", reason);

            // Zero token movement, in both directions.
            prop_assert_eq!(
                s.owner_balance(), owner_before,
                "owner balance moved on rejected batch ({:?})", reason
            );
            prop_assert_eq!(
                s.registry_balance(), registry_before,
                "registry balance moved on rejected batch ({:?})", reason
            );

            // And no state change either.
            prop_assert_eq!(
                s.registry.task_count(), count_before,
                "task counter advanced on rejected batch ({:?})", reason
            );
            prop_assert_eq!(
                s.open_escrow(), escrow_before,
                "open escrow changed on rejected batch ({:?})", reason
            );

            // I-1 still holds afterwards.
            s.assert_solvent(&keepers, "rejected batch");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// I-3: single payout, for tasks created via the batch path
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// I-3 must not weaken for batch-created tasks: exactly one terminal
    /// transition consumes each task's escrow, and a second attempt moves no
    /// tokens.
    #[test]
    fn property_batch_created_task_pays_out_exactly_once(
        count in 1usize..=4usize,
        reward in 10_000i128..=100_000i128,
    ) {
        let s = setup();
        let keeper = Address::generate(&s.env);
        let keepers = [keeper.clone()];

        let rewards: std::vec::Vec<i128> =
            (0..count).map(|i| reward + i as i128).collect();
        let ids = register_batch(&s, &rewards);

        for (i, expected_reward) in rewards.iter().enumerate() {
            let id = ids.get(i as u32).unwrap();

            s.registry.claim_task(&keeper, &id);
            let credit_before = s.registry.keeper_balance(&keeper);
            s.registry
                .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"proof"));
            let credited = s.registry.keeper_balance(&keeper) - credit_before;

            // Paid exactly once, at the configured rate.
            let expected_fee = expected_reward * FEE_BPS as i128 / 10_000;
            prop_assert_eq!(credited, expected_reward - expected_fee);

            // A second terminal transition must fail and move nothing.
            let registry_before = s.registry_balance();
            let credit_after_first = s.registry.keeper_balance(&keeper);

            prop_assert!(s
                .registry
                .try_execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"proof"))
                .is_err());
            prop_assert!(s.registry.try_cancel_task(&s.owner, &id).is_err());

            prop_assert_eq!(s.registry_balance(), registry_before, "double payout moved tokens");
            prop_assert_eq!(
                s.registry.keeper_balance(&keeper), credit_after_first,
                "keeper credited twice for one task"
            );
            prop_assert_eq!(s.registry.get_task(&id).status, TaskStatus::Executed);

            s.assert_solvent(&keepers, "post double-spend attempt");
        }
    }
//! # Stateful model-checking harness for `KeeperRegistry`
//!
//! Built for issue 0087 so that 0054, 0055, and 0058 don't each hand-roll the
//! same generator. The technique is *model-based testing*: keep a plain-Rust
//! `Model` of what the contract's state **should** be, drive a randomized but
//! mostly-valid sequence of `Action`s into both the real contract and the
//! `Model`, and after **every** step assert the contract's observable state
//! matches the `Model`.
//!
//! ## Why this rather than a plain proptest
//!
//! A proptest that only checks one aggregate at the end (say, solvency) finds
//! far less than one that diffs the whole observable state after each step: the
//! step-by-step diff tells you *which* call broke the invariant, and it catches
//! per-task bookkeeping bugs a single global sum would net out to zero.
//!
//! ## How to build a new property on this harness
//!
//! Everything you need is on [`Harness`] and [`Model`]:
//!
//! 1. Write a strategy for the plan. Reuse [`plan_strategy`] unless you need a
//!    different action mix; it already guarantees the first action is a
//!    registration so later actions have something to target.
//! 2. Call [`Harness::run`], which applies each action to contract and model,
//!    asserts [`Harness::assert_matches_model`] after each one, and returns
//!    [`Stats`].
//! 3. Add your own invariant. Two hook points:
//!    - a per-step check — pass it as the `after_each` closure to
//!      [`Harness::run_with`];
//!    - a whole-run check — assert against [`Harness::model`] once `run`
//!      returns.
//!
//! Worked example, and the harness's first consumer: 0054's solvency invariant
//! is [`Harness::assert_solvent`], asserted after every step inside
//! [`Harness::assert_matches_model`]. Ship a new property by following that
//! shape.
//!
//! ### Notes for 0055 / 0058
//!
//! - The model deliberately **reimplements** the reward split rather than
//!   calling `keeper_registry::split_reward`, so a bug in the contract's own
//!   arithmetic can't hide by being present on both sides of the comparison.
//!   See [`Model::split_reward`]. (`split_reward` itself is covered directly by
//!   `property_i4_split_reward_fee_is_bounded` in `src/test.rs`.)
//! - Ledger advancement is a first-class [`Action::Advance`], not a side
//!   effect, so the generator can interleave "wait N ledgers" with calls. This
//!   is what makes lock-window expiry and deadline expiry reachable.
//! - Per the issue's suggested scope, `increase_reward`, `extend_deadline`,
//!   `update_verifier`, and the admin functions are **not** modeled yet. Adding
//!   one means: a variant on [`Action`], a case in [`Model::is_valid`], a case
//!   in [`Model::apply`], and a case in [`Harness::call`]. Nothing else.
//! - Every task is registered with `verifier: None`, so `execute_task` takes
//!   its no-verifier path. Modeling verifiers needs a mock verifier contract
//!   and belongs in its own issue.
//! - The registry is never paused, so the pause matrix is out of scope here;
//!   `src/test.rs` covers it entry-point by entry-point.
//!
//! ## Validity of generated sequences
//!
//! The generator emits *selectors*, not concrete task ids: an action says "the
//! `pick`-th currently-claimable task", and the runner resolves that against
//! the model's live state. So a generated action is either
//!
//! - **applied** — the model considered it valid, and the contract accepted it;
//! - **skipped** — no eligible target existed at that point (e.g. a `Withdraw`
//!   when no keeper has a balance). Counted, and bounded by the property.
//!
//! There is deliberately no third "rejected" bucket in normal operation:
//! [`Stats::rejected`] counts calls the model predicted would succeed but the
//! contract refused, and the property asserts it is **zero**. A non-zero value
//! is a real finding — either the contract or [`Model::is_valid`] is wrong —
//! not generator noise to be tolerated.
//!
//! `test_generator_validity_rate` measures the applied/skipped split over many
//! sampled plans and fails if useful work drops too low, so the generator can't
//! silently decay into mostly-skips.

use std::collections::BTreeMap;

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config as ProptestRunnerConfig, TestRunner};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Bytes, Env,
};

use keeper_registry::{KeeperRegistry, KeeperRegistryClient, TaskStatus, TaskType};

// ─────────────────────────────────────────────────────────────────────────────
// Fixture constants
// ─────────────────────────────────────────────────────────────────────────────

/// Addresses in the scenario. Each can act as task owner and as keeper, which
/// is what makes owner/keeper confusion bugs reachable. Three is enough for
/// "someone else's task" without inflating the state space.
const ACTORS: usize = 3;

/// Minted to every actor up front — comfortably more than the largest plan can
/// escrow, so a failure is never just "ran out of tokens".
const MINT: i128 = 1_000_000_000i128;

const FEE_BPS: u32 = 300;

/// Rewards are generated in this range so `reward * 300 / 10_000` truncates for
/// most values, keeping rounding dust in play on every execution.
const REWARD_MIN: i128 = 1_000i128;
const REWARD_MAX: i128 = 1_000_000i128;

/// Deadline offsets, in seconds from the current ledger timestamp.
const DEADLINE_OFFSET_MIN: u64 = 600;
const DEADLINE_OFFSET_MAX: u64 = 7_200;

/// `register_task` requires `ttl_ledgers >= deadline_offset / 5 + 17_280`,
/// which at [`DEADLINE_OFFSET_MAX`] is `18_720`. A single fixed value above
/// that keeps TTL out of the state space — TTL bounds have their own tests in
/// `src/test.rs`.
const TTL_LEDGERS: u32 = 20_000;

/// Lock windows are generated small (the contract's floor is 12) so that
/// [`Action::Advance`] can realistically outlive one and make re-claim and
/// cancel-after-lock reachable.
const LOCK_LEDGERS_MIN: u32 = 12;
const LOCK_LEDGERS_MAX: u32 = 200;

// ─────────────────────────────────────────────────────────────────────────────
// Actions
// ─────────────────────────────────────────────────────────────────────────────

/// One state-mutating step. Task-targeting variants carry a `pick` *selector*
/// rather than a task id: the runner resolves it against the set the model
/// currently considers eligible, which is what keeps generated plans valid
/// instead of mostly-rejected.
///
/// Variants that must be called by a specific address (`Execute` by the
/// claimer, `Cancel` by the owner) deliberately do **not** generate a caller —
/// it is read off the model, so the happy path is reachable. Wrong-caller
/// rejection is a negative test, and `src/test.rs` already covers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Register {
        owner: usize,
        reward: i128,
        deadline_offset: u64,
        lock_ledgers: u32,
    },
    /// Claim (or re-claim, once the previous lock lapsed) the `pick`-th
    /// claimable task, as `keeper`.
    Claim { keeper: usize, pick: usize },
    /// Execute the `pick`-th executable task, as that task's own claimer.
    Execute { pick: usize },
    /// Cancel the `pick`-th cancellable task, as that task's own owner.
    Cancel { pick: usize },
    /// Permissionlessly expire the `pick`-th past-deadline task.
    Expire { pick: usize },
    /// Withdraw the whole balance of the `pick`-th actor that has one.
    Withdraw { pick: usize },
    /// Advance ledger sequence and timestamp. Modeled explicitly so waiting is
    /// something the generator chooses, not a hidden side effect.
    Advance { ledgers: u32, seconds: u64 },
}

// ─────────────────────────────────────────────────────────────────────────────
// Model
// ─────────────────────────────────────────────────────────────────────────────

/// What the model expects one task to look like. Mirrors only the fields the
/// harness asserts on or needs for validity decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelTask {
    owner: usize,
    reward: i128,
    deadline: u64,
    status: TaskStatus,
    claimer: Option<usize>,
    claim_ledger: Option<u32>,
    lock_ledgers: u32,
}

/// Expected contract state, tracked in plain Rust with no contract calls.
#[derive(Debug, Clone)]
struct Model {
    tasks: BTreeMap<u64, ModelTask>,
    /// Withdrawable balance per actor. Absent == zero.
    balances: BTreeMap<usize, i128>,
    fees_accrued: i128,
    /// Registrations ever made — never decremented, so it also encodes the
    /// next id (see I-7).
    task_count: u64,
    fee_bps: u32,
    /// Mirror of the ledger the harness advances.
    timestamp: u64,
    sequence: u32,
}

impl Model {
    fn new(fee_bps: u32, timestamp: u64, sequence: u32) -> Self {
        Model {
            tasks: BTreeMap::new(),
            balances: BTreeMap::new(),
            fees_accrued: 0,
            task_count: 0,
            fee_bps,
            timestamp,
            sequence,
        }
    }

    /// Independent reimplementation of the contract's reward split. Kept
    /// separate from `keeper_registry::split_reward` on purpose — a model that
    /// borrows the implementation's arithmetic cannot detect a bug in it.
    fn split_reward(&self, reward: i128) -> (i128, i128) {
        let fee = reward * (self.fee_bps as i128) / 10_000;
        (reward - fee, fee)
    }

    fn balance(&self, actor: usize) -> i128 {
        self.balances.get(&actor).copied().unwrap_or(0)
    }

    /// Mirrors the contract's `lock_expired`, including its inclusive `>=`
    /// boundary: at exactly `claim_ledger + lock_ledgers` the lock is already
    /// considered lapsed.
    fn lock_expired(&self, task: &ModelTask) -> bool {
        match task.claim_ledger {
            Some(claimed_at) => self.sequence >= claimed_at.saturating_add(task.lock_ledgers),
            None => true,
        }
    }

    /// Sum of escrow the contract still owes on tasks that haven't terminated.
    fn open_escrow(&self) -> i128 {
        self.tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Pending | TaskStatus::Claimed))
            .map(|t| t.reward)
            .sum()
    }

    fn credited(&self) -> i128 {
        self.balances.values().sum()
    }

    // ── eligibility sets — these are what keep generated plans valid ─────────

    /// `claim_task`: Pending, or Claimed with a lapsed lock; and strictly
    /// before the deadline.
    fn claimable(&self) -> Vec<u64> {
        self.tasks
            .iter()
            .filter(|(_, t)| {
                self.timestamp < t.deadline
                    && match t.status {
                        TaskStatus::Pending => true,
                        TaskStatus::Claimed => self.lock_expired(t),
                        _ => false,
                    }
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// `execute_task`: Claimed, before the deadline. The lock window does not
    /// gate execution — only re-claiming — so the current claimer may execute
    /// even after its lock lapsed, provided nobody re-claimed.
    fn executable(&self) -> Vec<u64> {
        self.tasks
            .iter()
            .filter(|(_, t)| {
                t.status == TaskStatus::Claimed
                    && t.claimer.is_some()
                    && self.timestamp < t.deadline
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// `cancel_task`: Pending, or Claimed with a lapsed lock. Note there is
    /// deliberately **no** deadline condition — the contract lets an owner
    /// cancel a past-deadline task that nobody expired.
    fn cancellable(&self) -> Vec<u64> {
        self.tasks
            .iter()
            .filter(|(_, t)| match t.status {
                TaskStatus::Pending => true,
                TaskStatus::Claimed => self.lock_expired(t),
                _ => false,
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// `expire_task`: Pending or Claimed, and the deadline has been reached.
    fn expirable(&self) -> Vec<u64> {
        self.tasks
            .iter()
            .filter(|(_, t)| {
                self.timestamp >= t.deadline
                    && matches!(t.status, TaskStatus::Pending | TaskStatus::Claimed)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// `withdraw_rewards`: any actor with a strictly positive balance.
    fn withdrawable(&self) -> Vec<usize> {
        (0..ACTORS).filter(|a| self.balance(*a) > 0).collect()
    }

    /// Resolves an [`Action`]'s selector into the concrete call the runner
    /// should make, or `None` if nothing is eligible (the action is skipped).
    ///
    /// This is the single place that decides what "valid" means, so extending
    /// the harness with a new entry point means adding one case here.
    fn is_valid(&self, action: Action) -> Option<Resolved> {
        fn pick_from<T: Copy>(items: &[T], pick: usize) -> Option<T> {
            if items.is_empty() {
                None
            } else {
                Some(items[pick % items.len()])
            }
        }

        match action {
            Action::Register {
                owner,
                reward,
                deadline_offset,
                lock_ledgers,
            } => Some(Resolved::Register {
                owner,
                reward,
                deadline: self.timestamp + deadline_offset,
                lock_ledgers,
            }),
            Action::Claim { keeper, pick } => {
                pick_from(&self.claimable(), pick).map(|id| Resolved::Claim { keeper, id })
            }
            Action::Execute { pick } => {
                pick_from(&self.executable(), pick).map(|id| Resolved::Execute {
                    keeper: self.tasks[&id].claimer.expect("Claimed task has a claimer"),
                    id,
                })
            }
            Action::Cancel { pick } => {
                pick_from(&self.cancellable(), pick).map(|id| Resolved::Cancel {
                    owner: self.tasks[&id].owner,
                    id,
                })
            }
            Action::Expire { pick } => {
                pick_from(&self.expirable(), pick).map(|id| Resolved::Expire { id })
            }
            Action::Withdraw { pick } => {
                pick_from(&self.withdrawable(), pick).map(|actor| Resolved::Withdraw { actor })
            }
            Action::Advance { ledgers, seconds } => Some(Resolved::Advance { ledgers, seconds }),
        }
    }

    /// Applies a resolved call's expected effects. Must stay in lockstep with
    /// [`Harness::call`].
    fn apply(&mut self, resolved: Resolved) {
        match resolved {
            Resolved::Register {
                owner,
                reward,
                deadline,
                lock_ledgers,
            } => {
                self.task_count += 1;
                let id = self.task_count;
                self.tasks.insert(
                    id,
                    ModelTask {
                        owner,
                        reward,
                        deadline,
                        status: TaskStatus::Pending,
                        claimer: None,
                        claim_ledger: None,
                        lock_ledgers,
                    },
                );
            }
            Resolved::Claim { keeper, id } => {
                let sequence = self.sequence;
                let task = self.tasks.get_mut(&id).expect("claimed task exists");
                task.status = TaskStatus::Claimed;
                task.claimer = Some(keeper);
                task.claim_ledger = Some(sequence);
            }
            Resolved::Execute { keeper, id } => {
                let reward = self.tasks[&id].reward;
                let (net, fee) = self.split_reward(reward);
                *self.balances.entry(keeper).or_insert(0) += net;
                self.fees_accrued += fee;
                // The reward field is left as-is by the contract, so the model
                // must leave it too — `get_task` still reports it after payout.
                self.tasks
                    .get_mut(&id)
                    .expect("executed task exists")
                    .status = TaskStatus::Executed;
            }
            Resolved::Cancel { owner: _, id } => {
                self.tasks
                    .get_mut(&id)
                    .expect("cancelled task exists")
                    .status = TaskStatus::Cancelled;
            }
            Resolved::Expire { id } => {
                self.tasks.get_mut(&id).expect("expired task exists").status = TaskStatus::Expired;
            }
            Resolved::Withdraw { actor } => {
                self.balances.insert(actor, 0);
            }
            Resolved::Advance { ledgers, seconds } => {
                self.sequence += ledgers;
                self.timestamp += seconds;
            }
        }
    }
}

/// An [`Action`] with its selector resolved to concrete arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolved {
    Register {
        owner: usize,
        reward: i128,
        deadline: u64,
        lock_ledgers: u32,
    },
    Claim {
        keeper: usize,
        id: u64,
    },
    Execute {
        keeper: usize,
        id: u64,
    },
    Cancel {
        owner: usize,
        id: u64,
    },
    Expire {
        id: u64,
    },
    Withdraw {
        actor: usize,
    },
    Advance {
        ledgers: u32,
        seconds: u64,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Generator
// ─────────────────────────────────────────────────────────────────────────────

fn register_strategy() -> impl Strategy<Value = Action> {
    (
        0..ACTORS,
        REWARD_MIN..=REWARD_MAX,
        DEADLINE_OFFSET_MIN..=DEADLINE_OFFSET_MAX,
        LOCK_LEDGERS_MIN..=LOCK_LEDGERS_MAX,
    )
        .prop_map(
            |(owner, reward, deadline_offset, lock_ledgers)| Action::Register {
                owner,
                reward,
                deadline_offset,
                lock_ledgers,
            },
        )
}

/// Weights favour building state up (register / claim / execute) over tearing
/// it down, because a plan that cancels and expires as fast as it registers
/// never reaches the deep multi-task states worth checking.
fn action_strategy() -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => register_strategy(),
        4 => (0..ACTORS, 0..8usize).prop_map(|(keeper, pick)| Action::Claim { keeper, pick }),
        4 => (0..8usize).prop_map(|pick| Action::Execute { pick }),
        2 => (0..8usize).prop_map(|pick| Action::Withdraw { pick }),
        3 => (1u32..=400u32, 0u64..=2_000u64)
                .prop_map(|(ledgers, seconds)| Action::Advance { ledgers, seconds }),
        1 => (0..8usize).prop_map(|pick| Action::Cancel { pick }),
        1 => (0..8usize).prop_map(|pick| Action::Expire { pick }),
    ]
}

/// A plan whose first action is always a registration, so the actions that
/// follow have something to target instead of being skipped.
fn plan_strategy(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<Action>> {
    (
        register_strategy(),
        prop::collection::vec(action_strategy(), len),
    )
        .prop_map(|(first, rest)| {
            let mut plan = Vec::with_capacity(rest.len() + 1);
            plan.push(first);
            plan.extend(rest);
            plan
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// Harness
// ─────────────────────────────────────────────────────────────────────────────

/// Outcome counts for one run. See the module docs on validity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Stats {
    /// Model said valid, contract accepted.
    applied: usize,
    /// No eligible target existed; no call was made.
    skipped: usize,
    /// Model said valid, contract refused. Always a finding — asserted zero.
    rejected: usize,
}

impl Stats {
    fn total(&self) -> usize {
        self.applied + self.skipped + self.rejected
    }
}

struct Harness {
    env: Env,
    registry_id: Address,
    token_id: Address,
    actors: Vec<Address>,
    model: Model,
    stats: Stats,
}

impl Harness {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();

        let minter = token::StellarAssetClient::new(&env, &token_id);
        let actors: Vec<Address> = (0..ACTORS).map(|_| Address::generate(&env)).collect();
        for actor in &actors {
            minter.mint(actor, &MINT);
        }

        let registry_id = env.register(KeeperRegistry, ());
        KeeperRegistryClient::new(&env, &registry_id).initialize(&admin, &token_id, &FEE_BPS);

        let model = Model::new(FEE_BPS, env.ledger().timestamp(), env.ledger().sequence());

        Harness {
            env,
            registry_id,
            token_id,
            actors,
            model,
            stats: Stats::default(),
        }
    }

    fn registry(&self) -> KeeperRegistryClient<'_> {
        KeeperRegistryClient::new(&self.env, &self.registry_id)
    }

    /// Makes the real call. Returns `Err(())` if the contract refused an action
    /// the model predicted would succeed — which the caller records as
    /// [`Stats::rejected`] and the property then fails on.
    ///
    /// Must stay in lockstep with [`Model::apply`].
    fn call(&mut self, resolved: Resolved) -> Result<(), ()> {
        let registry = self.registry();
        match resolved {
            Resolved::Register {
                owner,
                reward,
                deadline,
                lock_ledgers,
            } => registry
                .try_register_task(
                    &self.actors[owner],
                    &TaskType::Liquidation,
                    &Bytes::from_slice(&self.env, b"model-harness"),
                    &reward,
                    &deadline,
                    &TTL_LEDGERS,
                    &lock_ledgers,
                    &None,
                )
                .map(|_| ())
                .map_err(|_| ()),
            Resolved::Claim { keeper, id } => registry
                .try_claim_task(&self.actors[keeper], &id)
                .map(|_| ())
                .map_err(|_| ()),
            Resolved::Execute { keeper, id } => registry
                .try_execute_task(
                    &self.actors[keeper],
                    &id,
                    &Bytes::from_slice(&self.env, b"model-proof"),
                )
                .map(|_| ())
                .map_err(|_| ()),
            Resolved::Cancel { owner, id } => registry
                .try_cancel_task(&self.actors[owner], &id)
                .map(|_| ())
                .map_err(|_| ()),
            Resolved::Expire { id } => registry.try_expire_task(&id).map(|_| ()).map_err(|_| ()),
            Resolved::Withdraw { actor } => registry
                .try_withdraw_rewards(&self.actors[actor])
                .map(|_| ())
                .map_err(|_| ()),
            Resolved::Advance { ledgers, seconds } => {
                self.env.ledger().with_mut(|ledger| {
                    ledger.sequence_number += ledgers;
                    ledger.timestamp += seconds;
                });
                Ok(())
            }
        }
    }

    /// 0054's solvency invariant, and the harness's first consumer: the
    /// registry's token balance is exactly what it owes — open escrow, credited
    /// keeper balances, and accrued fees. Uses the model's numbers, so it is a
    /// genuine cross-check of on-chain funds against expected bookkeeping.
    fn assert_solvent(&self) -> Result<(), TestCaseError> {
        let held = token::Client::new(&self.env, &self.token_id).balance(&self.registry_id);
        let escrow = self.model.open_escrow();
        let credited = self.model.credited();
        let fees = self.model.fees_accrued;

        prop_assert_eq!(
            held,
            escrow + credited + fees,
            "registry holds {} but owes escrow {} + credited {} + fees {}",
            held,
            escrow,
            credited,
            fees
        );
        Ok(())
    }

    /// Diffs every piece of observable contract state against the model.
    fn assert_matches_model(&self) -> Result<(), TestCaseError> {
        let registry = self.registry();

        prop_assert_eq!(
            registry.task_count(),
            self.model.task_count,
            "task_count diverged"
        );
        prop_assert_eq!(
            registry.fees_accrued(),
            self.model.fees_accrued,
            "fees_accrued diverged"
        );

        for (actor, address) in self.actors.iter().enumerate() {
            prop_assert_eq!(
                registry.keeper_balance(address),
                self.model.balance(actor),
                "keeper_balance diverged for actor {}",
                actor
            );
        }

        for (id, expected) in &self.model.tasks {
            let actual = registry.get_task(id);
            prop_assert_eq!(&actual.status, &expected.status, "task {} status", id);
            prop_assert_eq!(actual.reward, expected.reward, "task {} reward", id);
            prop_assert_eq!(
                &actual.owner,
                &self.actors[expected.owner],
                "task {} owner",
                id
            );
            prop_assert_eq!(
                actual.claimer,
                expected.claimer.map(|a| self.actors[a].clone()),
                "task {} claimer",
                id
            );
            prop_assert_eq!(
                actual.claim_ledger,
                expected.claim_ledger,
                "task {} claim_ledger",
                id
            );
        }

        self.assert_solvent()
    }

    /// Applies a whole plan, asserting the full model diff after every step.
    fn run(&mut self, plan: &[Action]) -> Result<Stats, TestCaseError> {
        self.run_with(plan, |_| Ok(()))
    }

    /// [`Harness::run`] plus a caller-supplied per-step invariant — the hook
    /// 0055 and 0058 should use rather than reimplementing the loop.
    fn run_with(
        &mut self,
        plan: &[Action],
        mut after_each: impl FnMut(&Harness) -> Result<(), TestCaseError>,
    ) -> Result<Stats, TestCaseError> {
        for (step, &action) in plan.iter().enumerate() {
            let Some(resolved) = self.model.is_valid(action) else {
                self.stats.skipped += 1;
                continue;
            };

            match self.call(resolved) {
                Ok(()) => {
                    self.model.apply(resolved);
                    self.stats.applied += 1;
                }
                Err(()) => {
                    // The model predicted success and the contract disagreed.
                    // Recorded rather than asserted here so the message can
                    // name the step; the property asserts `rejected == 0`.
                    self.stats.rejected += 1;
                    return Err(TestCaseError::fail(format!(
                        "step {step}: contract rejected {resolved:?}, which the model \
                         considered valid — either the contract or Model::is_valid is wrong"
                    )));
                }
            }

            self.assert_matches_model()?;
            after_each(self)?;
        }
        Ok(self.stats)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Properties
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    // Each case stands up a fresh `Env` and makes up to ~25 real contract
    // calls, each followed by a full state diff, so the case count is tuned for
    // sequence depth over breadth.
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// The harness's headline property: across a randomized multi-task,
    /// multi-keeper sequence, the contract's observable state matches the model
    /// after every single step — and 0054's solvency invariant holds
    /// throughout (asserted inside `assert_matches_model`).
    #[test]
    fn property_model_matches_contract_after_every_step(
        plan in plan_strategy(0..24usize),
    ) {
        let mut harness = Harness::new();
        let stats = harness.run(&plan)?;

        prop_assert_eq!(stats.rejected, 0, "model and contract disagreed on validity");
        prop_assert_eq!(stats.total(), plan.len());
        // A plan that applied nothing would pass every assertion vacuously.
        prop_assert!(stats.applied > 0, "plan applied no actions at all");
    }
}

/// Measures the generator's applied/skipped split, so "mostly-valid input"
/// is a checked claim rather than an aspiration. Fails if useful work decays.
///
/// Samples plans from the same strategy the property uses, with a fixed
/// configuration for reproducibility, and runs each one for real.
#[test]
fn test_generator_validity_rate() {
    // Enough actions to make the percentage meaningful without dominating the
    // suite's runtime — every plan runs real contract calls.
    const PLANS: usize = 24;

    let strategy = plan_strategy(12..24usize);
    let mut runner = TestRunner::new(ProptestRunnerConfig::default());
    let mut total = Stats::default();

    for _ in 0..PLANS {
        let plan = strategy
            .new_tree(&mut runner)
            .expect("strategy produced a value")
            .current();

        let mut harness = Harness::new();
        let stats = harness
            .run(&plan)
            .expect("sampled plan should not diverge from the model");

        total.applied += stats.applied;
        total.skipped += stats.skipped;
        total.rejected += stats.rejected;
    }

    let applied_pct = 100 * total.applied / total.total();
    std::println!(
        "generator over {} plans / {} actions: {} applied ({}%), {} skipped, {} rejected",
        PLANS,
        total.total(),
        total.applied,
        applied_pct,
        total.skipped,
        total.rejected
    );

    assert_eq!(
        total.rejected, 0,
        "the model and the contract disagreed on whether a call was valid"
    );
    assert!(
        applied_pct >= 60,
        "only {applied_pct}% of generated actions did real work; the generator is \
         spending its budget on ineligible targets"
    );
}

/// Sanity check that the harness can actually reach every modeled action, so a
/// selector bug can't quietly make a whole entry point unreachable — which
/// would leave the property passing while testing less than it claims.
#[test]
fn test_harness_reaches_every_action() {
    // Register two tasks; execute one, cancel the other after its lock lapses;
    // let a third expire; then withdraw the executed reward.
    let plan = [
        Action::Register {
            owner: 0,
            reward: 100_001,
            deadline_offset: 3_600,
            lock_ledgers: 12,
        },
        Action::Claim { keeper: 1, pick: 0 },
        Action::Execute { pick: 0 },
        Action::Withdraw { pick: 0 },
        Action::Register {
            owner: 1,
            reward: 50_003,
            deadline_offset: 3_600,
            lock_ledgers: 12,
        },
        Action::Claim { keeper: 2, pick: 0 },
        // Outlive the 12-ledger lock so the owner may cancel.
        Action::Advance {
            ledgers: 20,
            seconds: 60,
        },
        Action::Cancel { pick: 0 },
        Action::Register {
            owner: 2,
            reward: 7_777,
            deadline_offset: 600,
            lock_ledgers: 12,
        },
        // Cross the deadline so the task becomes expirable.
        Action::Advance {
            ledgers: 200,
            seconds: 601,
        },
        Action::Expire { pick: 0 },
    ];

    let mut harness = Harness::new();
    let stats = harness
        .run(&plan)
        .expect("scripted plan should apply cleanly");

    assert_eq!(stats.rejected, 0);
    assert_eq!(
        stats.skipped, 0,
        "every scripted action should have a target"
    );
    assert_eq!(stats.applied, plan.len());

    // Each terminal state was reached exactly once, and ids were never reused.
    let statuses: Vec<&TaskStatus> = harness.model.tasks.values().map(|t| &t.status).collect();
    assert_eq!(
        statuses,
        vec![
            &TaskStatus::Executed,
            &TaskStatus::Cancelled,
            &TaskStatus::Expired
        ]
    );
    assert_eq!(harness.model.task_count, 3);
    // Withdrawn, so nothing is left credited; the fee from the one execution
    // stays behind for `sweep_fees`.
    assert_eq!(harness.model.credited(), 0);
    assert_eq!(harness.model.fees_accrued, 100_001 * 300 / 10_000);
    assert_eq!(harness.model.open_escrow(), 0);
}
