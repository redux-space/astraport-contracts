//! Core data types and storage key definitions for the Prediction Markets module.

use soroban_sdk::{contracterror, contracttype, Address, Symbol, Vec};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// 18-decimal precision factor (10^18).
pub const DECIMAL_PRECISION: i128 = 1_000_000_000_000_000_000;

/// Basis-point denominator (10,000 bps = 100%).
pub const BPS_DENOM: i128 = 10_000;

/// Maximum number of outcomes per market.
pub const MAX_OUTCOMES_PER_MARKET: u32 = 10;

/// Maximum number of active markets (design target: 1M+ but capped at Vec limits for scalability).
pub const MAX_ACTIVE_MARKETS: u32 = 10_000;

/// Default trading fee in basis points (0.3%).
pub const DEFAULT_TRADING_FEE_BPS: i128 = 30;

/// Minimum liquidity for a market to be tradable.
pub const MIN_LIQUIDITY: i128 = 1_000; // In collateral units (USDC)

/// Dispute period in seconds (24 hours).
pub const DISPUTE_PERIOD_SECS: u64 = 86_400;

/// LP token scaling factor for fee distribution.
pub const LP_FEE_SCALE: i128 = 1_000_000;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the PredictionMarket contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum PredictionError {
    /// Contract has already been initialized.
    AlreadyInitialized = 1,
    /// Caller is not the admin.
    Unauthorized = 2,
    /// The market was not found.
    MarketNotFound = 3,
    /// Invalid market parameters (e.g., zero outcomes, invalid times).
    InvalidMarketParams = 4,
    /// The market is not in a tradable state.
    MarketNotTradable = 5,
    /// The outcome index is out of bounds.
    InvalidOutcomeIndex = 6,
    /// Insufficient balance for the operation.
    InsufficientBalance = 7,
    /// The market has already been resolved.
    MarketAlreadyResolved = 8,
    /// The market has not yet been resolved.
    MarketNotResolved = 9,
    /// The outcome token being redeemed is not the winning outcome.
    NotWinningOutcome = 10,
    /// The resolution window has not closed yet.
    ResolutionWindowNotClosed = 11,
    /// The market has been disputed and cannot be resolved until the dispute is settled.
    MarketDisputed = 12,
    /// No dispute exists for this market.
    NoDispute = 13,
    /// The dispute period has expired.
    DisputePeriodExpired = 14,
    /// The dispute period has not yet expired (cannot resolve yet).
    DisputePeriodNotExpired = 15,
    /// Arithmetic overflow or underflow.
    ArithmeticOverflow = 16,
    /// The market cap has been reached.
    MarketCapReached = 17,
    /// No liquidity pool exists for this market.
    NoLiquidityPool = 18,
    /// Insufficient liquidity for the trade.
    InsufficientLiquidity = 19,
    /// The order price is invalid.
    InvalidOrderPrice = 20,
    /// The order amount is invalid.
    InvalidOrderAmount = 21,
    /// Order not found.
    OrderNotFound = 22,
    /// Only the order owner can cancel.
    NotOrderOwner = 23,
    /// The oracle source is not configured for this market.
    OracleNotConfigured = 24,
    /// Slippage tolerance exceeded.
    SlippageExceeded = 25,
    /// Maximum number of outcomes exceeded.
    TooManyOutcomes = 26,
    /// Invalid category.
    InvalidCategory = 27,
    /// Cannot close market that has active positions.
    ActivePositionsExist = 28,
    /// An item already exists (e.g., duplicate dispute).
    AlreadyExists = 29,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Market status lifecycle.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketStatus {
    /// Market is accepting trades and LP provision.
    Active,
    /// Resolution window closed; awaiting oracle resolution.
    PendingResolution,
    /// Market has been resolved with a winning outcome.
    Resolved,
    /// Market is under dispute.
    Disputed,
    /// Market was closed early by admin/creator.
    Closed,
    /// Market was cancelled (no trades occurred or admin action).
    Cancelled,
}

/// Market categories for filtering.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketCategory {
    Sports,
    Crypto,
    Politics,
    Events,
    Weather,
    Other,
}

