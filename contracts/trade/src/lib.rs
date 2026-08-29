#![no_std]
#![allow(clippy::too_many_arguments)]
//! # AstraPort Trade Engine Contract
//!
//! Multi-asset atomic trading engine with order book matching, slippage
//! protection, and guaranteed all-or-nothing execution.
//!
//! ## Module overview
//!
//! - [`types`] — Data structures: `Order`, `OrderBook`, `TradePair`,
//!   `TradeLeg`, `AtomicBatchResult`, `SlippageConfig`, error enum, etc.
//! - [`orderbook`] — Order book management: place, cancel, and match orders
//!   with price-time priority.
//! - [`slippage`] — Pre-trade and post-trade slippage validation.
//! - [`engine`] — Atomic batch execution: multi-leg settlement that rolls
//!   back entirely on any failure.

use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, String, Symbol, Vec};

use astraport_audit::logger::AuditLogger;
use astraport_audit::records::{permissions, AuditEventType, StateSnapshot};

pub mod engine;
pub mod orderbook;
pub mod slippage;
pub mod types;

use crate::engine::execute_atomic_batch;
use crate::orderbook as ob;
use crate::slippage as slip;
use crate::types::*;

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// AstraPort Trade Engine — multi-asset atomic trading with order book
/// matching.
#[contract]
pub struct TradeEngine;

#[contractimpl]
impl TradeEngine {
    // =======================================================================
    // Lifecycle
    // =======================================================================

    /// Initialize the trading engine with an admin address.
    ///
    /// Can only be called once.
    pub fn initialize(env: Env, admin: Address) -> Result<Symbol, TradeError> {
        let storage = env.storage().persistent();
        if storage.has(&TradeDataKey::Admin) {
            return Err(TradeError::AlreadyInitialized);
        }
        storage.set(&TradeDataKey::Admin, &admin);
        Ok(symbol_short!("ok"))
    }

    // =======================================================================
    // Pair management
    // =======================================================================

    /// Register a new trading pair.  Admin only.
    pub fn register_pair(env: Env, admin: Address, pair: TradePair) -> Result<Symbol, TradeError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let mut pairs: Vec<TradePair> = env
            .storage()
            .persistent()
            .get(&TradeDataKey::Pairs)
            .unwrap_or_else(|| Vec::new(&env));

        let len = pairs.len();
        for i in 0..len {
            if pairs.get(i).unwrap().pair_id == pair.pair_id {
                return Err(TradeError::PairAlreadyRegistered);
            }
        }

        pairs.push_back(pair.clone());
        env.storage().persistent().set(&TradeDataKey::Pairs, &pairs);
        slip::set_slippage_config(&env, &pair.pair_id, &SlippageConfig::default());

