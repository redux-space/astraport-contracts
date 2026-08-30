//! Soroban-typed records for tracking yield accrual, history, and schedules.
//!
//! These types are marked `#[contracttype]` so they can be persisted in
//! contract storage and returned across the contract boundary. They wrap the
//! pure-math results from [`crate::compounding`] and [`crate::apy`] into durable,
//! queryable structures keyed by staker and asset.

use soroban_sdk::{contracttype, Address, Symbol};

/// The compounding model, mirrored as a `#[contracttype]` for storage.
///
/// [`crate::compounding::Compounding`] is the pure-Rust enum used by the math
/// layer; this is its serializable twin used at the contract boundary. Convert
/// between them with [`CompoundingMode::to_strategy`] and [`CompoundingMode::from_strategy`].
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundingMode {
    /// Daily compounding (365 periods/year).
    Daily,
    /// Continuous compounding (`e^(rt)`).
    Continuous,
}

impl CompoundingMode {
    /// Convert to the pure-math [`crate::compounding::Compounding`] strategy.
    pub fn to_strategy(self) -> crate::compounding::Compounding {
        match self {
            CompoundingMode::Daily => crate::compounding::Compounding::Daily,
            CompoundingMode::Continuous => crate::compounding::Compounding::Continuous,
        }
    }

    /// Build from the pure-math [`crate::compounding::Compounding`] strategy.
    pub fn from_strategy(s: crate::compounding::Compounding) -> Self {
        match s {
            crate::compounding::Compounding::Daily => CompoundingMode::Daily,
            crate::compounding::Compounding::Continuous => CompoundingMode::Continuous,
        }
    }
}

// ---------------------------------------------------------------------------
// Core yield records
// ---------------------------------------------------------------------------

/// A staker's active yield-accruing position for a single asset.
///
/// `accrued_yield` is the yield already realized and checkpointed up to
/// `last_accrual_ts`; yield earned since then is computed on demand from the
/// current time. `apr` is fixed-point (see [`crate::fixed_point::SCALE`]).
#[contracttype]
#[derive(Debug, Clone)]
pub struct YieldRecord {
    /// The staker who owns the position.
    pub staker: Address,
    /// The asset being staked (a symbol such as `XLM`, `USDC`).
    pub asset: Symbol,
    /// Principal currently staked, in the asset's base units.
    pub principal: i128,
    /// Current annual percentage rate for this position, fixed-point.
    pub apr: i128,
    /// Compounding model applied to this position.
    pub mode: CompoundingMode,
    /// Ledger timestamp (seconds) at which yield was last checkpointed.
    pub last_accrual_ts: u64,
    /// Yield realized and checkpointed up to `last_accrual_ts`, base units.
    pub accrued_yield: i128,
}

/// A single immutable entry in a staker/asset yield history log.
///
/// One entry is appended each time yield is checkpointed (accrued), the rate
/// changes, or yield is claimed, forming a complete, queryable audit trail.
#[contracttype]
#[derive(Debug, Clone)]
pub struct YieldHistoryEntry {
    /// Ledger timestamp (seconds) the entry covers up to.
    pub timestamp: u64,
    /// Duration in seconds this entry accounts for since the previous entry.
    pub period_seconds: u64,
    /// APR in effect over this period, fixed-point.
    pub apr: i128,
    /// Yield earned during this period, base units.
    pub yield_earned: i128,
    /// Cumulative unclaimed yield after this entry, base units.
    pub cumulative_yield: i128,
    /// True when this is a zero-period marker recording a yield claim.
    /// Claim markers have `yield_earned == 0` and `cumulative_yield == 0`.
    pub is_claim: bool,
}

/// A projected future-earnings estimate for a position.
#[contracttype]
#[derive(Debug, Clone)]
pub struct YieldProjection {
    /// Principal the projection is based on, base units.
    pub principal: i128,
    /// APR assumed for the projection, fixed-point.
    pub apr: i128,
    /// Compounding model assumed.
    pub mode: CompoundingMode,
    /// Horizon of the projection in seconds from now.
    pub horizon_seconds: u64,
    /// Projected yield over the horizon, base units.
    pub projected_yield: i128,
    /// Projected total balance (principal + yield) at the horizon, base units.
    pub projected_balance: i128,
    /// Effective APY implied by the assumed APR and mode, fixed-point.
    pub effective_apy: i128,
}

