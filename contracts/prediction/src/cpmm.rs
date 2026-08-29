//! Constant Product Market Maker (CPMM) for prediction market outcome tokens.
//!
//! Maintains the invariant: for each outcome i,
//!   k_i = collateral_reserve × outcome_reserve_i
//!
//! All outcomes share the same collateral reserve but have independent outcome
//! token reserves. Buying outcome i reduces its reserve and increases the
//! collateral reserve; the collateral reserve increase is distributed across
//! ALL outcomes (since they share collateral), maintaining the k invariant
//! for each outcome pair.

use soroban_sdk::{symbol_short, Env, Symbol, Vec};

use crate::types::{
    LPResult, LiquidityPool, PredictionDataKey, PredictionError, SwapResult,
    DECIMAL_PRECISION, MIN_LIQUIDITY,
};

// ---------------------------------------------------------------------------
// Pool lifecycle
// ---------------------------------------------------------------------------

/// Create a new liquidity pool for a market.
///
/// `initial_collateral` is the seed liquidity provided by the market creator.
/// Outcome token reserves are initialized proportionally.
pub fn create_pool(
    env: &Env,
    market_id: u64,
    num_outcomes: u32,
    initial_collateral: i128,
) -> Result<LiquidityPool, PredictionError> {
    if initial_collateral < MIN_LIQUIDITY {
        return Err(PredictionError::InsufficientLiquidity);
    }

    let mut outcome_reserves = Vec::new(env);
    let mut k_per_outcome = Vec::new(env);

    // Each outcome starts with the same reserve equal to the initial collateral.
    // This gives equal initial pricing: price_i = collateral / outcome_reserve = 1.0
    for _ in 0..num_outcomes {
        outcome_reserves.push_back(initial_collateral);
        k_per_outcome.push_back(
            initial_collateral
                .checked_mul(initial_collateral)
                .ok_or(PredictionError::ArithmeticOverflow)?,
        );
    }

    let pool = LiquidityPool {
        market_id,
        collateral_reserve: initial_collateral,
        outcome_reserves,
        lp_supply: initial_collateral, // LP tokens proportional to initial collateral
        k_per_outcome,
        fees_accumulated: 0,
        total_volume: 0,
    };

    save_pool(env, &pool);
    Ok(pool)
}

/// Load a pool from storage.
pub fn load_pool(env: &Env, market_id: u64) -> Result<LiquidityPool, PredictionError> {
    env.storage()
        .persistent()
        .get(&PredictionDataKey::LiquidityPool(market_id))
        .ok_or(PredictionError::NoLiquidityPool)
}

/// Save a pool to storage.
pub fn save_pool(env: &Env, pool: &LiquidityPool) {
    env.storage().persistent().set(
        &PredictionDataKey::LiquidityPool(pool.market_id),
        pool,
    );
}

// ---------------------------------------------------------------------------
// Pricing
// ---------------------------------------------------------------------------

/// Get the current price for a specific outcome.
///
/// price_i = collateral_reserve / outcome_reserve_i
///
/// Returns price scaled by DECIMAL_PRECISION.
pub fn get_outcome_price(pool: &LiquidityPool, outcome_index: u32) -> Result<i128, PredictionError> {
    let reserve = pool
        .outcome_reserves
        .get(outcome_index)
        .ok_or(PredictionError::InvalidOutcomeIndex)?;

    if reserve <= 0 {
        return Err(PredictionError::ArithmeticOverflow);
    }

    let price = pool
        .collateral_reserve
        .checked_mul(DECIMAL_PRECISION)
        .ok_or(PredictionError::ArithmeticOverflow)?
        .checked_div(reserve)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    Ok(price)
}

/// Get prices for all outcomes.
pub fn get_all_outcome_prices(
    pool: &LiquidityPool,
) -> Result<Vec<i128>, PredictionError> {
    let mut prices = Vec::new(pool.outcome_reserves.env());
    let num_outcomes = pool.outcome_reserves.len();
    for i in 0..num_outcomes {
        let price = get_outcome_price(pool, i)?;
        prices.push_back(price);
    }
    Ok(prices)
}

// ---------------------------------------------------------------------------
// Trading (CPMM swaps)
// ---------------------------------------------------------------------------