/// Dispute status.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeStatus {
    /// Dispute has been filed and is under review.
    Pending,
    /// Dispute was accepted; resolution is overturned.
    Accepted,
    /// Dispute was rejected; original resolution stands.
    Rejected,
}

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Top-level storage keys used by the contract.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredictionDataKey {
    /// The contract administrator address.
    Admin,
    /// Registered market by ID.
    Market(u64),
    /// List of all market IDs.
    MarketList,
    /// Outcome token balances: (market_id, user, outcome_index).
    OutcomeBalance(u64, Address, u32),
    /// Collateral (USDC) balances: (user).
    CollateralBalance(Address),
    /// CPMM pool for a market: (market_id).
    LiquidityPool(u64),
    /// LP token balances: (market_id, user).
    LPBalance(u64, Address),
    /// Total LP supply for a market: (market_id).
    LPTotalSupply(u64),
    /// User positions: (market_id, user).
    Position(u64, Address),
    /// Order book for a market: (market_id).
    OrderBook(u64),
    /// Order for a market: (market_id, order_id).
    Order(u64, u64),
    /// Order ID counter: (market_id).
    OrderIdCounter(u64),
    /// Market ID counter.
    MarketIdCounter,
    /// Oracle source configuration for a market: (market_id).
    OracleSource(u64),
    /// Oracle resolution data for a market: (market_id).
    ResolutionData(u64),
    /// Dispute for a market: (market_id).
    Dispute(u64),
    /// User's total deposited collateral: (user).
    TotalDeposited(Address),
    /// Market trading volume: (market_id).
    TradingVolume(u64),
    /// Markets by category.
    MarketsByCategory(MarketCategory),
    /// Total fees collected per market: (market_id).
    TotalFeesCollected(u64),
}

// ---------------------------------------------------------------------------
// Core structs
// ---------------------------------------------------------------------------

/// Represents a single outcome in a prediction market.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The outcome name (e.g., "Yes", "No", "Team A Wins").
    pub name: Symbol,
    /// Index of this outcome (0-based).
    pub index: u32,
}

/// Configuration for a prediction market.
#[contracttype]
#[derive(Debug, Clone)]
pub struct Market {
    /// Unique market identifier.
    pub market_id: u64,
    /// Human-readable market question/description.
    pub description: Symbol,
    /// Category for filtering.
    pub category: MarketCategory,
    /// The outcomes for this market.
    pub outcomes: Vec<Outcome>,
    /// Collateral token symbol (e.g., "USDC").
    pub collateral_token: Symbol,
    /// Market status.
    pub status: MarketStatus,
    /// Timestamp when the market was created.
    pub created_at: u64,
    /// Timestamp when trading ends (resolution window starts).
    pub trading_end_time: u64,
    /// Timestamp when the resolution window closes.
    pub resolution_time: u64,
    /// The oracle source for resolving this market.
    pub oracle_source: Symbol,
    /// The resolved outcome index (only valid after resolution).
    pub resolved_outcome: Option<u32>,
    /// Market creator.
    pub creator: Address,
    /// Maximum total supply of outcome tokens (market cap).
    pub max_outcome_supply: i128,
    /// Total collateral deposited in the CPMM pool.
    pub total_collateral: i128,
    /// Trading fee in basis points.
    pub fee_bps: i128,
    /// Whether the market allows early closing.
    pub allow_early_close: bool,
    /// If resolved, timestamp of resolution.
    pub resolved_at: Option<u64>,
}

impl Market {
    /// Check if the market is in a tradable state.
    pub fn is_tradable(&self) -> bool {
        self.status == MarketStatus::Active
    }

    /// Check if the resolution window has closed.
    pub fn is_resolution_window_closed(&self, now: u64) -> bool {
        now >= self.resolution_time
    }

    /// Check if the trading window is open.
    pub fn is_trading_open(&self, now: u64) -> bool {
        self.status == MarketStatus::Active && now < self.trading_end_time
    }

    /// Get the number of outcomes.
    pub fn num_outcomes(&self) -> u32 {
        self.outcomes.len()
    }

    /// Check if this is a binary market (exactly 2 outcomes).
    pub fn is_binary(&self) -> bool {
        self.outcomes.len() == 2
    }
}

/// The Constant Product Market Maker pool for a market.
///
/// Maintains the invariant: product(reserves) = k
/// Each outcome has its own reserve of outcome tokens and shares a common
/// collateral reserve.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidityPool {
    /// Market this pool belongs to.
    pub market_id: u64,
    /// Reserve of collateral (USDC) in the pool.
    pub collateral_reserve: i128,
    /// Reserve of outcome tokens per outcome index.
    pub outcome_reserves: Vec<i128>,
    /// Total LP tokens minted for this pool.
    pub lp_supply: i128,
    /// The constant product invariant k = collateral * product(outcome_reserves).
    /// Stored as a numerator; actual k = collateral_reserve * product(outcome_reserves).
    /// We store the per-outcome pair product for efficiency.
    /// k_per_outcome[i] = collateral_reserve * outcome_reserves[i]
    pub k_per_outcome: Vec<i128>,
    /// Trading fees accumulated (not yet distributed).
    pub fees_accumulated: i128,
    /// Total trading volume through this pool.
    pub total_volume: i128,
}

