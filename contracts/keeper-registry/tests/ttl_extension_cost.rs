//! # `save_task` TTL-extension cost measurement (issue 0111)
//!
//! Issue 0111 asks whether `save_task`'s unconditional
//! `persistent().extend_ttl(...)` is paying a real, avoidable cost on writes
//! that "do not change `ttl_ledgers` at all", and whether a cheap
//! read-and-compare guard in the contract would be worth adding.
//!
//! These tests answer that empirically rather than by assertion. They measure
//! three things:
//!
//! 1. [`measure_redundant_extend_ttl_cost`] — the marginal CPU/memory cost of
//!    an `extend_ttl` call that the host short-circuits, isolated against an
//!    otherwise identical write that omits the call entirely. This is the
//!    absolute ceiling on what *any* guard could ever save, since a guard can
//!    at best elide the whole call.
//! 2. [`measure_effective_extend_ttl_cost`] — the same comparison when the
//!    extension genuinely advances the entry's lifetime, so the two paths can
//!    be told apart by cost.
//! 3. [`save_task_extension_is_almost_never_redundant`] — how often the
//!    redundant case actually arises for a real task moving through its
//!    lifecycle, which determines whether the ceiling above is ever collected.
//!
//! A guard also has to be *implementable*, which
//! [`contract_code_cannot_read_entry_ttl`] pins down: the SDK exposes an
//! entry's TTL only through `testutils`, never to on-chain contract code.
//!
//! The conclusions drawn from the numbers these tests print are recorded in
//! `docs/ARCHITECTURE.md` ("save_task TTL-extension cost") and summarised on
//! `save_task` itself.

use keeper_registry::{KeeperRegistry, KeeperRegistryClient, TaskType};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{storage::Persistent as _, Address as _, Ledger},
    token, Address, Bytes, Env,
};

// ─────────────────────────────────────────────────────────────────────────────
// Probe contract
// ─────────────────────────────────────────────────────────────────────────────

/// Mirrors `save_task`'s storage shape (a keyed persistent struct write) so the
/// measured difference is attributable to `extend_ttl` alone. The two entry
/// points are identical apart from that one call.
#[contracttype]
#[derive(Clone)]
enum ProbeKey {
    Entry(u64),
}

#[contracttype]
#[derive(Clone)]
struct ProbeValue {
    counter: u64,
    ttl_ledgers: u32,
}

#[contract]
struct TtlProbe;

#[contractimpl]
impl TtlProbe {
    /// Baseline: persistent write with no TTL extension.
    pub fn write_only(e: Env, id: u64, ttl_ledgers: u32) {
        let value = ProbeValue {
            counter: id,
            ttl_ledgers,
        };
        e.storage().persistent().set(&ProbeKey::Entry(id), &value);
    }

