//! Data types and storage keys for the AstraPort Fee Management contract.
//!
//! Defines the core structures for fee configuration, calculation, collection,
//! revenue distribution, and reporting.
//!
//! Note: Soroban SDK v21 does not support `Option<Symbol>` or `Option<Address>`
//! inside `#[contracttype]` structs.  Where an "optional" value is needed, we
//! use sentinel values:
//!
//! * **Symbol** — an empty symbol (`symbol_short!("")`) means "not set".
//! * **bool flag** — companion booleans (`has_address`, `has_portfolio`)
//!   indicate whether the accompanying field is meaningful.

use soroban_sdk::{contracterror, contracttype, symbol_short, Address, Symbol};

// ---------------------------------------------------------------------------

/// Sentinel symbol representing "not set" / "any".
pub fn sym_empty() -> Symbol {
    symbol_short!("")
}

// ---------------------------------------------------------------------------

/// Basis-point denominator (100% = 10_000 bps).
pub const BPS_DENOM: i128 = 10_000;

/// Maximum number of fee records retained in the circular history buffer.
pub const MAX_HISTORY: u32 = 200;

/// Maximum number of revenue distribution recipients.
pub const MAX_RECIPIENTS: u32 = 20;

/// Maximum number of fee waivers.
pub const MAX_WAIVERS: u32 = 50;

/// Maximum number of fee categories that can be tracked simultaneously.
pub const MAX_CATEGORIES: u32 = 16;

// ---------------------------------------------------------------------------

/// Errors returned by the Fee Management contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum FeeError {
    FeeNotFound = 1,
    FeeInactive = 2,
    InvalidFeeConfiguration = 3,
    ArithmeticOverflow = 4,
    FeeWaiverNotFound = 5,
    TooManyRecipients = 6,
    AlreadyInitialized = 7,
    Unauthorized = 8,
    FeeCapExceeded = 9,
    InvalidFeeCategory = 10,
}

// ---------------------------------------------------------------------------

/// The mathematical model used to calculate a fee.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeeType {
    Flat,
    Percentage,
    Tiered,
}

/// High-level category for grouping and reporting fees.
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeeCategory {
    Rebalancing,
    Yield,
    Management,
    Trading,
    Protocol,
    Custom,
}

impl FeeCategory {
    pub fn label(&self) -> &'static str {
        match self {
            FeeCategory::Rebalancing => "rebalancing",
            FeeCategory::Yield => "yield",
            FeeCategory::Management => "management",
            FeeCategory::Trading => "trading",
            FeeCategory::Protocol => "protocol",
            FeeCategory::Custom => "custom",
        }
    }
}

// ---------------------------------------------------------------------------

/// A single tier in a [`FeeType::Tiered`] fee structure.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TierEntry {
    pub threshold: i128,
    pub fee_bps: i128,
}

/// Complete definition of a fee model.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeStructure {
    pub fee_id: Symbol,
    pub fee_type: FeeType,
    /// For `Flat`: the fixed amount.  For `Percentage`: rate in bps.
    pub amount_bps: i128,
    pub tiered_entries: soroban_sdk::Vec<TierEntry>,
    pub category: FeeCategory,
    pub active: bool,
    /// Optional maximum fee cap in base units.  `None` means no cap.
    /// `Option<i128>` is supported because i128 is a scalar type.
    pub fee_cap: Option<i128>,
}

// ---------------------------------------------------------------------------

/// A revenue recipient with their proportional share.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevenueRecipient {
    /// Wallet address that receives revenue.
    pub address: Address,
    /// Numerator of the recipient's share (denominator is the sum of all).
    pub share_numerator: u32,
    /// Optional label — use [`sym_empty`] for "no label".
    pub label: Symbol,
}

// ---------------------------------------------------------------------------

/// An immutable record of a single fee collection event.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeRecord {
    pub fee_id: Symbol,
    pub category: FeeCategory,
    pub portfolio_id: Symbol,
    pub base_amount: i128,
    pub fee_amount: i128,
    pub discount_bps: i128,
    pub waived: bool,
    pub timestamp: u64,
    pub collector: Address,
}

// ---------------------------------------------------------------------------

/// A waiver or discount rule that can be applied during fee collection.
///
/// The fields `address` and `portfolio_id` are always present, but
/// `has_address` / `has_portfolio` control whether they participate in
/// matching.  This avoids `Option<Address>` / `Option<Symbol>` inside a
/// `contracttype` (unsupported in Soroban SDK v21).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeWaiver {
    /// The address this waiver applies to (ignored when `has_address == false`).
    pub address: Address,
    /// Whether address-based matching is active.
    pub has_address: bool,
    /// The portfolio this waiver applies to (ignored when `has_portfolio == false`).
    pub portfolio_id: Symbol,
    /// Whether portfolio-based matching is active.
    pub has_portfolio: bool,
    /// Discount in basis points (0 = no discount, 10_000 = full waiver).
    pub discount_bps: i128,
    /// If true, the fee is completely waived regardless of `discount_bps`.
    pub waived: bool,
    /// Optional label — use [`sym_empty`] for "no label".
    pub label: Symbol,
    /// Expiry timestamp (0 = never expires).
    pub expires_at: u64,
}

// ---------------------------------------------------------------------------

/// The result of a fee calculation (read-only, does not mutate storage).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeCalculationResult {
    pub fee_id: Symbol,
    pub category: FeeCategory,
    pub gross_amount: i128,
    pub raw_fee: i128,
    pub discount_bps: i128,
    pub fee_cap: Option<i128>,
    pub fee_amount: i128,
    pub waived: bool,
}

// ---------------------------------------------------------------------------

/// Aggregated fee summary for a reporting period.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeSummary {
    pub total_collected: i128,
    pub total_events: u32,
    pub by_category: soroban_sdk::Vec<(FeeCategory, i128)>,
    pub by_portfolio: soroban_sdk::Vec<(Symbol, i128)>,
    pub total_discounts: i128,
    pub waived_count: u32,
}

/// Per-portfolio fee breakdown.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortfolioFeeReport {
    pub portfolio_id: Symbol,
    pub total_fees: i128,
    pub event_count: u32,
    pub by_category: soroban_sdk::Vec<(FeeCategory, i128)>,
    /// The fee structure currently assigned — [`sym_empty`] if none.
    pub assigned_fee_id: Symbol,
}

// ---------------------------------------------------------------------------

/// Typed storage keys for the fee management contract.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FeeDataKey {
    Admin,
    FeeIds,
    FeeStructure(Symbol),
    PortfolioFee(Symbol),
    FeeHistory,
    FeeWaivers,
    RevenueRecipients,
    TotalCollected,
    CategoryTotal(FeeCategory),
    PortfolioTotal(Symbol),
    PortfolioFeeCap(Symbol),
    GlobalFeeCap,
}
