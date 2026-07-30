//! # `INSTANCE_BUMP_*` constant evaluation (issue 0112)
//!
//! Wave 1's instance-TTL fix (issue 0015) picked `INSTANCE_BUMP_THRESHOLD` and
//! `INSTANCE_BUMP_LEDGERS` as round numbers justified by rough ledger-time
//! math. Issue 0112 asks for those values to be re-derived against real
//! traffic, or — failing that — against an explicit, stated assumption, and for
//! the conclusion to be recorded so nobody has to redo the analysis.
//!
//! These tests pin down the behavioural facts the conclusion rests on:
//!
//! - [`bump_is_a_no_op_until_the_threshold_is_crossed`] — the renewal really is
//!   free on the common path, which is the whole point of having a threshold
//!   below `INSTANCE_BUMP_LEDGERS`.
//! - [`bump_renews_once_the_threshold_is_crossed`] — and it really does fire in
//!   the danger band, so the instance cannot silently archive under traffic.
//! - [`measure_instance_bump_cost`] — the before/after resource comparison
//!   issue 0112 asks for, in the same discipline as issue 0111.
//! - [`idle_registry_archives_after_the_full_window`] — the accepted tradeoff,
//!   made executable: a registry with no mutating traffic for the full window
//!   does lapse, and that is by design.
//!
//! The traffic assumption, the observed testnet data, and the conclusion drawn
//! are recorded in `docs/ARCHITECTURE.md` ("Instance TTL and traffic
//! assumptions") and on the constants themselves in `lib.rs`.

use keeper_registry::{KeeperRegistry, KeeperRegistryClient, TaskType};
use soroban_sdk::{
    testutils::{storage::Instance as _, Address as _, Ledger},
    token, Address, Bytes, Env,
};

/// Mirrors `INSTANCE_BUMP_LEDGERS` in `lib.rs`. Both constants are private to
/// the contract crate, so they are restated here; the tests below fail loudly
/// if the contract's values are changed without updating these.
const INSTANCE_BUMP_LEDGERS: u32 = 100_000;
/// Mirrors `INSTANCE_BUMP_THRESHOLD` in `lib.rs`.
const INSTANCE_BUMP_THRESHOLD: u32 = 50_000;

struct Setup {
    env: Env,
    owner: Address,
    registry_id: Address,
    registry: KeeperRegistryClient<'static>,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    token::StellarAssetClient::new(&env, &token_id).mint(&owner, &100_000_000i128);

    let registry_id = env.register(KeeperRegistry, ());
    let registry = KeeperRegistryClient::new(&env, &registry_id);
    registry.initialize(&owner, &token_id, &300u32);

    let env = unsafe { core::mem::transmute::<Env, Env>(env) };
    Setup {
        env,
        owner,
        registry_id,
        registry: unsafe { core::mem::transmute(registry) },
    }
}

fn instance_ttl(s: &Setup) -> u32 {
    s.env
        .as_contract(&s.registry_id, || s.env.storage().instance().get_ttl())
}

/// Any cheap state-mutating admin call; every such call routes through
/// `bump_instance`.
fn mutate(s: &Setup) {
    s.registry.set_fee_bps(&s.owner, &300u32);
}

fn advance(s: &Setup, ledgers: u32) {
    s.env.ledger().with_mut(|li| li.sequence_number += ledgers);
}

// ─────────────────────────────────────────────────────────────────────────────
// Threshold behaviour
// ─────────────────────────────────────────────────────────────────────────────

