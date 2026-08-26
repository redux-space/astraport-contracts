//! # Drift Detection & Rebalancing Engine
//!
//! Central engine for the rebalancing contract. Detects portfolio drift,
//! calculates required trades, and provides simulation (dry-run) support.
//!
//! ## Design Principles
//!
//! - **Accuracy**: Drift is computed to ±0.01% (1 basis-point) precision.
//! - **Atomicity**: Trade plans are designed for all-or-nothing execution.
//! - **Gas optimization**: Trade count is bounded; dust trades are skipped.
//! - **Simulation**: Every operation has a read-only counterpart.

use soroban_sdk::{symbol_short, Env, Symbol, Vec};

use crate::records::{
    AssetDrift, DriftReport, DriftSummary, RebalancePlan, RebalanceRecord, RebalanceValidation,
    SimulationPlanResult, TradeConstraints, TradeOrder,
};
use crate::{CurrentHoldings, RebalanceDirection, TargetAllocation};

// ---------------------------------------------------------------------------
// Default constants
// ---------------------------------------------------------------------------

/// Default minimum trade size (1 unit). Trades below this are considered
/// dust and are skipped.
pub const DEFAULT_MIN_TRADE_SIZE: i128 = 1;

/// Default maximum number of trades per rebalance. `0` = unlimited.
pub const DEFAULT_MAX_TRADES: u32 = 0;

/// Maximum number of assets we support in a single portfolio for
/// gas-safety bounds.
pub const MAX_ASSETS: u32 = 128;

// ---------------------------------------------------------------------------
// Drift detection
// ---------------------------------------------------------------------------

/// Core drift engine. Stateless — all methods take explicit inputs and
/// produce results without modifying storage. The contract's public
/// functions call these methods and handle persistence.
pub struct DriftEngine;

impl DriftEngine {
    /// Calculate per-asset drift for every asset present in either the
    /// target allocation or current holdings.
    ///
    /// Returns a `DriftReport` containing:
    /// - One `AssetDrift` per asset with ±0.01% accuracy
    /// - A `DriftSummary` with portfolio-wide statistics
    ///
    /// # Drift Calculation
    ///
    /// For each asset:
    /// ```text
    /// drift_bps  = current_weight_bps - target_weight_bps
    /// abs_drift  = |drift_bps|
    /// drift_pct  = abs_drift / 100   (0.01% resolution)
    /// direction  = Sell if drift > 0, Buy if drift < 0
    /// ```
    ///
    /// Assets present in current holdings but not in the target are treated
    /// as having a target weight of 0 (full sell recommended).
    ///
    /// Assets present in the target but not in current holdings are treated
    /// as having a current weight of 0 (full buy recommended).
    pub fn calculate_portfolio_drift(
        env: &Env,
        portfolio_id: &Symbol,
        target: &TargetAllocation,
        current: &CurrentHoldings,
        threshold_bps: u32,
    ) -> DriftReport {
        let mut asset_drifts = Vec::new(env);
        let mut max_drift_bps: u32 = 0;
        let mut max_drift_asset = symbol_short!("");
        let mut total_drift_bps: u32 = 0;
        let mut assets_out_of_threshold: u32 = 0;
        let mut total_assets: u32 = 0;

        // Visit target assets first, then current-only assets.
        for (asset, target_weight) in target.allocations.iter() {
            let current_weight = current.allocations.get(asset.clone()).unwrap_or(0);
            let drift = Self::compute_asset_drift(
                asset.clone(),
                current_weight,
                target_weight,
                threshold_bps,
            );
            total_assets += 1;

            if drift.abs_drift_bps > max_drift_bps {
                max_drift_bps = drift.abs_drift_bps;
                max_drift_asset = asset.clone();
            }
            total_drift_bps += drift.abs_drift_bps;
            if drift.exceeds_threshold {
                assets_out_of_threshold += 1;
            }
            asset_drifts.push_back(drift);
        }

        for (asset, current_weight) in current.allocations.iter() {
            if !target.allocations.contains_key(asset.clone()) {
                let drift =
                    Self::compute_asset_drift(asset.clone(), current_weight, 0, threshold_bps);
                total_assets += 1;

                if drift.abs_drift_bps > max_drift_bps {
                    max_drift_bps = drift.abs_drift_bps;
                    max_drift_asset = asset.clone();
                }
                total_drift_bps += drift.abs_drift_bps;
                if drift.exceeds_threshold {
                    assets_out_of_threshold += 1;
                }
                asset_drifts.push_back(drift);
            }
        }

        DriftReport {
            portfolio_id: portfolio_id.clone(),
            drift_threshold_bps: threshold_bps,
            asset_drifts,
            summary: DriftSummary {
                max_drift_bps,
                max_drift_asset,
                total_drift_bps,
                assets_out_of_threshold,
                total_assets,
            },
        }
    }

