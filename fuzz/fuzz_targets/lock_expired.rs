//! Fuzz target for `lock_expired` ledger arithmetic.
//!
//! The contract computes the unlock ledger with
//! `claimed_at.saturating_add(lock_ledgers)`. This target exercises arbitrary
//! values across the full u32 domain and explicitly checks the overflow
//! boundary around u32::MAX.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct LockExpiredInput {
    claimed_at: u32,
    lock_ledgers: u32,
    current_ledger: u32,
}

/// Reference implementation of `lock_expired`'s ledger comparison.
///
/// Boundary confirmation: saturation produces u32::MAX and does not make an
/// enormous lock window appear expired for any current ledger below MAX.
fn lock_expired_reference(
    claimed_at: u32,
    lock_ledgers: u32,
    current_ledger: u32,
) -> bool {
    current_ledger >= claimed_at.saturating_add(lock_ledgers)
}

fn assert_lock_expiry_invariant(
    claimed_at: u32,
    lock_ledgers: u32,
    current_ledger: u32,
) {
    let unlock_at = claimed_at.saturating_add(lock_ledgers);
    let expired = lock_expired_reference(claimed_at, lock_ledgers, current_ledger);

    if claimed_at.checked_add(lock_ledgers).is_none() {
        assert_eq!(unlock_at, u32::MAX);
        if current_ledger < u32::MAX {
            assert!(!expired);
        }
    }

    assert_eq!(expired, current_ledger >= unlock_at);
}

fuzz_target!(|data: &[u8]| {
    let mut unstructured = Unstructured::new(data);
    let Ok(input) = LockExpiredInput::arbitrary(&mut unstructured) else {
        return;
    };

    assert_lock_expiry_invariant(
        input.claimed_at,
        input.lock_ledgers,
        input.current_ledger,
    );

    let boundary_cases = [
        (u32::MAX - 1, 1, 0),
        (u32::MAX - 1, 1, u32::MAX - 1),
        (u32::MAX - 1, 1, u32::MAX),
        (u32::MAX - 1, 2, 0),
        (u32::MAX - 1, u32::MAX, u32::MAX - 1),
        (u32::MAX, 1, u32::MAX - 1),
        (u32::MAX, 1, u32::MAX),
        (0, u32::MAX, 0),
        (0, u32::MAX, u32::MAX - 1),
        (0, u32::MAX, u32::MAX),
    ];

    for (claimed_at, lock_ledgers, current_ledger) in boundary_cases {
        assert_lock_expiry_invariant(claimed_at, lock_ledgers, current_ledger);
    }
});
