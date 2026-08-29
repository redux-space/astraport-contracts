//! Market settlement and outcome token redemption.
//!
//! After a market is resolved (and any dispute period has passed or disputes
//! have been settled), holders of the winning outcome token can redeem them
//! for collateral at a 1:1 rate.

use soroban_sdk::{symbol_short, Env, Symbol, Vec};

use crate::types::{
    LiquidityPool, Market, MarketStatus, Position, PredictionDataKey, PredictionError,
    PredictionOrder, DECIMAL_PRECISION,
};

// ---------------------------------------------------------------------------
// Position management
// ---------------------------------------------------------------------------

/// Get a user's position in a market.
pub fn get_position(env: &Env, market_id: u64, user: &Symbol) -> Option<Position> {
    let key = PredictionDataKey::Position(market_id, user.clone());
    env.storage().persistent().get(&key)
}

/// Save a user's position.
pub fn save_position(env: &Env, position: &Position) {
    let key = PredictionDataKey::Position(position.market_id, position.user.clone());
    env.storage().persistent().set(&key, position);
}

/// Record a trade in the user's position.
pub fn record_trade(
    env: &Env,
    market_id: u64,
    user: &Symbol,
    outcome_index: u32,
    is_buy: bool,
    amount: i128,
    price: i128,
) -> Result<(), PredictionError> {
    let num_outcomes = 3u32; // Will be updated from market
    let mut position = get_position(env, market_id, user).unwrap_or_else(|| {
        let mut outcome_amounts = Vec::new(env);
        let mut entry_prices = Vec::new(env);
        for _ in 0..num_outcomes {
            outcome_amounts.push_back(0);
            entry_prices.push_back(0);
        }
        Position {
            market_id,
            user: user.clone(),
            outcome_amounts,
            entry_prices,
            total_spent: 0,
            settled: false,
            pnl: None,
        }
    });

    // Ensure vectors are large enough
    while position.outcome_amounts.len() <= outcome_index {
        position.outcome_amounts.push_back(0);
        position.entry_prices.push_back(0);
    }

    let current_amount = position.outcome_amounts.get(outcome_index).unwrap_or(0);
    let current_entry = position.entry_prices.get(outcome_index).unwrap_or(0);

    if is_buy {
        // Buying outcome tokens
        let new_amount = current_amount
            .checked_add(amount)
            .ok_or(PredictionError::ArithmeticOverflow)?;
        position.outcome_amounts.set(outcome_index, new_amount);

        // Update average entry price
        let total_cost = current_entry
            .checked_mul(current_amount)
            .ok_or(PredictionError::ArithmeticOverflow)?
            .checked_add(
                price
                    .checked_mul(amount)
                    .ok_or(PredictionError::ArithmeticOverflow)?,
            )
            .ok_or(PredictionError::ArithmeticOverflow)?;

        let new_entry = if new_amount > 0 {
            total_cost
                .checked_div(new_amount)
                .ok_or(PredictionError::ArithmeticOverflow)?
        } else {
            0
        };
        position.entry_prices.set(outcome_index, new_entry);

        position.total_spent = position
            .total_spent
            .checked_add(
                price
                    .checked_mul(amount)
                    .ok_or(PredictionError::ArithmeticOverflow)?,
            )
            .ok_or(PredictionError::ArithmeticOverflow)?;
    } else {
        // Selling outcome tokens
        let new_amount = current_amount
            .checked_sub(amount)
            .ok_or(PredictionError::InsufficientBalance)?;
        position.outcome_amounts.set(outcome_index, new_amount);

        position.total_spent = position
            .total_spent
            .checked_sub(
                price
                    .checked_mul(amount)
                    .ok_or(PredictionError::ArithmeticOverflow)?,
            )
            .unwrap_or(0);
    }

    save_position(env, &position);
    Ok(())
}

// ---------------------------------------------------------------------------
// Settlement
// ---------------------------------------------------------------------------

