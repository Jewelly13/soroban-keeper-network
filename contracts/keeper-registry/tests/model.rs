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
}