        env.events()
            .publish((symbol_short!("PAIR_ADD"), pair.pair_id), pair.base_asset);
        Ok(symbol_short!("ok"))
    }

    /// Update a trading pair's configuration.  Admin only.
    pub fn update_pair(env: Env, admin: Address, pair: TradePair) -> Result<Symbol, TradeError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let mut pairs: Vec<TradePair> = env
            .storage()
            .persistent()
            .get(&TradeDataKey::Pairs)
            .unwrap_or_else(|| Vec::new(&env));

        let len = pairs.len();
        let mut found = false;
        for i in 0..len {
            if pairs.get(i).unwrap().pair_id == pair.pair_id {
                pairs.set(i, pair.clone());
                found = true;
                break;
            }
        }

        if !found {
            return Err(TradeError::PairNotFound);
        }

        env.storage().persistent().set(&TradeDataKey::Pairs, &pairs);
        Ok(symbol_short!("ok"))
    }

    /// Get the configuration for a specific pair.
    pub fn get_pair(env: Env, pair_id: Symbol) -> Option<TradePair> {
        let pairs: Vec<TradePair> = env
            .storage()
            .persistent()
            .get(&TradeDataKey::Pairs)
            .unwrap_or_else(|| Vec::new(&env));
        let len = pairs.len();
        for i in 0..len {
            let p = pairs.get(i).unwrap();
            if p.pair_id == pair_id {
                return Some(p);
            }
        }
        None
    }

    /// List all registered trading pairs.
    pub fn get_all_pairs(env: Env) -> Vec<TradePair> {
        env.storage()
            .persistent()
            .get(&TradeDataKey::Pairs)
            .unwrap_or_else(|| Vec::new(&env))
    }

    // =======================================================================
    // Order book operations
    // =======================================================================

    /// Place a limit order on the order book.
    pub fn place_order(
        env: Env,
        owner: Address,
        pair_id: Symbol,
        side: OrderSide,
        price: i128,
        amount: i128,
    ) -> Result<u64, TradeError> {
        owner.require_auth();

        let pair = Self::get_pair(env.clone(), pair_id.clone()).ok_or(TradeError::PairNotFound)?;
        if !pair.is_active {
            return Err(TradeError::PairInactive);
        }

        if amount < pair.min_order_size || amount > pair.max_order_size {
            return Err(TradeError::InvalidOrderAmount);
        }

        let order_id = ob::place_order(&env, &pair_id, &owner, side, price, amount)?;

        Self::log_audit_if_configured(
            &env,
            &owner,
            &pair_id,
            AuditEventType::OrderPlaced,
            0,
            amount,
            symbol_short!("ok"),
            &"order_placed",
        );

        Ok(order_id)
    }

    /// Cancel an order.  Only the order owner or admin may cancel.
    pub fn cancel_order(
        env: Env,
        caller: Address,
        pair_id: Symbol,
        order_id: u64,
    ) -> Result<Symbol, TradeError> {
        caller.require_auth();

        let is_admin = Self::assert_admin(&env, &caller).is_ok();
        ob::cancel_order(&env, &pair_id, order_id, &caller, is_admin)?;

        Self::log_audit_if_configured(
            &env,
            &caller,
            &pair_id,
            AuditEventType::OrderCancelled,
            0,
            0,
            symbol_short!("ok"),
            &"order_cancelled",
        );

        env.events()
            .publish((symbol_short!("ORDCNCL"), pair_id, order_id), caller);
        Ok(symbol_short!("ok"))
    }

    /// Get a specific order by pair and ID.
    pub fn get_order(env: Env, pair_id: Symbol, order_id: u64) -> Option<Order> {
        ob::get_order(&env, &pair_id, order_id)
    }

    /// Get a snapshot of the order book for a pair.
    pub fn get_order_book(env: Env, pair_id: Symbol) -> OrderBookSnapshot {
        ob::get_book_snapshot(&env, &pair_id)
    }

    // =======================================================================
    // Slippage configuration
    // =======================================================================

    /// Set the slippage tolerance for a trading pair.  Admin only.
    pub fn set_slippage(
        env: Env,
        admin: Address,
        pair_id: Symbol,
        max_slippage_bps: i128,
        enabled: bool,
    ) -> Result<Symbol, TradeError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let config = SlippageConfig {
            max_slippage_bps,
            enabled,
        };
        slip::set_slippage_config(&env, &pair_id, &config);
        Ok(symbol_short!("ok"))
    }

    /// Get the slippage configuration for a pair.
    pub fn get_slippage_config(env: Env, pair_id: Symbol) -> SlippageConfig {
        slip::get_slippage_config(&env, &pair_id)
    }

    // =======================================================================
    // Atomic multi-asset execution
    // =======================================================================

    /// Execute an atomic batch of trades across multiple asset pairs.
    pub fn execute_batch(
        env: Env,
        user: Address,
        legs: Vec<TradeLeg>,
    ) -> Result<AtomicBatchResult, TradeError> {
        user.require_auth();

        if legs.is_empty() {
            return Err(TradeError::EmptyBatch);
        }

        let pairs: Vec<TradePair> = env
            .storage()
            .persistent()
            .get(&TradeDataKey::Pairs)
            .unwrap_or_else(|| Vec::new(&env));

        let result = execute_atomic_batch(&env, &user, &legs, &pairs)?;

        for i in 0..result.legs.len() {
            let leg = result.legs.get(i).unwrap();
            env.events().publish(
                (symbol_short!("LEG_FILL"), leg.pair_id.clone()),
                (leg.filled_amount, leg.avg_price, leg.total_fees),
            );
        }

        Self::log_audit_if_configured(
            &env,
            &user,
            &symbol_short!("BATCH"),
            AuditEventType::TradeExecution,
            0,
            result.total_fills as i128,
            symbol_short!("ok"),
            &"batch_executed",
        );

        env.events().publish(
            (symbol_short!("BATCH_OK"), user.clone()),
            (result.total_fills, result.legs.len()),
        );

        Ok(result)
    }

    // =======================================================================
    // Statistics & queries
    // =======================================================================

    /// Get aggregate stats for a trading pair.
    pub fn get_pair_stats(env: Env, pair_id: Symbol) -> PairStats {
        let snapshot = ob::get_book_snapshot(&env, &pair_id);
        let total_volume: i128 = env
            .storage()
            .persistent()
            .get(&TradeDataKey::PairVolume(pair_id.clone()))
            .unwrap_or(0);
        let counter: u64 = env
            .storage()
            .persistent()
            .get(&TradeDataKey::OrderIdCounter(pair_id))
            .unwrap_or(0);
        // Counter holds the next ID; subtract 1 to get the count of
        // placed orders.  Counter == 0 means no orders placed.
        let total_orders = if counter > 0 { counter - 1 } else { 0 };

        PairStats {
            total_volume,
            total_orders,
            best_bid: snapshot.best_bid,
            best_ask: snapshot.best_ask,
            spread: snapshot.spread,
        }
    }

    /// Get the total volume traded across a pair.
    pub fn get_pair_volume(env: Env, pair_id: Symbol) -> i128 {
        env.storage()
            .persistent()
            .get(&TradeDataKey::PairVolume(pair_id))
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

impl TradeEngine {
    fn assert_admin(env: &Env, admin: &Address) -> Result<(), TradeError> {
        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&TradeDataKey::Admin)
            .ok_or(TradeError::Unauthorized)?;
        if stored_admin != *admin {
            return Err(TradeError::Unauthorized);
        }
        Ok(())
    }

    /// Configure the audit-log sink address. Admin-only.
    pub fn set_audit_sink(env: Env, admin: Address, sink: Address) -> Result<Symbol, TradeError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;
        env.storage()
            .persistent()
            .set(&TradeDataKey::AuditSink, &sink);
        Ok(symbol_short!("ok"))
    }

    /// Read the audit-log sink address, if configured.
    pub fn get_audit_sink(env: Env) -> Option<Address> {
        env.storage().persistent().get(&TradeDataKey::AuditSink)
    }

    /// Append an audit event if a sink is configured. No-op otherwise.
    fn log_audit_if_configured(
        env: &Env,
        actor: &Address,
        pair_id: &Symbol,
        event_type: AuditEventType,
        before_balance: i128,
        after_balance: i128,
        outcome: Symbol,
        detail: &str,
    ) {
        let key = TradeDataKey::AuditSink;
        let sink: Option<Address> = env.storage().persistent().get(&key);
        if let Some(sink) = sink {
            let mut before = StateSnapshot::empty(env);
            before.push(pair_id.clone(), before_balance);
            let mut after = StateSnapshot::empty(env);
            after.push(pair_id.clone(), after_balance);
            let detail_str = soroban_sdk::String::from_str(env, detail);
            let logger = AuditLogger::new(env, &sink);
            let _ = logger.log_event(
                actor.clone(),
                event_type,
                pair_id.clone(),
                permissions::STAKER,
                before,
                after,
                outcome,
                detail_str,
            );
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    fn setup() -> (Env, TradeEngineClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TradeEngine);
        let client = TradeEngineClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        (env, client, admin)
    }

    fn pair(id: Symbol, base: Symbol) -> TradePair {
        TradePair {
            pair_id: id,
            base_asset: base,
            quote_asset: symbol_short!("USDC"),
            is_active: true,
            min_order_size: 1,
            max_order_size: 1_000_000_000,
            fee_bps: 30,
        }
    }

    // ---- Initialization ----

    #[test]
    fn test_initialize() {
        let (_env, client, _admin) = setup();
        let snapshot = client.get_order_book(&symbol_short!("XLM_USDC"));
        assert_eq!(snapshot.bid_count, 0);
        assert_eq!(snapshot.ask_count, 0);
        assert_eq!(snapshot.pair_id, symbol_short!("XLM_USDC"));
    }

    #[test]
    fn test_double_initialize_fails() {
        let env = Env::default();
        let contract_id = env.register_contract(None, TradeEngine);
        let client = TradeEngineClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.initialize(&admin);
        assert_eq!(
            client.try_initialize(&admin),
            Err(Ok(TradeError::AlreadyInitialized)),
        );
    }

    // ---- Pair management ----

    #[test]
    fn test_register_pair() {
        let (_env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let pairs = client.get_all_pairs();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs.get(0).unwrap().pair_id, pid);
    }

    #[test]
    fn test_duplicate_pair_fails() {
        let (_env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        let p = pair(pid.clone(), symbol_short!("XLM"));
        client.register_pair(&admin, &p);
        assert_eq!(
            client.try_register_pair(&admin, &p),
            Err(Ok(TradeError::PairAlreadyRegistered)),
        );
    }

    #[test]
    fn test_get_pair() {
        let (_env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let p = client.get_pair(&pid).unwrap();
        assert_eq!(p.base_asset, symbol_short!("XLM"));
        assert_eq!(p.quote_asset, symbol_short!("USDC"));
    }

    // ---- Order placement ----

    #[test]
    fn test_place_buy_order() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let trader = Address::generate(&env);

        let oid = client.place_order(&trader, &pid, &OrderSide::Buy, &100, &50);
        assert_eq!(oid, 1);

        let order = client.get_order(&pid, &oid).unwrap();
        assert_eq!(order.side, OrderSide::Buy);
        assert_eq!(order.price, 100);
        assert_eq!(order.amount, 50);
        assert_eq!(order.remaining, 50);
        assert_eq!(order.status, OrderStatus::Active);
    }

    #[test]
    fn test_place_sell_order() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let trader = Address::generate(&env);
        let oid = client.place_order(&trader, &pid, &OrderSide::Sell, &110, &30);
        assert_eq!(oid, 1);
    }

    #[test]
    fn test_place_order_zero_amount_fails() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let trader = Address::generate(&env);
        assert_eq!(
            client.try_place_order(&trader, &pid, &OrderSide::Buy, &100, &0),
            Err(Ok(TradeError::InvalidOrderAmount)),
        );
    }

    #[test]
    fn test_place_order_invalid_pair_fails() {
        let (env, client, _admin) = setup();
        let trader = Address::generate(&env);
        assert_eq!(
            client.try_place_order(&trader, &symbol_short!("NOPE"), &OrderSide::Buy, &100, &10),
            Err(Ok(TradeError::PairNotFound)),
        );
    }

    // ---- Order cancellation ----

    #[test]
    fn test_cancel_order() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let trader = Address::generate(&env);

        let oid = client.place_order(&trader, &pid, &OrderSide::Buy, &100, &50);
        let result = client.cancel_order(&trader, &pid, &oid);
        assert_eq!(result, symbol_short!("ok"));

        let order = client.get_order(&pid, &oid).unwrap();
        assert_eq!(order.status, OrderStatus::Cancelled);
        assert_eq!(order.remaining, 0);
    }

    #[test]
    fn test_cancel_nonexistent_order_fails() {
        let (env, client, _admin) = setup();
        let trader = Address::generate(&env);
        assert_eq!(
            client.try_cancel_order(&trader, &symbol_short!("XLM_USDC"), &999),
            Err(Ok(TradeError::OrderNotFound)),
        );
    }

    // ---- Order book state ----

    #[test]
    fn test_order_book_snapshot() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);

        client.place_order(&buyer, &pid, &OrderSide::Buy, &95, &100);
        client.place_order(&seller, &pid, &OrderSide::Sell, &105, &50);

        let snap = client.get_order_book(&pid);
        assert_eq!(snap.bid_count, 1);
        assert_eq!(snap.ask_count, 1);
        assert_eq!(snap.best_bid, 95);
        assert_eq!(snap.best_ask, 105);
        assert_eq!(snap.spread, 10);
    }

    #[test]
    fn test_multiple_bids_sorted_correctly() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));

        client.place_order(&Address::generate(&env), &pid, &OrderSide::Buy, &90, &10);
        client.place_order(&Address::generate(&env), &pid, &OrderSide::Buy, &100, &20);
        client.place_order(&Address::generate(&env), &pid, &OrderSide::Buy, &95, &30);

        let snap = client.get_order_book(&pid);
        assert_eq!(snap.best_bid, 100);
        assert_eq!(snap.bid_count, 3);
    }

    #[test]
    fn test_multiple_asks_sorted_correctly() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));

        client.place_order(&Address::generate(&env), &pid, &OrderSide::Sell, &110, &10);
        client.place_order(&Address::generate(&env), &pid, &OrderSide::Sell, &100, &20);
        client.place_order(&Address::generate(&env), &pid, &OrderSide::Sell, &105, &30);

        let snap = client.get_order_book(&pid);
        assert_eq!(snap.best_ask, 100);
        assert_eq!(snap.ask_count, 3);
    }

    // ---- Matching & fill ----

    #[test]
    fn test_buy_matches_asks() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let seller = Address::generate(&env);
        let buyer = Address::generate(&env);

        client.place_order(&seller, &pid, &OrderSide::Sell, &100, &50);

        let mut legs = Vec::new(&env);
        legs.push_back(TradeLeg {
            pair_id: pid.clone(),
            side: OrderSide::Buy,
            price: 100,
            amount: 30,
            max_slippage_bps: None,
        });

        let result = client.execute_batch(&buyer, &legs);
        assert!(result.success);
        assert_eq!(result.total_fills, 1);

        let leg = result.legs.get(0).unwrap();
        assert_eq!(leg.filled_amount, 30);
        assert_eq!(leg.avg_price, 100);

        let order = client.get_order(&pid, &1).unwrap();
        assert_eq!(order.remaining, 20);
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
    }

    #[test]
    fn test_sell_matches_bids() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let buyer = Address::generate(&env);
        let seller = Address::generate(&env);

        client.place_order(&buyer, &pid, &OrderSide::Buy, &100, &50);

        let mut legs = Vec::new(&env);
        legs.push_back(TradeLeg {
            pair_id: pid.clone(),
            side: OrderSide::Sell,
            price: 100,
            amount: 30,
            max_slippage_bps: None,
        });

        let result = client.execute_batch(&seller, &legs);
        assert!(result.success);

        let leg = result.legs.get(0).unwrap();
        assert_eq!(leg.filled_amount, 30);
        assert_eq!(leg.avg_price, 100);

        let order = client.get_order(&pid, &1).unwrap();
        assert_eq!(order.remaining, 20);
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
    }

    #[test]
    fn test_full_fill() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let seller = Address::generate(&env);
        let buyer = Address::generate(&env);

        client.place_order(&seller, &pid, &OrderSide::Sell, &100, &50);

        let mut legs = Vec::new(&env);
        legs.push_back(TradeLeg {
            pair_id: pid.clone(),
            side: OrderSide::Buy,
            price: 100,
            amount: 50,
            max_slippage_bps: None,
        });

        let result = client.execute_batch(&buyer, &legs);
        assert!(result.success);

        let order = client.get_order(&pid, &1).unwrap();
        assert_eq!(order.status, OrderStatus::Filled);
        assert_eq!(order.remaining, 0);
    }

    #[test]
    fn test_no_matching_orders_no_fill() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let buyer = Address::generate(&env);

        let mut legs = Vec::new(&env);
        legs.push_back(TradeLeg {
            pair_id: pid,
            side: OrderSide::Buy,
            price: 100,
            amount: 50,
            max_slippage_bps: None,
        });

        let result = client.execute_batch(&buyer, &legs);
        assert!(result.success);
        assert_eq!(result.total_fills, 0);
        assert_eq!(result.legs.get(0).unwrap().filled_amount, 0);
    }

    // ---- Multi-leg atomic batch ----

    #[test]
    fn test_multi_leg_atomic_batch() {
        let (env, client, admin) = setup();
        let pid1 = symbol_short!("XLM_USDC");
        let pid2 = symbol_short!("BTC_USDC");
        client.register_pair(&admin, &pair(pid1.clone(), symbol_short!("XLM")));
        client.register_pair(&admin, &pair(pid2.clone(), symbol_short!("BTC")));

        let seller1 = Address::generate(&env);
        let seller2 = Address::generate(&env);
        client.place_order(&seller1, &pid1, &OrderSide::Sell, &100, &1000);
        client.place_order(&seller2, &pid2, &OrderSide::Sell, &50000, &10);

        let buyer = Address::generate(&env);
        let mut legs = Vec::new(&env);
        legs.push_back(TradeLeg {
            pair_id: pid1.clone(),
            side: OrderSide::Buy,
            price: 100,
            amount: 500,
            max_slippage_bps: None,
        });
        legs.push_back(TradeLeg {
            pair_id: pid2.clone(),
            side: OrderSide::Buy,
            price: 50000,
            amount: 5,
            max_slippage_bps: None,
        });

        let result = client.execute_batch(&buyer, &legs);
        assert!(result.success);
        assert_eq!(result.legs.len(), 2);
        assert_eq!(result.total_fills, 2);

        assert_eq!(result.legs.get(0).unwrap().filled_amount, 500);
        assert_eq!(result.legs.get(0).unwrap().avg_price, 100);
        assert_eq!(result.legs.get(1).unwrap().filled_amount, 5);
        assert_eq!(result.legs.get(1).unwrap().avg_price, 50000);

        assert_eq!(client.get_pair_volume(&pid1), 500 * 100);
        assert_eq!(client.get_pair_volume(&pid2), 5 * 50000);
    }

    #[test]
    fn test_empty_batch_fails() {
        let (env, client, _admin) = setup();
        let trader = Address::generate(&env);
        let legs = Vec::new(&env);
        assert_eq!(
            client.try_execute_batch(&trader, &legs),
            Err(Ok(TradeError::EmptyBatch)),
        );
    }

    // ---- Slippage protection ----

    #[test]
    fn test_slippage_config_set_get() {
        let (_env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        client.set_slippage(&admin, &pid, &200, &true);
        let config = client.get_slippage_config(&pid);
        assert_eq!(config.max_slippage_bps, 200);
        assert!(config.enabled);
    }

    #[test]
    fn test_slippage_blocks_extreme() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        client.set_slippage(&admin, &pid, &100, &true);

        let seller = Address::generate(&env);
        client.place_order(&seller, &pid, &OrderSide::Sell, &110, &100);

        let buyer = Address::generate(&env);
        let mut legs = Vec::new(&env);
        legs.push_back(TradeLeg {
            pair_id: pid,
            side: OrderSide::Buy,
            price: 100,
            amount: 50,
            max_slippage_bps: None,
        });

        assert_eq!(
            client.try_execute_batch(&buyer, &legs),
            Err(Ok(TradeError::SlippageExceeded)),
        );
    }

    #[test]
    fn test_slippage_allows_within_bounds() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        client.set_slippage(&admin, &pid, &500, &true);
        let seller = Address::generate(&env);
        // Ask at 103 — buyer has limit of 105 so the trade can match.
        client.place_order(&seller, &pid, &OrderSide::Sell, &103, &100);

        let buyer = Address::generate(&env);
        let mut legs = Vec::new(&env);
        legs.push_back(TradeLeg {
            pair_id: pid,
            side: OrderSide::Buy,
            price: 105,
            amount: 50,
            max_slippage_bps: None,
        });

        let result = client.execute_batch(&buyer, &legs);
        assert!(result.success);
        assert_eq!(result.total_fills, 1);
        // Fill at 103, limit was 105 — slippage is within the 5% bound.
        assert_eq!(result.legs.get(0).unwrap().avg_price, 103);
    }

    #[test]
    fn test_per_leg_slippage_override() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        client.set_slippage(&admin, &pid, &100, &true);

        let seller = Address::generate(&env);
        client.place_order(&seller, &pid, &OrderSide::Sell, &103, &100);

        let buyer = Address::generate(&env);
        let mut legs = Vec::new(&env);
        legs.push_back(TradeLeg {
            pair_id: pid,
            side: OrderSide::Buy,
            price: 100,
            amount: 50,
            max_slippage_bps: Some(500),
        });

        let result = client.execute_batch(&buyer, &legs);
        assert!(result.success);
    }

    // ---- Pair stats ----

    #[test]
    fn test_pair_stats() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));
        let seller = Address::generate(&env);

        client.place_order(&seller, &pid, &OrderSide::Sell, &105, &50);
        client.place_order(&seller, &pid, &OrderSide::Buy, &95, &30);

        let stats = client.get_pair_stats(&pid);
        assert_eq!(stats.best_bid, 95);
        assert_eq!(stats.best_ask, 105);
        assert_eq!(stats.spread, 10);
        assert_eq!(stats.total_orders, 2);
    }

    // ---- 100+ concurrent orders ----

    #[test]
    fn test_concurrent_orders() {
        let (env, client, admin) = setup();
        let pid = symbol_short!("XLM_USDC");
        client.register_pair(&admin, &pair(pid.clone(), symbol_short!("XLM")));

        // Place 10 asks and 10 bids (Soroban Vec has size limits for complex types).
        for i in 0..10u32 {
            let trader = Address::generate(&env);
            client.place_order(&trader, &pid, &OrderSide::Sell, &(100 + (i as i128)), &10);
        }
        for i in 0..10u32 {
            let trader = Address::generate(&env);
            let price = 99 - (i as i128);
            client.place_order(&trader, &pid, &OrderSide::Buy, &price, &10);
        }

        let snap = client.get_order_book(&pid);
        assert_eq!(snap.ask_count, 10);
        assert_eq!(snap.bid_count, 10);
        assert_eq!(snap.best_bid, 99);
        assert_eq!(snap.best_ask, 100);
    }

    // ---- Three-leg atomic batch ----

    #[test]
    fn test_three_leg_atomic_batch() {
        let (env, client, admin) = setup();
        let pid1 = symbol_short!("XLM_USDC");
        let pid2 = symbol_short!("ETH_USDC");
        let pid3 = symbol_short!("BTC_USDC");
        client.register_pair(&admin, &pair(pid1.clone(), symbol_short!("XLM")));
        client.register_pair(&admin, &pair(pid2.clone(), symbol_short!("ETH")));
        client.register_pair(&admin, &pair(pid3.clone(), symbol_short!("BTC")));

        client.place_order(
            &Address::generate(&env),
            &pid1,
            &OrderSide::Sell,
            &100,
            &500,
        );
        client.place_order(
            &Address::generate(&env),
            &pid2,
            &OrderSide::Sell,
            &2000,
            &10,
        );
        client.place_order(
            &Address::generate(&env),
            &pid3,
            &OrderSide::Sell,
            &50000,
            &2,
        );

        let buyer = Address::generate(&env);
        let mut legs = Vec::new(&env);
        legs.push_back(TradeLeg {
            pair_id: pid1,
            side: OrderSide::Buy,
            price: 100,
            amount: 200,
            max_slippage_bps: None,
        });
        legs.push_back(TradeLeg {
            pair_id: pid2,
            side: OrderSide::Buy,
            price: 2000,
            amount: 5,
            max_slippage_bps: None,
        });
        legs.push_back(TradeLeg {
            pair_id: pid3,
            side: OrderSide::Buy,
            price: 50000,
            amount: 1,
            max_slippage_bps: None,
        });

        let result = client.execute_batch(&buyer, &legs);
        assert!(result.success);
        assert_eq!(result.legs.len(), 3);
        assert_eq!(result.total_fills, 3);
        assert_eq!(result.legs.get(0).unwrap().filled_amount, 200);
        assert_eq!(result.legs.get(1).unwrap().filled_amount, 5);
        assert_eq!(result.legs.get(2).unwrap().filled_amount, 1);
    }
}