/// Redeem winning outcome tokens for collateral.
///
/// After market resolution (and dispute resolution if applicable), holders
/// of the winning outcome token can redeem their tokens at a 1:1 rate
/// with collateral.
pub fn redeem_winning_tokens(
    env: &Env,
    market: &Market,
    user: &Symbol,
    amount: i128,
) -> Result<i128, PredictionError> {
    // Market must be resolved
    if market.status != MarketStatus::Resolved {
        return Err(PredictionError::MarketNotResolved);
    }

    let winning_outcome = market
        .resolved_outcome
        .ok_or(PredictionError::MarketNotResolved)?;

    if amount <= 0 {
        return Err(PredictionError::InvalidOrderAmount);
    }

    // Get user's position
    let mut position = get_position(env, market.market_id, user)
        .ok_or(PredictionError::InsufficientBalance)?;

    if position.settled {
        return Err(PredictionError::MarketAlreadyResolved);
    }

    // Check user has enough winning outcome tokens
    let user_balance = position
        .outcome_amounts
        .get(winning_outcome)
        .unwrap_or(0);

    if user_balance < amount {
        return Err(PredictionError::InsufficientBalance);
    }

    // Calculate payout (1:1 with collateral)
    let payout = amount;

    // Deduct redeemed tokens
    let new_balance = user_balance
        .checked_sub(amount)
        .ok_or(PredictionError::ArithmeticOverflow)?;
    position.outcome_amounts.set(winning_outcome, new_balance);

    // Mark as settled if all outcome tokens are zero
    let mut all_zero = true;
    for i in 0..position.outcome_amounts.len() {
        if position.outcome_amounts.get(i).unwrap_or(0) > 0 {
            all_zero = false;
            break;
        }
    }

    if all_zero {
        position.settled = true;
        // Calculate PnL: payout - total_spent
        position.pnl = Some(
            payout
                .checked_sub(position.total_spent)
                .unwrap_or(-position.total_spent),
        );
    }

    save_position(env, &position);

    Ok(payout)
}

/// Calculate the total payout for a user if they redeem all their winning tokens.
pub fn calculate_payout(
    market: &Market,
    position: &Position,
) -> Result<i128, PredictionError> {
    let winning_outcome = market
        .resolved_outcome
        .ok_or(PredictionError::MarketNotResolved)?;

    let amount = position
        .outcome_amounts
        .get(winning_outcome)
        .unwrap_or(0);

    // 1:1 redemption
    Ok(amount)
}

/// Settle a user's entire position (redeem all winning tokens at once).
pub fn settle_position(
    env: &Env,
    market: &Market,
    user: &Symbol,
) -> Result<i128, PredictionError> {
    let mut position = get_position(env, market.market_id, user)
        .ok_or(PredictionError::InsufficientBalance)?;

    if position.settled {
        return Err(PredictionError::MarketAlreadyResolved);
    }

    let payout = calculate_payout(market, &position)?;

    if payout > 0 {
        // Deduct all winning tokens
        let winning_outcome = market.resolved_outcome.unwrap();
        position.outcome_amounts.set(winning_outcome, 0);
    }

    // Calculate PnL
    position.pnl = Some(
        payout
            .checked_sub(position.total_spent)
            .unwrap_or(-position.total_spent),
    );
    position.settled = true;

    save_position(env, &position);

    Ok(payout)
}

/// Get all positions for a market.
pub fn get_market_positions(
    env: &Env,
    market_id: u64,
    users: &Vec<Symbol>,
) -> Vec<Position> {
    let mut positions = Vec::new(env);
    for i in 0..users.len() {
        let user = users.get(i).unwrap();
        if let Some(pos) = get_position(env, market_id, &user) {
            positions.push_back(pos);
        }
    }
    positions
}

// ---------------------------------------------------------------------------
// Liquidity pool settlement
// ---------------------------------------------------------------------------

/// Distribute accumulated fees to LP token holders proportionally.
///
/// Called after market resolution to distribute trading fees.
pub fn distribute_lp_fees(
    env: &Env,
    pool: &mut LiquidityPool,
) -> Result<i128, PredictionError> {
    let fees = pool.fees_accumulated;
    if fees <= 0 {
        return Ok(0);
    }

    // Fees are added to the collateral reserve, benefiting LP holders
    pool.collateral_reserve = pool
        .collateral_reserve
        .checked_add(fees)
        .ok_or(PredictionError::ArithmeticOverflow)?;

    pool.fees_accumulated = 0;

    // Update k for all outcomes
    let num_outcomes = pool.outcome_reserves.len();
    for i in 0..num_outcomes {
        let outcome_reserve = pool.outcome_reserves.get(i).unwrap_or(0);
        let new_k = pool
            .collateral_reserve
            .checked_mul(outcome_reserve)
            .ok_or(PredictionError::ArithmeticOverflow)?;
        pool.k_per_outcome.set(i, new_k);
    }

    Ok(fees)
}