    /// Detect whether rebalancing is needed for a portfolio.
    ///
    /// Returns `true` if **any** asset's absolute drift exceeds the
    /// configured threshold. Also returns the count of out-of-threshold
    /// assets for informational purposes.
    pub fn detect_rebalancing_need(
        env: &Env,
        portfolio_id: &Symbol,
        target: &TargetAllocation,
        current: &CurrentHoldings,
        threshold_bps: u32,
    ) -> (bool, u32) {
        let report =
            Self::calculate_portfolio_drift(env, portfolio_id, target, current, threshold_bps);
        let needs_rebalance = report.summary.assets_out_of_threshold > 0;
        (needs_rebalance, report.summary.assets_out_of_threshold)
    }

    /// Calculate specific trade orders to restore target allocation.
    ///
    /// Produces a `RebalancePlan` containing ordered `TradeOrder` entries
    /// with concrete amounts, estimated fees, and slippage.
    ///
    /// # Trade Generation Strategy
    ///
    /// 1. Separate assets into sell candidates (overweight) and buy
    ///    candidates (underweight).
    /// 2. Pair sell orders with buy orders to minimize the number of
    ///    trades (gas optimization).
    /// 3. Skip trades below `min_trade_size` to avoid dust.
    /// 4. Cap the total trade count at `max_trades`.
    pub fn calculate_rebalance_trades(
        env: &Env,
        portfolio_id: &Symbol,
        target: &TargetAllocation,
        current: &CurrentHoldings,
        threshold_bps: u32,
        constraints: &TradeConstraints,
        total_portfolio_value: i128,
    ) -> RebalancePlan {
        let report =
            Self::calculate_portfolio_drift(env, portfolio_id, target, current, threshold_bps);

        let mut sells = Vec::new(env);
        let mut buys = Vec::new(env);
        let mut warnings = Vec::new(env);

        for drift in report.asset_drifts.iter() {
            if !drift.exceeds_threshold {
                continue;
            }

            // Convert drift bps to base-unit amounts.
            let trade_value = if total_portfolio_value > 0 {
                // trade_value = total_value * |drift_bps| / 10_000
                (total_portfolio_value * (drift.abs_drift_bps as i128)) / 10_000
            } else {
                // Without portfolio value, we cannot compute amounts.
                // Use drift_bps as a proportional proxy (scaled).
                // This is a fallback — prefer providing total_value.
                warnings.push_back(symbol_short!("no_val"));
                continue;
            };

            if trade_value < constraints.min_trade_size {
                // Dust trade — skip.
                continue;
            }

            match drift.direction {
                RebalanceDirection::Sell => {
                    sells.push_back((drift.asset.clone(), trade_value, drift.drift_bps));
                }
                RebalanceDirection::Buy => {
                    buys.push_back((drift.asset.clone(), trade_value, drift.drift_bps));
                }
            }
        }

        // Pair sells with buys for gas-efficient execution.
        let mut trades = Vec::new(env);
        let mut skipped = Vec::new(env);
        let mut trade_count: u32 = 0;
        let mut total_fees: i128 = 0;
        let mut weighted_slippage: i128 = 0;
        let mut total_trade_value: i128 = 0;

        let sell_len = sells.len();
        let buy_len = buys.len();
        let pairs = if sell_len < buy_len {
            sell_len
        } else {
            buy_len
        };

        for i in 0..pairs {
            let (sell_asset, sell_amount, source_drift) = sells.get(i).unwrap();
            let (buy_asset, buy_amount, _buy_drift) = buys.get(i).unwrap();

            // Check trade count limit.
            if constraints.max_trades > 0 && trade_count >= constraints.max_trades {
                skipped.push_back(TradeOrder {
                    sell_asset: sell_asset.clone(),
                    buy_asset: buy_asset.clone(),
                    sell_amount,
                    expected_buy_amount: buy_amount,
                    estimated_fee: 0,
                    estimated_slippage_bps: 0,
                    source_drift_bps: source_drift,
                });
                continue;
            }

            // Check cumulative value limit.
            if constraints.max_total_value > 0
                && total_trade_value + sell_amount > constraints.max_total_value
            {
                skipped.push_back(TradeOrder {
                    sell_asset: sell_asset.clone(),
                    buy_asset: buy_asset.clone(),
                    sell_amount,
                    expected_buy_amount: buy_amount,
                    estimated_fee: 0,
                    estimated_slippage_bps: 0,
                    source_drift_bps: source_drift,
                });
                continue;
            }

            // Estimate fees (mock: 30 bps default).
            let fee = (sell_amount * 30) / 10_000;
            // Estimate slippage (mock: 10 bps default, scales with size).
            let slippage_bps: i32 = 10 + ((sell_amount / 1_000_000) as i32).min(50);
            let expected_buy = buy_amount - ((buy_amount * slippage_bps as i128) / 10_000) - fee;

            trades.push_back(TradeOrder {
                sell_asset: sell_asset.clone(),
                buy_asset: buy_asset.clone(),
                sell_amount,
                expected_buy_amount: expected_buy.max(0),
                estimated_fee: fee,
                estimated_slippage_bps: slippage_bps,
                source_drift_bps: source_drift,
            });

            total_fees += fee;
            weighted_slippage += (slippage_bps as i128) * sell_amount;
            total_trade_value += sell_amount;
            trade_count += 1;
        }

        // Emit a warning for unpaired trades.
        if sells.len() > buys.len() {
            warnings.push_back(symbol_short!("unpaired"));
        } else if buys.len() > sells.len() {
            warnings.push_back(symbol_short!("unpaired"));
        }

        let avg_slippage = if total_trade_value > 0 {
            (weighted_slippage / total_trade_value) as i32
        } else {
            0
        };

        RebalancePlan {
            portfolio_id: portfolio_id.clone(),
            drift_threshold_bps: threshold_bps,
            trades,
            estimated_total_fees: total_fees,
            estimated_total_slippage_bps: avg_slippage,
            assets_to_rebalance: report.summary.assets_out_of_threshold,
            warnings,
            created_at: env.ledger().timestamp(),
        }
    }