/// The threshold's purpose: while more than `INSTANCE_BUMP_THRESHOLD` ledgers
/// of lifetime remain, a mutating call must NOT renew the entry, so the
/// overwhelming majority of transactions pay nothing for instance liveness.
///
/// This is the property that makes the choice of threshold a cost question at
/// all — with `threshold == INSTANCE_BUMP_LEDGERS` every single call would
/// perform a storage write.
#[test]
fn bump_is_a_no_op_until_the_threshold_is_crossed() {
    let s = setup();

    mutate(&s);
    assert_eq!(
        instance_ttl(&s),
        INSTANCE_BUMP_LEDGERS,
        "a mutating call on a fresh registry should provision the full window"
    );

    // Land just inside the safe band: one ledger more than the threshold
    // remains, so renewal must not fire.
    advance(&s, INSTANCE_BUMP_LEDGERS - INSTANCE_BUMP_THRESHOLD - 1);
    let before = instance_ttl(&s);
    assert_eq!(before, INSTANCE_BUMP_THRESHOLD + 1);

    mutate(&s);
    assert_eq!(
        instance_ttl(&s),
        before,
        "with more than THRESHOLD ledgers left, bump_instance must be a no-op"
    );
}

/// The other half: once remaining lifetime falls to the threshold, the next
/// mutating call must restore the full window. If this stopped holding, an
/// actively-used registry could archive.
#[test]
fn bump_renews_once_the_threshold_is_crossed() {
    let s = setup();

    mutate(&s);
    assert_eq!(instance_ttl(&s), INSTANCE_BUMP_LEDGERS);

    // Land exactly on the threshold -- the host's renewal condition is
    // `remaining <= threshold`, so this is the first ledger at which a call
    // renews.
    advance(&s, INSTANCE_BUMP_LEDGERS - INSTANCE_BUMP_THRESHOLD);
    assert_eq!(instance_ttl(&s), INSTANCE_BUMP_THRESHOLD);

    mutate(&s);
    assert_eq!(
        instance_ttl(&s),
        INSTANCE_BUMP_LEDGERS,
        "at the threshold, a mutating call must restore the full window"
    );

    // Deep into the danger band it must also still renew.
    advance(&s, INSTANCE_BUMP_LEDGERS - 1);
    assert_eq!(instance_ttl(&s), 1);
    mutate(&s);
    assert_eq!(
        instance_ttl(&s),
        INSTANCE_BUMP_LEDGERS,
        "a call in the last ledger before archival must still rescue the entry"
    );
}

