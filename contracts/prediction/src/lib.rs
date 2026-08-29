#![no_std]
#![allow(clippy::too_many_arguments)]
//! # AstraPort Prediction Markets Contract
//!
//! Binary and multi-outcome event prediction markets with automated
//! market-making (AMM), outcome resolution through oracles, dispute
//! mechanisms, and settlement.
//!
//! ## Module overview
//!
//! - [`types`] — Data structures: `Market`, `Outcome`, `LiquidityPool`,
//!   `PredictionOrder`, `Position`, error enum, etc.
//! - [`cpmm`] — Constant Product Market Maker for outcome token pricing.
//! - [`orderbook`] — Order book management for direct outcome token trading.
//! - [`oracle`] — Oracle-based resolution and dispute mechanism.
//! - [`settlement`] — Market resolution, token redemption, and LP fee
//!   distribution.

use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, Symbol, Vec};

pub mod cpmm;
pub mod orderbook;
pub mod oracle;
pub mod settlement;
pub mod types;

use crate::cpmm as pool;
use crate::orderbook as ob;
use crate::settlement as settle;
use crate::types::*;

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// AstraPort Prediction Markets — binary and multi-outcome event trading
/// with CPMM AMM.
#[contract]
pub struct PredictionMarket;

#[contractimpl]
impl PredictionMarket {
    // =======================================================================
    // Lifecycle
    // =======================================================================

    /// Initialize the prediction market contract with an admin address.
    ///
    /// Can only be called once.
    pub fn initialize(env: Env, admin: Address) -> Result<Symbol, PredictionError> {
        let storage = env.storage().persistent();
        if storage.has(&PredictionDataKey::Admin) {
            return Err(PredictionError::AlreadyInitialized);
        }
        storage.set(&PredictionDataKey::Admin, &admin);
        Ok(symbol_short!("ok"))
    }

    // =======================================================================
    // Admin helpers
    // =======================================================================

