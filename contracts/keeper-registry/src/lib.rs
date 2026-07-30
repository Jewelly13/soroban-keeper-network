//! # Soroban Keeper Network — Keeper Registry Contract
//!
//! This is the on-chain coordination layer of the Soroban Keeper Network.
//! dApps register automation tasks (liquidations, oracle pushes, TTL extensions…)
//! with an XLM reward bounty. Permissionless keeper bots compete to execute them.
//!
//! ## Implemented surface (MVP complete)
//! - Full schema: storage keys, types, errors, and events
//! - `initialize` / `register_task` — deploy, configure, and post funded tasks
//! - `claim_task` — first-come-first-served keeper locking with re-claim after
//!   the lock window elapses
//! - `execute_task` — proof submission, reward split, keeper crediting
//! - `cancel_task` / `expire_task` — owner refund and permissionless expiry
//! - `withdraw_rewards` — keeper pulls its accrued balance (CEI-safe)
//! - Admin: `pause`/`unpause`, `set_fee_bps`, `transfer_admin`, `upgrade`,
//!   `sweep_fees`
//! - Read-only views — `get_task`, `task_count`, `keeper_balance`,
//!   `fees_accrued`, `is_paused`, etc.
//!
//! ## Where contributors come in
//! The MVP is functional; the open issues now target Phase 2 (see README
//! Roadmap): on-chain execution verifiers, batch registration, keeper
//! staking/reputation, and an events indexer. See CONTRIBUTING.md.
//!
//! ## Storage Layout
//! - Instance:   Admin, FeeBps, Paused, TaskCounter, RewardToken, FeesAccrued
//! - Persistent: Task(id) → Task struct, KeeperReward(address) → i128

#![no_std]
// register_task's own #[allow(clippy::too_many_arguments)] covers the
// function body, but #[contractimpl]'s macro-generated dispatch code (the
// contractargs expansion) is checked as free-standing code clippy attributes
// distant lint spans to — not lexically inside the impl block or the
// function — so a function- or impl-level #[allow] doesn't reach it. A
// crate-level allow is the only attribute clippy actually honors for this
// specific macro-generated warning.
// `register_task` grows to 8 parameters once `verifier: Option<Address>` is
// added (see #98). A function-level `#[allow(...)]` on `register_task`
// doesn't reach the warning clippy raises against `#[contractimpl]`'s
// generated dispatch code for that function, so this has to be crate-level.
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, log, symbol_short, token, Address, Bytes,
    BytesN, Env, Vec,
};

// ─────────────────────────────────────────────────────────────────────────────
// Storage Keys
// ─────────────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    FeeBps,
    Paused,
    TaskCounter,
    RewardToken,
    Task(u64),
    KeeperReward(Address),
    /// Running total of protocol fees withheld from executed tasks, awaiting
    /// `sweep_fees`. Kept separate from task escrow so a sweep can never touch
    /// funds owed to owners or keepers.
    FeesAccrued,
    /// Minimum reward a task may be registered with. Guards against dust-spam
    /// tasks that would cost keepers more in fees than they pay out. Default 0.
    MinReward,
}

// ─────────────────────────────────────────────────────────────────────────────
// Domain Types
// ─────────────────────────────────────────────────────────────────────────────

/// The kind of automation this task represents.
/// Contributors: add new variants here as the network supports more use-cases.
#[contracttype]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TaskType {
    Liquidation = 0,
    OraclePricePush = 1,
    FundingRateUpdate = 2,
    LiquidityRebalance = 3,
    TtlExtension = 4,
    Custom = 5,
}

/// Lifecycle state of a task. Transitions are enforced by each function.
///
/// ```text
/// PENDING ──claim──▶ CLAIMED ──execute──▶ EXECUTED
///    │                  │
///  cancel             expire (deadline passed)
///    ▼                  ▼
/// CANCELLED          EXPIRED
/// ```
#[contracttype]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TaskStatus {
    Pending = 0,
    Claimed = 1,
    Executed = 2,
    Cancelled = 3,
    Expired = 4,
}