/// A limit order in the outcome token order book.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictionOrder {
    /// Unique order ID.
    pub order_id: u64,
    /// Market ID.
    pub market_id: u64,
    /// Owner of the order.
    pub owner: Address,
    /// Outcome index this order is for.
    pub outcome_index: u32,
    /// Buy or Sell side.
    pub side: OrderSide,
    /// Price per outcome token in collateral units (scaled by DECIMAL_PRECISION).
    pub price: i128,
    /// Amount of outcome tokens.
    pub amount: i128,
    /// Remaining amount not yet filled.
    pub remaining: i128,
    /// Current status.
    pub status: OrderStatus,
    /// Ledger timestamp when the order was placed.
    pub created_at: u64,
}

/// Side of an order.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Status of an order.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Active,
    PartiallyFilled,
    Filled,
    Cancelled,
}

/// The order book for outcome tokens in a market.
#[contracttype]
#[derive(Debug, Clone)]
pub struct OutcomeOrderBook {
    /// Market ID.
    pub market_id: u64,
    /// Active buy orders (bids).
    pub bids: Vec<PredictionOrder>,
    /// Active sell orders (asks).
    pub asks: Vec<PredictionOrder>,
}

/// A user's position in a prediction market.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    /// Market ID.
    pub market_id: u64,
    /// User address.
    pub user: Address,
    /// Amount of outcome tokens held per outcome index.
    pub outcome_amounts: Vec<i128>,
    /// Average entry price per outcome token.
    pub entry_prices: Vec<i128>,
    /// Total collateral spent on this position.
    pub total_spent: i128,
    /// Whether the position has been settled.
    pub settled: bool,
    /// Profit/loss after settlement (only if settled).
    pub pnl: Option<i128>,
}

/// Oracle source configuration for a market.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleSource {
    /// Oracle provider identifier.
    pub provider_id: Symbol,
    /// The data feed endpoint identifier.
    pub feed_id: Symbol,
    /// Maximum staleness allowed (in seconds).
    pub max_staleness: u64,
    /// Whether the oracle is currently active.
    pub is_active: bool,
}

/// Resolution data submitted by oracles.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionData {
    /// Market ID.
    pub market_id: u64,
    /// The resolved outcome index.
    pub resolved_outcome: u32,
    /// Oracle provider that submitted the resolution.
    pub oracle_provider: Symbol,
    /// Timestamp of resolution submission.
    pub submitted_at: u64,
    /// Whether this resolution has been confirmed.
    pub confirmed: bool,
}

/// A dispute filed against a market resolution.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispute {
    /// Market ID.
    pub market_id: u64,
    /// Address of the dispute filer.
    pub disputer: Address,
    /// The outcome the disputer claims is correct.
    pub claimed_outcome: u32,
    /// Evidence/reason for the dispute (as a symbol for simplicity).
    pub evidence: Symbol,
    /// Timestamp when the dispute was filed.
    pub filed_at: u64,
    /// Current dispute status.
    pub status: DisputeStatus,
    /// Amount of collateral staked as dispute bond.
    pub bond_amount: i128,
    /// Timestamp when the dispute was resolved (if resolved).
    pub resolved_at: Option<u64>,
}

/// A trade fill event.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeFill {
    /// The order that was matched against.
    pub order_id: u64,
    /// Taker address.
    pub taker: Address,
    /// Maker address.
    pub maker: Address,
    /// Outcome index traded.
    pub outcome_index: u32,
    /// Side of the taker.
    pub side: OrderSide,
    /// Fill price.
    pub price: i128,
    /// Filled quantity.
    pub amount: i128,
    /// Fee charged.
    pub fee: i128,
}

/// Snapshot of the order book for a market.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderBookSnapshot {
    /// Market ID.
    pub market_id: u64,
    /// Number of active bids.
    pub bid_count: u32,
    /// Number of active asks.
    pub ask_count: u32,
    /// Best bid price (0 if empty).
    pub best_bid: i128,
    /// Best ask price (0 if empty).
    pub best_ask: i128,
    /// Spread.
    pub spread: i128,
}

/// Market statistics for queries.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketStats {
    /// Market ID.
    pub market_id: u64,
    /// Total trading volume.
    pub total_volume: i128,
    /// Total number of trades.
    pub total_trades: u64,
    /// Total fees collected.
    pub total_fees: i128,
    /// Number of participants.
    pub participants: u64,
    /// Current CPMM prices for each outcome.
    pub prices: Vec<i128>,
}

/// Summary result of a CPMM swap.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapResult {
    /// Amount of outcome tokens received (buy) or sent (sell).
    pub outcome_amount: i128,
    /// Amount of collateral spent (buy) or received (sell).
    pub collateral_amount: i128,
    /// Fee charged in collateral.
    pub fee: i128,
    /// Price impact in basis points.
    pub price_impact_bps: i128,
}

/// LP provision result.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LPResult {
    /// Number of LP tokens minted.
    pub lp_tokens_minted: i128,
    /// Amount of collateral deposited.
    pub collateral_deposited: i128,
    /// Amount of outcome tokens deposited per outcome.
    pub outcome_deposits: Vec<i128>,
}