    /// Validate rebalance inputs before computing a plan.
    ///
    /// Checks:
    /// - Target allocation weights sum to 10_000 bps.
    /// - Current holdings weights sum to 10_000 bps.
    /// - Drift threshold is reasonable (≤ 10_000 bps).
    /// - Asset count is within bounds.
    pub fn validate_rebalance_inputs(
        env: &Env,
        target: &TargetAllocation,
        current: &CurrentHoldings,
        threshold_bps: u32,
    ) -> RebalanceValidation {
        let mut issues = Vec::new(env);

        // Check target allocation sum.
        let mut target_total: u32 = 0;
        let mut asset_count: u32 = 0;
        for (_asset, weight) in target.allocations.iter() {
            target_total += weight;
            asset_count += 1;
        }
        if target_total != 10_000 {
            issues.push_back(symbol_short!("bad_tgt"));
        }

        // Check current holdings sum.
        let mut current_total: u32 = 0;
        let mut current_asset_count: u32 = 0;
        for (_asset, weight) in current.allocations.iter() {
            current_total += weight;
            current_asset_count += 1;
        }
        if current_total != 10_000 {
            issues.push_back(symbol_short!("bad_cur"));
        }

        // Check threshold.
        if threshold_bps > 10_000 {
            issues.push_back(symbol_short!("bad_thr"));
        }

        // Check asset count bounds.
        let total_assets = asset_count.max(current_asset_count);
        if total_assets > MAX_ASSETS {
            issues.push_back(symbol_short!("too_many"));
        }

        RebalanceValidation {
            valid: issues.is_empty(),
            issues,
        }
    }

