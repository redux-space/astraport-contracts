//! Data types and storage keys for the AstraPort Asset Management contract.
//!
//! Defines the core structures for portfolio asset management, including
//! asset representation, type classification, risk levels, and storage keys.

use soroban_sdk::{contracterror, contracttype, Address, Symbol};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Fixed-point scale factor (1e18) for price representation.
pub const SCALE: i128 = 1_000_000_000_000_000_000;

/// Maximum number of assets per portfolio.
pub const MAX_ASSETS_PER_PORTFOLIO: u32 = 64;

/// Maximum asset name length in characters.
pub const MAX_NAME_LENGTH: u32 = 64;

/// Maximum contract address length.
pub const MAX_ADDRESS_LENGTH: u32 = 64;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Type of asset in the portfolio.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetType {
    /// A fungible token (e.g., XLM, USDC).
    Token,
    /// A derivative instrument (e.g., options, futures).
    Derivative,
}

impl AssetType {
    /// Validate that the asset type is supported.
    pub fn is_valid(&self) -> bool {
        matches!(self, AssetType::Token | AssetType::Derivative)
    }
}

/// Risk level classification for an asset.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    /// Low risk (e.g., stablecoins, government bonds).
    Low,
    /// Medium risk (e.g., established cryptocurrencies).
    Medium,
    /// High risk (e.g., altcoins, DeFi tokens).
    High,
    /// Very high risk (e.g., new tokens, leveraged products).
    VeryHigh,
}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// An asset held in a portfolio.
///
/// Tracks the asset's identity, type, contract address, balance, and metadata.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// Unique asset symbol (e.g., "XLM", "USDC").
    pub symbol: Symbol,
    /// Type of asset (Token or Derivative).
    pub asset_type: AssetType,
    /// On-chain contract address for the asset.
    pub contract_address: Address,
    /// Current balance held in the portfolio (in base units).
    pub balance: i128,
    /// Human-readable asset name.
    pub name: Symbol,
    /// Number of decimal places for the asset.
    pub decimals: u32,
    /// Risk level classification.
    pub risk_level: RiskLevel,
    /// Whether the asset is currently active.
    pub is_active: bool,
}

/// Price data for an asset.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetPrice {
    /// The asset symbol.
    pub symbol: Symbol,
    /// Current price in fixed-point (SCALE = 1.0).
    pub price: i128,
    /// Unix timestamp when the price was last updated.
    pub updated_at: u64,
    /// Price source identifier (e.g., "oracle", "manual").
    pub source: Symbol,
}

/// Summary information about a portfolio's assets.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioSummary {
    /// Portfolio identifier.
    pub portfolio_id: Symbol,
    /// Total number of assets in the portfolio.
    pub asset_count: u32,
    /// Total value of all assets (sum of balance * price).
    pub total_value: i128,
    /// Number of active assets.
    pub active_asset_count: u32,
}

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Storage keys for the asset management contract.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetDataKey {
    /// The contract administrator address.
    Admin,
    /// Portfolio assets: (portfolio_id) -> Vec<Asset>
    PortfolioAssets(Symbol),
    /// Individual asset lookup: (portfolio_id, asset_symbol) -> Option<Asset>
    AssetEntry(Symbol, Symbol),
    /// Asset price: (asset_symbol) -> Option<AssetPrice>
    AssetPrice(Symbol),
    /// Portfolio summary: (portfolio_id) -> Option<PortfolioSummary>
    PortfolioSummary(Symbol),
    /// List of all registered portfolios.
    Portfolios,
    /// Portfolio owner: (portfolio_id) -> Address
    PortfolioOwner(Symbol),
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the Asset Management contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum AssetError {
    /// Contract has already been initialized.
    AlreadyInitialized = 1,
    /// Caller is not the admin.
    Unauthorized = 2,
    /// Invalid asset type.
    InvalidAssetType = 3,
    /// Invalid asset symbol (empty or too long).
    InvalidSymbol = 4,
    /// Invalid asset name (empty or too long).
    InvalidName = 5,
    /// Asset already exists in the portfolio.
    AssetAlreadyExists = 6,
    /// Asset not found in the portfolio.
    AssetNotFound = 7,
    /// Cannot remove asset with non-zero balance.
    NonZeroBalance = 8,
    /// Portfolio has reached maximum asset limit.
    MaxAssetsReached = 9,
    /// Invalid contract address.
    InvalidContractAddress = 10,
    /// Invalid decimals value.
    InvalidDecimals = 11,
    /// Invalid balance value.
    InvalidBalance = 12,
    /// Invalid price value.
    InvalidPrice = 13,
    /// Portfolio not found.
    PortfolioNotFound = 14,
    /// Asset is inactive.
    AssetInactive = 15,
}
