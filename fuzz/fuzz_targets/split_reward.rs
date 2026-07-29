//! Fuzz target for the pure `split_reward` arithmetic helper.
//!
//! The first 16 input bytes are interpreted as an arbitrary `i128` reward and
//! the next 4 bytes as an arbitrary `u32` fee rate. No contract-level input
//! validation is applied: this deliberately covers the complete input domain.
//!
//! A panic is a fuzzing failure. In particular, this target exercises the
//! checked multiplication boundary reached by extreme reward magnitudes and
//! fee rates.

#![no_main]

use keeper_registry::split_reward;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // A complete input needs 16 bytes for i128 and 4 bytes for u32. Returning
    // for shorter inputs preserves the full domain for all inputs we test.
    if data.len() < 20 {
        return;
    }

    let mut reward_bytes = [0u8; 16];
    reward_bytes.copy_from_slice(&data[..16]);

    let mut fee_bps_bytes = [0u8; 4];
    fee_bps_bytes.copy_from_slice(&data[16..20]);

    let reward = i128::from_le_bytes(reward_bytes);
    let fee_bps = u32::from_le_bytes(fee_bps_bytes);

    // Any panic from split_reward is intentionally allowed to reach
    // libFuzzer, which records the exact overflowing input as a crash.
    let _ = split_reward(reward, fee_bps);
});
