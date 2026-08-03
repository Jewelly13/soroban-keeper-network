//! `withdraw_rewards` and `sweep_fees`.

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events as _},
    token, Address, Bytes, Symbol, TryIntoVal,
};

use super::common::*;
use crate::{split_reward, KeeperError, TaskStatus};

// ─────────────────────────────────────────────────────────────────────────────
// withdraw_rewards / sweep_fees
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_withdraw_transfers_balance_and_zeroes_it() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let keeper = executed_task_keeper(&s); // credited 970_000

    assert_eq!(token.balance(&keeper), 0i128);
    let withdrawn = s.registry.withdraw_rewards(&keeper);

    assert_eq!(withdrawn, 970_000i128);
    assert_eq!(token.balance(&keeper), 970_000i128);
    assert_eq!(s.registry.keeper_balance(&keeper), 0i128);
}

/// The design credits keepers to an internal balance so they can execute
/// many tasks and pay one withdrawal fee. `test_withdraw_transfers_balance_and_zeroes_it`
/// only proves this for a single credit; this test drives multiple credits
/// per keeper (three for keeper1, two for keeper2, interleaved) and checks
/// the running balance after every single one — a regression that overwrote
/// instead of accumulated (`set` instead of `checked_add`) would fail on the
/// very first assertion after a second credit.
#[test]
fn test_keeper_balance_accumulates_across_tasks_and_withdraws_as_one_sum() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let fee_bps = s.registry.get_fee_bps();

    let keeper1 = Address::generate(&s.env);
    let keeper2 = Address::generate(&s.env);

    // Rewards deliberately chosen so `reward * fee_bps / 10_000` does not
    // divide evenly (default fee_bps = 300 / 3%) — the running sum has to
    // exercise split_reward's truncating division, not just clean multiples.
    let keeper1_rewards = [850_003i128, 623_777i128, 1_234_567i128];
    let keeper2_rewards = [111_113i128, 777_779i128];

    let mut keeper1_balance = 0i128;
    let mut keeper2_balance = 0i128;
    let mut expected_fees = 0i128;

    // Interleave keeper2's tasks between keeper1's so that DataKey::KeeperReward
    // keys colliding between the two addresses would show up as
    // cross-contamination of the running balances, not be hidden by ordering.
    for (i, &reward1) in keeper1_rewards.iter().enumerate() {
        let id1 = register_reward_task(&s, reward1);
        s.registry.claim_task(&keeper1, &id1);
        s.registry
            .execute_task(&keeper1, &id1, &Bytes::from_slice(&s.env, b"proof"));

        let (net1, fee1) = split_reward(reward1, fee_bps).unwrap();
        keeper1_balance += net1;
        expected_fees += fee1;

        // Asserting after each step localises a failure to the exact
        // execution that broke accumulation.
        assert_eq!(s.registry.keeper_balance(&keeper1), keeper1_balance);
        assert_eq!(s.registry.keeper_balance(&keeper2), keeper2_balance);

        if let Some(&reward2) = keeper2_rewards.get(i) {
            let id2 = register_reward_task(&s, reward2);
            s.registry.claim_task(&keeper2, &id2);
            s.registry
                .execute_task(&keeper2, &id2, &Bytes::from_slice(&s.env, b"proof"));

            let (net2, fee2) = split_reward(reward2, fee_bps).unwrap();
            keeper2_balance += net2;
            expected_fees += fee2;

            assert_eq!(s.registry.keeper_balance(&keeper2), keeper2_balance);
            assert_eq!(s.registry.keeper_balance(&keeper1), keeper1_balance);
        }
    }

    // Sanity: the chosen rewards actually produced non-round fee splits.
    assert!(keeper1_rewards
        .iter()
        .any(|&r| r.checked_mul(fee_bps as i128).unwrap() % 10_000 != 0));

    assert_eq!(s.registry.fees_accrued(), expected_fees);

    // A single withdrawal transfers the full accumulated sum and zeroes the
    // balance.
    assert_eq!(token.balance(&keeper1), 0i128);
    let withdrawn = s.registry.withdraw_rewards(&keeper1);

    // Exactly one RewardsWithdrawn event was emitted, carrying the total —
    // not one per credited task, and not the token contract's own transfer
    // event (which carries a different topic pair).
    let mut withdraw_event_count = 0u32;
    let mut withdraw_event_amount = 0i128;
    for (contract, topics, data) in s.env.events().all().iter() {
        if contract != s.registry.address {
            continue;
        }
        let t0: Option<Symbol> = topics.get(0).and_then(|v| v.try_into_val(&s.env).ok());
        let t1: Option<Symbol> = topics.get(1).and_then(|v| v.try_into_val(&s.env).ok());
        if topics.len() == 2
            && t0 == Some(symbol_short!("wdraw"))
            && t1 == Some(symbol_short!("reward"))
        {
            withdraw_event_count += 1;
            let (event_keeper, amount): (Address, i128) = data.try_into_val(&s.env).unwrap();
            assert_eq!(event_keeper, keeper1);
            withdraw_event_amount = amount;
        }
    }
    assert_eq!(withdraw_event_count, 1);
    assert_eq!(withdraw_event_amount, keeper1_balance);

    assert_eq!(withdrawn, keeper1_balance);
    assert_eq!(token.balance(&keeper1), keeper1_balance);
    assert_eq!(s.registry.keeper_balance(&keeper1), 0i128);

    // keeper2's balance stayed independent — untouched by keeper1's
    // accumulation, withdrawal, or the KeeperReward key derivation.
    assert_eq!(s.registry.keeper_balance(&keeper2), keeper2_balance);
    assert_eq!(token.balance(&keeper2), 0i128);
}