/// A scheduled yield distribution to a staker.
#[contracttype]
#[derive(Debug, Clone)]
pub struct DistributionSchedule {
    /// The staker to receive the distribution.
    pub staker: Address,
    /// The asset being distributed.
    pub asset: Symbol,
    /// Ledger timestamp (seconds) at which the distribution becomes due.
    pub due_ts: u64,
    /// Interval in seconds between recurring distributions (0 = one-off).
    pub interval_seconds: u64,
    /// Amount scheduled for distribution, base units.
    pub amount: i128,
    /// Whether this schedule has been fully distributed / closed.
    pub executed: bool,
}

/// Describes the lock-up parameters for a staker's position.
///
/// When a staker creates a locked stake, this record is written to
/// [`StakeDataKey::LockPosition`]. It is used by the emergency-unstake system to
/// compute how much of the lock has elapsed and derive the applicable penalty.
///
/// A position with `unlock_ts == 0` is treated as unlocked (no lock-up penalty
/// applies).
#[contracttype]
#[derive(Debug, Clone)]
pub struct LockPosition {
    /// The staker who owns this lock.
    pub staker: Address,
    /// Ledger timestamp (seconds) when the lock period started.
    pub lock_start_ts: u64,
    /// Ledger timestamp (seconds) when the lock expires and normal unstaking
    /// is allowed without penalty.
    pub unlock_ts: u64,
    /// Total principal locked, in base units.
    pub locked_amount: i128,
}

/// Storage keys for the yield engine's persistent data.
///
/// Keeping keys in a single enum avoids stringly-typed lookups and keeps the
/// storage layout easy to audit.
#[contracttype]
#[derive(Debug, Clone)]
pub enum YieldDataKey {
    /// The active [`YieldRecord`] for a `(staker, asset)` pair.
    Record(Address, Symbol),
    /// The [`YieldHistoryEntry`] list for a `(staker, asset)` pair.
    History(Address, Symbol),
    /// The [`DistributionSchedule`] list for a `(staker, asset)` pair.
    Schedule(Address, Symbol),
    /// The contract admin address set during `initialize`.
    Admin,
    /// The alert threshold value.
    AlertThreshold,
    /// Append-only log of [`YieldDistributionRecord`] for a `(staker, asset)` pair.
    DistributionHistory(Address, Symbol),
    /// The yield escrow/reserve balance for an asset.
    ReserveBalance(Symbol),
    /// Whether distributions are globally paused.
    DistributionsPaused,
}

/// Default yield parameters applied when a position is first opened by a stake.
///
/// A single position, once opened, keeps its own APR and compounding mode across
/// subsequent stakes/unstakes (which only adjust principal); these defaults seed
/// brand-new positions and can be reconfigured before the first stake.
#[contracttype]
#[derive(Debug, Clone)]
pub struct StakingConfig {
    /// APR seeded onto a newly opened yield position, fixed-point (see
    /// [`crate::fixed_point::SCALE`]).
    pub default_apr: i128,
    /// Compounding mode seeded onto a newly opened yield position.
    pub default_mode: CompoundingMode,
}

/// Storage keys for the staking layer that sits in front of the yield engine.
///
/// Balances are keyed by `(staker, asset)` so the protocol can track totals
/// per asset and a distinct-active-staker count globally.
#[contracttype]
#[derive(Debug, Clone)]
pub enum StakeDataKey {
    /// The current staked balance for a `(staker, asset)` pair, in base units.
    Balance(Address, Symbol),
    /// The default [`StakingConfig`] used when opening new positions.
    Config,
    /// The [`LockPosition`] for a staker address, if any.
    LockPosition(Address),
    /// Running aggregate of every staker's balance for one asset, in base
    /// units. Maintained by `stake`/`unstake`/`emergency_unstake`.
    TotalStaked(Symbol),
    /// Global count of distinct stakers with at least one non-zero balance
    /// across any asset. Maintained alongside the per-staker
    /// [`StakeDataKey::StakerPositionCount`] so the count can be derived
    /// without scanning storage when a position reaches zero.
    ActiveStakerCount,
    /// Number of distinct (staker, asset) positions a staker currently holds
    /// with non-zero balance. Used internally to transition the
    /// [`StakeDataKey::ActiveStakerCount`] on full exits.
    StakerPositionCount(Address),
    /// The [`StakingPosition`] for a `(staker, asset)` pair.
    Position(Address, Symbol),
    /// The list of asset symbols a staker is currently staked in.
    StakerAssets(Address),
    /// Optional audit-log sink address. When set, the staking contract
    /// invokes the audit contract on every state-changing event.
    AuditSink,
}

