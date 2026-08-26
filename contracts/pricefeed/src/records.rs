//! Data types and storage keys for the Price Feed Oracle contract.

use soroban_sdk::{contracterror, contracttype, Address, Symbol};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum allowed price deviation in basis points (e.g., 5000 = 50%) before
/// a price is flagged as anomalous.
pub const DEFAULT_MAX_DEVIATION_BPS: i128 = 5000;
/// Default time-to-live for cached prices in seconds (5 minutes).
pub const DEFAULT_CACHE_TTL_SECONDS: u64 = 300;
/// Maximum number of historical price points stored per asset.
pub const MAX_PRICE_HISTORY: u32 = 100;
/// Number of decimals used for price representation (8 decimals = 1e8).
pub const PRICE_PRECISION: i128 = 100_000_000;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Aggregation strategy when multiple oracle providers report prices.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AggregationMethod {
    /// Use the median price across all sources (resistant to outliers).
    Median,
    /// Use the most recently updated price.
    Latest,
    /// Time-weighted average price over a configurable window.
    TWAP,
    /// Weighted average using provider trust weights.
    WeightedAverage,
}

/// Status of a price data point.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PriceStatus {
    /// Price is fresh and valid.
    Fresh,
    /// Price has exceeded the configured TTL.
    Stale,
    /// Price was flagged as anomalous (deviates too much from the median/mean).
    Anomalous,
    /// Price is from a fallback source (primary oracle failed).
    Fallback,
    /// No price data available.
    Unknown,
}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// An oracle provider registration.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleProvider {
    /// Unique identifier for this oracle provider.
    pub provider_id: Symbol,
    /// Human-readable name.
    pub name: Symbol,
    /// Contract address that serves as the oracle endpoint.
    pub endpoint: Address,
    /// Trust weight for weighted-average aggregation (0–10 000 bps).
    pub trust_weight: u32,
    /// Whether this oracle is currently active.
    pub is_active: bool,
    /// Maximum acceptable staleness in seconds before this oracle is skipped.
    pub max_staleness: u64,
}

/// A single price observation from one oracle provider.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceDataPoint {
    /// The asset being priced (e.g., "USDC", "ETH").
    pub asset: Symbol,
    /// The oracle provider that reported this price.
    pub provider_id: Symbol,
    /// The price in the smallest unit (scaled by PRICE_PRECISION).
    pub price: i128,
    /// Unix timestamp when the price was observed.
    pub timestamp: u64,
    /// Confidence interval in basis points around the price.
    pub confidence_bps: u32,
    /// Status of this price point.
    pub status: PriceStatus,
}

/// Aggregated price for an asset across all oracle sources.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregatedPrice {
    /// The asset being priced.
    pub asset: Symbol,
    /// The aggregated price (scaled by PRICE_PRECISION).
    pub price: i128,
    /// Unix timestamp of the aggregation.
    pub timestamp: u64,
    /// Number of oracle sources that contributed to this price.
    pub num_sources: u32,
    /// Aggregation method used.
    pub method: AggregationMethod,
    /// Overall status (worst status among contributors).
    pub status: PriceStatus,
}

/// A historical price entry for trend analysis.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceHistoryEntry {
    /// The aggregated price at this point in time.
    pub price: i128,
    /// Unix timestamp of this history entry.
    pub timestamp: u64,
    /// Number of sources that contributed.
    pub num_sources: u32,
}

/// Cache entry for a price feed, tracking freshness.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedPrice {
    /// The cached aggregated price.
    pub aggregated: AggregatedPrice,
    /// Unix timestamp when the cache entry was created.
    pub cached_at: u64,
    /// Time-to-live in seconds for this cache entry.
    pub ttl_seconds: u64,
}

/// Configuration for price validation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceValidationConfig {
    /// Maximum allowed deviation from the median in basis points.
    /// Prices deviating more than this are flagged as anomalous.
    pub max_deviation_bps: i128,
    /// Default cache TTL in seconds.
    pub default_ttl_seconds: u64,
    /// Maximum number of history entries per asset.
    pub max_history_entries: u32,
    /// Whether to emit events on anomalous price detection.
    pub alert_on_anomaly: bool,
}

/// Batch request item for fetching multiple asset prices at once.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchPriceRequest {
    /// The asset to request a price for.
    pub asset: Symbol,
    /// Override for the aggregation method.
    pub method_override: AggregationMethod,
}

/// Batch response item with the aggregated price.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchPriceResponse {
    /// The asset.
    pub asset: Symbol,
    /// The aggregated price.
    pub price: AggregatedPrice,
    /// Whether a price was found for this asset.
    pub found: bool,
}

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Storage keys for the pricefeed contract.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PriceFeedDataKey {
    /// Global admin address.
    Admin,
    /// Validation configuration.
    ValidationConfig,
    /// All registered oracle providers: Map<Symbol, OracleProvider>.
    Oracles,
    /// Latest cached price per asset: Map<Symbol, CachedPrice>.
    CachedPrices(Symbol),
    /// Most recent raw price from each oracle per asset:
    /// Map<(Symbol, Symbol), PriceDataPoint> keyed by (asset, provider_id).
    LatestDataPoint(Symbol, Symbol),
    /// Price history per asset: Vec<PriceHistoryEntry> (capped).
    PriceHistory(Symbol),
    /// Global aggregation method default.
    DefaultAggregationMethod,
    /// Set of tracked asset symbols: Vec<Symbol>.
    TrackedAssets,
    /// Fallback prices set by admin for emergency use: Map<Symbol, i128>.
    FallbackPrices,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the price feed contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum PriceFeedError {
    /// Caller is not the admin.
    Unauthorized = 1,
    /// Oracle provider already registered.
    OracleAlreadyExists = 2,
    /// Oracle provider not found.
    OracleNotFound = 3,
    /// Price data is stale (exceeded TTL).
    PriceStale = 4,
    /// Price flagged as anomalous.
    PriceAnomalous = 5,
    /// No price data available for the asset.
    NoPriceData = 6,
    /// No fallback price set for the asset.
    NoFallbackPrice = 7,
    /// Asset not being tracked.
    AssetNotTracked = 8,
    /// Batch request contains too many items.
    BatchTooLarge = 9,
    /// Invalid configuration values.
    InvalidConfig = 10,
    /// Oracle endpoint is not active.
    OracleInactive = 11,
}
