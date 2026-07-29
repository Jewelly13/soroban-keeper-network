//! # Oracle Verifier — reference `IKeeperVerifier` implementation
//!
//! Verifies an `OraclePricePush` task's `proof` against a configured
//! oracle contract's own on-chain state, read via a cross-contract call —
//! never trusted from the `proof` bytes alone.
//!
//! ## Scope
//! This is a reference **pattern**, not a production-ready oracle
//! integration: it's built and tested against [`MockOracle`], a minimal
//! stand-in exposing exactly the two reads this verifier needs (current
//! price, last-updated timestamp). A real integration (Reflector, Band,
//! etc., tracked separately under epic E11 — see this crate's `README`
//! reference in the workspace docs) will need to adapt the `OracleClient`
//! call this verifier makes to whatever that oracle's actual interface
//! looks like; the point demonstrated here is the cross-check pattern
//! itself — read independent on-chain state, don't trust the proof bytes
//! — not a specific oracle's wire format.
//!
//! ## What `proof` encodes
//! `proof` is the XDR encoding of a `(price: i128, timestamp: u64)` tuple
//! — the price and timestamp the keeper claims the off-chain action was
//! executed against. `verify` decodes it, reads the configured oracle's
//! *current* price and last-updated timestamp, and confirms:
//! 1. The claimed price is within [`OracleVerifier::tolerance_bps`] of the
//!    oracle's current price (prices can move between the keeper reading
//!    the price and the transaction landing on-chain — an exact-match
//!    requirement would make every submission racy against normal price
//!    drift).
//! 2. The oracle's own last-updated timestamp is no older than
//!    [`OracleVerifier::staleness_threshold_secs`] relative to the current
//!    ledger timestamp.
//!
//! A malformed `proof` (wrong length/undecodable) is rejected with a clean
//! `false`, not a panic — this contract's `IKeeperVerifier::verify`
//! implementation doesn't call anything with a documented panic-on-
//! failure API the way `signature-verifier`'s `ed25519_verify` does, so
//! it doesn't share that contract's panic-on-invalid-input caveat.
#![no_std]

use keeper_registry::{IKeeperVerifier, Task};
use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, Address, Bytes, BytesN,
    Env,
};

#[contracttype]
enum DataKey {
    Oracle,
    ToleranceBps,
    StalenessThresholdSecs,
}

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OracleVerifierError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    InvalidToleranceBps = 3,
}

/// The minimal interface this verifier needs from an oracle contract. A
/// real oracle integration (E11) will have a different, richer interface;
/// this is deliberately narrowed to exactly what the cross-check pattern
/// needs, so [`MockOracle`] can stand in for it in tests without
/// pretending to be a production oracle.
#[contractclient(name = "OracleClient")]
pub trait Oracle {
    /// Current price, in the oracle's own fixed-point convention (this
    /// reference pattern treats it as an opaque `i128` and compares
    /// like-for-like against `proof`'s claimed price — a real integration
    /// must ensure both sides use the same convention, e.g. matching
    /// decimals).
    fn price(env: Env) -> i128;
    /// Unix timestamp (seconds) the price was last updated.
    fn last_updated(env: Env) -> u64;
}

#[contract]
pub struct OracleVerifier;

#[contractimpl]
impl OracleVerifier {
    /// One-time setup.
    ///
    /// `tolerance_bps`: maximum allowed deviation between the proof's
    /// claimed price and the oracle's current price, in basis points of
    /// the oracle's price (e.g. `50` = 0.5%). Must be `<= 10_000` (100%).
    ///
    /// `staleness_threshold_secs`: maximum age, in seconds, the oracle's
    /// own `last_updated` may be relative to the current ledger timestamp
    /// before a proof is rejected as stale.
    pub fn initialize(
        e: Env,
        oracle: Address,
        tolerance_bps: u32,
        staleness_threshold_secs: u64,
    ) -> Result<(), OracleVerifierError> {
        if e.storage().instance().has(&DataKey::Oracle) {
            return Err(OracleVerifierError::AlreadyInitialized);
        }
        if tolerance_bps > 10_000 {
            return Err(OracleVerifierError::InvalidToleranceBps);
        }
        e.storage().instance().set(&DataKey::Oracle, &oracle);
        e.storage()
            .instance()
            .set(&DataKey::ToleranceBps, &tolerance_bps);
        e.storage().instance().set(
            &DataKey::StalenessThresholdSecs,
            &staleness_threshold_secs,
        );
        Ok(())
    }