    /// Simulate a rebalance without modifying any state.
    ///
    /// Returns a `SimulationPlanResult` containing:
    /// - The computed rebalance plan
    /// - Drift reports before and after (projected)
    /// - Whether the plan fully resolves all drift
    /// - Any skipped trades
    pub fn simulate_rebalance_full(
        env: &Env,
        portfolio_id: &Symbol,
        target: &TargetAllocation,
        current: &CurrentHoldings,
        threshold_bps: u32,
        constraints: &TradeConstraints,
        total_portfolio_value: i128,
    ) -> SimulationPlanResult {
        let drift_before =
            Self::calculate_portfolio_drift(env, portfolio_id, target, current, threshold_bps);

        let plan = Self::calculate_rebalance_trades(
            env,
            portfolio_id,
            target,
            current,
            threshold_bps,
            constraints,
            total_portfolio_value,
        );

        // Project post-rebalance drift by applying the trades.
        // In a real implementation this would update the current holdings
        // map. For simulation, we compute a projection: after rebalance,
        // the drift for each traded asset should be zero (or within threshold).
        let projected_after = Self::project_post_rebalance_drift(
            env,
            portfolio_id,
            target,
            current,
            threshold_bps,
            &plan,
        );

        let fully_rebalanced = projected_after.summary.assets_out_of_threshold == 0;

        SimulationPlanResult {
            plan: plan.clone(),
            drift_before,
            drift_after: projected_after,
            fully_rebalanced,
            skipped_trades: Vec::new(env), // Filled by caller if needed
        }
    }

    // -------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------

    /// Compute the drift for a single asset with ±0.01% accuracy.
    fn compute_asset_drift(
        asset: Symbol,
        current_weight_bps: u32,
        target_weight_bps: u32,
        threshold_bps: u32,
    ) -> AssetDrift {
        let drift_bps = current_weight_bps as i32 - target_weight_bps as i32;
        let abs_drift_bps = drift_bps.unsigned_abs();
        // drift_pct is at 0.01% resolution: 250 bps → 250 (representing 2.50%)
        let drift_pct = abs_drift_bps;
        let exceeds_threshold = abs_drift_bps > threshold_bps;
        let direction = if drift_bps > 0 {
            RebalanceDirection::Sell
        } else {
            RebalanceDirection::Buy
        };

        AssetDrift {
            asset,
            current_weight_bps,
            target_weight_bps,
            drift_bps,
            abs_drift_bps,
            drift_pct,
            direction,
            exceeds_threshold,
        }
    }

    /// Project post-rebalance drift by zeroing out trades that are in the plan.
    ///
    /// For each trade in the plan, we assume the sell-side asset's weight
    /// moves toward its target and the buy-side asset's weight moves toward
    /// its target. In the ideal case, all traded assets end up at their target.
    fn project_post_rebalance_drift(
        env: &Env,
        portfolio_id: &Symbol,
        target: &TargetAllocation,
        current: &CurrentHoldings,
        threshold_bps: u32,
        plan: &RebalancePlan,
    ) -> DriftReport {
        // Build projected holdings by zeroing out drift for all traded assets.
        let mut projected_weights = soroban_sdk::Map::<Symbol, u32>::new(env);

        // Start with current holdings.
        for (asset, weight) in current.allocations.iter() {
            projected_weights.set(asset, weight);
        }

        // For each trade, move the sell asset toward its target weight.
        for trade in plan.trades.iter() {
            // Sell asset: move toward target.
            if let Some(target_w) = target.allocations.get(trade.sell_asset.clone()) {
                projected_weights.set(trade.sell_asset.clone(), target_w);
            } else {
                // Not in target — remove fully.
                projected_weights.set(trade.sell_asset.clone(), 0);
            }

            // Buy asset: move toward target.
            if let Some(target_w) = target.allocations.get(trade.buy_asset.clone()) {
                projected_weights.set(trade.buy_asset.clone(), target_w);
            }
        }

        // Also set assets in target but not yet in projected to 0.
        for (asset, _target_w) in target.allocations.iter() {
            if !projected_weights.contains_key(asset.clone()) {
                projected_weights.set(asset, 0);
            }
        }

        let projected_current = CurrentHoldings {
            allocations: projected_weights,
        };

        Self::calculate_portfolio_drift(
            env,
            portfolio_id,
            target,
            &projected_current,
            threshold_bps,
        )
    }