/// The type of a yield distribution event, distinguishing claims from
/// scheduled payouts.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistributionType {
    /// A staker-initiated on-demand claim.
    Claim,
    /// A scheduled (recurring or one-off) distribution.
    Scheduled,
    /// A batch claim covering multiple stakers in one transaction.
    BatchClaim,
}

/// An immutable record of a single yield distribution.
///
/// Appended to the per-`(staker, asset)` distribution history log on every
/// claim or scheduled payout, providing a complete, queryable audit trail.
#[contracttype]
#[derive(Debug, Clone)]
pub struct YieldDistributionRecord {
    /// The staker who received the distribution.
    pub staker: Address,
    /// The asset that was distributed.
    pub asset: Symbol,
    /// Base-unit amount distributed to the staker.
    pub amount: i128,
    /// Ledger timestamp (seconds) at which the distribution occurred.
    pub timestamp: u64,
    /// Whether this was a claim, scheduled payout, or batch claim.
    pub distribution_type: DistributionType,
    /// Accrued yield at the time of distribution (before claim reset).
    pub accrued_at_claim: i128,
    /// Remaining reserve balance for the asset after this distribution.
    pub reserve_after: i128,
}

// ---------------------------------------------------------------------------
// Multi-asset staking types
// ---------------------------------------------------------------------------

/// Yield rate configuration for an asset.
#[contracttype]
#[derive(Debug, Clone)]
pub struct AssetYieldRate {
    pub apr: i128,
    pub mode: CompoundingMode,
    pub unlock_schedule: UnlockSchedule,
    pub max_stake: i128,
}

/// The state of a staking position, tracking its lifecycle.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StakingState {
    /// Position is actively staking and locked (cannot withdraw yet)
    Locked,
    /// Position is active and unlocked (can withdraw or continue staking)
    Active,
    /// Position has finished its lock period and yield is available to claim
    Claimable,
    /// Position has been fully withdrawn and closed
    Withdrawn,
}

/// A staking position for a specific (staker, asset) pair.
#[contracttype]
#[derive(Debug, Clone)]
pub struct StakingPosition {
    pub staker: Address,
    pub asset: Symbol,
    pub principal: i128,
    pub apr: i128,
    pub mode: CompoundingMode,
    pub opened_at: u64,
    pub unlock_schedule: UnlockSchedule,
    pub accrued_yield: i128,
    pub state: StakingState,
    pub locked: bool, // Whether the position is immutable once locked
}

impl PartialEq for StakingPosition {
    fn eq(&self, other: &Self) -> bool {
        self.staker == other.staker
            && self.asset == other.asset
            && self.principal == other.principal
            && self.apr == other.apr
            && self.mode == other.mode
            && self.opened_at == other.opened_at
            && self.accrued_yield == other.accrued_yield
            && self.state == other.state
            && self.locked == other.locked
            // Note: unlock_schedule is not compared for equality here
            // because GraduatedUnlock fields are not PartialEq.
            // For test purposes we compare the key fields above.
            && core::mem::discriminant(&self.unlock_schedule) == core::mem::discriminant(&other.unlock_schedule)
    }
}

/// How a staked position unlocks over time.
#[contracttype]
#[derive(Debug, Clone)]
pub enum UnlockSchedule {
    /// Immediately available.
    Immediate,
    /// Locked until a specific timestamp.
    Cliff(u64),
    /// Gradual unlock via tranches.
    Graduated(GraduatedUnlock),
}

/// Configuration for graduated (tranche-based) unlock.
#[contracttype]
#[derive(Debug, Clone)]
pub struct GraduatedUnlock {
    pub start_ts: u64,
    pub interval_seconds: u64,
    pub tranche_pct_bps: u32,
}

/// A point-in-time snapshot of a staker's full portfolio.
#[contracttype]
#[derive(Debug, Clone)]
pub struct PortfolioSnapshot {
    pub total_principal: i128,
    pub total_accrued_yield: i128,
    pub asset_count: u32,
    pub weighted_avg_apr: i128,
    pub positions: soroban_sdk::Vec<StakingPosition>,
}