    pub fn oracle(e: Env) -> Result<Address, OracleVerifierError> {
        e.storage()
            .instance()
            .get(&DataKey::Oracle)
            .ok_or(OracleVerifierError::NotInitialized)
    }

    pub fn tolerance_bps(e: Env) -> Result<u32, OracleVerifierError> {
        e.storage()
            .instance()
            .get(&DataKey::ToleranceBps)
            .ok_or(OracleVerifierError::NotInitialized)
    }

    pub fn staleness_threshold_secs(e: Env) -> Result<u64, OracleVerifierError> {
        e.storage()
            .instance()
            .get(&DataKey::StalenessThresholdSecs)
            .ok_or(OracleVerifierError::NotInitialized)
    }
}

/// Absolute difference between two prices, as a fraction of `oracle_price`
/// expressed in basis points. `oracle_price` is assumed positive (a
/// well-formed oracle never reports a non-positive price); this is only
/// ever called with values read directly from the configured oracle.
fn deviation_bps(claimed: i128, oracle_price: i128) -> Option<i128> {
    let diff = (claimed - oracle_price).checked_abs()?;
    diff.checked_mul(10_000)?.checked_div(oracle_price)
}

/// `proof`'s fixed wire format: a 16-byte big-endian `i128` claimed price,
/// immediately followed by an 8-byte big-endian `u64` claimed timestamp —
/// 24 bytes total. Decoded manually (rather than via XDR) so a malformed
/// `proof` fails the length check and returns `None` cleanly, with no
/// panicking decode step anywhere in the path.
fn decode_proof(proof: &Bytes) -> Option<(i128, u64)> {
    if proof.len() != 24 {
        return None;
    }
    let price_bytes: BytesN<16> = proof.slice(0..16).try_into().ok()?;
    let timestamp_bytes: BytesN<8> = proof.slice(16..24).try_into().ok()?;
    let price = i128::from_be_bytes(price_bytes.to_array());
    let timestamp = u64::from_be_bytes(timestamp_bytes.to_array());
    Some((price, timestamp))
}

#[contractimpl]
impl IKeeperVerifier for OracleVerifier {
    fn verify(env: Env, _task: Task, _keeper: Address, proof: Bytes) -> bool {
        let (oracle, tolerance_bps, staleness_threshold_secs): (Address, u32, u64) =
            match (
                env.storage().instance().get(&DataKey::Oracle),
                env.storage().instance().get(&DataKey::ToleranceBps),
                env.storage()
                    .instance()
                    .get(&DataKey::StalenessThresholdSecs),
            ) {
                (Some(o), Some(t), Some(s)) => (o, t, s),
                // Not initialized: fail closed rather than panicking.
                _ => return false,
            };

        let (claimed_price, claimed_timestamp) = match decode_proof(&proof) {
            Some(pair) => pair,
            None => return false,
        };

        let oracle_price = OracleClient::new(&env, &oracle).price();
        let oracle_last_updated = OracleClient::new(&env, &oracle).last_updated();

        let now = env.ledger().timestamp();
        if now.saturating_sub(oracle_last_updated) > staleness_threshold_secs {
            return false;
        }

        let deviation = match deviation_bps(claimed_price, oracle_price) {
            Some(d) => d,
            None => return false,
        };
        if deviation > tolerance_bps as i128 {
            return false;
        }

        // `claimed_timestamp` is part of the proof format for a future
        // integration that wants to bind the proof to a specific price
        // observation, but this reference pattern doesn't independently
        // corroborate it beyond the staleness check above (there's no
        // oracle history to check it against in the minimal `MockOracle`
        // interface) — accepted as-is once price/staleness pass.
        let _ = claimed_timestamp;
        true
    }
}

#[cfg(test)]
mod test;