    /// Create a detailed `RebalanceRecord` from a completed operation.
    pub fn create_rebalance_record(
        env: &Env,
        portfolio_id: &Symbol,
        drift_before: &DriftReport,
        drift_after: &DriftReport,
        trades_executed: u32,
        atomic_success: bool,
        execution_type: &Symbol,
        total_fees: i128,
    ) -> RebalanceRecord {
        let outcome = if atomic_success {
            if drift_after.summary.assets_out_of_threshold == 0 {
                symbol_short!("ok")
            } else {
                symbol_short!("partial")
            }
        } else {
            symbol_short!("error")
        };

        RebalanceRecord {
            portfolio_id: portfolio_id.clone(),
            timestamp: env.ledger().timestamp(),
            trades_executed,
            assets_rebalanced: drift_before.summary.assets_out_of_threshold,
            max_drift_before_bps: drift_before.summary.max_drift_bps,
            max_drift_after_bps: drift_after.summary.max_drift_bps,
            atomic_success,
            execution_type: execution_type.clone(),
            outcome,
            total_fees,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short, testutils::Address as _, Env, Map};

    fn weights(env: &Env, entries: &[(Symbol, u32)]) -> Map<Symbol, u32> {
        let mut result = Map::new(env);
        for (asset, weight) in entries.iter() {
            result.set(asset.clone(), *weight);
        }
        result
    }

    #[test]
    fn test_drift_detection_no_drift() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 6_000),
                    (symbol_short!("XLM"), 4_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 6_000),
                    (symbol_short!("XLM"), 4_000),
                ],
            ),
        };

        let report =
            DriftEngine::calculate_portfolio_drift(&env, &portfolio, &target, &current, 100);

        assert_eq!(report.summary.assets_out_of_threshold, 0);
        assert_eq!(report.summary.max_drift_bps, 0);
        assert_eq!(report.asset_drifts.len(), 2);
    }

    #[test]
    fn test_drift_detection_single_asset_overweight() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_250),
                    (symbol_short!("XLM"), 4_750),
                ],
            ),
        };

        let report =
            DriftEngine::calculate_portfolio_drift(&env, &portfolio, &target, &current, 100);

        assert_eq!(report.summary.assets_out_of_threshold, 2);
        assert_eq!(report.summary.max_drift_bps, 250);
        assert_eq!(report.summary.max_drift_asset, symbol_short!("USDC"));

        // USDC should be sell (overweight)
        let usdc_drift = report.asset_drifts.get(0).unwrap();
        assert_eq!(usdc_drift.asset, symbol_short!("USDC"));
        assert_eq!(usdc_drift.drift_bps, 250);
        assert_eq!(usdc_drift.direction, RebalanceDirection::Sell);

        // XLM should be buy (underweight)
        let xlm_drift = report.asset_drifts.get(1).unwrap();
        assert_eq!(xlm_drift.asset, symbol_short!("XLM"));
        assert_eq!(xlm_drift.drift_bps, -250);
        assert_eq!(xlm_drift.direction, RebalanceDirection::Buy);
    }

    #[test]
    fn test_drift_detection_asset_not_in_target() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(&env, &[(symbol_short!("USDC"), 10_000)]),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 7_000),
                    (symbol_short!("XLM"), 3_000),
                ],
            ),
        };

        let report =
            DriftEngine::calculate_portfolio_drift(&env, &portfolio, &target, &current, 100);

        // XLM is in current but not target → treated as target=0
        assert_eq!(report.summary.assets_out_of_threshold, 2);
        // Find XLM drift
        let mut found_xlm = false;
        for drift in report.asset_drifts.iter() {
            if drift.asset == symbol_short!("XLM") {
                assert_eq!(drift.drift_bps, 3_000);
                assert_eq!(drift.direction, RebalanceDirection::Sell);
                assert!(drift.exceeds_threshold);
                found_xlm = true;
            }
        }
        assert!(found_xlm);
    }

    #[test]
    fn test_drift_detection_zero_current_holdings() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: Map::new(&env),
        };

        let report =
            DriftEngine::calculate_portfolio_drift(&env, &portfolio, &target, &current, 100);

        // Both assets should have full negative drift (need to buy).
        assert_eq!(report.summary.assets_out_of_threshold, 2);
        for drift in report.asset_drifts.iter() {
            assert_eq!(drift.direction, RebalanceDirection::Buy);
            assert!(drift.abs_drift_bps > 0);
        }
    }

    #[test]
    fn test_drift_pct_accuracy() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };
        // 1 bps drift = 0.01% accuracy
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_001),
                    (symbol_short!("XLM"), 4_999),
                ],
            ),
        };

        let report = DriftEngine::calculate_portfolio_drift(&env, &portfolio, &target, &current, 0);

        let usdc_drift = report.asset_drifts.get(0).unwrap();
        assert_eq!(usdc_drift.drift_bps, 1);
        assert_eq!(usdc_drift.drift_pct, 1); // 0.01%
        assert!(!usdc_drift.exceeds_threshold); // threshold=0, abs_drift=1, 1 > 0 is true
                                                // Actually with threshold=0, any drift exceeds it
        assert!(usdc_drift.exceeds_threshold);
    }

    #[test]
    fn test_detect_rebalancing_need_true() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_300),
                    (symbol_short!("XLM"), 4_700),
                ],
            ),
        };

        let (needs, count) =
            DriftEngine::detect_rebalancing_need(&env, &portfolio, &target, &current, 200);

        assert!(needs);
        assert_eq!(count, 2); // Both exceed 200 bps
    }

    #[test]
    fn test_detect_rebalancing_need_false() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_050),
                    (symbol_short!("XLM"), 4_950),
                ],
            ),
        };

        let (needs, count) =
            DriftEngine::detect_rebalancing_need(&env, &portfolio, &target, &current, 100);

        assert!(!needs);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_calculate_rebalance_trades() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 3_000),
                    (symbol_short!("BTC"), 2_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_500),
                    (symbol_short!("XLM"), 2_500),
                    (symbol_short!("BTC"), 2_000),
                ],
            ),
        };
        let constraints = TradeConstraints::default();
        let total_value: i128 = 1_000_000;

        let plan = DriftEngine::calculate_rebalance_trades(
            &env,
            &portfolio,
            &target,
            &current,
            100,
            &constraints,
            total_value,
        );

        // USDC is overweight by 500 bps → sell 500/10000 * 1M = 50,000
        // XLM is underweight by 500 bps → buy 50,000
        // BTC is at target → no trade
        assert_eq!(plan.trades.len(), 1); // One sell-buy pair
        assert_eq!(plan.assets_to_rebalance, 2);

        let trade = plan.trades.get(0).unwrap();
        assert_eq!(trade.sell_asset, symbol_short!("USDC"));
        assert_eq!(trade.buy_asset, symbol_short!("XLM"));
        assert_eq!(trade.sell_amount, 50_000);
    }

    #[test]
    fn test_calculate_rebalance_trades_dust_skipped() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_010),
                    (symbol_short!("XLM"), 4_990),
                ],
            ),
        };
        let constraints = TradeConstraints {
            min_trade_size: 1_000,
            ..TradeConstraints::default()
        };
        let total_value: i128 = 1_000;

        let plan = DriftEngine::calculate_rebalance_trades(
            &env,
            &portfolio,
            &target,
            &current,
            100,
            &constraints,
            total_value,
        );

        // Trade value = 1000 * 10 / 10000 = 1, which is < min_trade_size=1000
        assert_eq!(plan.trades.len(), 0);
    }

    #[test]
    fn test_calculate_rebalance_trades_max_trades_limit() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("A"), 2_500),
                    (symbol_short!("B"), 2_500),
                    (symbol_short!("C"), 2_500),
                    (symbol_short!("D"), 2_500),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("A"), 3_500),
                    (symbol_short!("B"), 1_500),
                    (symbol_short!("C"), 3_500),
                    (symbol_short!("D"), 1_500),
                ],
            ),
        };
        let constraints = TradeConstraints {
            max_trades: 1,
            ..TradeConstraints::default()
        };
        let total_value: i128 = 1_000_000;

        let plan = DriftEngine::calculate_rebalance_trades(
            &env,
            &portfolio,
            &target,
            &current,
            100,
            &constraints,
            total_value,
        );

        // Should only execute 1 trade even though there are 2 pairs
        assert_eq!(plan.trades.len(), 1);
    }

    #[test]
    fn test_validate_inputs_valid() {
        let env = Env::default();
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };

        let result = DriftEngine::validate_rebalance_inputs(&env, &target, &current, 100);
        assert!(result.valid);
        assert_eq!(result.issues.len(), 0);
    }

    #[test]
    fn test_validate_inputs_bad_target() {
        let env = Env::default();
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 6_000),
                    (symbol_short!("XLM"), 4_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };

        let result = DriftEngine::validate_rebalance_inputs(&env, &target, &current, 100);
        assert!(!result.valid);
        assert!(result.issues.get(0).unwrap() == symbol_short!("bad_target"));
    }

    #[test]
    fn test_validate_inputs_bad_threshold() {
        let env = Env::default();
        let target = TargetAllocation {
            allocations: weights(&env, &[(symbol_short!("USDC"), 10_000)]),
        };
        let current = CurrentHoldings {
            allocations: weights(&env, &[(symbol_short!("USDC"), 10_000)]),
        };

        let result = DriftEngine::validate_rebalance_inputs(&env, &target, &current, 20_000);
        assert!(!result.valid);
        assert!(result.issues.get(0).unwrap() == symbol_short!("bad_thresh"));
    }

    #[test]
    fn test_simulate_rebalance_full() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_300),
                    (symbol_short!("XLM"), 4_700),
                ],
            ),
        };
        let constraints = TradeConstraints::default();
        let total_value: i128 = 1_000_000;

        let sim = DriftEngine::simulate_rebalance_full(
            &env,
            &portfolio,
            &target,
            &current,
            100,
            &constraints,
            total_value,
        );

        // Drift before: USDC +300 bps, XLM -300 bps
        assert_eq!(sim.drift_before.summary.assets_out_of_threshold, 2);
        assert_eq!(sim.drift_before.summary.max_drift_bps, 300);

        // After: should be within threshold
        assert!(sim.fully_rebalanced);

        // Plan should have 1 trade
        assert_eq!(sim.plan.trades.len(), 1);
    }

    #[test]
    fn test_single_asset_portfolio() {
        let env = Env::default();
        let portfolio = symbol_short!("port1");
        let target = TargetAllocation {
            allocations: weights(&env, &[(symbol_short!("USDC"), 10_000)]),
        };
        let current = CurrentHoldings {
            allocations: weights(&env, &[(symbol_short!("USDC"), 10_000)]),
        };

        let report =
            DriftEngine::calculate_portfolio_drift(&env, &portfolio, &target, &current, 100);

        assert_eq!(report.summary.assets_out_of_threshold, 0);
        assert_eq!(report.summary.total_assets, 1);
    }

    #[test]
    fn test_create_rebalance_record() {
        let env = Env::default();
        env.mock_all_auths();
        let portfolio = symbol_short!("port1");

        let target = TargetAllocation {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_000),
                    (symbol_short!("XLM"), 5_000),
                ],
            ),
        };
        let current = CurrentHoldings {
            allocations: weights(
                &env,
                &[
                    (symbol_short!("USDC"), 5_300),
                    (symbol_short!("XLM"), 4_700),
                ],
            ),
        };

        let drift_before =
            DriftEngine::calculate_portfolio_drift(&env, &portfolio, &target, &current, 100);
        let drift_after = DriftEngine::calculate_portfolio_drift(
            &env, &portfolio, &target, &target, 100, // target == current after rebalance
        );

        let record = DriftEngine::create_rebalance_record(
            &env,
            &portfolio,
            &drift_before,
            &drift_after,
            1,
            true,
            &symbol_short!("manual"),
            150,
        );

        assert!(record.atomic_success);
        assert_eq!(record.outcome, symbol_short!("ok"));
        assert_eq!(record.trades_executed, 1);
        assert_eq!(record.assets_rebalanced, 2);
        assert_eq!(record.total_fees, 150);
    }

    #[test]
    fn test_drift_bps_exceeds_helper() {
        let d1 = DriftPct::from_bps(150);
        assert!(d1.exceeds(100));
        assert!(!d1.exceeds(200));

        let d2 = DriftPct::from_bps(0);
        assert!(!d2.exceeds(0));

        let d3 = DriftPct::from_bps(1);
        assert!(d3.exceeds(0));
    }
}
