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
