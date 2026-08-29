//! Core data types and storage key definitions for the AstraPort Trading Engine.

use soroban_sdk::{contracterror, contracttype, Address, Symbol, Vec};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of orders per order book pair.
pub const MAX_ORDERS_PER_PAIR: u32 = 256;
/// Maximum number of asset pairs supported simultaneously.
pub const MAX_ASSET_PAIRS: u32 = 64;
/// Basis-point denominator (10 000 bps = 100%).
pub const BPS_DENOM: i128 = 10_000;
/// Default maximum slippage in basis points (5%).
pub const DEFAULT_MAX_SLIPPAGE_BPS: i128 = 500;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the TradeEngine contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum TradeError {
    /// Contract has already been initialized.
    AlreadyInitialized = 1,
    /// Caller is not the admin.
    Unauthorized = 2,
    /// The order amount is zero or negative.
    InvalidOrderAmount = 3,
    /// No order book exists for the given pair.
    PairNotFound = 4,
    /// The order was not found (already filled or cancelled).
    OrderNotFound = 5,
    /// Only the order owner can cancel their order.
    NotOrderOwner = 6,
    /// Slippage exceeds the configured threshold.
    SlippageExceeded = 7,
    /// An order's total exceeds available liquidity.
    InsufficientLiquidity = 8,
    /// The asset pair is already registered.
    PairAlreadyRegistered = 9,
    /// The maximum number of concurrent orders has been reached.
    MaxOrdersReached = 10,
    /// Arithmetic overflow or underflow.
    ArithmeticOverflow = 11,
    /// Atomic batch is empty (no legs to execute).
    EmptyBatch = 12,
    /// The order book is empty for the requested side.
    NoMatchingOrders = 13,
    /// The trade pair is not active.
    PairInactive = 14,
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Side of an order in the book.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    /// A buy order (bid) — wants to acquire the base asset.
    Buy,
    /// A sell order (ask) — wants to sell the base asset.
    Sell,
}

/// Status of an order.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    /// Order is live in the book and waiting to be matched.
    Active,
    /// Order has been partially filled; remainder is still live.
    PartiallyFilled,
    /// Order has been fully filled.
    Filled,
    /// Order has been cancelled by the owner or admin.
    Cancelled,
}

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Top-level storage keys used by the contract.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradeDataKey {
    /// The contract administrator address.
    Admin,
    /// Registered trading pairs.
    Pairs,
    /// Active orders for a given pair: `(pair_id, order_id)`.
    Order(Symbol, u64),
    /// The order book for a given pair: `(pair_id)`.
    OrderBook(Symbol),
    /// Order ID counter for a given pair: `(pair_id)`.
    OrderIdCounter(Symbol),
    /// Global order ID counter.
    GlobalOrderIdCounter,
    /// Slippage config for a given pair: `(pair_id)`.
    SlippageConfig(Symbol),
    /// The pending atomic batch for a given user: `(user)`.
    PendingBatch(Address),
    /// Trade history log.
    TradeHistory,
    /// Total volume traded for a pair: `(pair_id)`.
    PairVolume(Symbol),
    /// Registered pair list.
    PairList,
    /// Optional audit-log sink address.
    AuditSink,
}

// ---------------------------------------------------------------------------
// Core structs
// ---------------------------------------------------------------------------

/// Configuration for a trading pair (e.g., "XLM/USDC").
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradePair {
    /// Unique symbol identifier, e.g. `symbol_short!("XLM_USDC")`.
    pub pair_id: Symbol,
    /// The base asset symbol (the asset being priced/traded).
    pub base_asset: Symbol,
    /// The quote asset symbol (the denomination asset).
    pub quote_asset: Symbol,
    /// Whether the pair is currently active for trading.
    pub is_active: bool,
    /// Minimum order size in base-asset units.
    pub min_order_size: i128,
    /// Maximum order size in base-asset units.
    pub max_order_size: i128,
    /// Trading fee in basis points.
    pub fee_bps: i128,
}

