use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Map, Symbol, Vec,
};

// Import RebalanceAdjustment from the parent module
use super::RebalanceAdjustment;
use astraport_pricefeed::records::{AggregatedPrice, PriceFeedError, PriceStatus};
use astraport_trade::types::{AtomicBatchResult, OrderSide, TradeLeg, TradeError};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RebalanceError {
    InvalidAllocation = 1,
    InvalidCurrentHoldings = 2,
    TargetAllocationNotFound = 3,
    CurrentHoldingsNotFound = 4,
    ExecutionFailed = 5,
    PriceFeedUnavailable = 6,
    PriceStale = 7,
    TradeEngineError = 8,
    InsufficientLiquidity = 9,
    SlippageExceeded = 10,
    NotionalMismatch = 11,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionStrategy {
    MinimalCost,
    MinimalTime,
    Balanced,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Tradeoff {
    Cost,
    Time,
    Balanced,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Trade {
    pub asset_to_sell: Symbol,
    pub asset_to_buy: Symbol,
    pub amount_to_sell: u128,
    pub expected_amount_to_buy: u128,
    pub notional_value: u128,
    pub fill_price: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulationResult {
    pub trades: Vec<Trade>,
    pub total_fee: u128,
    pub slippage_bps: u128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Percentage(u32); // Basis points

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalanceExecutedEvent {
    pub portfolio_id: Symbol,
    pub keeper: Address,
    pub trades_executed: u32,
    pub total_fees: u128,
    pub total_slippage_bps: u128,
    pub timestamp: u64,
    pub trade_details: Vec<TradeDetail>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeDetail {
    pub asset_to_sell: Symbol,
    pub asset_to_buy: Symbol,
    pub amount_sold: u128,
    pub amount_bought: u128,
    pub fill_price: u128,
    pub fee: u128,
    pub slippage_bps: u128,
}

#[contract]
pub struct MultiAssetRebalancer;

#[contractimpl]
impl MultiAssetRebalancer {
    pub fn rebalance(
        env: Env,
        portfolio_id: Symbol,
        strategy: ExecutionStrategy,
        adjustments: Vec<RebalanceAdjustment>,
        trade_engine: Address,
        price_feed: Address,
        total_portfolio_value: u128,
        keeper: Address,
    ) -> Result<(), RebalanceError> {
        keeper.require_auth();

        let (trades, total_fee, total_slippage) =
            Self::execute_strategy(&env, &strategy, &adjustments, &trade_engine, &price_feed, total_portfolio_value)?;

        // Execute trades through trade engine
        let batch_result = Self::execute_trades_via_engine(&env, &trade_engine, &trades, &keeper)?;

        // Emit REBAL_EXECUTED event
        Self::emit_rebalance_executed(&env, &portfolio_id, &keeper, &batch_result, total_fee, total_slippage);

        // Log to audit
        Self::log_audit(&env, &portfolio_id, &batch_result);

        Ok(())
    }

    pub fn simulate_rebalance(
        env: Env,
        _portfolio_id: Symbol,
        strategy: ExecutionStrategy,
        adjustments: Vec<RebalanceAdjustment>,
        trade_engine: Address,
        price_feed: Address,
        total_portfolio_value: u128,
    ) -> Result<SimulationResult, RebalanceError> {
        let (trades, total_fee, total_slippage_bps) =
            Self::execute_strategy(&env, &strategy, &adjustments, &trade_engine, &price_feed, total_portfolio_value)?;

        Ok(SimulationResult {
            trades,
            total_fee,
            slippage_bps: total_slippage_bps,
        })
    }

    fn execute_strategy(
        env: &Env,
        strategy: &ExecutionStrategy,
        adjustments: &Vec<RebalanceAdjustment>,
        trade_engine: &Address,
        price_feed: &Address,
        total_portfolio_value: u128,
    ) -> Result<(Vec<Trade>, u128, u128), RebalanceError> {
        let tradeoff = match strategy {
            ExecutionStrategy::MinimalCost => Tradeoff::Cost,
            ExecutionStrategy::MinimalTime => Tradeoff::Time,
            ExecutionStrategy::Balanced => Tradeoff::Balanced,
        };
        let trades = Self::generate_trades(env, adjustments, &tradeoff, price_feed, total_portfolio_value)?;

        let total_fee = Self::calculate_total_fees(env, &trades)?;
        let mut total_slippage_bps = 0u128;
        for trade in trades.iter() {
            let slippage = Self::predict_slippage(env, &trade.asset_to_sell, &trade.asset_to_buy, trade.amount_to_sell, trade_engine)?;
            total_slippage_bps += slippage.0 as u128;
        }

        Ok((trades, total_fee, total_slippage_bps))
    }

    fn generate_trades(
        env: &Env,
        adjustments: &Vec<RebalanceAdjustment>,
        tradeoff: &Tradeoff,
        price_feed: &Address,
        total_portfolio_value: u128,
    ) -> Result<Vec<Trade>, RebalanceError> {
        let mut trades = Vec::new(env);
        let mut sell_adjustments = Vec::new(env);
        let mut buy_adjustments = Vec::new(env);

        for adjustment in adjustments.iter() {
            if adjustment.drift_bps > 0 {
                sell_adjustments.push_back(adjustment.clone());
            } else if adjustment.drift_bps < 0 {
                buy_adjustments.push_back(adjustment.clone());
            }
        }

        // Fetch prices for all assets involved
        let mut asset_prices: Map<Symbol, u128> = Map::new(env);
        for adj in sell_adjustments.iter() {
            let price = Self::fetch_price(env, price_feed, &adj.asset)?;
            asset_prices.set(adj.asset.clone(), price);
        }
        for adj in buy_adjustments.iter() {
            if !asset_prices.contains_key(adj.asset.clone()) {
                let price = Self::fetch_price(env, price_feed, &adj.asset)?;
                asset_prices.set(adj.asset.clone(), price);
            }
        }

        // Sort sells by notional value descending (largest first)
        let mut sell_list = Vec::new(env);
        for i in 0..sell_adjustments.len() {
            let adj = sell_adjustments.get(i).unwrap();
            let price = asset_prices.get(adj.asset.clone()).unwrap();
            let notional = (total_portfolio_value * (adj.drift_bps.unsigned_abs() as u128)) / 10000;
            sell_list.push_back((adj.asset.clone(), notional, price, adj.drift_bps.unsigned_abs() as u32));
        }

        // Sort buys by notional value descending
        let mut buy_list = Vec::new(env);
        for i in 0..buy_adjustments.len() {
            let adj = buy_adjustments.get(i).unwrap();
            let price = asset_prices.get(adj.asset.clone()).unwrap();
            let notional = (total_portfolio_value * (adj.drift_bps.unsigned_abs() as u128)) / 10000;
            buy_list.push_back((adj.asset.clone(), notional, price, adj.drift_bps.unsigned_abs() as u32));
        }

        // Simple greedy matching by notional value
        let mut sell_idx = 0u32;
        let mut buy_idx = 0u32;

        while sell_idx < sell_list.len() && buy_idx < buy_list.len() {
            let (sell_asset, sell_notional, sell_price, sell_drift) = sell_list.get(sell_idx).unwrap();
            let (buy_asset, buy_notional, buy_price, buy_drift) = buy_list.get(buy_idx).unwrap();

            let trade_notional = sell_notional.min(buy_notional);
            let amount_to_sell = (trade_notional * 10000) / sell_price;
            let base_expected_amount = (trade_notional * buy_price) / sell_price;

            let expected_amount_to_buy = match tradeoff {
                Tradeoff::Cost => base_expected_amount,
                Tradeoff::Time => (base_expected_amount * 99) / 100,
                Tradeoff::Balanced => (base_expected_amount * 995) / 1000,
            };

            trades.push_back(Trade {
                asset_to_sell: sell_asset.clone(),
                asset_to_buy: buy_asset.clone(),
                amount_to_sell,
                expected_amount_to_buy,
                notional_value: trade_notional,
                fill_price: sell_price,
            });

            // Update remaining notional
            let new_sell_notional = sell_notional - trade_notional;
            let new_buy_notional = buy_notional - trade_notional;

            if new_sell_notional == 0 {
                sell_idx += 1;
            } else {
                sell_list.set(sell_idx, (sell_asset.clone(), new_sell_notional, sell_price, sell_drift));
            }

            if new_buy_notional == 0 {
                buy_idx += 1;
            } else {
                buy_list.set(buy_idx, (buy_asset.clone(), new_buy_notional, buy_price, buy_drift));
            }
        }

        Ok(trades)
    }

    fn fetch_price(env: &Env, price_feed: &Address, asset: &Symbol) -> Result<u128, RebalanceError> {
        // Call price feed contract's get_price
        let client = astraport_pricefeed::PriceFeedContractClient::new(env, price_feed);
        let aggregated: AggregatedPrice = client.get_price(asset);
        if aggregated.status == PriceStatus::Stale {
            return Err(RebalanceError::PriceStale);
        }
        if aggregated.status == PriceStatus::Anomalous {
            return Err(RebalanceError::PriceFeedUnavailable);
        }
        if aggregated.status == PriceStatus::Unknown {
            return Err(RebalanceError::PriceFeedUnavailable);
        }
        Ok(aggregated.price as u128)
    }

    fn calculate_total_fees(_env: &Env, trades: &Vec<Trade>) -> Result<u128, RebalanceError> {
        // In a real implementation, we'd query the trade engine for fee rates per pair
        // For now, use reasonable defaults
        let mut total_fee = 0u128;
        for trade in trades.iter() {
            // Default fee: 30 bps
            total_fee += (trade.amount_to_sell * 30) / 10000;
        }
        Ok(total_fee)
    }

    fn predict_slippage(
        _env: &Env,
        _asset_to_sell: &Symbol,
        _asset_to_buy: &Symbol,
        amount_to_sell: u128,
        _trade_engine: &Address,
    ) -> Result<Percentage, RebalanceError> {
        // Query trade engine for pair stats to estimate slippage
        // For now, use simple model: 10 bps base + amount-based scaling
        let base_slippage = 10u128;
        let amount_slippage = amount_to_sell / 1_000_000; // 1 bps per 1M units
        let total = base_slippage + amount_slippage;
        Ok(Percentage(total as u32))
    }

    fn execute_trades_via_engine(
        env: &Env,
        trade_engine: &Address,
        trades: &Vec<Trade>,
        keeper: &Address,
    ) -> Result<AtomicBatchResult, RebalanceError> {
        let client = astraport_trade::TradeEngineClient::new(env, trade_engine);

        let mut legs = Vec::new(env);
        for trade in trades.iter() {
            legs.push_back(TradeLeg {
                pair_id: Self::pair_id_from_assets(&trade.asset_to_sell, &trade.asset_to_buy),
                side: OrderSide::Sell,
                price: trade.fill_price as i128,
                amount: trade.amount_to_sell as i128,
                max_slippage_bps: None,
            });
        }

        let batch_result: AtomicBatchResult = client.execute_batch(keeper, &legs);
        // The trade engine doesn't return Result, it panics on error
        // We'll handle errors through the event system or assume success
        Ok(batch_result)
    }

    fn pair_id_from_assets(_base: &Symbol, _quote: &Symbol) -> Symbol {
        // Convert asset symbols to pair ID format (e.g., "BTC_USDC")
        // This is a simplified version - in production would use a registry
        symbol_short!("PAIR")
    }

    fn emit_rebalance_executed(
        env: &Env,
        portfolio_id: &Symbol,
        keeper: &Address,
        batch_result: &AtomicBatchResult,
        total_fee: u128,
        total_slippage: u128,
    ) {
        let mut details = Vec::new(env);
        for i in 0..batch_result.legs.len() {
            let leg = batch_result.legs.get(i).unwrap();
            details.push_back(TradeDetail {
                asset_to_sell: leg.pair_id.clone(),
                asset_to_buy: leg.pair_id.clone(),
                amount_sold: leg.filled_amount as u128,
                amount_bought: leg.filled_amount as u128,
                fill_price: leg.avg_price as u128,
                fee: leg.total_fees as u128,
                slippage_bps: 0, // Would calculate from avg_price vs expected
            });
        }

        let event = RebalanceExecutedEvent {
            portfolio_id: portfolio_id.clone(),
            keeper: keeper.clone(),
            trades_executed: batch_result.total_fills,
            total_fees: total_fee,
            total_slippage_bps: total_slippage,
            timestamp: env.ledger().timestamp(),
            trade_details: details,
        };

        env.events().publish(
            (symbol_short!("REB_EXEC"), portfolio_id.clone()),
            event,
        );
    }

    fn log_audit(_env: &Env, _portfolio_id: &Symbol, _batch_result: &AtomicBatchResult) {
        // Integration with audit logger
        // This would call the audit contract if configured
    }

    fn log_trade(env: &Env, trade: &Trade, _fee: u128, _slippage_bps: u128) {
        soroban_sdk::log!(
            env,
            "Rebalancing trade: sell={}, buy={}, amount={}",
            trade.asset_to_sell,
            trade.asset_to_buy,
            trade.amount_to_sell
        );
    }
}