#[test]
fn test_withdraw_with_no_balance_fails() {
    let s = setup();
    let keeper = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_withdraw_rewards(&keeper),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );
}

#[test]
fn test_double_withdraw_fails() {
    let s = setup();
    let keeper = executed_task_keeper(&s);
    s.registry.withdraw_rewards(&keeper);
    assert_eq!(
        s.registry.try_withdraw_rewards(&keeper),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );
}

#[test]
fn test_execute_accrues_protocol_fee() {
    let s = setup();
    let _ = executed_task_keeper(&s);
    // 3% of 1_000_000 withheld.
    assert_eq!(s.registry.fees_accrued(), 30_000i128);
}

#[test]
fn test_sweep_fees_to_treasury() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let _ = executed_task_keeper(&s); // 30_000 fee accrued
    let treasury = Address::generate(&s.env);

    s.registry.sweep_fees(&s.admin, &treasury, &30_000i128);

    assert_eq!(token.balance(&treasury), 30_000i128);
    assert_eq!(s.registry.fees_accrued(), 0i128);
}

#[test]
fn test_sweep_more_than_accrued_fails() {
    let s = setup();
    let _ = executed_task_keeper(&s); // 30_000 accrued
    let treasury = Address::generate(&s.env);
    // Guard: cannot sweep into task escrow / keeper balances.
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &30_001i128),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );
}

#[test]
fn test_sweep_by_non_admin_fails() {
    let s = setup();
    let _ = executed_task_keeper(&s);
    let stranger = Address::generate(&s.env);
    let treasury = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_sweep_fees(&stranger, &treasury, &1i128),
        Err(Ok(KeeperError::Unauthorized))
    );
}

#[test]
fn test_sweep_zero_amount_fails() {
    let s = setup();
    let _ = executed_task_keeper(&s); // 30_000 accrued
    let treasury = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &0i128),
        Err(Ok(KeeperError::InvalidReward))
    );
    assert_eq!(s.registry.fees_accrued(), 30_000i128);
}

#[test]
fn test_sweep_negative_amount_fails() {
    let s = setup();
    let _ = executed_task_keeper(&s); // 30_000 accrued
    let treasury = Address::generate(&s.env);
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &-1i128),
        Err(Ok(KeeperError::InvalidReward))
    );
    assert_eq!(s.registry.fees_accrued(), 30_000i128);
}

#[test]
fn test_sweep_with_nothing_accrued_fails() {
    let s = setup();
    // Fresh contract — no task has ever executed, so nothing is accrued.
    let treasury = Address::generate(&s.env);
    assert_eq!(s.registry.fees_accrued(), 0i128);
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &1i128),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );
}

#[test]
fn test_sweep_partial_sequence_conserves_remainder_and_leaves_other_balances_untouched() {
    let s = setup();
    let token = token::Client::new(&s.env, &s.token_id);
    let treasury = Address::generate(&s.env);

    // An unrelated open task and a credited keeper — the accumulator is the
    // only thing sweep_fees is allowed to draw from, so neither should ever
    // move as a result of sweeping.
    let untouched_task_id = register_default_task(&s); // 1_000_000 escrowed
    let keeper = executed_task_keeper(&s); // credits keeper 970_000, accrues 30_000 fee

    assert_eq!(s.registry.fees_accrued(), 30_000i128);

    // Three uneven partial sweeps summing to the full 30_000 accrued.
    let parts = [12_000i128, 9_000i128, 9_000i128];
    let mut swept_so_far = 0i128;
    for &part in parts.iter() {
        s.registry.sweep_fees(&s.admin, &treasury, &part);
        swept_so_far += part;
        assert_eq!(s.registry.fees_accrued(), 30_000i128 - swept_so_far);
        assert_eq!(token.balance(&treasury), swept_so_far);
    }
    assert_eq!(s.registry.fees_accrued(), 0i128);

    // Nothing left: a further sweep of 1 is rejected.
    assert_eq!(
        s.registry.try_sweep_fees(&s.admin, &treasury, &1i128),
        Err(Ok(KeeperError::NoRewardsAvailable))
    );

    // The unrelated task's escrow and the keeper's credited balance are
    // exactly as they were before any sweep — proving sweeping never dipped
    // into either.
    assert_eq!(
        s.registry.get_task(&untouched_task_id).reward,
        1_000_000i128
    );
    assert_eq!(
        s.registry.get_task(&untouched_task_id).status,
        TaskStatus::Pending
    );
    assert_eq!(s.registry.keeper_balance(&keeper), 970_000i128);
}
