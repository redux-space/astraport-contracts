//! Fee calculation engine.
//!
//! Provides pure computation helpers for the three fee models (Flat, Percentage,
//! Tiered) plus discount and cap logic.  All functions in this module are
//! stateless — they operate on inputs only and never touch storage.

use soroban_sdk::Env;

use super::records::{FeeError, FeeStructure, FeeType, TierEntry, BPS_DENOM};

// ---------------------------------------------------------------------------
// Raw fee computation
// ---------------------------------------------------------------------------

/// Compute the raw fee before discounts and caps.
///
/// # Arguments
///
/// * `env` — Soroban environment (needed for overflow panics).
/// * `fee_type` — The mathematical model.
/// * `amount_bps` — Flat amount or percentage in bps (ignored for `Tiered`).
/// * `tiered_entries` — Tier definitions (only used for `Tiered`).
/// * `base_amount` — The base value to compute the fee against.
///
/// # Returns
///
/// The raw fee amount.  May exceed `base_amount` for `Flat` fees; callers
/// should clamp afterwards.
pub fn compute_raw_fee(
    env: &Env,
    fee_type: &FeeType,
    amount_bps: &i128,
    tiered_entries: &soroban_sdk::Vec<TierEntry>,
    base_amount: i128,
) -> i128 {
    match fee_type {
        FeeType::Flat => *amount_bps,
        FeeType::Percentage => {
            base_amount.checked_mul(*amount_bps).unwrap_or_else(|| {
                soroban_sdk::panic_with_error!(env, FeeError::ArithmeticOverflow)
            }) / BPS_DENOM
        }
        FeeType::Tiered => calculate_tiered_fee(env, tiered_entries, base_amount),
    }
}

/// Compute the raw fee directly from a [`FeeStructure`].
pub fn compute_fee_from_structure(env: &Env, fs: &FeeStructure, base_amount: i128) -> i128 {
    compute_raw_fee(
        env,
        &fs.fee_type,
        &fs.amount_bps,
        &fs.tiered_entries,
        base_amount,
    )
}

// ---------------------------------------------------------------------------
// Tiered fee calculation
// ---------------------------------------------------------------------------

/// Calculate a tiered fee by finding the matching tier for `amount`.
///
/// Tiers are evaluated from the highest threshold downward.  The first tier
/// whose `threshold <= amount` wins.
fn calculate_tiered_fee(env: &Env, tiers: &soroban_sdk::Vec<TierEntry>, amount: i128) -> i128 {
    if tiers.is_empty() {
        return 0;
    }

    let mut selected_bps: Option<i128> = None;
    let mut i = tiers.len();
    while i > 0 {
        i -= 1;
        let tier = tiers.get(i).unwrap();
        if amount >= tier.threshold {
            selected_bps = Some(tier.fee_bps);
            break;
        }
    }

    match selected_bps {
        Some(bps) => {
            amount.checked_mul(bps).unwrap_or_else(|| {
                soroban_sdk::panic_with_error!(env, FeeError::ArithmeticOverflow)
            }) / BPS_DENOM
        }
        None => 0,
    }
}

// ---------------------------------------------------------------------------
// Clamping
// ---------------------------------------------------------------------------

/// Clamp a fee to `[0, base_amount]`.
///
/// Fees are never negative and never exceed the base amount (ensures the
/// protocol never overcharges).
pub fn clamp_fee(fee: i128, base: i128) -> i128 {
    let clamped = if fee < 0 { 0 } else { fee };
    if clamped > base {
        base
    } else {
        clamped
    }
}

// ---------------------------------------------------------------------------
// Discount application
// ---------------------------------------------------------------------------

/// Apply a discount (in bps) to a fee amount.
///
/// Returns `0` if `waived` is true.  Otherwise reduces `fee` by `discount_bps`
/// out of `BPS_DENOM`.
pub fn apply_discount(env: &Env, fee: i128, discount_bps: i128, waived: bool) -> i128 {
    if waived {
        return 0;
    }
    if discount_bps <= 0 {
        return fee;
    }
    let discounted = fee
        .checked_mul(BPS_DENOM - discount_bps)
        .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, FeeError::ArithmeticOverflow))
        / BPS_DENOM;
    if discounted < 0 {
        0
    } else {
        discounted
    }
}

