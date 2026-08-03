//! CPU-instruction regression ceilings.

use soroban_sdk::{testutils::Address as _, Address, Bytes};

use super::common::*;

// ─────────────────────────────────────────────────────────────────────────────
// CPU-instruction regression ceilings — issue 0107. `claim_task` and
// `execute_task` are the two entry points most likely to be called under real
// load (every keeper bot calls them once per task), so a silent cost
// regression there is the one most likely to surprise a keeper's transaction
// budget in production.
//
// Measured via `env.cost_estimate().budget().cpu_instruction_cost()` (the
// same budget-tracking API issue 0100's CI job uses) at the time these tests
// were written: `claim_task` costs ~100,555 instructions, `execute_task`
// costs ~158,338. The ceilings below are set at roughly 3x each measured
// value — loose enough that an ordinary change (one extra storage read, a
// slightly bigger event) won't trip it, but tight enough to catch an
// accidental order-of-magnitude regression, such as a refactor that starts
// calling `bump_instance` twice by mistake, or a verifier integration that
// reruns the whole load/save path per call. (Confirmed these have teeth: a
// temporary ceiling of 1 during development made both fail with the exact
// measured instruction count in the message, not an opaque error.)
const CLAIM_TASK_CPU_INSN_CEILING: u64 = 350_000;
const EXECUTE_TASK_CPU_INSN_CEILING: u64 = 500_000;

#[test]
fn test_claim_task_cpu_instructions_within_ceiling() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);

    s.env.cost_estimate().budget().reset_default();
    s.registry.claim_task(&keeper, &id);
    let consumed = s.env.cost_estimate().budget().cpu_instruction_cost();

    assert!(
        consumed < CLAIM_TASK_CPU_INSN_CEILING,
        "claim_task consumed {consumed} CPU instructions, exceeding the regression \
         ceiling of {CLAIM_TASK_CPU_INSN_CEILING}"
    );
}

#[test]
fn test_execute_task_cpu_instructions_within_ceiling() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    let id = register_default_task(&s);
    s.registry.claim_task(&keeper, &id);

    s.env.cost_estimate().budget().reset_default();
    s.registry
        .execute_task(&keeper, &id, &Bytes::from_slice(&s.env, b"proof"));
    let consumed = s.env.cost_estimate().budget().cpu_instruction_cost();

    assert!(
        consumed < EXECUTE_TASK_CPU_INSN_CEILING,
        "execute_task consumed {consumed} CPU instructions, exceeding the regression \
         ceiling of {EXECUTE_TASK_CPU_INSN_CEILING}"
    );
}