    /// Assert that the caller is the admin.
    fn assert_admin(env: &Env, caller: &Address) -> Result<(), PredictionError> {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Admin)
            .ok_or(PredictionError::Unauthorized)?;
        if *caller != admin {
            return Err(PredictionError::Unauthorized);
        }
        Ok(())
    }

    // =======================================================================
    // Market creation & management
    // =======================================================================

    /// Create a new prediction market.
    ///
    /// Supports binary (2 outcomes) and multi-outcome (up to 10) markets.
    pub fn create_market(
        env: Env,
        creator: Address,
        description: Symbol,
        category: MarketCategory,
        outcomes: Vec<Outcome>,
        trading_end_time: u64,
        resolution_time: u64,
        oracle_source: Symbol,
        max_outcome_supply: i128,
        fee_bps: i128,
        allow_early_close: bool,
    ) -> Result<u64, PredictionError> {
        creator.require_auth();

        let num_outcomes = outcomes.len();
        if num_outcomes < 2 || num_outcomes > MAX_OUTCOMES_PER_MARKET {
            return Err(PredictionError::TooManyOutcomes);
        }

        let now = env.ledger().timestamp();
        if trading_end_time <= now || resolution_time <= trading_end_time {
            return Err(PredictionError::InvalidMarketParams);
        }

        let market_id = next_market_id(&env);

        let market = Market {
            market_id,
            description,
            category,
            outcomes,
            collateral_token: symbol_short!("USDC"),
            status: MarketStatus::Active,
            created_at: now,
            trading_end_time,
            resolution_time,
            oracle_source: oracle_source.clone(),
            resolved_outcome: None,
            creator: creator.clone(),
            max_outcome_supply,
            total_collateral: 0,
            fee_bps,
            allow_early_close,
            resolved_at: None,
        };

        // Store the market
        env.storage()
            .persistent()
            .set(&PredictionDataKey::Market(market_id), &market);

        // Add to market list
        let mut market_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::MarketList)
            .unwrap_or_else(|| Vec::new(&env));
        market_ids.push_back(market_id);
        env.storage()
            .persistent()
            .set(&PredictionDataKey::MarketList, &market_ids);

        // Add to category index
        add_market_to_category(&env, &market.category, market_id);

        // Set oracle source
        let source = OracleSource {
            provider_id: oracle_source,
            feed_id: symbol_short!("default"),
            max_staleness: 300,
            is_active: true,
        };
        let _ = oracle::set_oracle_source(&env, market_id, &source);

        env.events().publish(
            (symbol_short!("MKT_ADD"), market_id),
            (description, creator),
        );

        Ok(market_id)
    }

    /// Get a market by ID.
    pub fn get_market(env: Env, market_id: u64) -> Option<Market> {
        env.storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
    }

    /// Get all market IDs.
    pub fn get_all_market_ids(env: Env) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&PredictionDataKey::MarketList)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get markets by category.
    pub fn get_markets_by_category(env: Env, category: MarketCategory) -> Vec<u64> {
        let key = PredictionDataKey::MarketsByCategory(category);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get the total number of markets.
    pub fn get_market_count(env: Env) -> u32 {
        let ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::MarketList)
            .unwrap_or_else(|| Vec::new(&env));
        ids.len()
    }

    // =======================================================================
    // Liquidity provision
    // =======================================================================

    /// Create the liquidity pool for a market with initial seed liquidity.
    pub fn create_liquidity_pool(
        env: Env,
        creator: Address,
        market_id: u64,
        initial_collateral: i128,
    ) -> Result<LPResult, PredictionError> {
        creator.require_auth();

        let market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        if !market.is_tradable() {
            return Err(PredictionError::MarketNotTradable);
        }

        // Create the pool
        let mut lp_pool = pool::create_pool(
            &env,
            market_id,
            market.outcomes.len(),
            initial_collateral,
        )?;

        let lp_tokens = initial_collateral;

        // Store LP balance
        let key = PredictionDataKey::LPBalance(market_id, creator.clone());
        let existing_lp: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(
            &key,
            &(existing_lp
                .checked_add(lp_tokens)
                .ok_or(PredictionError::ArithmeticOverflow)?),
        );

        // Update total LP supply
        let supply_key = PredictionDataKey::LPTotalSupply(market_id);
        let current_supply: i128 = env.storage().persistent().get(&supply_key).unwrap_or(0);
        env.storage().persistent().set(
            &supply_key,
            &(current_supply
                .checked_add(lp_tokens)
                .ok_or(PredictionError::ArithmeticOverflow)?),
        );

        // Update market total collateral
        let mut market = market;
        market.total_collateral = initial_collateral;
        env.storage()
            .persistent()
            .set(&PredictionDataKey::Market(market_id), &market);

        // Create outcome amount vector
        let mut outcome_amounts = Vec::new(&env);
        let mut outcome_deposits = Vec::new(&env);
        for _ in 0..market.outcomes.len() {
            outcome_amounts.push_back(initial_collateral);
            outcome_deposits.push_back(initial_collateral);
        }

        env.events().publish(
            (symbol_short!("LP_ADD"), market_id, creator),
            (initial_collateral, lp_tokens),
        );

        Ok(LPResult {
            lp_tokens_minted: lp_tokens,
            collateral_deposited: initial_collateral,
            outcome_deposits,
        })
    }

    /// Add liquidity to an existing pool.
    pub fn add_liquidity(
        env: Env,
        provider: Address,
        market_id: u64,
        collateral_amount: i128,
    ) -> Result<LPResult, PredictionError> {
        provider.require_auth();

        let market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        if !market.is_tradable() {
            return Err(PredictionError::MarketNotTradable);
        }

        let mut lp_pool = pool::load_pool(&env, market_id)?;

        // Create proportional outcome amounts
        let mut outcome_amounts = Vec::new(&env);
        let num_outcomes = lp_pool.outcome_reserves.len();
        for _ in 0..num_outcomes {
            outcome_amounts.push_back(collateral_amount);
        }

        let result = pool::add_liquidity(&env, &mut lp_pool, collateral_amount, outcome_amounts)?;
        pool::save_pool(&env, &lp_pool);

        // Update LP balance
        let key = PredictionDataKey::LPBalance(market_id, provider.clone());
        let existing_lp: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(
            &key,
            &(existing_lp
                .checked_add(result.lp_tokens_minted)
                .ok_or(PredictionError::ArithmeticOverflow)?),
        );

        // Update total LP supply
        let supply_key = PredictionDataKey::LPTotalSupply(market_id);
        let current_supply: i128 = env.storage().persistent().get(&supply_key).unwrap_or(0);
        env.storage().persistent().set(
            &supply_key,
            &(current_supply
                .checked_add(result.lp_tokens_minted)
                .ok_or(PredictionError::ArithmeticOverflow)?),
        );

        // Update market total collateral
        let mut market = market;
        market.total_collateral = market
            .total_collateral
            .checked_add(collateral_amount)
            .ok_or(PredictionError::ArithmeticOverflow)?;
        env.storage()
            .persistent()
            .set(&PredictionDataKey::Market(market_id), &market);

        env.events().publish(
            (symbol_short!("LP_ADD"), market_id, provider),
            (collateral_amount, result.lp_tokens_minted),
        );

        Ok(result)
    }

    /// Remove liquidity from a pool.
    pub fn remove_liquidity(
        env: Env,
        provider: Address,
        market_id: u64,
        lp_amount: i128,
    ) -> Result<LPResult, PredictionError> {
        provider.require_auth();

        let mut lp_pool = pool::load_pool(&env, market_id)?;
        let result = pool::remove_liquidity(&env, &mut lp_pool, lp_amount)?;
        pool::save_pool(&env, &lp_pool);

        // Update LP balance
        let key = PredictionDataKey::LPBalance(market_id, provider.clone());
        let existing_lp: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new_lp = existing_lp
            .checked_sub(lp_amount)
            .ok_or(PredictionError::InsufficientBalance)?;
        env.storage().persistent().set(&key, &new_lp);

        // Update total LP supply
        let supply_key = PredictionDataKey::LPTotalSupply(market_id);
        let current_supply: i128 = env.storage().persistent().get(&supply_key).unwrap_or(0);
        env.storage().persistent().set(
            &supply_key,
            &(current_supply
                .checked_sub(lp_amount)
                .ok_or(PredictionError::ArithmeticOverflow)?),
        );

        env.events().publish(
            (symbol_short!("LP_RMV"), market_id, provider),
            (lp_amount, result.collateral_deposited),
        );

        Ok(result)
    }

    /// Get the LP token balance for a user in a market.
    pub fn get_lp_balance(env: Env, market_id: u64, user: Address) -> i128 {
        let key = PredictionDataKey::LPBalance(market_id, user);
        env.storage().persistent().get(&key).unwrap_or(0)
    }

    // =======================================================================
    // CPMM trading
    // =======================================================================

    /// Buy outcome tokens from the CPMM pool using collateral.
    pub fn buy_outcome(
        env: Env,
        buyer: Address,
        market_id: u64,
        outcome_index: u32,
        collateral_amount: i128,
        min_outcome_tokens: i128,
    ) -> Result<SwapResult, PredictionError> {
        buyer.require_auth();

        let market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        let now = env.ledger().timestamp();
        if !market.is_trading_open(now) {
            return Err(PredictionError::MarketNotTradable);
        }

        if outcome_index >= market.outcomes.len() {
            return Err(PredictionError::InvalidOutcomeIndex);
        }

        // Check market cap
        let lp_pool = pool::load_pool(&env, market_id)?;
        let total_outcome_supply: i128 = {
            let mut total = 0i128;
            for i in 0..lp_pool.outcome_reserves.len() {
                total = total
                    .checked_add(lp_pool.outcome_reserves.get(i).unwrap_or(0))
                    .ok_or(PredictionError::ArithmeticOverflow)?;
            }
            total
        };
        if market.max_outcome_supply > 0
            && total_outcome_supply
                .checked_add(collateral_amount)
                .ok_or(PredictionError::ArithmeticOverflow)?
                > market.max_outcome_supply
        {
            return Err(PredictionError::MarketCapReached);
        }

        let mut lp_pool = lp_pool;
        let result = pool::buy_outcome_tokens(
            &env,
            &mut lp_pool,
            outcome_index,
            collateral_amount,
            market.fee_bps,
            min_outcome_tokens,
        )?;
        pool::save_pool(&env, &lp_pool);

        // Update buyer's outcome token balance
        let balance_key = PredictionDataKey::OutcomeBalance(
            market_id,
            buyer.clone(),
            outcome_index,
        );
        let current_balance: i128 = env
            .storage()
            .persistent()
            .get(&balance_key)
            .unwrap_or(0);
        env.storage().persistent().set(
            &balance_key,
            &(current_balance
                .checked_add(result.outcome_amount)
                .ok_or(PredictionError::ArithmeticOverflow)?),
        );

        // Update position tracking
        let _ = settle::record_trade(
            &env,
            market_id,
            &symbol_short!("pos"),
            outcome_index,
            true,
            result.outcome_amount,
            result.collateral_amount / result.outcome_amount.max(1),
        );

        // Update trading volume
        let vol_key = PredictionDataKey::TradingVolume(market_id);
        let current_vol: i128 = env.storage().persistent().get(&vol_key).unwrap_or(0);
        env.storage().persistent().set(
            &vol_key,
            &(current_vol
                .checked_add(collateral_amount)
                .ok_or(PredictionError::ArithmeticOverflow)?),
        );

        env.events().publish(
            (symbol_short!("BUY"), market_id, buyer),
            (outcome_index, result.outcome_amount, result.collateral_amount),
        );

        Ok(result)
    }

    /// Sell outcome tokens for collateral via the CPMM pool.
    pub fn sell_outcome(
        env: Env,
        seller: Address,
        market_id: u64,
        outcome_index: u32,
        outcome_tokens: i128,
        min_collateral: i128,
    ) -> Result<SwapResult, PredictionError> {
        seller.require_auth();

        let market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        let now = env.ledger().timestamp();
        if !market.is_trading_open(now) {
            return Err(PredictionError::MarketNotTradable);
        }

        if outcome_index >= market.outcomes.len() {
            return Err(PredictionError::InvalidOutcomeIndex);
        }

        // Check seller has enough outcome tokens
        let balance_key = PredictionDataKey::OutcomeBalance(
            market_id,
            seller.clone(),
            outcome_index,
        );
        let current_balance: i128 = env
            .storage()
            .persistent()
            .get(&balance_key)
            .unwrap_or(0);
        if current_balance < outcome_tokens {
            return Err(PredictionError::InsufficientBalance);
        }

        let mut lp_pool = pool::load_pool(&env, market_id)?;
        let result = pool::sell_outcome_tokens(
            &env,
            &mut lp_pool,
            outcome_index,
            outcome_tokens,
            market.fee_bps,
            min_collateral,
        )?;
        pool::save_pool(&env, &lp_pool);

        // Deduct outcome tokens from seller
        env.storage().persistent().set(
            &balance_key,
            &(current_balance
                .checked_sub(outcome_tokens)
                .ok_or(PredictionError::ArithmeticOverflow)?),
        );

        // Update trading volume
        let vol_key = PredictionDataKey::TradingVolume(market_id);
        let current_vol: i128 = env.storage().persistent().get(&vol_key).unwrap_or(0);
        env.storage().persistent().set(
            &vol_key,
            &(current_vol
                .checked_add(result.collateral_amount)
                .ok_or(PredictionError::ArithmeticOverflow)?),
        );

        env.events().publish(
            (symbol_short!("SELL"), market_id, seller),
            (outcome_index, outcome_tokens, result.collateral_amount),
        );

        Ok(result)
    }

    /// Get the CPMM price for a specific outcome.
    pub fn get_outcome_price(env: Env, market_id: u64, outcome_index: u32) -> Result<i128, PredictionError> {
        let lp_pool = pool::load_pool(&env, market_id)?;
        pool::get_outcome_price(&lp_pool, outcome_index)
    }

    /// Get CPMM prices for all outcomes in a market.
    pub fn get_all_prices(env: Env, market_id: u64) -> Result<Vec<i128>, PredictionError> {
        let lp_pool = pool::load_pool(&env, market_id)?;
        pool::get_all_outcome_prices(&lp_pool)
    }

    /// Verify the CPMM invariant for a market's pool.
    pub fn verify_pool_invariant(env: Env, market_id: u64) -> Result<bool, PredictionError> {
        let lp_pool = pool::load_pool(&env, market_id)?;
        Ok(pool::verify_invariant(&lp_pool))
    }

    /// Get the outcome token balance for a user.
    pub fn get_outcome_balance(
        env: Env,
        market_id: u64,
        user: Address,
        outcome_index: u32,
    ) -> i128 {
        let key = PredictionDataKey::OutcomeBalance(market_id, user, outcome_index);
        env.storage().persistent().get(&key).unwrap_or(0)
    }

    // =======================================================================
    // Order book trading
    // =======================================================================

    /// Place a limit order on the outcome token order book.
    pub fn place_order(
        env: Env,
        owner: Address,
        market_id: u64,
        outcome_index: u32,
        side: OrderSide,
        price: i128,
        amount: i128,
    ) -> Result<u64, PredictionError> {
        owner.require_auth();

        let market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        let now = env.ledger().timestamp();
        if !market.is_trading_open(now) {
            return Err(PredictionError::MarketNotTradable);
        }

        if outcome_index >= market.outcomes.len() {
            return Err(PredictionError::InvalidOutcomeIndex);
        }

        let order_id = ob::place_order(&env, market_id, &owner, outcome_index, side, price, amount)?;

        env.events().publish(
            (symbol_short!("ORD_ADD"), market_id, order_id),
            (outcome_index, side, price, amount),
        );

        Ok(order_id)
    }

    /// Cancel an existing order.
    pub fn cancel_order(
        env: Env,
        caller: Address,
        market_id: u64,
        order_id: u64,
    ) -> Result<Symbol, PredictionError> {
        caller.require_auth();

        let is_admin = Self::assert_admin(&env, &caller).is_ok();
        ob::cancel_order(&env, market_id, order_id, &caller, is_admin)?;

        env.events().publish(
            (symbol_short!("ORD_CNCL"), market_id, order_id),
            caller,
        );

        Ok(symbol_short!("ok"))
    }

    /// Get a specific order by market and ID.
    pub fn get_order(env: Env, market_id: u64, order_id: u64) -> Option<PredictionOrder> {
        ob::get_order(&env, market_id, order_id)
    }

    /// Get the order book snapshot for a market outcome.
    pub fn get_order_book(env: Env, market_id: u64, outcome_index: u32) -> OrderBookSnapshot {
        ob::get_book_snapshot(&env, market_id, outcome_index)
    }

    // =======================================================================
    // Oracle resolution & disputes
    // =======================================================================

    /// Submit an oracle resolution for a market.
    pub fn submit_resolution(
        env: Env,
        market_id: u64,
        oracle_provider: Symbol,
        resolved_outcome: u32,
    ) -> Result<ResolutionData, PredictionError> {
        let mut market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        let result = oracle::submit_resolution(&env, &mut market, oracle_provider, resolved_outcome)?;

        // Persist the updated market
        env.storage()
            .persistent()
            .set(&PredictionDataKey::Market(market_id), &market);

        env.events().publish(
            (symbol_short!("RESOLVE"), market_id),
            (resolved_outcome, result.submitted_at),
        );

        Ok(result)
    }

    /// File a dispute against a market resolution.
    pub fn file_dispute(
        env: Env,
        disputer: Address,
        market_id: u64,
        claimed_outcome: u32,
        evidence: Symbol,
        bond_amount: i128,
    ) -> Result<Dispute, PredictionError> {
        disputer.require_auth();

        let market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        let result = oracle::file_dispute(
            &env,
            &market,
            &symbol_short!("disc"),
            claimed_outcome,
            evidence,
            bond_amount,
        )?;

        // Update market status to disputed
        let mut market = market;
        market.status = MarketStatus::Disputed;
        env.storage()
            .persistent()
            .set(&PredictionDataKey::Market(market_id), &market);

        env.events().publish(
            (symbol_short!("DISPUTE"), market_id),
            (claimed_outcome, bond_amount),
        );

        Ok(result)
    }

    /// Resolve a dispute. Admin only.
    pub fn resolve_dispute(
        env: Env,
        admin: Address,
        market_id: u64,
        accepted: bool,
    ) -> Result<Dispute, PredictionError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let mut market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        let result = oracle::resolve_dispute(&env, &mut market, accepted)?;

        // Market stays in Resolved state (dispute resolution doesn't change status)
        if accepted {
            market.status = MarketStatus::Resolved;
        } else {
            // Revert to resolved state
            market.status = MarketStatus::Resolved;
        }
        env.storage()
            .persistent()
            .set(&PredictionDataKey::Market(market_id), &market);

        env.events().publish(
            (symbol_short!("DSP_RES"), market_id),
            (accepted, result.claimed_outcome),
        );

        Ok(result)
    }

    /// Get the dispute for a market.
    pub fn get_dispute(env: Env, market_id: u64) -> Option<Dispute> {
        oracle::get_dispute(&env, market_id)
    }

    /// Get the resolution data for a market.
    pub fn get_resolution(env: Env, market_id: u64) -> Option<ResolutionData> {
        oracle::get_resolution_data(&env, market_id)
    }

    // =======================================================================
    // Market transitions
    // =======================================================================

    /// Transition a market to PendingResolution (called when trading ends).
    pub fn finalize_trading(env: Env, market_id: u64) -> Result<Symbol, PredictionError> {
        let mut market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        oracle::transition_to_pending_resolution(&env, &mut market)?;

        env.storage()
            .persistent()
            .set(&PredictionDataKey::Market(market_id), &market);

        env.events()
            .publish((symbol_short!("TRD_END"), market_id), ());

        Ok(symbol_short!("ok"))
    }

    /// Early close a market (admin or creator only).
    pub fn early_close(
        env: Env,
        caller: Address,
        market_id: u64,
    ) -> Result<Symbol, PredictionError> {
        caller.require_auth();

        let mut market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        // Only creator or admin can early close
        let is_admin = Self::assert_admin(&env, &caller).is_ok();
        if !is_admin && market.creator != caller {
            return Err(PredictionError::Unauthorized);
        }

        oracle::early_close_market(&env, &mut market)?;

        env.storage()
            .persistent()
            .set(&PredictionDataKey::Market(market_id), &market);

        env.events()
            .publish((symbol_short!("MKT_CLS"), market_id), caller);

        Ok(symbol_short!("ok"))
    }

    /// Cancel a market (admin only, no trades allowed).
    pub fn cancel_market(
        env: Env,
        admin: Address,
        market_id: u64,
    ) -> Result<Symbol, PredictionError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let mut market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        oracle::cancel_market(&env, &mut market)?;

        env.storage()
            .persistent()
            .set(&PredictionDataKey::Market(market_id), &market);

        env.events()
            .publish((symbol_short!("MKT_CNCL"), market_id), admin);

        Ok(symbol_short!("ok"))
    }

    // =======================================================================
    // Settlement & redemption
    // =======================================================================

    /// Redeem winning outcome tokens for collateral at 1:1 rate.
    pub fn redeem_tokens(
        env: Env,
        user: Address,
        market_id: u64,
        amount: i128,
    ) -> Result<i128, PredictionError> {
        user.require_auth();

        let market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        let symbol_user = symbol_short!("usr");
        let payout = settle::redeem_winning_tokens(&env, &market, &symbol_user, amount)?;

        env.events().publish(
            (symbol_short!("REDEEM"), market_id, user),
            (payout, amount),
        );

        Ok(payout)
    }

    /// Settle a user's entire position (redeem all winning tokens).
    pub fn settle_position(
        env: Env,
        user: Address,
        market_id: u64,
    ) -> Result<i128, PredictionError> {
        user.require_auth();

        let market: Market = env
            .storage()
            .persistent()
            .get(&PredictionDataKey::Market(market_id))
            .ok_or(PredictionError::MarketNotFound)?;

        let symbol_user = symbol_short!("usr");
        let payout = settle::settle_position(&env, &market, &symbol_user)?;

        env.events().publish(
            (symbol_short!("SETTLE"), market_id, user),
            payout,
        );

        Ok(payout)
    }

    /// Distribute accumulated LP fees after market resolution.
    pub fn distribute_fees(
        env: Env,
        admin: Address,
        market_id: u64,
    ) -> Result<i128, PredictionError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let mut lp_pool = pool::load_pool(&env, market_id)?;
        let distributed = settle::distribute_lp_fees(&env, &mut lp_pool)?;
        pool::save_pool(&env, &lp_pool);

        env.events()
            .publish((symbol_short!("FEES_DST"), market_id), distributed);

        Ok(distributed)
    }

    // =======================================================================
    // Statistics & queries
    // =======================================================================

    /// Get the liquidity pool for a market.
    pub fn get_liquidity_pool(env: Env, market_id: u64) -> Result<LiquidityPool, PredictionError> {
        pool::load_pool(&env, market_id)
    }

    /// Get the trading volume for a market.
    pub fn get_trading_volume(env: Env, market_id: u64) -> i128 {
        let key = PredictionDataKey::TradingVolume(market_id);
        env.storage().persistent().get(&key).unwrap_or(0)
    }

    /// Get the user's position in a market.
    pub fn get_user_position(
        env: Env,
        market_id: u64,
        user: Address,
    ) -> Option<Position> {
        let symbol_user = symbol_short!("usr");
        settle::get_position(&env, market_id, &symbol_user)
    }

    /// Check if a market's pool CPMM invariant is maintained.
    pub fn check_invariant(env: Env, market_id: u64) -> bool {
        match pool::load_pool(&env, market_id) {
            Ok(lp_pool) => pool::verify_invariant(&lp_pool),
            Err(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Get the next market ID and increment the counter.
fn next_market_id(env: &Env) -> u64 {
    let key = PredictionDataKey::MarketIdCounter;
    let current: u64 = env.storage().persistent().get(&key).unwrap_or(1);
    env.storage().persistent().set(&key, &(current + 1));
    current
}

/// Add a market to a category index.
fn add_market_to_category(env: &Env, category: &MarketCategory, market_id: u64) {
    let key = PredictionDataKey::MarketsByCategory(*category);
    let mut ids: Vec<u64> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env));
    ids.push_back(market_id);
    env.storage().persistent().set(&key, &ids);
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests;