/// Full task record stored in Persistent storage.
#[contracttype]
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Task {
    /// Address that registered and funded this task.
    pub owner: Address,
    pub task_type: TaskType,
    /// Arbitrary bytes the keeper uses to reconstruct the target call
    /// off-chain. Bounded to [`MAX_CALLDATA_LEN`] at registration.
    pub calldata: Bytes,
    /// Reward escrowed in this contract (token units / XLM stroops).
    pub reward: i128,
    /// Unix timestamp IN SECONDS after which the task may be expired. Not
    /// directly comparable to `ttl_ledgers` — see that field.
    pub deadline: u64,
    /// Ledger TTL for this storage entry, IN LEDGERS (not seconds). Ledgers
    /// close roughly every `SECONDS_PER_LEDGER` seconds, so this and
    /// `deadline` are different units; `register_task`/`extend_deadline`
    /// enforce that this always covers `deadline` plus a safety margin so the
    /// entry cannot be evicted while its escrow is still live (see
    /// `required_ttl_ledgers`).
    pub ttl_ledgers: u32,
    pub status: TaskStatus,
    /// Set when a keeper claims the task.
    pub claimer: Option<Address>,
    /// Ledger sequence at claim time — used to enforce the lock window.
    pub claim_ledger: Option<u32>,
    /// Ledgers the claimer holds exclusive rights before re-claim is allowed.
    pub lock_ledgers: u32,
    /// Optional on-chain proof-verification callback (see [`IKeeperVerifier`]).
    /// `None` behaves exactly as the pre-verifier MVP: `execute_task` trusts
    /// the claimer's `proof` unconditionally. `Some(addr)` gates crediting
    /// the keeper on `addr.verify(...)` returning `true` — see `execute_task`.
    pub verifier: Option<Address>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Verifier interface — optional on-chain proof verification (Phase 2)
// ─────────────────────────────────────────────────────────────────────────────

/// A verifier is any contract a task owner opts into at registration (or,
/// while the task is still `Pending`, via `update_verifier`). `execute_task`
/// calls `verify` before crediting the keeper's reward; a `false` return
/// rejects the execution attempt with `KeeperError::VerificationFailed`
/// without transferring anything or changing the task's status, so the
/// keeper (or another keeper, once the lock lapses) may retry.
///
/// `keeper` is passed alongside `task` and `proof` specifically so a
/// verifier can bind its check to the address actually claiming credit —
/// without it, a valid proof observed on-chain (e.g. from a prior attempt's
/// calldata, or another task) could be replayed by a different keeper to
/// claim a reward it didn't earn.
///
/// A verifier is permissionless, consistent with this protocol's general
/// design philosophy: any address a task owner supplies is accepted, with
/// no registry-level allow-list. This puts the trust decision where it
/// belongs — with the task owner choosing what proof standard their task
/// requires — rather than centralizing it in the registry.
///
/// # Failure semantics
/// `execute_task` calls this via `Env::try_invoke_contract`, which recovers
/// gracefully from a verifier returning a *typed contract error* but does
/// **not** isolate a genuine panic (a raw `panic!`, an out-of-bounds access,
/// a WASM trap, `unwrap()` on `None`, etc.) — Soroban's host only converts
/// `ScErrorType::Contract` errors to a recoverable `Err`; any other error
/// class re-panics the caller (see `soroban-env-host`'s `Host::try_call`).
/// A verifier that panics therefore aborts the entire `execute_task`
/// transaction rather than being caught as a rejection: the task remains
/// `Claimed` and the only recovery path is `expire_task` once the deadline
/// passes (see `execute_task`'s doc comment for the full reasoning — this
/// is the same eventual-recovery guarantee every other stuck-task scenario
/// in this contract already relies on, not a new gap introduced by
/// verifiers). A well-behaved verifier should therefore prefer returning
/// `false` over panicking wherever the failure is a normal "proof didn't
/// check out" outcome, reserving an actual panic for conditions that are
/// genuinely exceptional.
/// A task owner may attach a contract implementing this interface to gate
/// `execute_task` on an on-chain check of the keeper's `proof`, instead of
/// trusting it unconditionally (the pre-verifier MVP default, `verifier:
/// None`).
///
/// ## Trust model
/// The verifier is chosen by the task owner, not the registry. It receives
/// the full `Task` (read-only), the claiming `keeper`, and the submitted
/// `proof`, and returns `true` to approve crediting or `false` to reject.
/// It cannot move funds, credit itself, or redirect the payout — only gate
/// whether `execute_task`'s own crediting logic runs (see `execute_task`).
///
/// ## Cross-contract call semantics — panics are NOT isolated
/// Per Soroban's host (`soroban-env-host`'s `Host::try_call`), only *typed
/// contract errors* are recovered as a graceful outcome across a
/// cross-contract call boundary. A genuine panic in a verifier (a WASM trap,
/// `unwrap()` on `None`, etc.) is a non-recoverable host error and re-panics
/// the caller — the whole `execute_task` transaction aborts, the task stays
/// `Claimed`, and the only recovery path is `expire_task` once the deadline
/// passes. A well-behaved verifier should therefore return `false` for a
/// "proof didn't check out" outcome rather than panicking, reserving an
/// actual panic for conditions that are genuinely exceptional.
/// ## Interface versioning
/// Every verifier must expose [`IKeeperVerifier::interface_version`] returning
/// [`KEEPER_VERIFIER_INTERFACE_VERSION`]. `execute_task` checks that value
/// before calling `verify` and rejects with
/// [`KeeperError::IncompatibleVerifierInterface`] on mismatch, so a verifier
/// written against an older calling convention cannot be invoked with a newer
/// one (and vice versa).
#[soroban_sdk::contractclient(name = "IKeeperVerifierClient")]
pub trait IKeeperVerifier {
    /// Version of the `IKeeperVerifier` calling convention this contract
    /// implements. Must equal [`KEEPER_VERIFIER_INTERFACE_VERSION`] for the
    /// registry to call [`IKeeperVerifier::verify`].
    fn interface_version(env: Env) -> u32;

    /// Returns `true` if `proof` is a valid attestation that `keeper`
    /// performed the off-chain action `task` describes.
    fn verify(env: Env, task: Task, keeper: Address, proof: Bytes) -> bool;
}

/// Current `IKeeperVerifier` calling-convention version. Verifiers must
/// return this from [`IKeeperVerifier::interface_version`]. Bump when
/// `verify`'s parameters or semantics change in a way that would make an
/// older verifier misbehave if called under the new convention.
pub const KEEPER_VERIFIER_INTERFACE_VERSION: u32 = 1;

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeeperError {
    AlreadyInitialized = 1,
    Unauthorized = 2,
    ContractPaused = 3,
    TaskNotFound = 4,
    InvalidTaskStatus = 5,
    DeadlinePassed = 6,
    DeadlineNotPassed = 7,
    InvalidReward = 8,
    LockPeriodActive = 9,
    InvalidFeeBps = 10,
    NotTaskOwner = 11,
    NotTaskClaimer = 12,
    NoRewardsAvailable = 13,
    /// `proof` passed to `execute_task` exceeded `MAX_PROOF_LEN`.
    ProofTooLarge = 14,
    /// A function requiring configured state (`initialize` must have been
    /// called) was invoked on a registry that isn't configured yet.
    NotInitialized = 15,
    /// `ttl_ledgers` does not cover the task's `deadline` plus the safety
    /// margin — the storage entry could expire while the escrow is still
    /// live. See [`required_ttl_ledgers`].
    TtlTooShort = 16,
    // 17 is reserved for `CalldataTooLarge`, added by a sibling in-flight PR
    // (see #13 / register_task calldata bounding). Left as a gap rather than
    // reused so the two branches don't collide on the same discriminant.
    // 16 is reserved for `TtlTooShort`, added by a sibling in-flight PR (see
    // #11 / register_task deadline-vs-TTL invariant). Left as a gap rather
    // than reused so the two branches don't collide on the same discriminant.
    /// `calldata` exceeds [`MAX_CALLDATA_LEN`].
    CalldataTooLarge = 17,
    /// `lock_ledgers` or `ttl_ledgers` passed to `register_task` fell outside
    /// their allowed bounds.
    InvalidTaskParams = 18,
    /// The task's attached verifier rejected `proof` (returned `false`).
    /// Distinct from `InvalidTaskStatus`/`NotTaskClaimer`: those mean the
    /// task moved out from under the caller and retrying the same way can't
    /// help; this means the specific proof was rejected, and the same
    /// keeper may retry `execute_task` with a different proof against the
    /// same claim.
    VerificationFailed = 19,
    /// Arithmetic operation would overflow or underflow.
    ArithmeticOverflow = 20,
    /// The attached verifier reported an `interface_version` other than
    /// [`KEEPER_VERIFIER_INTERFACE_VERSION`]. `verify` was not called.
    IncompatibleVerifierInterface = 21,
    /// A batch read (`get_tasks` / `get_tasks_range`) asked for more than
    /// [`MAX_BATCH_READ`] task ids. Returned rather than silently truncating,
    /// so a caller can never mistake a clipped page for the end of a range.
    BatchTooLarge = 22,
}

// ─────────────────────────────────────────────────────────────────────────────
// Task parameter bounds
// ─────────────────────────────────────────────────────────────────────────────

/// Stellar closes a ledger roughly every 5 seconds. A lock window shorter than
/// this gives the claiming keeper no realistic chance to build and submit its
/// `execute_task` transaction before another keeper can reclaim the task out
/// from under it.
const MIN_LOCK_LEDGERS: u32 = 12; // ~1 minute

/// A lock window longer than this lets a single unresponsive keeper hold a
/// task hostage for the better part of a day, with no possibility of
/// takeover until `expire_task` becomes callable at the deadline.
const MAX_LOCK_LEDGERS: u32 = 17_280; // ~1 day

/// Persistent storage entries need enough runway to survive from
/// registration through claim and execution without lapsing mid-flight.
/// Below this, the TTL extension is not worth writing and risks the entry
/// (and its escrowed reward) becoming inaccessible before a keeper can act.
const MIN_TTL_LEDGERS: u32 = 1_000; // ~83 minutes

// ─────────────────────────────────────────────────────────────────────────────
// Events — emitted for off-chain keeper bots to consume
// ─────────────────────────────────────────────────────────────────────────────

pub fn emit_task_registered(e: &Env, task_id: u64, owner: &Address, reward: i128, deadline: u64) {
    e.events().publish(
        (symbol_short!("reg"), symbol_short!("task")),
        (task_id, owner.clone(), reward, deadline),
    );
}

pub fn emit_task_claimed(e: &Env, task_id: u64, keeper: &Address) {
    e.events().publish(
        (symbol_short!("claim"), symbol_short!("task")),
        (task_id, keeper.clone(), e.ledger().sequence()),
    );
}

pub fn emit_task_executed(
    e: &Env,
    task_id: u64,
    keeper: &Address,
    net_reward: i128,
    proof: &Bytes,
) {
    e.events().publish(
        (symbol_short!("exec"), symbol_short!("task")),
        (task_id, keeper.clone(), net_reward, proof.clone()),
    );
}

pub fn emit_task_expired(e: &Env, task_id: u64) {
    e.events()
        .publish((symbol_short!("exp"), symbol_short!("task")), (task_id,));
}

pub fn emit_task_cancelled(e: &Env, task_id: u64, owner: &Address) {
    e.events().publish(
        (symbol_short!("cancel"), symbol_short!("task")),
        (task_id, owner.clone()),
    );
}

pub fn emit_rewards_withdrawn(e: &Env, keeper: &Address, amount: i128) {
    e.events().publish(
        (symbol_short!("wdraw"), symbol_short!("reward")),
        (keeper.clone(), amount),
    );
}

pub fn emit_paused(e: &Env, paused: bool) {
    e.events()
        .publish((symbol_short!("paused"), symbol_short!("admin")), (paused,));
}

pub fn emit_fee_updated(e: &Env, old_bps: u32, new_bps: u32) {
    e.events().publish(
        (symbol_short!("fee"), symbol_short!("admin")),
        (old_bps, new_bps),
    );
}

pub fn emit_admin_transferred(e: &Env, old_admin: &Address, new_admin: &Address) {
    e.events().publish(
        (symbol_short!("admin"), symbol_short!("xfer")),
        (old_admin.clone(), new_admin.clone()),
    );
}

pub fn emit_reward_increased(e: &Env, task_id: u64, new_reward: i128) {
    e.events().publish(
        (symbol_short!("topup"), symbol_short!("task")),
        (task_id, new_reward),
    );
}

pub fn emit_deadline_extended(e: &Env, task_id: u64, new_deadline: u64) {
    e.events().publish(
        (symbol_short!("extend"), symbol_short!("task")),
        (task_id, new_deadline),
    );
}

/// Fired when a task's attached verifier rejects a proof in `execute_task`.
/// `symbol_short!` is limited to 9 characters, so this uses `verfail` (the
/// natural `verifailed` doesn't fit) — the topic pair still uniquely
/// identifies the event alongside the "task" second topic, matching every
/// other per-task event in this file.
pub fn emit_verification_failed(e: &Env, task_id: u64, keeper: &Address) {
    e.events().publish(
        (symbol_short!("verfail"), symbol_short!("task")),
        (task_id, keeper.clone()),
    );
}

pub fn emit_verifier_updated(e: &Env, task_id: u64, verifier: &Option<Address>) {
    e.events().publish(
        (symbol_short!("verifier"), symbol_short!("task")),
        (task_id, verifier.clone()),
    );
}

pub fn emit_min_reward_updated(e: &Env, old_min: i128, new_min: i128) {
    e.events().publish(
        (symbol_short!("minrwd"), symbol_short!("admin")),
        (old_min, new_min),
    );
}

pub fn emit_fees_swept(e: &Env, treasury: &Address, amount: i128, remaining: i128) {
    e.events().publish(
        (symbol_short!("sweep"), symbol_short!("admin")),
        (treasury.clone(), amount, remaining),
    );
}

pub fn emit_initialized(e: &Env, admin: &Address, reward_token: &Address, fee_bps: u32) {
    e.events().publish(
        (symbol_short!("init"), symbol_short!("admin")),
        (admin.clone(), reward_token.clone(), fee_bps),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// TTL constants
// ─────────────────────────────────────────────────────────────────────────────
//
// Soroban archives storage entries once their TTL reaches zero; an archived
// entry must be explicitly restored before it can be read or written again.
// Instance storage holds the admin, reward token, pause flag, fee, and task
// counter — every entry point reads it, so it must never be allowed to lapse
// on an actively-used contract.

/// Ledgers of instance-storage lifetime requested on each state-mutating
/// call. At ~5s per ledger this is roughly 6 days; renewing it on every
/// mutation means a contract that sees regular traffic never approaches
/// archival.
const INSTANCE_BUMP_LEDGERS: u32 = 100_000;
/// Renew instance TTL only once fewer than this many ledgers remain, so the
/// extension is a no-op on most calls and only costs resources when the
/// entry is genuinely approaching expiry.
const INSTANCE_BUMP_THRESHOLD: u32 = 50_000;

/// Ledgers of persistent-storage lifetime requested for a keeper's reward
/// balance entry each time it is credited. Mirrors [`INSTANCE_BUMP_LEDGERS`].
const KEEPER_BALANCE_BUMP_LEDGERS: u32 = 100_000;
/// Renew a keeper balance entry only once fewer than this many ledgers
/// remain. Mirrors [`INSTANCE_BUMP_THRESHOLD`].
const KEEPER_BALANCE_BUMP_THRESHOLD: u32 = 50_000;

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Renews instance-storage TTL. Called from every state-mutating entry point
/// (never from read-only views): views are simulated by clients for free and
/// must stay side-effect-free, so instance liveness is kept up purely by
/// actual write traffic. A registry that goes completely idle — no
/// registrations, claims, executions, or admin calls — for the full TTL
/// window can still archive; that is an accepted tradeoff over charging real
/// transactions for simulated reads.
fn bump_instance(e: &Env) {
    e.storage()
        .instance()
        .extend_ttl(INSTANCE_BUMP_THRESHOLD, INSTANCE_BUMP_LEDGERS);
}

fn require_not_paused(e: &Env) -> Result<(), KeeperError> {
    if e.storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
    {
        Err(KeeperError::ContractPaused)
    } else {
        Ok(())
    }
}

fn require_admin(e: &Env, caller: &Address) -> Result<(), KeeperError> {
    // An admin key that hasn't been set yet means `initialize` was never
    // called — that's a different failure than an authenticated caller who
    // simply isn't the admin, so it gets its own error rather than being
    // folded into Unauthorized.
    let admin: Address = e
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(KeeperError::NotInitialized)?;
    caller.require_auth();
    if *caller != admin {
        return Err(KeeperError::Unauthorized);
    }
    Ok(())
}

fn next_task_id(e: &Env) -> u64 {
    let id: u64 = e
        .storage()
        .instance()
        .get(&DataKey::TaskCounter)
        .unwrap_or(0u64);
    // Unreachable: exhausting u64 task ids requires ~1.8e19 registrations, far
    // beyond any plausible lifetime of this contract.
    let next = id.checked_add(1).expect("task id counter exhausted");
    e.storage().instance().set(&DataKey::TaskCounter, &next);
    next
}

/// Ledgers close roughly every 5 seconds on Stellar. Used only to sanity-check
/// that a task's storage outlives its deadline; a conservative estimate is
/// correct here because over-estimating the ledger rate over-provisions TTL.
const SECONDS_PER_LEDGER: u64 = 5;

/// Extra ledgers kept beyond the deadline so `expire_task` (and `cancel_task`/
/// `execute_task`) are still callable for a while after the deadline passes,
/// giving a margin against clock drift between the two units below.
const TTL_SAFETY_MARGIN_LEDGERS: u32 = 17_280; // ~1 day

/// Minimum `ttl_ledgers` a task with the given `deadline` must be stored with
/// so its Persistent storage entry cannot be evicted while the escrow it
/// guards is still live. `deadline` is a unix timestamp (seconds);
/// `ttl_ledgers` is a ledger count — the two are different units with no
/// fixed conversion, so this is deliberately conservative
/// (see [`SECONDS_PER_LEDGER`], [`TTL_SAFETY_MARGIN_LEDGERS`]).
fn required_ttl_ledgers(e: &Env, deadline: u64) -> u64 {
    let seconds_until_deadline = deadline.saturating_sub(e.ledger().timestamp());
    let ledgers_until_deadline = seconds_until_deadline / SECONDS_PER_LEDGER;
    ledgers_until_deadline + TTL_SAFETY_MARGIN_LEDGERS as u64
}

fn load_task(e: &Env, task_id: u64) -> Result<Task, KeeperError> {
    e.storage()
        .persistent()
        .get(&DataKey::Task(task_id))
        .ok_or(KeeperError::TaskNotFound)
}

fn save_task(e: &Env, task_id: u64, task: &Task) {
    e.storage().persistent().set(&DataKey::Task(task_id), task);
    e.storage().persistent().extend_ttl(
        &DataKey::Task(task_id),
        task.ttl_ledgers,
        task.ttl_ledgers,
    );
}

fn reward_token(e: &Env) -> Result<token::Client<'_>, KeeperError> {
    let addr: Address = e
        .storage()
        .instance()
        .get(&DataKey::RewardToken)
        .ok_or(KeeperError::NotInitialized)?;
    Ok(token::Client::new(e, &addr))
}

/// Protocol fee applied when `FeeBps` has never been written. Kept at zero so
/// an uninitialized or partially-migrated registry can never silently skim
/// from a keeper's reward: a fee is a transfer of value away from the keeper,
/// and defaulting to charging one on a contract whose configuration is
/// unknown is the more surprising of the two failure modes.
pub const DEFAULT_FEE_BPS: u32 = 0;

/// Single source of truth for the current protocol fee. Every read of
/// `FeeBps` — views and the execution path alike — must go through this, so
/// a caller can never observe a fee rate that differs from the rate the
/// contract would actually apply.
fn fee_bps(e: &Env) -> u32 {
    e.storage()
        .instance()
        .get(&DataKey::FeeBps)
        .unwrap_or(DEFAULT_FEE_BPS)
}

/// Returns (keeper_net, protocol_fee).
///
/// # Rounding guarantee
///
/// The protocol fee is `floor(reward * fee_bps / 10_000)` and the keeper
/// receives the entire remainder. Rounding is therefore **always down for the
/// protocol and always in the keeper's favour**, and this is a guarantee, not
/// an incidental property of integer division:
///
/// - The protocol can never collect **more** than the nominal `fee_bps` rate.
///   It may collect very slightly less.
/// - The shortfall is bounded by **one stroop per execution** — the discarded
///   remainder is strictly less than the divisor.
/// - `keeper_net + fee == reward` holds exactly, for every input. No value is
///   created or destroyed by the split (invariant I-1; see
///   `docs/ARCHITECTURE.md`, "I-4: Fees are bounded and rounded down").
///
/// Rust's integer division truncates toward zero, which coincides with `floor`
/// here because `register_task` rejects a non-positive `reward`, so this
/// function is only ever reached with `reward > 0`.
///
/// ## Dust threshold
///
/// A consequence worth stating explicitly: for small rewards the fee rounds to
/// **zero** entirely. The fee is non-zero only once
///
/// ```text
/// reward >= ceil(10_000 / fee_bps)
/// ```
///
/// At the 300 bps (3%) default that threshold is 34 stroops: a reward of 33
/// yields a fee of 0 and the keeper takes all of it, while a reward of 34
/// yields a fee of 1. Setting `min_reward` below that threshold means the
/// protocol earns nothing on such tasks while still bearing their storage
/// cost, which is why `min_reward` and `fee_bps` should be chosen together
/// rather than independently. See the README tokenomics section.
///
/// Anyone reconciling expected against actual protocol revenue should expect a
/// deficit of up to one stroop per executed task. That is this rounding rule,
/// not a bug.
///
/// `pub` (not crate-private) so the `invariants` module and fuzz targets in
/// the separate `keeper-registry-fuzz` crate can call the exact same
/// arithmetic the contract itself uses, rather than reimplementing the
/// formula and risking the two drifting apart.
pub fn split_reward(reward: i128, fee_bps: u32) -> Result<(i128, i128), KeeperError> {
    let fee = reward
        .checked_mul(fee_bps as i128)
        .ok_or(KeeperError::ArithmeticOverflow)?
        / 10_000; // Divisor is a non-zero literal, cannot fail
    let net = reward
        .checked_sub(fee)
        .ok_or(KeeperError::ArithmeticOverflow)?;
    Ok((net, fee))
}

/// Adds `amount` to a keeper's withdrawable balance in Persistent storage.
/// Shared by `execute_task` (credit) and used as the source of truth for
/// `withdraw_rewards`. Kept as a single helper so the CEI invariant lives in
/// one place.
///
/// TTL is renewed here (on credit) and in `withdraw_rewards` (on
/// zero-out/write), but deliberately *not* on `keeper_balance` reads — see
/// the doc comment there for why a keeper that never returns can still see
/// its balance entry archive.
fn credit_keeper(e: &Env, keeper: &Address, amount: i128) -> Result<(), KeeperError> {
    let key = DataKey::KeeperReward(keeper.clone());
    let current: i128 = e.storage().persistent().get(&key).unwrap_or(0);
    let updated = current
        .checked_add(amount)
        .ok_or(KeeperError::ArithmeticOverflow)?;
    e.storage().persistent().set(&key, &updated);
    e.storage().persistent().extend_ttl(
        &key,
        KEEPER_BALANCE_BUMP_THRESHOLD,
        KEEPER_BALANCE_BUMP_LEDGERS,
    );
    Ok(())
}

/// Adds `amount` to the swept-able protocol fee accumulator (instance storage).
fn accrue_fee(e: &Env, amount: i128) -> Result<(), KeeperError> {
    if amount == 0 {
        return Ok(());
    }
    let current: i128 = e
        .storage()
        .instance()
        .get(&DataKey::FeesAccrued)
        .unwrap_or(0);
    let updated = current
        .checked_add(amount)
        .ok_or(KeeperError::ArithmeticOverflow)?;
    e.storage().instance().set(&DataKey::FeesAccrued, &updated);
    Ok(())
}

/// True once a claimed task's exclusive lock window has elapsed, meaning any
/// keeper may re-claim it. This is what prevents a keeper from claiming and then
/// never executing: after `lock_ledgers`, the task is fair game again.
///
/// The boundary is inclusive: at `claim_ledger + lock_ledgers` exactly, the
/// lock is already considered expired (`>=`, not `>`), so a re-claim is
/// allowed in the same ledger the window ends.
fn lock_expired(e: &Env, task: &Task) -> bool {
    match task.claim_ledger {
        Some(claimed_at) => {
            let unlock_at = claimed_at.saturating_add(task.lock_ledgers);
            e.ledger().sequence() >= unlock_at
        }
        // Unreachable in practice: every path that sets `status = Claimed`
        // (only `claim_task`) sets `claim_ledger` in the same write, so a
        // `Claimed` task always has `Some(claim_ledger)`. Both callers of
        // `lock_expired` only reach this branch after already matching on
        // `TaskStatus::Claimed`. Treated as "no active lock" if it ever were
        // reached, which is the safe default.
        None => true,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract
// ─────────────────────────────────────────────────────────────────────────────

/// Semantic version of the contract logic. Bumped on behavior changes so
/// off-chain clients and indexers can detect which ABI they are talking to.
pub const VERSION: u32 = 4;

/// Maximum `calldata` length, in bytes. Sized to hold an encoded contract
/// call — a target address, a function symbol, and a handful of scalar or
/// address arguments (an XDR-encoded `Address` is ~40 bytes, a `Symbol` up to
/// 32) — with headroom, without letting a task owner push storage and
/// re-serialisation cost onto the keepers and passers-by who load and
/// re-write this `Task` on every later lifecycle call (`claim_task`,
/// `execute_task`, and the permissionless `expire_task`).
pub const MAX_CALLDATA_LEN: u32 = 1024;

/// Maximum length, in bytes, of the `proof` accepted by `execute_task`.
/// Event data is charged against the paying keeper's transaction resource
/// budget, so an unbounded proof would make execution arbitrarily expensive.
/// 256 bytes comfortably fits a 32-byte tx hash or a small state witness —
/// the two shapes of proof this MVP expects — while keeping the emitted
/// event's cost bounded and predictable.
pub const MAX_PROOF_LEN: u32 = 256;

/// Maximum number of task ids a single [`KeeperRegistry::get_tasks`] or
/// [`KeeperRegistry::get_tasks_range`] call will accept.
///
/// Each id costs exactly one Persistent storage read, and every read is
/// charged against the transaction's read-entry and read-bytes resource
/// limits. A `Task` is dominated by its `calldata`, capped at
/// [`MAX_CALLDATA_LEN`] (1 KiB), so a worst-case batch of 50 reads moves on the
/// order of 50 KiB plus 50 ledger entries — comfortably inside a single
/// simulation on both counts, with room for the rest of a caller's footprint.
///
/// This is a deliberately conservative bound rather than the largest that
/// would fit: a batch read that intermittently exceeds the resource budget is
/// worse for a polling bot than one that is always cheap, because the failure
/// depends on the *contents* of the range rather than on anything the caller
/// controls. Callers needing more than 50 tasks issue several calls.
pub const MAX_BATCH_READ: u32 = 50;

#[contract]
pub struct KeeperRegistry;

#[contractimpl]
impl KeeperRegistry {
    // ── initialize ───────────────────────────────────────────────────────────
    //
    // Fully implemented. Call once after deployment.
    //
    // Arguments:
    //   admin        — address that controls admin functions
    //   reward_token — SAC / XLM token contract address used for escrow
    //   fee_bps      — platform fee in basis points (e.g. 300 = 3%)

    pub fn initialize(
        e: Env,
        admin: Address,
        reward_token: Address,
        fee_bps: u32,
    ) -> Result<(), KeeperError> {
        if e.storage().instance().has(&DataKey::Admin) {
            return Err(KeeperError::AlreadyInitialized);
        }
        if fee_bps > 10_000 {
            return Err(KeeperError::InvalidFeeBps);
        }
        admin.require_auth();

        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage()
            .instance()
            .set(&DataKey::RewardToken, &reward_token);
        e.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        e.storage().instance().set(&DataKey::Paused, &false);
        e.storage().instance().set(&DataKey::TaskCounter, &0u64);
        bump_instance(&e);

        emit_initialized(&e, &admin, &reward_token, fee_bps);
        log!(&e, "KeeperRegistry initialized by {}", admin);
        Ok(())
    }

    // ── register_task ────────────────────────────────────────────────────────
    //
    // Fully implemented. Any dApp or wallet calls this to post a task.
    // The reward is escrowed in this contract immediately on registration.
    //
    // Arguments:
    //   owner        — address funding the task (must auth)
    //   task_type    — classification (Liquidation, OraclePricePush, …)
    //   calldata     — encoded params the keeper uses to build the target
    //                  call; capped at MAX_CALLDATA_LEN bytes, rejected with
    //                  CalldataTooLarge otherwise
    //   reward       — XLM stroops escrowed as bounty
    //   deadline     — unix timestamp after which the task expires
    //   ttl_ledgers  — how long to keep the storage entry alive; must be at
    //                  least `MIN_TTL_LEDGERS`
    //   lock_ledgers — ledgers the claimer holds exclusive rights; must be in
    //                  `[MIN_LOCK_LEDGERS, MAX_LOCK_LEDGERS]`
    //   verifier     — optional on-chain proof-verification callback (see
    //                  `IKeeperVerifier`); `None` preserves the pre-verifier
    //                  MVP behavior exactly (execute_task trusts the proof
    //                  unconditionally)
    //                  `IKeeperVerifier`); `None` = trust the proof
    //                  unconditionally, same as before this parameter existed
    //
    // Returns the new task_id.

    // The task parameters are all distinct scalars a caller must supply; a
    // params struct would just move them without improving the ABI.
    pub fn register_task(
        e: Env,
        owner: Address,
        task_type: TaskType,
        calldata: Bytes,
        reward: i128,
        deadline: u64,
        ttl_ledgers: u32,
        lock_ledgers: u32,
        verifier: Option<Address>,
    ) -> Result<u64, KeeperError> {
        require_not_paused(&e)?;
        owner.require_auth();

        if reward <= 0 {
            return Err(KeeperError::InvalidReward);
        }
        let min_reward: i128 = e.storage().instance().get(&DataKey::MinReward).unwrap_or(0);
        if reward < min_reward {
            return Err(KeeperError::InvalidReward);
        }
        if deadline <= e.ledger().timestamp() {
            return Err(KeeperError::DeadlinePassed);
        }
        if calldata.len() > MAX_CALLDATA_LEN {
            return Err(KeeperError::CalldataTooLarge);
        }
        if !(MIN_LOCK_LEDGERS..=MAX_LOCK_LEDGERS).contains(&lock_ledgers) {
            return Err(KeeperError::InvalidTaskParams);
        }
        if ttl_ledgers < MIN_TTL_LEDGERS {
            return Err(KeeperError::InvalidTaskParams);
        }

        bump_instance(&e);

        // Escrow the reward from the owner into this contract.
        let token = reward_token(&e)?;
        if (ttl_ledgers as u64) < required_ttl_ledgers(&e, deadline) {
            return Err(KeeperError::TtlTooShort);
        }
        token.transfer(&owner, &e.current_contract_address(), &reward);

        let task_id = next_task_id(&e);
        let task = Task {
            owner: owner.clone(),
            task_type,
            calldata,
            reward,
            deadline,
            ttl_ledgers,
            status: TaskStatus::Pending,
            claimer: None,
            claim_ledger: None,
            lock_ledgers,
            verifier,
        };
        save_task(&e, task_id, &task);
        emit_task_registered(&e, task_id, &owner, reward, deadline);

        log!(&e, "Task {} registered reward={}", task_id, reward);
        Ok(task_id)
    }

    // ── increase_reward ──────────────────────────────────────────────────────
    //
    // The owner tops up the bounty on a task that hasn't finished yet (Pending
    // or Claimed) to attract keepers. The extra amount is escrowed immediately.

    pub fn increase_reward(
        e: Env,
        owner: Address,
        task_id: u64,
        additional: i128,
    ) -> Result<(), KeeperError> {
        require_not_paused(&e)?;
        owner.require_auth();

        if additional <= 0 {
            return Err(KeeperError::InvalidReward);
        }
        let mut task = load_task(&e, task_id)?;
        if task.owner != owner {
            return Err(KeeperError::NotTaskOwner);
        }
        match task.status {
            TaskStatus::Pending | TaskStatus::Claimed => {}
            _ => return Err(KeeperError::InvalidTaskStatus),
        }

        bump_instance(&e);
        reward_token(&e)?.transfer(&owner, &e.current_contract_address(), &additional);
        task.reward = task
            .reward
            .checked_add(additional)
            .expect("reward overflow");
        save_task(&e, task_id, &task);

        emit_reward_increased(&e, task_id, task.reward);
        log!(&e, "Task {} reward increased to {}", task_id, task.reward);
        Ok(())
    }

    // ── extend_deadline ──────────────────────────────────────────────────────
    //
    // The owner pushes out the deadline on an unfinished task so keepers have
    // more time. The new deadline must be strictly later than the current one.

    pub fn extend_deadline(
        e: Env,
        owner: Address,
        task_id: u64,
        new_deadline: u64,
    ) -> Result<(), KeeperError> {
        owner.require_auth();

        let mut task = load_task(&e, task_id)?;
        if task.owner != owner {
            return Err(KeeperError::NotTaskOwner);
        }
        match task.status {
            TaskStatus::Pending | TaskStatus::Claimed => {}
            _ => return Err(KeeperError::InvalidTaskStatus),
        }
        if new_deadline <= task.deadline {
            return Err(KeeperError::DeadlinePassed);
        }
        if (task.ttl_ledgers as u64) < required_ttl_ledgers(&e, new_deadline) {
            return Err(KeeperError::TtlTooShort);
        }

        bump_instance(&e);
        task.deadline = new_deadline;
        save_task(&e, task_id, &task);

        emit_deadline_extended(&e, task_id, new_deadline);
        log!(&e, "Task {} deadline extended to {}", task_id, new_deadline);
        Ok(())
    }

    // ── update_verifier ──────────────────────────────────────────────────────
    //
    // Lets the owner attach, replace, or remove (`None`) a task's verifier
    // before anyone claims it. Restricted to `Pending` only — once a keeper
    // has claimed the task and started acting on the terms it saw at claim
    // time, swapping in a verifier it cannot satisfy would let the owner
    // grief that keeper's uncompensated off-chain work. See `IKeeperVerifier`
    // for the full griefing-protection rationale.
    // Lets the owner change or clear a task's attached verifier before it's
    // claimed. Unlike `increase_reward`/`extend_deadline`, this is Pending-only
    // (not also Claimed): once a keeper has claimed a task, it has committed
    // to a specific proof requirement, and changing that requirement out from
    // under an already-claimed keeper would be a bait-and-switch — a keeper
    // could do all the off-chain work for a `None`/easy verifier only to have
    // the owner swap in a verifier its proof can't satisfy, with no way to
    // recover the work already done beyond waiting for the lock to lapse.

    pub fn update_verifier(
        e: Env,
        owner: Address,
        task_id: u64,
        new_verifier: Option<Address>,
    ) -> Result<(), KeeperError> {
        require_not_paused(&e)?;
        owner.require_auth();

        let mut task = load_task(&e, task_id)?;
        if task.owner != owner {
            return Err(KeeperError::NotTaskOwner);
        }
        if task.status != TaskStatus::Pending {
            return Err(KeeperError::InvalidTaskStatus);
        }

        bump_instance(&e);
        task.verifier = new_verifier.clone();
        save_task(&e, task_id, &task);

        emit_verifier_updated(&e, task_id, &new_verifier);
        log!(&e, "Task {} verifier updated", task_id);
        Ok(())
    }

    // ── claim_task ───────────────────────────────────────────────────────────
    //
    // Permissionless first-come-first-served claiming. A Pending task may be
    // claimed by anyone; a Claimed task may be re-claimed only after its
    // previous claimer's lock window has elapsed (see `lock_expired`), which
    // stops a keeper from squatting on a task it never intends to execute.

    pub fn claim_task(e: Env, keeper: Address, task_id: u64) -> Result<(), KeeperError> {
        require_not_paused(&e)?;
        keeper.require_auth();

        let mut task = load_task(&e, task_id)?;

        if e.ledger().timestamp() >= task.deadline {
            return Err(KeeperError::DeadlinePassed);
        }

        match task.status {
            TaskStatus::Pending => {}
            TaskStatus::Claimed => {
                // Only allow a takeover once the current lock has expired.
                if !lock_expired(&e, &task) {
                    return Err(KeeperError::LockPeriodActive);
                }
            }
            _ => return Err(KeeperError::InvalidTaskStatus),
        }

        bump_instance(&e);
        task.status = TaskStatus::Claimed;
        task.claimer = Some(keeper.clone());
        task.claim_ledger = Some(e.ledger().sequence());
        save_task(&e, task_id, &task);

        emit_task_claimed(&e, task_id, &keeper);
        log!(&e, "Task {} claimed by {}", task_id, keeper);
        Ok(())
    }

    // ── execute_task ─────────────────────────────────────────────────────────
    //
    // The claiming keeper submits proof that it performed the off-chain action
    // and is credited its share of the escrowed reward. The protocol fee stays
    // in the contract (later swept by admin via `sweep_fees`). The reward is
    // credited to an internal balance rather than transferred out here so the
    // keeper controls when it pays the withdrawal transfer cost.
    //
    // The proof is emitted in `TaskExecuted` (not just logged) so it is
    // publicly recoverable off-chain — this MVP trusts the claimer to submit
    // it (see README's Known Design Decisions), and that trade-off only holds
    // if a keeper submitting garbage can be identified after the fact. Its
    // size is bounded by `MAX_PROOF_LEN` since event data is charged against
    // the paying keeper's transaction resource budget.
    //
    // If the task has a `verifier` attached (see `IKeeperVerifier`), it is
    // called here — after the status/claimer/deadline checks above, before
    // any crediting or status mutation — and a `false` result rejects the
    // call with `VerificationFailed` without transferring anything or
    // changing the task's status, so the same keeper may retry with a
    // different proof. A task with no verifier (`None`) behaves exactly as
    // before verifiers existed: this is a strictly additive code path.
    // If the task has an attached verifier (see `IKeeperVerifier`), its
    // `verify` is called after the checks above and before any crediting —
    // rejection (`false`) leaves the task `Claimed` with nothing transferred
    // or mutated, so the keeper may retry with a different proof.

    pub fn execute_task(
        e: Env,
        keeper: Address,
        task_id: u64,
        proof: Bytes,
    ) -> Result<(), KeeperError> {
        require_not_paused(&e)?;
        keeper.require_auth();

        if proof.len() > MAX_PROOF_LEN {
            return Err(KeeperError::ProofTooLarge);
        }

        let mut task = load_task(&e, task_id)?;

        if task.status != TaskStatus::Claimed {
            return Err(KeeperError::InvalidTaskStatus);
        }
        // Only the keeper that currently holds the claim may execute.
        if task.claimer.as_ref() != Some(&keeper) {
            return Err(KeeperError::NotTaskClaimer);
        }
        if e.ledger().timestamp() >= task.deadline {
            return Err(KeeperError::DeadlinePassed);
        }

        if let Some(verifier) = task.verifier.clone() {
            let client = IKeeperVerifierClient::new(&e, &verifier);
            // Reject incompatible interface versions before `verify` so a
            // verifier written against a different calling convention cannot
            // silently mis-handle the current argument layout.
            if client.interface_version() != KEEPER_VERIFIER_INTERFACE_VERSION {
                return Err(KeeperError::IncompatibleVerifierInterface);
            }
            let approved: bool = client.verify(&task, &keeper, &proof);
            if !approved {
                emit_verification_failed(&e, task_id, &keeper);
                return Err(KeeperError::VerificationFailed);
            }
        }

        bump_instance(&e);
        let (keeper_net, fee) = split_reward(task.reward, fee_bps(&e))?;
        credit_keeper(&e, &keeper, keeper_net)?;
        accrue_fee(&e, fee)?;

        task.status = TaskStatus::Executed;
        save_task(&e, task_id, &task);

        emit_task_executed(&e, task_id, &keeper, keeper_net, &proof);
        log!(
            &e,
            "Task {} executed by {} net={} proof_len={}",
            task_id,
            keeper,
            keeper_net,
            proof.len()
        );
        Ok(())
    }

    // ── cancel_task ──────────────────────────────────────────────────────────
    //
    // The owner reclaims a task. Pending tasks can be cancelled immediately.
    // Claimed tasks can also be cancelled by the owner once the claimer's lock
    // period has expired (`lock_expired(&e, &task) == true`), so a keeper that
    // has started work has exclusive time to execute before escrow can be pulled.

    pub fn cancel_task(e: Env, owner: Address, task_id: u64) -> Result<(), KeeperError> {
        owner.require_auth();

        let mut task = load_task(&e, task_id)?;
        if task.owner != owner {
            return Err(KeeperError::NotTaskOwner);
        }
        match task.status {
            TaskStatus::Pending => {}
            TaskStatus::Claimed => {
                if !lock_expired(&e, &task) {
                    return Err(KeeperError::LockPeriodActive);
                }
            }
            _ => return Err(KeeperError::InvalidTaskStatus),
        }

        bump_instance(&e);
        // Effects before interaction: a re-entrant cancel must find the task
        // already Cancelled and be rejected by the status guard above.
        let refund = task.reward;
        task.status = TaskStatus::Cancelled;
        save_task(&e, task_id, &task);

        reward_token(&e)?.transfer(&e.current_contract_address(), &owner, &refund);

        emit_task_cancelled(&e, task_id, &owner);
        log!(
            &e,
            "Task {} cancelled, {} refunded to {}",
            task_id,
            refund,
            owner
        );
        Ok(())
    }

    // ── expire_task ──────────────────────────────────────────────────────────
    //
    // Permissionless deadline enforcement: once a task's deadline has passed
    // without execution, anyone may call this to return the escrow to the owner.
    // It is intentionally callable by any address (not just the owner) so a
    // stuck task can always be unwound and its funds recovered — a keeper bot
    // can even do this as a courtesy while scanning.

    pub fn expire_task(e: Env, task_id: u64) -> Result<(), KeeperError> {
        let mut task = load_task(&e, task_id)?;

        match task.status {
            TaskStatus::Pending | TaskStatus::Claimed => {}
            _ => return Err(KeeperError::InvalidTaskStatus),
        }
        if e.ledger().timestamp() < task.deadline {
            return Err(KeeperError::DeadlineNotPassed);
        }

        let refund = task.reward;
        let owner = task.owner.clone();

        // Effects before interaction: a re-entrant call for the same task_id
        // now sees status Expired and is rejected by the guard above, so the
        // refund can never be paid twice out of the contract's pooled escrow.
        bump_instance(&e);
        task.status = TaskStatus::Expired;
        save_task(&e, task_id, &task);

        reward_token(&e)?.transfer(&e.current_contract_address(), &owner, &refund);

        emit_task_expired(&e, task_id);
        log!(&e, "Task {} expired, {} refunded to owner", task_id, refund);
        Ok(())
    }

    // ── withdraw_rewards ─────────────────────────────────────────────────────
    //
    // A keeper pulls its accumulated balance. Follows checks-effects-
    // interactions: the stored balance is zeroed BEFORE the token transfer, so
    // even a malicious reward token that re-enters cannot double-spend the
    // balance. Returns the amount withdrawn.

    pub fn withdraw_rewards(e: Env, keeper: Address) -> Result<i128, KeeperError> {
        keeper.require_auth();

        let key = DataKey::KeeperReward(keeper.clone());
        let balance: i128 = e.storage().persistent().get(&key).unwrap_or(0);
        if balance <= 0 {
            return Err(KeeperError::NoRewardsAvailable);
        }

        bump_instance(&e);
        // Effects before interaction.
        e.storage().persistent().set(&key, &0i128);
        reward_token(&e)?.transfer(&e.current_contract_address(), &keeper, &balance);

        emit_rewards_withdrawn(&e, &keeper, balance);
        log!(&e, "Keeper {} withdrew {}", keeper, balance);
        Ok(balance)
    }

    // ── pause / unpause ───────────────────────────────────────────────────────
    //
    // Admin emergency circuit breaker. The rule of thumb: anything that opens
    // new exposure (new escrow, new claims, new execution payouts) is blocked;
    // anything that only lets value flow back out to whoever already owns it
    // stays open, so an incident response can never itself become a fund
    // freeze. Read-only views are never gated.
    //
    // Verified against `require_not_paused(&e)?` (or its absence) at the top
    // of each function, current as of the pause-policy-matrix test suite in
    // `test.rs` (`test_pause_policy_matrix_entry_point_by_entry_point` et al.)
    // — that test is the source of truth if this table and the code ever
    // drift apart again.
    //
    // | Entry point       | While paused | Why                                   |
    // |--------------------|-------------|----------------------------------------|
    // | `register_task`    | BLOCKED     | opens new escrow exposure              |
    // | `claim_task`       | BLOCKED     | opens new keeper exposure              |
    // | `execute_task`     | BLOCKED     | pays out new rewards                   |
    // | `increase_reward`  | BLOCKED     | opens new escrow exposure              |
    // | `extend_deadline`  | NOT gated   | **known bug**, tracked separately — see|
    // |                    | (allowed)   | TODO next to the test below. Should    |
    // |                    |             | arguably be blocked (it doesn't touch  |
    // |                    |             | funds either way, but was likely meant |
    // |                    |             | to follow register/claim/execute).     |
    // | `cancel_task`      | allowed     | owner reclaiming pending-task escrow;  |
    // |                    |             | liveness, not new exposure             |
    // | `expire_task`      | allowed     | permissionless fund recovery           |
    // | `withdraw_rewards` | allowed     | keeper pulling already-earned balance  |
    // | read-only views    | allowed     | side-effect-free, never gated          |
    //
    // `set_fee_bps`/`set_min_reward`/`transfer_admin`/`upgrade`/`sweep_fees`
    // are admin-only (`require_admin`) and were never in scope for the pause
    // gate at all — pausing doesn't restrict what the admin itself can do.

    pub fn pause(e: Env, admin: Address) -> Result<(), KeeperError> {
        require_admin(&e, &admin)?;
        bump_instance(&e);
        e.storage().instance().set(&DataKey::Paused, &true);
        emit_paused(&e, true);
        log!(&e, "Registry paused by {}", admin);
        Ok(())
    }

    pub fn unpause(e: Env, admin: Address) -> Result<(), KeeperError> {
        require_admin(&e, &admin)?;
        bump_instance(&e);
        e.storage().instance().set(&DataKey::Paused, &false);
        emit_paused(&e, false);
        log!(&e, "Registry unpaused by {}", admin);
        Ok(())
    }

    // ── set_fee_bps ───────────────────────────────────────────────────────────
    //
    // Admin adjusts the platform fee. The new rate only affects tasks executed
    // after this call; already-accrued fees are unaffected.

    pub fn set_fee_bps(e: Env, admin: Address, new_bps: u32) -> Result<(), KeeperError> {
        require_admin(&e, &admin)?;
        if new_bps > 10_000 {
            return Err(KeeperError::InvalidFeeBps);
        }
        bump_instance(&e);
        let old_bps = fee_bps(&e);
        e.storage().instance().set(&DataKey::FeeBps, &new_bps);
        emit_fee_updated(&e, old_bps, new_bps);
        log!(&e, "Fee updated to {} bps", new_bps);
        Ok(())
    }

    // ── set_min_reward ────────────────────────────────────────────────────────
    //
    // Admin sets the minimum reward a task may be registered with. Existing
    // tasks are unaffected; only future registrations are validated.

    pub fn set_min_reward(e: Env, admin: Address, min_reward: i128) -> Result<(), KeeperError> {
        require_admin(&e, &admin)?;
        if min_reward < 0 {
            return Err(KeeperError::InvalidReward);
        }
        bump_instance(&e);
        let old_min: i128 = e.storage().instance().get(&DataKey::MinReward).unwrap_or(0);
        e.storage().instance().set(&DataKey::MinReward, &min_reward);
        emit_min_reward_updated(&e, old_min, min_reward);
        log!(&e, "Min reward set to {}", min_reward);
        Ok(())
    }

    // ── transfer_admin ────────────────────────────────────────────────────────
    //
    // Hands the admin role to a new address. Both the current admin and the
    // incoming admin must authorize, so the role can never be transferred to an
    // address that has not consented to take it (no accidental lock-out).

    pub fn transfer_admin(e: Env, admin: Address, new_admin: Address) -> Result<(), KeeperError> {
        require_admin(&e, &admin)?;
        new_admin.require_auth();
        bump_instance(&e);
        e.storage().instance().set(&DataKey::Admin, &new_admin);
        emit_admin_transferred(&e, &admin, &new_admin);
        log!(&e, "Admin transferred from {} to {}", admin, new_admin);
        Ok(())
    }

    // ── upgrade ───────────────────────────────────────────────────────────────
    //
    // Admin swaps the contract WASM for a new hash (already installed on-chain).
    // Storage layout is preserved across the upgrade.

    pub fn upgrade(e: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), KeeperError> {
        require_admin(&e, &admin)?;
        bump_instance(&e);
        e.deployer().update_current_contract_wasm(new_wasm_hash);
        log!(&e, "Contract upgraded by {}", admin);
        Ok(())
    }

    // ── sweep_fees ────────────────────────────────────────────────────────────
    //
    // Admin moves up to the accrued protocol fees to a treasury address. The
    // amount is checked against the FeesAccrued accumulator, so a sweep can
    // never dip into task escrow or keeper balances.

    pub fn sweep_fees(
        e: Env,
        admin: Address,
        treasury: Address,
        amount: i128,
    ) -> Result<(), KeeperError> {
        require_admin(&e, &admin)?;

        if amount <= 0 {
            return Err(KeeperError::InvalidReward);
        }
        let accrued: i128 = e
            .storage()
            .instance()
            .get(&DataKey::FeesAccrued)
            .unwrap_or(0);
        if amount > accrued {
            return Err(KeeperError::NoRewardsAvailable);
        }

        bump_instance(&e);
        // Effects before interaction.
        e.storage()
            .instance()
            .set(&DataKey::FeesAccrued, &(accrued - amount));
        reward_token(&e)?.transfer(&e.current_contract_address(), &treasury, &amount);

        let remaining = accrued - amount;
        emit_fees_swept(&e, &treasury, amount, remaining);
        log!(&e, "Swept {} fees to {}", amount, treasury);
        Ok(())
    }

    /// Read-only: protocol fees accrued and awaiting sweep.
    pub fn fees_accrued(e: Env) -> i128 {
        e.storage()
            .instance()
            .get(&DataKey::FeesAccrued)
            .unwrap_or(0)
    }

    // ── Read-only views ───────────────────────────────────────────────────────
    //
    // Policy: views never return NotInitialized. Unlike a state-changing call,
    // a view on an uninitialized registry has an unambiguous, harmless answer
    // (zero tasks, zero balance, not paused, no admin configured) rather than
    // an operation that would silently do the wrong thing — `admin()` already
    // reflects this by returning `Option::None`, and `task_count`/`is_paused`/
    // `fees_accrued` return their natural default. Please keep this policy
    // rather than "fixing" these to error, so views stay side-effect-free and
    // safe to call speculatively (e.g. by a keeper bot probing a fresh
    // deployment before it knows whether `initialize` has run).

    pub fn get_task(e: Env, task_id: u64) -> Result<Task, KeeperError> {
        load_task(&e, task_id)
    }

    /// Reads up to [`MAX_BATCH_READ`] tasks in one call, so an indexer or
    /// keeper bot can inspect a set of tasks without one RPC round trip per
    /// task.
    ///
    /// **Missing ids.** The result is *positionally aligned* with `ids`: it has
    /// exactly `ids.len()` entries, and entry `i` is `Some(task)` if `ids[i]`
    /// exists and `None` if it does not. A single absent id therefore does not
    /// fail the whole call — a caller scanning a range does not need to know in
    /// advance which ids are live.
    ///
    /// `Vec<Option<Task>>` is used rather than a compacted `Vec<Task>` because
    /// [`Task`] does not carry its own `task_id`. Omitting missing ids from a
    /// bare `Vec<Task>` would make the mapping from result back to requested id
    /// unrecoverable — with two absent ids in a batch of ten, the caller cannot
    /// tell which eight it got. `None` is a void XDR variant, so the alignment
    /// costs almost nothing on the wire even for a sparse range.
    ///
    /// Returns [`KeeperError::BatchTooLarge`] if `ids` exceeds
    /// [`MAX_BATCH_READ`], rather than truncating: a silently clipped page is
    /// indistinguishable from the genuine end of a range.
    ///
    /// Duplicate ids are permitted and each is resolved independently; the
    /// caller pays for the repeated read.
    ///
    /// This does not violate the "no unbounded iteration" rule in the README.
    /// That rule is about *storage* — the contract keeps no growing
    /// `Vec<task_id>` that some operation must walk. Every read here is still
    /// O(1) by key against `DataKey::Task(id)`; the caller supplies the keys
    /// and the count is bounded by a constant.
    pub fn get_tasks(e: Env, ids: Vec<u64>) -> Result<Vec<Option<Task>>, KeeperError> {
        if ids.len() > MAX_BATCH_READ {
            return Err(KeeperError::BatchTooLarge);
        }

        let mut out = Vec::new(&e);
        for id in ids.iter() {
            out.push_back(load_task(&e, id).ok());
        }
        Ok(out)
    }

    /// Reads the `count` tasks with ids `from, from + 1, …, from + count - 1`.
    ///
    /// The convenience form of [`KeeperRegistry::get_tasks`] for the common
    /// "scan recent tasks" case — a bot walking backwards from
    /// [`KeeperRegistry::task_count`] does not have to build a `Vec<u64>` just
    /// to describe a contiguous range. Same missing-id policy: the result has
    /// exactly `count` entries, positionally aligned with the range, and ids
    /// that were never allocated or have been archived come back as `None`.
    ///
    /// `count == 0` returns an empty vector. `count` above [`MAX_BATCH_READ`]
    /// returns [`KeeperError::BatchTooLarge`], and a range whose end would
    /// exceed `u64::MAX` returns [`KeeperError::ArithmeticOverflow`] rather
    /// than wrapping around to low ids.
    pub fn get_tasks_range(
        e: Env,
        from: u64,
        count: u32,
    ) -> Result<Vec<Option<Task>>, KeeperError> {
        if count > MAX_BATCH_READ {
            return Err(KeeperError::BatchTooLarge);
        }

        // Reject a wrapping range up front rather than letting `from + i`
        // overflow mid-loop and silently return unrelated low-numbered tasks.
        //
        // The bound checked is the LAST id actually read (`from + count - 1`),
        // not the exclusive end (`from + count`): a window ending exactly on
        // `u64::MAX` is perfectly readable, and checking the exclusive end
        // would reject it for an overflow that never happens.
        if count > 0 {
            from.checked_add(count as u64 - 1)
                .ok_or(KeeperError::ArithmeticOverflow)?;
        }

        let mut out = Vec::new(&e);
        for i in 0..count as u64 {
            out.push_back(load_task(&e, from + i).ok());
        }
        Ok(out)
    }

    pub fn task_count(e: Env) -> u64 {
        e.storage()
            .instance()
            .get(&DataKey::TaskCounter)
            .unwrap_or(0u64)
    }

    /// Read-only: a keeper's withdrawable balance.
    ///
    /// TTL note: `KeeperReward(addr)` is only renewed when the balance is
    /// written — credited in `execute_task`/`credit_keeper`, or zeroed in
    /// `withdraw_rewards`. A keeper that executes exactly one task and never
    /// interacts with the contract again has a balance entry whose TTL is
    /// never touched afterward, so it *can* be archived like any other
    /// persistent entry. This view does not renew it on read: views are
    /// simulated by clients for free and must stay side-effect-free (same
    /// choice as instance storage, see `bump_instance`). An archived balance
    /// entry must be restored (RestoreFootprint) before it can be read or
    /// withdrawn again — the entry is not lost, just inaccessible until
    /// restored, and its value is preserved.
    pub fn keeper_balance(e: Env, keeper: Address) -> i128 {
        e.storage()
            .persistent()
            .get(&DataKey::KeeperReward(keeper))
            .unwrap_or(0i128)
    }

    pub fn admin(e: Env) -> Option<Address> {
        e.storage().instance().get(&DataKey::Admin)
    }

    pub fn get_fee_bps(e: Env) -> u32 {
        fee_bps(&e)
    }

    pub fn is_paused(e: Env) -> bool {
        e.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    pub fn reward_token_address(e: Env) -> Option<Address> {
        e.storage().instance().get(&DataKey::RewardToken)
    }

    /// True if the task can be claimed right now: it exists, its deadline has
    /// not passed, and it is either Pending or a Claimed task whose lock window
    /// has elapsed. Lets keeper bots pre-filter candidates without simulating a
    /// full claim_task call.
    pub fn is_claimable(e: Env, task_id: u64) -> bool {
        match load_task(&e, task_id) {
            Ok(task) => {
                if e.ledger().timestamp() >= task.deadline {
                    return false;
                }
                match task.status {
                    TaskStatus::Pending => true,
                    TaskStatus::Claimed => lock_expired(&e, &task),
                    _ => false,
                }
            }
            Err(_) => false,
        }
    }

    /// Minimum reward required to register a task (0 if unset).
    pub fn min_reward(e: Env) -> i128 {
        e.storage().instance().get(&DataKey::MinReward).unwrap_or(0)
    }

    /// Contract logic version. See [`VERSION`].
    pub fn version(_e: Env) -> u32 {
        VERSION
    }
}

#[cfg(any(test, fuzzing))]
pub mod invariants;

#[cfg(test)]
mod test;