/// Buy outcome tokens using collateral via the CPMM.
///
/// Uses the constant product formula:
///   new_collateral_reserve = collateral_reserve + collateral_in (after fee)
///   new_outcome_reserve = k_i / new_collateral_reserve
///   outcome_tokens_out = outcome_reserve_i - new_outcome_reserve
pub fn buy_outcome_tokens(
    env: &Env,
    pool: &mut LiquidityPool,
    outcome_index: u32,
    collateral_in: i128,
    fee_bps: i128,
    min_outcome_tokens: i128,
) -> Result<SwapResult, PredictionError> {
    if collateral_in <= 0 {
        return Err(PredictionError::InvalidOrderAmount);
    }

    let k_i = pool
        .k_per_outcome
        .get(outcome_index)
        .ok_or(PredictionError::InvalidOutcomeIndex)?;

    // Calculate fee
    let fee = collateral_in
        .checked_mul(fee_bps)
        .ok_or(PredictionError::ArithmeticOverflow)?
        .checked_div(10_000)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    let collateral_after_fee = collateral_in
        .checked_sub(fee)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    // Get current reserves
    let old_collateral = pool.collateral_reserve;
    let old_outcome_reserve = pool
        .outcome_reserves
        .get(outcome_index)
        .ok_or(PredictionError::InvalidOutcomeIndex)?;

    // Calculate new collateral reserve (increases as buyer deposits collateral)
    let new_collateral = old_collateral
        .checked_add(collateral_after_fee)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    // Calculate new outcome reserve using k invariant
    let new_outcome_reserve = k_i
        .checked_div(new_collateral)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    let outcome_tokens_out = old_outcome_reserve
        .checked_sub(new_outcome_reserve)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    if outcome_tokens_out < min_outcome_tokens {
        return Err(PredictionError::SlippageExceeded);
    }

    if outcome_tokens_out <= 0 {
        return Err(PredictionError::InsufficientLiquidity);
    }

    // Calculate price impact
    let price_impact_bps = if old_collateral > 0 {
        (collateral_after_fee
            .checked_mul(10_000)
            .ok_or(PredictionError::ArithmeticOverflow)?)
            .checked_div(old_collateral)
            .ok_or(PredictionError::ArithmeticOverflow)?
    } else {
        0
    };

    // Update pool state
    pool.collateral_reserve = new_collateral;
    pool.outcome_reserves.set(outcome_index, new_outcome_reserve);
    pool.fees_accumulated = pool
        .fees_accumulated
        .checked_add(fee)
        .ok_or(PredictionError::ArithmeticOverflow)?;
    pool.total_volume = pool
        .total_volume
        .checked_add(collateral_in)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    // Update k for all outcomes (since collateral reserve changed)
    update_all_k(pool)?;

    Ok(SwapResult {
        outcome_amount: outcome_tokens_out,
        collateral_amount: collateral_in,
        fee,
        price_impact_bps,
    })
}

/// Sell outcome tokens for collateral via the CPMM.
///
/// Uses the constant product formula:
///   new_outcome_reserve = outcome_reserve_i + outcome_tokens_in
///   new_collateral_reserve = k_i / new_outcome_reserve
///   collateral_out = collateral_reserve - new_collateral_reserve
pub fn sell_outcome_tokens(
    env: &Env,
    pool: &mut LiquidityPool,
    outcome_index: u32,
    outcome_tokens_in: i128,
    fee_bps: i128,
    min_collateral: i128,
) -> Result<SwapResult, PredictionError> {
    if outcome_tokens_in <= 0 {
        return Err(PredictionError::InvalidOrderAmount);
    }

    let k_i = pool
        .k_per_outcome
        .get(outcome_index)
        .ok_or(PredictionError::InvalidOutcomeIndex)?;

    // Get current reserves
    let old_collateral = pool.collateral_reserve;
    let old_outcome_reserve = pool
        .outcome_reserves
        .get(outcome_index)
        .ok_or(PredictionError::InvalidOutcomeIndex)?;

    // Calculate new outcome reserve (increases as seller deposits outcome tokens)
    let new_outcome_reserve = old_outcome_reserve
        .checked_add(outcome_tokens_in)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    // Calculate new collateral reserve using k invariant
    let new_collateral = k_i
        .checked_div(new_outcome_reserve)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    let collateral_before_fee = old_collateral
        .checked_sub(new_collateral)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    // Calculate fee
    let fee = collateral_before_fee
        .checked_mul(fee_bps)
        .ok_or(PredictionError::ArithmeticOverflow)?
        .checked_div(10_000)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    let collateral_out = collateral_before_fee
        .checked_sub(fee)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    if collateral_out < min_collateral {
        return Err(PredictionError::SlippageExceeded);
    }

    if collateral_out <= 0 {
        return Err(PredictionError::InsufficientLiquidity);
    }

    // Calculate price impact
    let price_impact_bps = if old_collateral > 0 {
        (collateral_before_fee
            .checked_mul(10_000)
            .ok_or(PredictionError::ArithmeticOverflow)?)
            .checked_div(old_collateral)
            .ok_or(PredictionError::ArithmeticOverflow)?
    } else {
        0
    };

    // Update pool state
    pool.collateral_reserve = new_collateral;
    pool.outcome_reserves.set(outcome_index, new_outcome_reserve);
    pool.fees_accumulated = pool
        .fees_accumulated
        .checked_add(fee)
        .ok_or(PredictionError::ArithmeticOverflow)?;
    pool.total_volume = pool
        .total_volume
        .checked_add(outcome_tokens_in)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    // Update k for all outcomes
    update_all_k(pool)?;

    Ok(SwapResult {
        outcome_amount: outcome_tokens_in,
        collateral_amount: collateral_out,
        fee,
        price_impact_bps,
    })
}