/// A single order in the book.
#[contracttype]
#[derive(Debug, Clone)]
pub struct Order {
    /// Unique order ID (monotonically increasing per pair).
    pub order_id: u64,
    /// The trading pair this order belongs to.
    pub pair_id: Symbol,
    /// Owner of the order.
    pub owner: Address,
    /// Buy or Sell side.
    pub side: OrderSide,
    /// Limit price in quote-asset units per base-asset unit (scaled).
    pub price: i128,
    /// Quantity in base-asset units.
    pub amount: i128,
    /// Remaining quantity that has not been filled.
    pub remaining: i128,
    /// Current status.
    pub status: OrderStatus,
    /// Ledger timestamp when the order was placed.
    pub created_at: u64,
}

/// The order book for a single trading pair, maintained as separate bid/ask
/// `Vec`s sorted by price.
#[contracttype]
#[derive(Debug, Clone)]
pub struct OrderBook {
    /// The pair this book belongs to.
    pub pair_id: Symbol,
    /// Buy orders (bids), sorted best-first (highest price first).
    pub bids: Vec<Order>,
    /// Sell orders (asks), sorted best-first (lowest price first).
    pub asks: Vec<Order>,
}

/// Per-pair slippage protection configuration.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlippageConfig {
    /// Maximum allowed slippage in basis points (0–10 000).
    /// Execution fails if the fill price deviates from the limit price by
    /// more than this.
    pub max_slippage_bps: i128,
    /// Whether slippage protection is enabled for this pair.
    pub enabled: bool,
}

impl Default for SlippageConfig {
    fn default() -> Self {
        Self {
            max_slippage_bps: DEFAULT_MAX_SLIPPAGE_BPS,
            enabled: true,
        }
    }
}

/// A single fill event produced during order matching.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fill {
    /// The order that was matched against.
    pub order_id: u64,
    /// The trading pair.
    pub pair_id: Symbol,
    /// Taker (the aggressor) address.
    pub taker: Address,
    /// Maker (passive order owner) address.
    pub maker: Address,
    /// Side of the taker's order.
    pub side: OrderSide,
    /// Fill price.
    pub price: i128,
    /// Filled quantity.
    pub amount: i128,
    /// Fee charged to the taker, in quote-asset units.
    pub fee: i128,
}

/// A trade pair execution leg within an atomic multi-asset batch.
#[contracttype]
#[derive(Debug, Clone)]
pub struct TradeLeg {
    /// The trading pair.
    pub pair_id: Symbol,
    /// Buy or Sell side.
    pub side: OrderSide,
    /// Limit price.
    pub price: i128,
    /// Quantity to trade.
    pub amount: i128,
    /// Maximum allowed slippage for this leg (in bps), or `None` to use
    /// the pair default.
    pub max_slippage_bps: Option<i128>,
}

/// Result of executing a single leg in a batch.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegResult {
    /// The pair that was traded.
    pub pair_id: Symbol,
    /// All fills produced for this leg.
    pub fills: Vec<Fill>,
    /// Total base-asset quantity filled.
    pub filled_amount: i128,
    /// Average fill price (0 if no fills).
    pub avg_price: i128,
    /// Total fees paid.
    pub total_fees: i128,
}

/// The result of an atomic multi-asset batch execution.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtomicBatchResult {
    /// Whether the entire batch succeeded.
    pub success: bool,
    /// Per-leg results.
    pub legs: Vec<LegResult>,
    /// Total number of fills across all legs.
    pub total_fills: u32,
    /// Overall execution timestamp.
    pub executed_at: u64,
    /// Reason for failure (empty string if successful).
    pub failure_reason: Symbol,
}

/// Aggregate statistics for a trading pair.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairStats {
    /// Total volume (in quote-asset units) traded on this pair.
    pub total_volume: i128,
    /// Number of orders that have been placed on this pair.
    pub total_orders: u64,
    /// Current best bid price (0 if no bids).
    pub best_bid: i128,
    /// Current best ask price (0 if no asks).
    pub best_ask: i128,
    /// Current spread (best_ask - best_bid), 0 if either side is empty.
    pub spread: i128,
}

/// Summary of an order book for read queries.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderBookSnapshot {
    /// Pair identifier.
    pub pair_id: Symbol,
    /// Number of active bids.
    pub bid_count: u32,
    /// Number of active asks.
    pub ask_count: u32,
    /// Best bid price (0 if empty).
    pub best_bid: i128,
    /// Best ask price (0 if empty).
    pub best_ask: i128,
    /// Spread in quote-asset units.
    pub spread: i128,
}