/// The accepted tradeoff from `bump_instance`'s doc comment, made executable:
/// instance liveness is maintained purely by write traffic, so a registry that
/// goes completely idle for the full window does lapse. Read-only views are
/// deliberately not allowed to renew it.
///
/// This is what fixes the traffic assumption's shape: the constants must be
/// chosen so that the quietest registry worth keeping alive still produces one
/// mutating call per `INSTANCE_BUMP_LEDGERS`.
#[test]
fn idle_registry_archives_after_the_full_window() {
    let s = setup();

    mutate(&s);
    assert_eq!(instance_ttl(&s), INSTANCE_BUMP_LEDGERS);

    // Read-only views must not renew: they are simulated by clients for free
    // and have to stay side-effect-free.
    advance(&s, INSTANCE_BUMP_LEDGERS / 2);
    let before = instance_ttl(&s);
    let _ = s.registry.task_count();
    let _ = s.registry.is_paused();
    let _ = s.registry.get_fee_bps();
    assert_eq!(
        instance_ttl(&s),
        before,
        "read-only views must never extend instance TTL"
    );

    // Ride out the rest of the window with no mutating call.
    advance(&s, INSTANCE_BUMP_LEDGERS / 2);
    assert_eq!(
        instance_ttl(&s),
        0,
        "with zero mutating traffic the instance reaches the end of its window"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Cost comparison (issue 0112 acceptance criterion 2)
// ─────────────────────────────────────────────────────────────────────────────

/// Measures what the threshold actually buys, by comparing the same entry
/// point in both states: once inside the safe band (renewal short-circuited)
/// and once inside the danger band (renewal performed).
///
/// The delta is the per-call cost the threshold avoids. Multiplied by how
/// often each state occurs, it is the entire economic case for the current
/// values.
#[test]
fn measure_instance_bump_cost() {
    // Safe band: renewal is a no-op.
    let s = setup();
    mutate(&s);
    advance(&s, 1);
    s.env.cost_estimate().budget().reset_default();
    mutate(&s);
    let noop_cpu = s.env.cost_estimate().budget().cpu_instruction_cost();
    let noop_mem = s.env.cost_estimate().budget().memory_bytes_cost();

    // Danger band: the same call, now performing the renewal.
    let s2 = setup();
    mutate(&s2);
    advance(&s2, INSTANCE_BUMP_LEDGERS - INSTANCE_BUMP_THRESHOLD);
    s2.env.cost_estimate().budget().reset_default();
    mutate(&s2);
    let renew_cpu = s2.env.cost_estimate().budget().cpu_instruction_cost();
    let renew_mem = s2.env.cost_estimate().budget().memory_bytes_cost();

    let cpu_delta = renew_cpu as i64 - noop_cpu as i64;
    let mem_delta = renew_mem as i64 - noop_mem as i64;

    println!("set_fee_bps with bump_instance:");
    println!("  renewal short-circuited  cpu={noop_cpu} mem={noop_mem}");
    println!("  renewal performed        cpu={renew_cpu} mem={renew_mem}");
    println!("  delta                    cpu={cpu_delta} mem={mem_delta}");

    assert!(
        cpu_delta > 0,
        "performing the renewal must cost more than short-circuiting it, got {cpu_delta}"
    );

    // Order-of-magnitude guard rail, not a tuned threshold. If renewal ever
    // becomes expensive enough to matter against a 100M-instruction budget,
    // the threshold trade-off deserves re-deriving rather than silently
    // passing.
    assert!(
        cpu_delta < 1_000_000,
        "instance renewal cost {cpu_delta} CPU instructions -- issue 0112's \
         conclusion should be re-derived"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The window in task-lifecycle terms
// ─────────────────────────────────────────────────────────────────────────────

/// Grounds the traffic assumption in something the contract itself guarantees.
///
/// A registry holding any live task cannot stay silent indefinitely:
/// `expire_task` is permissionless and mutating, and becomes callable the
/// moment the task's deadline passes. Whoever calls it — the owner recovering
/// escrow, or a keeper bot doing it as a courtesy while scanning — renews the
/// instance as a side effect.
///
/// The task here is deliberately given a long deadline and a correspondingly
/// long `ttl_ledgers`, so that expiry lands inside the renewal band rather than
/// in the safe band where the bump would be a no-op. That is the case worth
/// asserting: when instance liveness actually depends on a call arriving, an
/// ordinary lifecycle call is enough.
///
/// So the only registry that can archive is one with no open tasks at all —
/// the idle case [`idle_registry_archives_after_the_full_window`] covers, which
/// by construction has no escrow left to strand.
#[test]
fn a_registry_with_live_tasks_generates_renewing_traffic() {
    let s = setup();

    // Deadline far enough out that expiry falls past the renewal threshold.
    // `required_ttl_ledgers` demands (deadline - now) / 5 + 17_280 ledgers of
    // coverage, so a 300_000s deadline needs 60_000 + 17_280 = 77_280; 80_000
    // clears that and keeps the task entry alive across the advance below.
    let deadline_secs = 300_000u64;
    let deadline = s.env.ledger().timestamp() + deadline_secs;
    let task_id = s.registry.register_task(
        &s.owner,
        &TaskType::Liquidation,
        &Bytes::from_slice(&s.env, b"instance-ttl-probe"),
        &1_000_000i128,
        &deadline,
        &80_000u32,
        &120u32,
        &None,
    );
    assert_eq!(instance_ttl(&s), INSTANCE_BUMP_LEDGERS);

    // Move into the renewal band, and past the task's deadline so expiry is
    // legal. Timestamp and ledger sequence advance together at the ~5s/ledger
    // rate the contract assumes.
    advance(&s, INSTANCE_BUMP_LEDGERS - INSTANCE_BUMP_THRESHOLD);
    s.env
        .ledger()
        .with_mut(|li| li.timestamp += deadline_secs + 1);
    assert_eq!(instance_ttl(&s), INSTANCE_BUMP_THRESHOLD);

    s.registry.expire_task(&task_id);
    assert_eq!(
        instance_ttl(&s),
        INSTANCE_BUMP_LEDGERS,
        "permissionless expiry of a live task renews the instance"
    );
}