// ---------------------------------------------------------------------------
// Liquidity provision
// ---------------------------------------------------------------------------

/// Add liquidity to the pool. Returns LP tokens minted.
///
/// Liquidity providers deposit proportional amounts of collateral and all
/// outcome tokens. The first LP sets the ratio; subsequent LPs must match.
pub fn add_liquidity(
    env: &Env,
    pool: &mut LiquidityPool,
    collateral_amount: i128,
    outcome_amounts: Vec<i128>,
) -> Result<LPResult, PredictionError> {
    if collateral_amount <= 0 {
        return Err(PredictionError::InvalidOrderAmount);
    }

    let num_outcomes = pool.outcome_reserves.len();
    if outcome_amounts.len() != num_outcomes {
        return Err(PredictionError::InvalidMarketParams);
    }

    let lp_tokens;
    let mut actual_collateral = collateral_amount;
    let mut actual_outcomes = Vec::new(env);

    if pool.lp_supply == 0 {
        // First LP provider — set the initial ratio
        lp_tokens = collateral_amount;

        for i in 0..num_outcomes {
            let amount = outcome_amounts.get(i).unwrap();
            if amount <= 0 {
                return Err(PredictionError::InvalidOrderAmount);
            }
            actual_outcomes.push_back(amount);

            // Update reserves
            let new_reserve = pool
                .outcome_reserves
                .get(i)
                .unwrap()
                .checked_add(amount)
                .ok_or(PredictionError::ArithmeticOverflow)?;
            pool.outcome_reserves.set(i, new_reserve);
        }

        pool.collateral_reserve = pool
            .collateral_reserve
            .checked_add(actual_collateral)
            .ok_or(PredictionError::ArithmeticOverflow)?;
        pool.lp_supply = lp_tokens;
    } else {
        // Subsequent LPs must deposit proportionally
        // Calculate proportional amounts based on existing reserves
        let proportional_collateral = pool
            .outcome_reserves
            .get(0)
            .unwrap_or(0)
            .checked_mul(collateral_amount)
            .ok_or(PredictionError::ArithmeticOverflow)?
            .checked_div(pool.collateral_reserve.max(1))
            .ok_or(PredictionError::ArithmeticOverflow)?;

        // LP tokens minted proportional to deposit
        lp_tokens = collateral_amount
            .checked_mul(pool.lp_supply)
            .ok_or(PredictionError::ArithmeticOverflow)?
            .checked_div(pool.collateral_reserve)
            .ok_or(PredictionError::ArithmeticOverflow)?;

        actual_collateral = proportional_collateral;

        for i in 0..num_outcomes {
            let amount = outcome_amounts.get(i).unwrap();
            let proportional_amount = pool
                .outcome_reserves
                .get(i)
                .unwrap_or(0)
                .checked_mul(collateral_amount)
                .ok_or(PredictionError::ArithmeticOverflow)?
                .checked_div(pool.collateral_reserve.max(1))
                .ok_or(PredictionError::ArithmeticOverflow)?;

            actual_outcomes.push_back(proportional_amount);

            let new_reserve = pool
                .outcome_reserves
                .get(i)
                .unwrap()
                .checked_add(proportional_amount)
                .ok_or(PredictionError::ArithmeticOverflow)?;
            pool.outcome_reserves.set(i, new_reserve);
        }

        pool.collateral_reserve = pool
            .collateral_reserve
            .checked_add(actual_collateral)
            .ok_or(PredictionError::ArithmeticOverflow)?;
        pool.lp_supply = pool
            .lp_supply
            .checked_add(lp_tokens)
            .ok_or(PredictionError::ArithmeticOverflow)?;
    }

    // Update k for all outcomes
    update_all_k(pool)?;

    Ok(LPResult {
        lp_tokens_minted: lp_tokens,
        collateral_deposited: actual_collateral,
        outcome_deposits: actual_outcomes,
    })
}