/// Apply a fee cap.  Returns `min(fee, cap)`.
pub fn apply_fee_cap(fee: i128, cap: Option<i128>) -> i128 {
    match cap {
        Some(c) if c >= 0 && c < fee => c,
        _ => fee,
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Validate that a fee structure's configuration is internally consistent.
pub fn validate_fee_structure(fs: &FeeStructure) -> Result<(), FeeError> {
    match fs.fee_type {
        FeeType::Percentage => {
            if !(0..=BPS_DENOM).contains(&fs.amount_bps) {
                return Err(FeeError::InvalidFeeConfiguration);
            }
        }
        FeeType::Flat => {
            if fs.amount_bps < 0 {
                return Err(FeeError::InvalidFeeConfiguration);
            }
        }
        FeeType::Tiered => {
            if fs.tiered_entries.is_empty() {
                return Err(FeeError::InvalidFeeConfiguration);
            }
        }
    }
    // Validate cap is non-negative if present.
    if let Some(cap) = fs.fee_cap {
        if cap < 0 {
            return Err(FeeError::InvalidFeeConfiguration);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clamp_fee_negative() {
        assert_eq!(clamp_fee(-100, 1_000), 0);
    }

    #[test]
    fn test_clamp_fee_zero() {
        assert_eq!(clamp_fee(0, 1_000), 0);
    }

    #[test]
    fn test_clamp_fee_within_bounds() {
        assert_eq!(clamp_fee(500, 1_000), 500);
    }

    #[test]
    fn test_clamp_fee_exceeds_amount() {
        assert_eq!(clamp_fee(1_500, 1_000), 1_000);
    }

    #[test]
    fn test_apply_discount_none() {
        let env = Env::default();
        assert_eq!(apply_discount(&env, 1_000, 0, false), 1_000);
    }

    #[test]
    fn test_apply_discount_waived() {
        let env = Env::default();
        assert_eq!(apply_discount(&env, 1_000, 5000, true), 0);
    }

    #[test]
    fn test_apply_discount_50pct() {
        let env = Env::default();
        // 50% discount = 5000 bps
        assert_eq!(apply_discount(&env, 1_000, 5000, false), 500);
    }

    #[test]
    fn test_apply_discount_full() {
        let env = Env::default();
        // 100% discount = 10_000 bps
        assert_eq!(apply_discount(&env, 1_000, BPS_DENOM, false), 0);
    }

    #[test]
    fn test_apply_fee_cap_none() {
        assert_eq!(apply_fee_cap(500, None), 500);
    }

    #[test]
    fn test_apply_fee_cap_exceeds() {
        assert_eq!(apply_fee_cap(500, Some(300)), 300);
    }

    #[test]
    fn test_apply_fee_cap_within() {
        assert_eq!(apply_fee_cap(200, Some(300)), 200);
    }

    #[test]
    fn test_validate_percentage_ok() {
        let fs = FeeStructure {
            fee_id: soroban_sdk::symbol_short!("T"),
            fee_type: FeeType::Percentage,
            amount_bps: 250,
            tiered_entries: soroban_sdk::Vec::new(&Env::default()),
            category: super::super::records::FeeCategory::Trading,
            active: true,
            fee_cap: None,
        };
        assert!(validate_fee_structure(&fs).is_ok());
    }

    #[test]
    fn test_validate_percentage_out_of_range() {
        let fs = FeeStructure {
            fee_id: soroban_sdk::symbol_short!("T"),
            fee_type: FeeType::Percentage,
            amount_bps: 15_000, // > 100%
            tiered_entries: soroban_sdk::Vec::new(&Env::default()),
            category: super::super::records::FeeCategory::Trading,
            active: true,
            fee_cap: None,
        };
        assert_eq!(
            validate_fee_structure(&fs),
            Err(FeeError::InvalidFeeConfiguration)
        );
    }

    #[test]
    fn test_validate_flat_negative() {
        let fs = FeeStructure {
            fee_id: soroban_sdk::symbol_short!("T"),
            fee_type: FeeType::Flat,
            amount_bps: -1,
            tiered_entries: soroban_sdk::Vec::new(&Env::default()),
            category: super::super::records::FeeCategory::Trading,
            active: true,
            fee_cap: None,
        };
        assert_eq!(
            validate_fee_structure(&fs),
            Err(FeeError::InvalidFeeConfiguration)
        );
    }

    #[test]
    fn test_validate_tiered_empty() {
        let fs = FeeStructure {
            fee_id: soroban_sdk::symbol_short!("T"),
            fee_type: FeeType::Tiered,
            amount_bps: 0,
            tiered_entries: soroban_sdk::Vec::new(&Env::default()),
            category: super::super::records::FeeCategory::Trading,
            active: true,
            fee_cap: None,
        };
        assert_eq!(
            validate_fee_structure(&fs),
            Err(FeeError::InvalidFeeConfiguration)
        );
    }

    #[test]
    fn test_validate_negative_cap() {
        let fs = FeeStructure {
            fee_id: soroban_sdk::symbol_short!("T"),
            fee_type: FeeType::Flat,
            amount_bps: 100,
            tiered_entries: soroban_sdk::Vec::new(&Env::default()),
            category: super::super::records::FeeCategory::Trading,
            active: true,
            fee_cap: Some(-10),
        };
        assert_eq!(
            validate_fee_structure(&fs),
            Err(FeeError::InvalidFeeConfiguration)
        );
    }
}