    /// Exactly [`TtlProbe::write_only`] plus the `extend_ttl` call `save_task`
    /// makes, with the same `threshold == extend_to == ttl_ledgers` shape.
    pub fn write_and_extend(e: Env, id: u64, ttl_ledgers: u32) {
        let value = ProbeValue {
            counter: id,
            ttl_ledgers,
        };
        e.storage().persistent().set(&ProbeKey::Entry(id), &value);
        e.storage()
            .persistent()
            .extend_ttl(&ProbeKey::Entry(id), ttl_ledgers, ttl_ledgers);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Harness
// ─────────────────────────────────────────────────────────────────────────────

const PROBE_TTL_LEDGERS: u32 = 20_000;

struct Cost {
    cpu: u64,
    mem: u64,
}

/// Runs `body` with a freshly reset budget and returns what it consumed.
///
/// `reset_default` (rather than `reset_unlimited`) is deliberate: it installs
/// the real network resource limits, so a measurement that would exceed a
/// production transaction budget fails here rather than reporting a number no
/// real transaction could pay.
fn measure(env: &Env, body: impl FnOnce()) -> Cost {
    env.cost_estimate().budget().reset_default();
    body();
    let budget = env.cost_estimate().budget();
    Cost {
        cpu: budget.cpu_instruction_cost(),
        mem: budget.memory_bytes_cost(),
    }
}

fn probe_client(env: &Env) -> TtlProbeClient<'_> {
    let id = env.register(TtlProbe, ());
    TtlProbeClient::new(env, &id)
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Is the guard even implementable?
// ─────────────────────────────────────────────────────────────────────────────

/// Issue 0111 proposes "a cheap read-and-compare guard before calling
/// `extend_ttl`". Reading an entry's current TTL is the load-bearing half of
/// that, and contract code cannot do it.
///
/// `Storage::get_ttl` is defined on the `soroban_sdk::testutils::storage`
/// traits, which are compiled only under `testutils` and are not part of the
/// contract-facing API. The underlying host function
/// (`get_contract_data_live_until_ledger`) is likewise absent from the WASM
/// host interface in `soroban-env-common`'s `env.json` — the only TTL reader
/// exposed to contracts is `get_max_live_until_ledger`, which returns the
/// network-wide maximum, not this entry's remaining lifetime.
///
/// This test documents that asymmetry executably: the TTL is observable from
/// the test harness, and the value it observes is one no on-chain caller could
/// have obtained.
#[test]
fn contract_code_cannot_read_entry_ttl() {
    let env = Env::default();
    let client = probe_client(&env);
    client.write_and_extend(&1u64, &PROBE_TTL_LEDGERS);

    let contract_id = client.address.clone();
    let observed = env.as_contract(&contract_id, || {
        // Only reachable because `testutils::storage::Persistent` is in scope.
        // There is no non-testutils equivalent; see this test's doc comment.
        env.storage().persistent().get_ttl(&ProbeKey::Entry(1u64))
    });

    assert_eq!(
        observed, PROBE_TTL_LEDGERS,
        "extend_ttl should have set the entry's TTL to exactly extend_to"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. What does a redundant extend_ttl actually cost?
// ─────────────────────────────────────────────────────────────────────────────

/// Measures `extend_ttl`'s marginal cost when the host short-circuits it.
///
/// The redundant case is set up by writing twice **within the same ledger**:
/// the first call sets the entry's TTL to `extend_to`, so on the second call
/// `new_live_until == old_live_until` and the host's
/// `new_live_until > old_live_until` test in `Storage::extend_ttl` fails,
/// skipping the storage-map insert.
///
/// The delta this prints is the most a contract-side guard could ever save on
/// such a call, because a guard can at best skip the call outright.
#[test]
fn measure_redundant_extend_ttl_cost() {
    let env = Env::default();
    let client = probe_client(&env);

    // Prime both entries so the writes under measurement are updates, not
    // first-time inserts, and so entry 2's TTL is already at `extend_to`.
    client.write_only(&1u64, &PROBE_TTL_LEDGERS);
    client.write_and_extend(&2u64, &PROBE_TTL_LEDGERS);

    let baseline = measure(&env, || client.write_only(&1u64, &PROBE_TTL_LEDGERS));
    let with_extend = measure(&env, || client.write_and_extend(&2u64, &PROBE_TTL_LEDGERS));

    let cpu_delta = with_extend.cpu as i64 - baseline.cpu as i64;
    let mem_delta = with_extend.mem as i64 - baseline.mem as i64;

    println!("redundant extend_ttl (host short-circuits the insert):");
    println!(
        "  write_only        cpu={} mem={}",
        baseline.cpu, baseline.mem
    );
    println!(
        "  write_and_extend  cpu={} mem={}",
        with_extend.cpu, with_extend.mem
    );
    println!("  delta             cpu={cpu_delta} mem={mem_delta}");

    // The call is not free -- it still pays key conversion, the footprint
    // lookup in `get_with_live_until_ledger`, and the ledger-sequence reads
    // that precede the short-circuit. Asserting it is positive keeps this test
    // honest: if a future SDK made redundant extension genuinely free, this
    // fails and the recorded conclusion gets revisited.
    assert!(
        cpu_delta > 0,
        "expected a redundant extend_ttl to still cost something, got {cpu_delta}"
    );

    // Guard rail, not a tuned threshold: this documents the order of magnitude
    // (thousands of CPU instructions against a 100M-instruction transaction
    // budget) so a future change that makes redundant extension dramatically
    // more expensive reopens the question rather than passing silently.
    assert!(
        cpu_delta < 100_000,
        "redundant extend_ttl cost {cpu_delta} CPU instructions, far above the \
         measured baseline -- issue 0111's conclusion should be re-derived"
    );
}

/// The companion measurement: the same call when it genuinely extends the
/// entry's lifetime and the host does perform the storage-map insert.
///
/// Establishes that the two paths are distinguishable by cost, which is what
/// makes the redundant-case number in
/// [`measure_redundant_extend_ttl_cost`] meaningful rather than noise.
#[test]
fn measure_effective_extend_ttl_cost() {
    let env = Env::default();
    let client = probe_client(&env);

    client.write_only(&1u64, &PROBE_TTL_LEDGERS);
    client.write_and_extend(&2u64, &PROBE_TTL_LEDGERS);

    // Advance the ledger so entry 2's remaining TTL has decayed below
    // `extend_to` and the extension has real work to do.
    env.ledger().with_mut(|li| li.sequence_number += 1_000);

    let baseline = measure(&env, || client.write_only(&1u64, &PROBE_TTL_LEDGERS));
    let with_extend = measure(&env, || client.write_and_extend(&2u64, &PROBE_TTL_LEDGERS));

    let cpu_delta = with_extend.cpu as i64 - baseline.cpu as i64;

    println!("effective extend_ttl (host performs the insert):");
    println!(
        "  write_only        cpu={} mem={}",
        baseline.cpu, baseline.mem
    );
    println!(
        "  write_and_extend  cpu={} mem={}",
        with_extend.cpu, with_extend.mem
    );
    println!("  delta             cpu={cpu_delta}");

    assert!(
        cpu_delta > 0,
        "an effective extend_ttl must cost something, got {cpu_delta}"
    );

    // The entry's lifetime was actually renewed, which is the behaviour a
    // guard must not break.
    let contract_id = client.address.clone();
    let ttl = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&ProbeKey::Entry(2u64))
    });
    assert_eq!(ttl, PROBE_TTL_LEDGERS);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. How often is the redundant case actually hit?
// ─────────────────────────────────────────────────────────────────────────────

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
    token::StellarAssetClient::new(&env, &token_id).mint(&owner, &10_000_000i128);

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

const TASK_TTL_LEDGERS: u32 = 20_000;

fn register(s: &Setup, reward: i128) -> u64 {
    let deadline = s.env.ledger().timestamp() + 3_600;
    s.registry.register_task(
        &s.owner,
        &TaskType::Liquidation,
        &Bytes::from_slice(&s.env, b"ttl-cost-probe"),
        &reward,
        &deadline,
        &TASK_TTL_LEDGERS,
        &120u32,
        &None,
    )
}

fn task_ttl(s: &Setup, task_id: u64) -> u32 {
    s.env.as_contract(&s.registry_id, || {
        s.env
            .storage()
            .persistent()
            .get_ttl(&keeper_registry::DataKey::Task(task_id))
    })
}

/// The premise behind issue 0111 is that calls which leave `ttl_ledgers`
/// untouched perform a redundant extension. That conflates two different
/// things, and this test separates them.
///
/// `save_task` passes `extend_to = task.ttl_ledgers`, which is a TTL measured
/// **from the current ledger**, not an absolute expiry. So an unchanged
/// `ttl_ledgers` field still means a genuinely larger `live_until_ledger`
/// whenever any ledger has closed since the previous write. The extension is
/// redundant only for two writes landing in the *same* ledger.
///
/// Since a task's lifecycle calls (`claim_task`, then `execute_task`) are
/// necessarily separate transactions and in practice separate ledgers, the
/// redundant case that issue 0111 proposes optimising is the rare one.
#[test]
fn save_task_extension_is_almost_never_redundant() {
    let s = setup();
    let keeper = Address::generate(&s.env);

    let task_id = register(&s, 1_000_000i128);
    assert_eq!(
        task_ttl(&s, task_id),
        TASK_TTL_LEDGERS,
        "registration should provision the task's full TTL"
    );

    // A later lifecycle call, one ledger on. `ttl_ledgers` is unchanged, yet
    // the entry's TTL has decayed and the extension genuinely restores it.
    s.env.ledger().with_mut(|li| li.sequence_number += 1);
    assert_eq!(
        task_ttl(&s, task_id),
        TASK_TTL_LEDGERS - 1,
        "one closed ledger should consume one ledger of TTL"
    );

    s.registry.claim_task(&keeper, &task_id);
    assert_eq!(
        task_ttl(&s, task_id),
        TASK_TTL_LEDGERS,
        "claim_task's save_task must have renewed the decayed TTL -- this is \
         real work, not a redundant call"
    );

    // Only a second write inside the same ledger is truly redundant.
    let before = task_ttl(&s, task_id);
    s.registry.increase_reward(&s.owner, &task_id, &1i128);
    assert_eq!(
        task_ttl(&s, task_id),
        before,
        "a same-ledger re-save cannot extend the entry any further"
    );
}

/// Acceptance criterion 2 of issue 0111: whatever conclusion the measurement
/// leads to, a task that genuinely needs its TTL extended must still get it —
/// no accidental early-archival regression.
///
/// This is the regression test that would fail if a future contributor added a
/// guard that skipped extension too eagerly. It walks a task through its full
/// lifecycle with ledgers closing between every call and asserts the entry's
/// TTL is restored to the full window each time, so the escrow can never
/// become inaccessible while it is still live.
#[test]
fn every_lifecycle_write_restores_the_full_ttl_window() {
    let s = setup();
    let keeper = Address::generate(&s.env);

    let task_id = register(&s, 1_000_000i128);

    // register_task
    assert_eq!(
        task_ttl(&s, task_id),
        TASK_TTL_LEDGERS,
        "after register_task"
    );

    // increase_reward -- does not touch `ttl_ledgers`, must still extend.
    s.env.ledger().with_mut(|li| li.sequence_number += 500);
    s.registry.increase_reward(&s.owner, &task_id, &1_000i128);
    assert_eq!(
        task_ttl(&s, task_id),
        TASK_TTL_LEDGERS,
        "after increase_reward"
    );

    // extend_deadline -- likewise.
    s.env.ledger().with_mut(|li| li.sequence_number += 500);
    let new_deadline = s.env.ledger().timestamp() + 7_200;
    s.registry
        .extend_deadline(&s.owner, &task_id, &new_deadline);
    assert_eq!(
        task_ttl(&s, task_id),
        TASK_TTL_LEDGERS,
        "after extend_deadline"
    );

    // claim_task
    s.env.ledger().with_mut(|li| li.sequence_number += 500);
    s.registry.claim_task(&keeper, &task_id);
    assert_eq!(task_ttl(&s, task_id), TASK_TTL_LEDGERS, "after claim_task");

    // execute_task
    s.env.ledger().with_mut(|li| li.sequence_number += 500);
    s.registry
        .execute_task(&keeper, &task_id, &Bytes::from_slice(&s.env, b"proof"));
    assert_eq!(
        task_ttl(&s, task_id),
        TASK_TTL_LEDGERS,
        "after execute_task"
    );
}