/// Remove liquidity from the pool.
///
/// Burns LP tokens and returns proportional share of all reserves.
pub fn remove_liquidity(
    env: &Env,
    pool: &mut LiquidityPool,
    lp_amount: i128,
) -> Result<LPResult, PredictionError> {
    if lp_amount <= 0 || lp_amount > pool.lp_supply {
        return Err(PredictionError::InvalidOrderAmount);
    }

    let num_outcomes = pool.outcome_reserves.len();
    let share_num = lp_amount;
    let share_den = pool.lp_supply;

    // Calculate proportional collateral to return
    let collateral_out = pool
        .collateral_reserve
        .checked_mul(share_num)
        .ok_or(PredictionError::ArithmeticOverflow)?
        .checked_div(share_den)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    let mut outcome_returns = Vec::new(env);

    for i in 0..num_outcomes {
        let reserve = pool.outcome_reserves.get(i).unwrap_or(0);
        let outcome_out = reserve
            .checked_mul(share_num)
            .ok_or(PredictionError::ArithmeticOverflow)?
            .checked_div(share_den)
            .ok_or(PredictionError::ArithmeticOverflow)?;
        outcome_returns.push_back(outcome_out);

        let new_reserve = reserve
            .checked_sub(outcome_out)
            .ok_or(PredictionError::ArithmeticOverflow)?;
        pool.outcome_reserves.set(i, new_reserve);
    }

    pool.collateral_reserve = pool
        .collateral_reserve
        .checked_sub(collateral_out)
        .ok_or(PredictionError::ArithmeticOverflow)?;
    pool.lp_supply = pool
        .lp_supply
        .checked_sub(lp_amount)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    // Update k for all outcomes
    update_all_k(pool)?;

    Ok(LPResult {
        lp_tokens_minted: 0,
        collateral_deposited: collateral_out,
        outcome_deposits: outcome_returns,
    })
}

// ---------------------------------------------------------------------------
// Invariant verification
// ---------------------------------------------------------------------------

/// Verify the CPMM invariant holds for all outcomes.
///
/// Returns true if for all i: k_i = collateral_reserve * outcome_reserve_i
/// allowing for small rounding tolerance.
pub fn verify_invariant(pool: &LiquidityPool) -> bool {
    let num_outcomes = pool.outcome_reserves.len();
    for i in 0..num_outcomes {
        let outcome_reserve = pool.outcome_reserves.get(i).unwrap_or(0);
        let k_i = pool.k_per_outcome.get(i).unwrap_or(0);
        let expected_k = match pool.collateral_reserve.checked_mul(outcome_reserve) {
            Some(k) => k,
            None => return false,
        };
        // Allow 1 unit of rounding tolerance
        if (k_i - expected_k).abs() > 1 {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Update k_i for all outcomes after a state change.
fn update_all_k(pool: &mut LiquidityPool) -> Result<(), PredictionError> {
    let num_outcomes = pool.outcome_reserves.len();
    for i in 0..num_outcomes {
        let outcome_reserve = pool.outcome_reserves.get(i).unwrap_or(0);
        let new_k = pool
            .collateral_reserve
            .checked_mul(outcome_reserve)
            .ok_or(PredictionError::ArithmeticOverflow)?;
        pool.k_per_outcome.set(i, new_k);
    }
    Ok(())
}
