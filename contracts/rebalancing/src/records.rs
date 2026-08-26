//! # Rebalancing Engine Records
//!
//! Data types used by the core rebalancing engine for drift detection,
//! trade calculation, and rebalance history tracking.
//!
//! All weight values are expressed in **basis points** (1 bps = 0.01%).
//! A total of 10_000 bps equals 100%.

use soroban_sdk::{contracttype, Map, Symbol, Vec};

use super::RebalanceDirection;

// ---------------------------------------------------------------------------
// Drift detection records
// ---------------------------------------------------------------------------

/// Per-asset drift data point.
///
/// Measures how far a single asset has deviated from its target weight.
/// `drift_bps` is signed: positive means overweight (sell), negative means
/// underweight (buy). `drift_pct` is the same value expressed as a
/// percentage with 0.01% resolution (e.g., 250 bps = 2.50%).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetDrift {
    /// The asset symbol.
    pub asset: Symbol,
    /// Current weight in basis points.
    pub current_weight_bps: u32,
    /// Target weight in basis points.
    pub target_weight_bps: u32,
    /// Signed drift: `current - target`. Positive = overweight.
    pub drift_bps: i32,
    /// Absolute drift in basis points.
    pub abs_drift_bps: u32,
    /// Absolute drift as a percentage with 0.01% resolution (bps / 100).
    pub drift_pct: u32,
    /// Direction of trade required to correct the drift.
    pub direction: RebalanceDirection,
    /// Whether this asset exceeds the configured drift threshold.
    pub exceeds_threshold: bool,
}

/// Summary of portfolio-wide drift.
///
/// Provides a single overall drift score alongside per-asset details.
/// `max_drift_bps` identifies the worst-offending asset, and
/// `total_drift_bps` is the sum of absolute per-asset drifts (useful for
/// a coarse "how out of balance" metric).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriftSummary {
    /// Maximum absolute drift across all assets (in bps).
    pub max_drift_bps: u32,
    /// Symbol of the asset with the maximum drift.
    pub max_drift_asset: Symbol,
    /// Sum of absolute per-asset drifts (in bps).
    pub total_drift_bps: u32,
    /// Number of assets that exceed the drift threshold.
    pub assets_out_of_threshold: u32,
    /// Total number of assets in the portfolio (target + current-only).
    pub total_assets: u32,
}

/// Full drift report for a portfolio.
///
/// Returned by `calculate_portfolio_drift()`. Contains both the
/// per-asset breakdown and the overall summary.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriftReport {
    /// Portfolio identifier.
    pub portfolio_id: Symbol,
    /// Drift threshold in basis points that was applied.
    pub drift_threshold_bps: u32,
    /// Per-asset drift data, one entry per asset in target + current.
    pub asset_drifts: Vec<AssetDrift>,
    /// Portfolio-wide summary.
    pub summary: DriftSummary,
}

// ---------------------------------------------------------------------------
// Trade calculation records
// ---------------------------------------------------------------------------

/// A concrete trade order with amounts.
///
/// Unlike `RebalanceAdjustment` (which only records direction and drift),
/// `TradeOrder` specifies exact amounts to sell and buy, and includes
/// estimated fees and slippage for gas-budgeting purposes.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeOrder {
    /// Asset to sell.
    pub sell_asset: Symbol,
    /// Asset to buy.
    pub buy_asset: Symbol,
    /// Amount to sell (in asset base units).
    pub sell_amount: i128,
    /// Expected amount to receive after fees and slippage.
    pub expected_buy_amount: i128,
    /// Estimated fee for this trade (in buy-asset base units).
    pub estimated_fee: i128,
    /// Estimated slippage in basis points for this pair.
    pub estimated_slippage_bps: i32,
    /// The drift that triggered this trade (in bps).
    pub source_drift_bps: i32,
}

/// Minimum trade size constraint.
///
/// Trades below this threshold are skipped to avoid dust transactions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradeConstraints {
    /// Minimum absolute trade amount (in base units). Trades smaller
    /// than this are dropped.
    pub min_trade_size: i128,
    /// Maximum number of trades in a single rebalance operation.
    /// `0` means unlimited.
    pub max_trades: u32,
    /// Maximum total rebalance value (in base units). Set to `0` for
    /// no cap.
    pub max_total_value: i128,
}

impl Default for TradeConstraints {
    /// Default constraints: 1 unit minimum, no trade limit, no value cap.
    fn default() -> Self {
        Self {
            min_trade_size: 1,
            max_trades: 0,
            max_total_value: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Rebalance plan
// ---------------------------------------------------------------------------

/// Complete rebalance plan with trades, costs, and warnings.
///
/// Generated by `calculate_rebalance_trades()` and used as input to
/// `execute_rebalance()` or `simulate_rebalance_full()`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalancePlan {
    /// Portfolio identifier.
    pub portfolio_id: Symbol,
    /// Drift threshold that was used.
    pub drift_threshold_bps: u32,
    /// The ordered list of trades to execute.
    pub trades: Vec<TradeOrder>,
    /// Estimated total fees across all trades (in base units of the
    /// portfolio's base currency).
    pub estimated_total_fees: i128,
    /// Estimated total slippage across all trades (in bps, weighted
    /// by trade value).
    pub estimated_total_slippage_bps: i32,
    /// Number of assets that would be rebalanced.
    pub assets_to_rebalance: u32,
    /// Warnings generated during plan creation (e.g., dust trades
    /// skipped, insufficient liquidity).
    pub warnings: Vec<Symbol>,
    /// Timestamp when the plan was generated.
    pub created_at: u64,
}

// ---------------------------------------------------------------------------
// Rebalance execution history
// ---------------------------------------------------------------------------

/// Detailed record of a completed rebalance operation.
///
/// Appended to the execution history after every manual or scheduled
/// rebalance, providing a full audit trail.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalanceRecord {
    /// Portfolio identifier.
    pub portfolio_id: Symbol,
    /// Timestamp of the rebalance operation.
    pub timestamp: u64,
    /// Number of trades executed.
    pub trades_executed: u32,
    /// Total number of assets that were out of threshold before rebalance.
    pub assets_rebalanced: u32,
    /// Maximum absolute drift before rebalance (in bps).
    pub max_drift_before_bps: u32,
    /// Maximum absolute drift after rebalance (in bps).
    pub max_drift_after_bps: u32,
    /// Whether the operation was atomic (all trades succeeded).
    pub atomic_success: bool,
    /// "manual", "scheduled", or "simulated".
    pub execution_type: Symbol,
    /// Summary symbol: "ok", "partial", "skipped", "error".
    pub outcome: Symbol,
    /// Estimated total fees paid (in base units).
    pub total_fees: i128,
}

// ---------------------------------------------------------------------------
// Simulation mode
// ---------------------------------------------------------------------------

/// Full simulation result including pre/post drift data.
///
/// Provides a dry-run preview of what a rebalance would accomplish,
/// including per-trade details and before/after drift snapshots.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulationPlanResult {
    /// The computed rebalance plan.
    pub plan: RebalancePlan,
    /// Drift report before rebalancing.
    pub drift_before: DriftReport,
    /// Projected drift report after rebalancing (estimated).
    pub drift_after: DriftReport,
    /// Whether the rebalance would fully resolve all drift.
    pub fully_rebalanced: bool,
    /// Trades that were skipped (dust, below minimum, etc.).
    pub skipped_trades: Vec<TradeOrder>,
}

// ---------------------------------------------------------------------------
// Edge case helpers
// ---------------------------------------------------------------------------

/// Portfolio balance snapshot used for edge case validation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortfolioSnapshot {
    /// Portfolio identifier.
    pub portfolio_id: Symbol,
    /// Total portfolio value in base units (e.g., USD equivalent).
    /// `0` means value is unknown.
    pub total_value: i128,
    /// Per-asset balances (symbol → base-unit amount).
    pub balances: Map<Symbol, i128>,
}

/// Validation result for rebalance inputs.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalanceValidation {
    /// Whether all inputs are valid.
    pub valid: bool,
    /// Error or warning symbols (empty when valid).
    pub issues: Vec<Symbol>,
}

/// Drift percentage with ±0.01% accuracy, stored as basis points.
///
/// A value of `250` means `2.50%`. Negative values indicate underweight.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriftPct(pub i32);

impl DriftPct {
    /// Create from basis points.
    pub fn from_bps(bps: i32) -> Self {
        Self(bps)
    }

    /// Return the value as basis points.
    pub fn as_bps(&self) -> i32 {
        self.0
    }

    /// Return the value as a percentage (divide by 100).
    /// For display: 250 bps → "2.50%".
    pub fn percentage_bps(&self) -> i32 {
        self.0
    }

    /// Return `true` if the drift exceeds the given threshold.
    pub fn exceeds(&self, threshold_bps: u32) -> bool {
        (self.0.unsigned_abs()) > threshold_bps
    }
}
