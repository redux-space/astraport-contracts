#![no_std]
#![allow(clippy::too_many_arguments)]
//! # AstraPort Staking Contract
//!
//! Manages asset staking together with an accurate, compounding **yield
//! calculation engine** and an **emergency unstaking system** that allows
//! early withdrawal with time-decaying penalties.
//!
//! ## Module overview
//!
//! - [`fixed_point`] — deterministic fixed-point math (`mul`, `div`, `pow`,
//!   `exp`, `ln`) used in place of floating point.
//! - [`compounding`] — [`compounding::CompoundingStrategy`] trait with `Daily`
//!   and `Continuous` variants, plus [`compounding::YieldCalculator`].
//! - [`apy`] — [`apy::APYCalculator`] for accurate APR ⇄ APY conversion.
//! - [`records`] — Soroban-typed storage structs and key enums:
//!   [`records::YieldRecord`], [`records::YieldHistoryEntry`],
//!   [`records::YieldProjection`], [`records::DistributionSchedule`],
//!   [`records::LockPosition`], [`records::StakingConfig`].
//! - [`engine`] — the storage-backed [`engine::YieldEngine`] that performs
//!   real-time accrual, time-weighted rate changes, history logging, and
//!   distribution scheduling.
//! - [`projection`] — [`projection::YieldProjector`] for future-earnings
//!   estimates.
//! - [`emergency`] — [`emergency::EmergencyUnstakeExecutor`],
//!   [`emergency::PenaltyCalculator`], [`emergency::EmergencyUnstakeConfig`],
//!   [`emergency::EmergencyUnstakeRecord`], and query helpers.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Symbol, Vec,
};

use astraport_audit::logger::AuditLogger;
use astraport_audit::records::{permissions, AuditEventType, StateSnapshot};

pub mod alerts;
pub mod apy;
pub mod compounding;
pub mod emergency;
pub mod engine;
pub mod fixed_point;
pub mod multi_asset;
pub mod projection;
pub mod records;

use crate::apy::APYCalculator;
use crate::emergency::{
    EmergencyDataKey, EmergencyUnstakeConfig, EmergencyUnstakeExecutor, EmergencyUnstakeQuery,
    EmergencyUnstakeRecord,
};
use crate::engine::YieldEngine;
use crate::fixed_point::SCALE;
use crate::projection::YieldProjector;
use crate::records::{
    CompoundingMode, DistributionSchedule, DistributionType, LockPosition, StakeDataKey,
    StakingConfig, YieldDataKey, YieldDistributionRecord, YieldHistoryEntry, YieldProjection,
    YieldRecord,
};

// ---------------------------------------------------------------------------
// Defaults for newly opened yield positions.
// ---------------------------------------------------------------------------

/// Default APR (5%) used when no custom config is stored.
const DEFAULT_APR: i128 = SCALE / 20;
/// Default compounding mode.
const DEFAULT_MODE: CompoundingMode = CompoundingMode::Daily;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the staking contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Invalid amount: must be positive.
    InvalidStakeAmount = 1,
    /// Insufficient balance: cannot unstake more than currently staked.
    InsufficientBalance = 2,
    /// Emergency unstaking is disabled on this contract.
    EmergencyUnstakeDisabled = 3,
    /// The staker is in a cooldown period from a previous emergency unstake.
    CooldownActive = 4,
    /// The [`EmergencyUnstakeConfig`] has not been initialized.
    EmergencyConfigNotInitialized = 5,
    /// The amount requested for emergency unstake is invalid (≤ 0).
    InvalidEmergencyUnstakeAmount = 6,
    /// Distributions are globally paused.
    DistributionsPaused = 7,
    /// The yield reserve has insufficient balance for this distribution.
    InsufficientReserve = 8,
    /// The claim amount must be positive.
    InvalidClaimAmount = 9,
    /// No yield position exists for this staker/asset pair.
    NoYieldPosition = 10,
    /// Contract has already been initialized.
    AlreadyInitialized = 11,
    /// Caller is not authorized.
    Unauthorized = 12,
    /// Amount exceeds the maximum stake limit.
    ExceedsMaximumStake = 13,
    /// Amount exceeds the unlocked (available) amount.
    ExceedsUnlockedAmount = 14,
    /// Cannot modify or withdraw from a position that is still locked.
    PositionStillLocked = 15,
    /// Cannot modify a position that has been marked as immutable.
    ImmutablePosition = 16,
    /// Invalid state transition attempted for a staking position.
    InvalidStateTransition = 17,
    /// No staking position exists for the requested (staker, asset) pair.
    NoStakingPosition = 18,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Event emitted when assets are staked.
#[contracttype]
#[derive(Debug, Clone)]
pub struct StakeEvent {
    pub staker: Address,
    pub asset: Symbol,
    pub amount: i128,
    pub new_balance: i128,
}

/// Event emitted when assets are unstaked normally (after lock expiry).
#[contracttype]
#[derive(Debug, Clone)]
pub struct UnstakeEvent {
    pub staker: Address,
    pub asset: Symbol,
    pub amount: i128,
    pub new_balance: i128,
}

/// Event emitted when a staking position changes state.
#[contracttype]
#[derive(Debug, Clone)]
pub struct PositionStateTransitionEvent {
    pub staker: Address,
    pub asset: Symbol,
    pub old_state: crate::records::StakingState,
    pub new_state: crate::records::StakingState,
    pub timestamp: u64,
}

/// Event emitted when emergency withdrawal is executed.
#[contracttype]
#[derive(Debug, Clone)]
pub struct EmergencyWithdrawalEvent {
    pub staker: Address,
    pub asset: Symbol,
    pub gross_amount: i128,
    pub penalty_amount: i128,
    pub net_amount: i128,
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// Staking contract for AstraPort.
///
/// Manages staking operations, yield calculation, and emergency early
/// withdrawal with configurable time-decaying penalties.
#[contract]
pub struct StakingContract;

// Soroban contract entrypoints unavoidably carry a long argument list
// (Env, Address, ...). The crate-level `#![allow(clippy::too_many_arguments)]`
// (above `#![no_std]`) blanket-suppresses this lint for both the manual impl
// and the `contractimpl`-macro expansion.
#[contractimpl]
impl StakingContract {
    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Initialize the staking contract with an admin.
    ///
    /// Can only be called once; subsequent calls will panic.
    pub fn initialize(env: Env, admin: Address) -> Result<Symbol, Error> {
        let storage = env.storage().persistent();
        if storage.has(&YieldDataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        storage.set(&YieldDataKey::Admin, &admin);
        Ok(symbol_short!("ok"))
    }

    // -----------------------------------------------------------------------
    // Staking
    // -----------------------------------------------------------------------

    /// Stake `amount` of `asset` into the contract with the specified unlock schedule.
    ///
    /// Requires authorization from `staker`. Increases the staker's balance
    /// for `(staker, asset)` by `amount`, maintains the protocol-level
    /// `TotalStaked(asset)` aggregate and the distinct-active-staker count,
    /// creates/updates the staking position, and emits a `StakeEvent`.
    ///
    /// If a position already exists and is marked as immutable (locked), it cannot
    /// be modified - a new position must be created for additional stakes.
    pub fn stake(
        env: Env,
        staker: Address,
        asset: Symbol,
        amount: i128,
        unlock_schedule: crate::records::UnlockSchedule,
        lock_position: bool, // Whether to mark this position as immutable once created
    ) -> Result<Symbol, Error> {
        staker.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidStakeAmount);
        }

        // Validate unlock schedule parameters
        let current_ts = env.ledger().timestamp();
        match &unlock_schedule {
            crate::records::UnlockSchedule::Cliff(unlock_ts) => {
                if *unlock_ts <= current_ts {
                    return Err(Error::InvalidStakeAmount); // Lock must be in the future
                }
            }
            crate::records::UnlockSchedule::Graduated(graduated) => {
                if graduated.start_ts <= current_ts
                    || graduated.interval_seconds == 0
                    || graduated.tranche_pct_bps > 10000
                {
                    return Err(Error::InvalidStakeAmount); // Invalid graduated unlock parameters
                }
            }
            _ => {} // Immediate is always valid
        }

        // Check if we already have a position for this staker/asset
        let position_key = StakeDataKey::Position(staker.clone(), asset.clone());
        let existing_position: Option<crate::records::StakingPosition> =
            env.storage().persistent().get(&position_key);

        if let Some(pos) = existing_position {
            // If existing position is locked/immutable, we cannot modify it
            if pos.locked {
                return Err(Error::ImmutablePosition);
            }
            // Otherwise, update the existing position's principal
            let updated_principal = pos
                .principal
                .checked_add(amount)
                .ok_or(Error::InvalidStakeAmount)?;
            let mut updated_position = pos;
            updated_position.principal = updated_principal;
            env.storage()
                .persistent()
                .set(&position_key, &updated_position);
        } else {
            // Create new staking position
            let initial_state = match &unlock_schedule {
                crate::records::UnlockSchedule::Cliff(_)
                | crate::records::UnlockSchedule::Graduated(_) => {
                    crate::records::StakingState::Locked
                }
                _ => crate::records::StakingState::Active,
            };

            let config: crate::records::StakingConfig = env
                .storage()
                .persistent()
                .get(&StakeDataKey::Config)
                .unwrap_or_else(|| crate::records::StakingConfig {
                    default_apr: crate::fixed_point::SCALE / 20, // 5% default APR
                    default_mode: crate::records::CompoundingMode::Daily,
                });

            let new_position = crate::records::StakingPosition {
                staker: staker.clone(),
                asset: asset.clone(),
                principal: amount,
                apr: config.default_apr,
                mode: config.default_mode,
                opened_at: current_ts,
                unlock_schedule,
                accrued_yield: 0,
                state: initial_state,
                locked: lock_position,
            };

            env.storage().persistent().set(&position_key, &new_position);

            // Emit state transition event for new position
            env.events().publish(
                (symbol_short!("state_chg"), staker.clone(), asset.clone()),
                PositionStateTransitionEvent {
                    staker: staker.clone(),
                    asset: asset.clone(),
                    old_state: crate::records::StakingState::Withdrawn,
                    new_state: initial_state,
                    timestamp: current_ts,
                },
            );
        }

        // Update balance
        let balance_key = StakeDataKey::Balance(staker.clone(), asset.clone());
        let current_balance: i128 = env
            .storage()
            .persistent()
            .get(&balance_key)
            .unwrap_or_default();
        let new_balance = current_balance
            .checked_add(amount)
            .ok_or(Error::InvalidStakeAmount)?;
        env.storage().persistent().set(&balance_key, &new_balance);

        Self::update_totals_on_stake(&env, &staker, &asset, current_balance, new_balance);

        Self::log_audit_if_configured(
            &env,
            &staker,
            &asset,
            current_balance,
            new_balance,
            AuditEventType::Stake,
            permissions::STAKER,
            symbol_short!("ok"),
            "stake",
        );

        env.events().publish(
            (symbol_short!("stake"), staker.clone()),
            StakeEvent {
                staker: staker.clone(),
                asset: asset.clone(),
                amount,
                new_balance,
            },
        );

        Self::check_balance_threshold(&env, &staker, &asset, new_balance);

        Ok(symbol_short!("ok"))
    }

    /// Unstake `amount` of `asset` from the contract (normal, after lock expiry).
    ///
    /// Requires authorization from `staker`. Decreases the staker's balance
    /// for `(staker, asset)` by `amount`, maintains the protocol-level
    /// `TotalStaked(asset)` aggregate and the distinct-active-staker count,
    /// and emits an `UnstakeEvent`. Returns an error if the staker's
    /// balance is insufficient or if the position is still locked.
    ///
    /// For early withdrawal before the lock expires, use
    /// [`Self::emergency_unstake`] instead.
    pub fn unstake(
        env: Env,
        staker: Address,
        asset: Symbol,
        amount: i128,
    ) -> Result<Symbol, Error> {
        staker.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidStakeAmount);
        }

        // Get the staking position to check lock status
        let position_key = StakeDataKey::Position(staker.clone(), asset.clone());
        let mut position: crate::records::StakingPosition = env
            .storage()
            .persistent()
            .get(&position_key)
            .ok_or(Error::NoStakingPosition)?;

        let current_ts = env.ledger().timestamp();
        let mut can_unstake = true;
        let mut new_state = position.state;

        // Check if the position is still locked
        match &position.unlock_schedule {
            crate::records::UnlockSchedule::Cliff(unlock_ts) => {
                if current_ts < *unlock_ts {
                    can_unstake = false;
                } else if position.state == crate::records::StakingState::Locked {
                    // Position just unlocked, update state
                    new_state = crate::records::StakingState::Claimable;
                }
            }
            crate::records::UnlockSchedule::Graduated(graduated) => {
                // Calculate how much can be unstaked for graduated unlocks
                let elapsed = current_ts.saturating_sub(graduated.start_ts);
                if elapsed == 0 {
                    can_unstake = false;
                } else {
                    let tranches = elapsed / graduated.interval_seconds;
                    let unlocked_pct_bps = (tranches as u32)
                        .saturating_mul(graduated.tranche_pct_bps)
                        .min(10000);
                    let unlocked_amount = position.principal * (unlocked_pct_bps as i128) / 10000;
                    let current_balance: i128 = env
                        .storage()
                        .persistent()
                        .get(&StakeDataKey::Balance(staker.clone(), asset.clone()))
                        .unwrap_or(0);
                    let currently_unstaked: i128 = position.principal - current_balance;
                    let available_to_unstake: i128 = unlocked_amount - currently_unstaked;

                    if amount > available_to_unstake {
                        return Err(Error::ExceedsUnlockedAmount);
                    }

                    // If fully unlocked, update state
                    if unlocked_pct_bps >= 10000
                        && position.state == crate::records::StakingState::Locked
                    {
                        new_state = crate::records::StakingState::Claimable;
                    }
                }
            }
            _ => {} // Immediate unlocks can always be unstaked
        }

        if !can_unstake {
            return Err(Error::PositionStillLocked);
        }

        // Emit state transition if state changed
        if new_state != position.state {
            let old_state = position.state;
            position.state = new_state;
            env.events().publish(
                (symbol_short!("state_chg"), staker.clone(), asset.clone()),
                PositionStateTransitionEvent {
                    staker: staker.clone(),
                    asset: asset.clone(),
                    old_state,
                    new_state,
                    timestamp: current_ts,
                },
            );
        }

        // Check and update balance
        let balance_key = StakeDataKey::Balance(staker.clone(), asset.clone());
        let current_balance: i128 = env
            .storage()
            .persistent()
            .get(&balance_key)
            .unwrap_or_default();
        if amount > current_balance {
            return Err(Error::InsufficientBalance);
        }
        let new_balance = current_balance - amount;

        // Update position principal
        position.principal = new_balance;

        if new_balance == 0 {
            // Position is fully withdrawn
            env.storage().persistent().remove(&balance_key);
            position.state = crate::records::StakingState::Withdrawn;
            env.storage().persistent().set(&position_key, &position);

            // Emit state transition to withdrawn
            env.events().publish(
                (symbol_short!("state_chg"), staker.clone(), asset.clone()),
                PositionStateTransitionEvent {
                    staker: staker.clone(),
                    asset: asset.clone(),
                    old_state: new_state,
                    new_state: crate::records::StakingState::Withdrawn,
                    timestamp: current_ts,
                },
            );
        } else {
            env.storage().persistent().set(&balance_key, &new_balance);
            env.storage().persistent().set(&position_key, &position);
        }

        Self::update_totals_on_unstake(&env, &staker, &asset, current_balance, new_balance);

        Self::log_audit_if_configured(
            &env,
            &staker,
            &asset,
            current_balance,
            new_balance,
            AuditEventType::Unstake,
            permissions::STAKER,
            symbol_short!("ok"),
            "unstake",
        );

        env.events().publish(
            (symbol_short!("unstake"), staker.clone()),
            UnstakeEvent {
                staker: staker.clone(),
                asset: asset.clone(),
                amount,
                new_balance,
            },
        );

        Self::check_balance_threshold(&env, &staker, &asset, new_balance);

        Ok(symbol_short!("ok"))
    }

    /// Return the staked balance for a `(staker, asset)` pair, defaulting to 0.
    pub fn get_balance(env: Env, staker: Address, asset: Symbol) -> i128 {
        env.storage()
            .persistent()
            .get(&StakeDataKey::Balance(staker, asset))
            .unwrap_or_default()
    }

    /// Return the full staking position details for a `(staker, asset)` pair.
    /// Returns an error if no position exists.
    pub fn get_position(
        env: Env,
        staker: Address,
        asset: Symbol,
    ) -> Result<crate::records::StakingPosition, Error> {
        env.storage()
            .persistent()
            .get(&StakeDataKey::Position(staker, asset))
            .ok_or(Error::NoStakingPosition)
    }

    // -----------------------------------------------------------------------
    // Protocol-level totals
    // -----------------------------------------------------------------------

    /// Total amount of `asset` currently staked across every staker.
    ///
    /// Maintained incrementally by `stake` / `unstake` / `emergency_unstake`
    /// and equals the sum of every non-zero balance for the asset.
    pub fn total_staked(env: Env, asset: Symbol) -> i128 {
        env.storage()
            .persistent()
            .get(&StakeDataKey::TotalStaked(asset))
            .unwrap_or_default()
    }

    /// Number of distinct stakers with at least one non-zero balance across
    /// any asset.
    ///
    /// Incremented the first time a staker takes a balance above zero in any
    /// asset, and decremented when their last non-zero balance returns to zero
    /// (across any asset).
    pub fn staker_count(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&StakeDataKey::ActiveStakerCount)
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Lock positions
    // -----------------------------------------------------------------------

    /// Record a lock-up period for a staker.
    ///
    /// Sets (or overwrites) the staker's [`LockPosition`] so the
    /// emergency-unstake system knows when the lock started and when it expires.
    ///
    /// Only the admin may call this; stakers should not be able to extend their
    /// own lock to reduce their penalty.
    pub fn set_lock_position(
        env: Env,
        admin: Address,
        staker: Address,
        lock_start_ts: u64,
        unlock_ts: u64,
        locked_amount: i128,
    ) -> Result<Symbol, Error> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let pos = LockPosition {
            staker: staker.clone(),
            lock_start_ts,
            unlock_ts,
            locked_amount,
        };
        env.storage()
            .persistent()
            .set(&StakeDataKey::LockPosition(staker), &pos);
        Ok(symbol_short!("ok"))
    }

    /// Query the lock position for a staker, if any.
    pub fn get_lock_position(env: Env, staker: Address) -> Option<LockPosition> {
        env.storage()
            .persistent()
            .get(&StakeDataKey::LockPosition(staker))
    }

    // -----------------------------------------------------------------------
    // Emergency unstaking
    // -----------------------------------------------------------------------

    /// Configure the emergency-unstake system.
    ///
    /// Admin-only. Sets penalty rates, decay function, cooldown duration, and
    /// treasury address. Calling this a second time overwrites the previous
    /// configuration.
    ///
    /// # Arguments
    ///
    /// * `penalty_start_bps` — penalty at the start of the lock period, in
    ///   basis points (0–10 000).
    /// * `penalty_end_bps` — penalty at the unlock date (0–10 000). Typically
    ///   lower than `penalty_start_bps`.
    /// * `decay_function` — how the penalty decays between start and end.
    /// * `cooldown_seconds` — mandatory wait between emergency unstakes. `0`
    ///   disables the cooldown.
    /// * `treasury` — address that receives all collected penalties.
    /// * `enabled` — whether emergency unstaking is currently available.
    pub fn configure_emergency_unstake(
        env: Env,
        admin: Address,
        penalty_start_bps: i128,
        penalty_end_bps: i128,
        decay_function: emergency::PenaltyDecayFunction,
        cooldown_seconds: u64,
        treasury: Address,
        enabled: bool,
    ) -> Result<Symbol, Error> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let config = EmergencyUnstakeConfig {
            penalty_start_bps,
            penalty_end_bps,
            decay_function,
            cooldown_seconds,
            treasury,
            enabled,
        };
        env.storage()
            .persistent()
            .set(&EmergencyDataKey::Config, &config);
        Ok(symbol_short!("ok"))
    }

    /// Perform an emergency unstake before the lock-up period expires.
    ///
    /// Requires authorization from the staker.
    ///
    /// The system:
    /// 1. Reads the staker's [`LockPosition`] to determine elapsed/total lock
    ///    duration.
    /// 2. Computes a time-decaying penalty via [`PenaltyCalculator`].
    /// 3. Deducts the penalty from `amount` and records it as earmarked for the
    ///    treasury (via a `PENALTY` event — actual token transfer is handled
    ///    off-chain or by a future token integration).
    /// 4. Reduces the staker's on-chain balance by `amount` (the full gross
    ///    amount, including penalty).
    /// 5. Appends an [`EmergencyUnstakeRecord`] to the staker's history.
    /// 6. Activates a cooldown period to prevent rapid emergency unstakes.
    ///
    /// Returns the full [`EmergencyUnstakeRecord`] describing the operation.
    ///
    /// # Panics
    ///
    /// Panics (with descriptive messages) if:
    /// - Emergency unstaking is disabled.
    /// - The staker is in an active cooldown period.
    /// - The staker has insufficient balance.
    /// - `amount` is ≤ 0.
    pub fn emergency_unstake(
        env: Env,
        staker: Address,
        asset: Symbol,
        amount: i128,
    ) -> Result<EmergencyUnstakeRecord, Error> {
        staker.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidEmergencyUnstakeAmount);
        }

        // Get the staking position to get lock timestamps
        let position_key = StakeDataKey::Position(staker.clone(), asset.clone());
        let mut position: crate::records::StakingPosition = env
            .storage()
            .persistent()
            .get(&position_key)
            .ok_or(Error::NoStakingPosition)?;

        // --- current balance --------------------------------------------
        let balance_key = StakeDataKey::Balance(staker.clone(), asset.clone());
        let current_balance: i128 = env
            .storage()
            .persistent()
            .get(&balance_key)
            .unwrap_or_default();

        if amount > current_balance {
            return Err(Error::InsufficientBalance);
        }

        // --- get lock timestamps from staking position -------------------
        let (lock_start_ts, unlock_ts) = match &position.unlock_schedule {
            crate::records::UnlockSchedule::Cliff(unlock_ts) => (position.opened_at, *unlock_ts),
            crate::records::UnlockSchedule::Graduated(graduated) => (
                graduated.start_ts,
                graduated.start_ts
                    + (10000 / graduated.tranche_pct_bps as u64) * graduated.interval_seconds,
            ),
            _ => {
                // No lock - use current timestamps to apply minimum penalty
                let now = env.ledger().timestamp();
                (now, now)
            }
        };

        // --- execute emergency unstake (validates, computes penalty, logs) --
        let record = EmergencyUnstakeExecutor::execute(
            &env,
            &staker,
            amount,
            current_balance,
            lock_start_ts,
            unlock_ts,
        )?;

        // --- reduce the staked balance by the FULL gross amount ---------
        // The penalty is deducted from `amount_returned`; the full `amount`
        // leaves the staking pool (penalty stays in the treasury bucket).
        let new_balance = current_balance - amount;
        let current_ts = env.ledger().timestamp();

        // Update position
        position.principal = new_balance;

        if new_balance == 0 {
            // Position is fully withdrawn
            env.storage().persistent().remove(&balance_key);
            let old_state = position.state;
            position.state = crate::records::StakingState::Withdrawn;
            env.storage().persistent().set(&position_key, &position);

            // Emit state transition to withdrawn
            env.events().publish(
                (symbol_short!("state_chg"), staker.clone(), asset.clone()),
                PositionStateTransitionEvent {
                    staker: staker.clone(),
                    asset: asset.clone(),
                    old_state,
                    new_state: crate::records::StakingState::Withdrawn,
                    timestamp: current_ts,
                },
            );
        } else {
            env.storage().persistent().set(&balance_key, &new_balance);
            env.storage().persistent().set(&position_key, &position);
        }

        // Emit emergency withdrawal event
        env.events().publish(
            (symbol_short!("emg_wdraw"), staker.clone(), asset.clone()),
            EmergencyWithdrawalEvent {
                staker: staker.clone(),
                asset: asset.clone(),
                gross_amount: amount,
                penalty_amount: record.penalty_amount,
                net_amount: record.amount_returned,
                timestamp: current_ts,
            },
        );

        // --- update protocol-level totals (same semantics as unstake) --
        Self::update_totals_on_unstake(&env, &staker, &asset, current_balance, new_balance);

        // --- update lock position locked_amount -------------------------
        if let Some(mut pos) = env
            .storage()
            .persistent()
            .get::<StakeDataKey, LockPosition>(&StakeDataKey::LockPosition(staker.clone()))
        {
            pos.locked_amount = pos.locked_amount.saturating_sub(amount);
            if pos.locked_amount == 0 {
                env.storage()
                    .persistent()
                    .remove(&StakeDataKey::LockPosition(staker.clone()));
            } else {
                env.storage()
                    .persistent()
                    .set(&StakeDataKey::LockPosition(staker.clone()), &pos);
            }
        }

        Self::log_audit_if_configured(
            &env,
            &staker,
            &asset,
            current_balance,
            new_balance,
            AuditEventType::EmergencyUnstake,
            permissions::STAKER,
            symbol_short!("ok"),
            "emergency_unstake",
        );

        Ok(record)
    }

    // -----------------------------------------------------------------------
    // Emergency unstake queries
    // -----------------------------------------------------------------------

    /// Return the [`EmergencyUnstakeConfig`], if initialized.
    pub fn get_emergency_config(env: Env) -> Option<EmergencyUnstakeConfig> {
        EmergencyUnstakeQuery::config(&env)
    }

    /// Return the ledger timestamp after which `staker` may emergency-unstake
    /// again. Returns `0` if no cooldown is active.
    pub fn get_cooldown_end(env: Env, staker: Address) -> u64 {
        EmergencyUnstakeQuery::cooldown_end(&env, &staker)
    }

    /// Return `true` if `staker` is currently in a cooldown period.
    pub fn is_in_cooldown(env: Env, staker: Address) -> bool {
        EmergencyUnstakeQuery::in_cooldown(&env, &staker)
    }

    /// The full emergency-unstake history for `staker`, oldest first.
    pub fn get_emergency_unstake_history(env: Env, staker: Address) -> Vec<EmergencyUnstakeRecord> {
        EmergencyUnstakeQuery::history(&env, &staker)
    }

    /// Preview the penalty basis points for a hypothetical emergency unstake
    /// without touching storage.
    ///
    /// Returns `None` if the emergency-unstake config has not been initialized.
    pub fn preview_emergency_penalty(env: Env, lock_start_ts: u64, unlock_ts: u64) -> Option<i128> {
        EmergencyUnstakeQuery::preview_penalty_bps(&env, lock_start_ts, unlock_ts)
    }

    // -----------------------------------------------------------------------
    // Admin
    // -----------------------------------------------------------------------

    /// Set the alert threshold for staking changes.
    ///
    /// Only callable by the admin set during `initialize`.
    pub fn set_alert_threshold(env: Env, admin: Address, threshold: i128) -> Result<Symbol, Error> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;
        env.storage()
            .persistent()
            .set(&YieldDataKey::AlertThreshold, &threshold);
        Ok(symbol_short!("ok"))
    }

    /// Return the current alert threshold, if set.
    pub fn get_alert_threshold(env: Env) -> Option<i128> {
        env.storage()
            .persistent()
            .get(&YieldDataKey::AlertThreshold)
    }

    /// Reconfigure the default APR and compounding mode for new yield positions.
    pub fn set_yield_defaults(env: Env, default_apr: i128, default_mode: CompoundingMode) {
        let config = StakingConfig {
            default_apr,
            default_mode,
        };
        env.storage()
            .persistent()
            .set(&StakeDataKey::Config, &config);
    }

    // -----------------------------------------------------------------------
    // Yield engine entrypoints
    // -----------------------------------------------------------------------

    /// Open (or reset) a yield-accruing position for a staker and asset.
    pub fn open_yield_position(
        env: Env,
        staker: Address,
        asset: Symbol,
        principal: i128,
        apr: i128,
        mode: CompoundingMode,
    ) -> YieldRecord {
        YieldEngine::new(&env)
            .open_position(&staker, &asset, principal, apr, mode)
            .expect("failed to open yield position")
    }

    /// Checkpoint a position, realizing all yield accrued up to the current
    /// ledger time.
    pub fn accrue_yield(env: Env, staker: Address, asset: Symbol) -> YieldRecord {
        YieldEngine::new(&env)
            .accrue(&staker, &asset)
            .expect("failed to accrue yield")
    }

    /// Claim all yield accrued by a staker for an asset.
    pub fn claim_yield(env: Env, staker: Address, asset: Symbol) -> i128 {
        staker.require_auth();
        let engine = YieldEngine::new(&env);
        let record = engine
            .accrue(&staker, &asset)
            .expect("failed to accrue yield before claim");
        let claimed = engine.finalize_claim(record);
        if claimed > 0 {
            // Record in distribution history.
            let reserve_after = engine.reserve_balance(&asset);
            engine.record_distribution(&records::YieldDistributionRecord {
                staker: staker.clone(),
                asset: asset.clone(),
                amount: claimed,
                timestamp: env.ledger().timestamp(),
                distribution_type: records::DistributionType::Claim,
                accrued_at_claim: claimed,
                reserve_after,
            });
        }
        env.events()
            .publish((symbol_short!("YLDCLAIM"), staker, asset), claimed);
        claimed
    }

    /// The total yield a position has earned as of now, without mutating storage.
    pub fn current_yield(env: Env, staker: Address, asset: Symbol) -> i128 {
        YieldEngine::new(&env)
            .current_yield(&staker, &asset)
            .expect("failed to read current yield")
    }

    /// Change the APR for a position, checkpointing prior yield at the old rate.
    pub fn set_yield_rate(env: Env, staker: Address, asset: Symbol, new_apr: i128) -> YieldRecord {
        YieldEngine::new(&env)
            .set_rate(&staker, &asset, new_apr)
            .expect("failed to set yield rate")
    }

    /// The complete yield history for a staker/asset pair, oldest entry first.
    pub fn yield_history(env: Env, staker: Address, asset: Symbol) -> Vec<YieldHistoryEntry> {
        YieldEngine::new(&env).history(&staker, &asset)
    }

    /// Project future earnings over a horizon.
    pub fn project_yield(
        _env: Env,
        principal: i128,
        apr: i128,
        mode: CompoundingMode,
        horizon_seconds: u64,
    ) -> YieldProjection {
        YieldProjector::project(principal, apr, mode, horizon_seconds)
            .expect("failed to project yield")
    }

    /// Convert a nominal APR to its effective APY.
    pub fn apr_to_apy(_env: Env, apr: i128, mode: CompoundingMode) -> i128 {
        APYCalculator::apr_to_apy(apr, mode.to_strategy()).expect("apr_to_apy failed")
    }

    /// Convert an effective APY back to its nominal APR.
    pub fn apy_to_apr(_env: Env, apy: i128, mode: CompoundingMode) -> i128 {
        APYCalculator::apy_to_apr(apy, mode.to_strategy()).expect("apy_to_apr failed")
    }

    // -----------------------------------------------------------------------
    // Distribution scheduling
    // -----------------------------------------------------------------------

    /// Schedule a yield distribution to a staker.
    pub fn schedule_distribution(
        env: Env,
        staker: Address,
        asset: Symbol,
        amount: i128,
        due_ts: u64,
        interval_seconds: u64,
    ) -> DistributionSchedule {
        YieldEngine::new(&env).schedule_distribution(
            &staker,
            &asset,
            amount,
            due_ts,
            interval_seconds,
        )
    }

    /// Process due distributions for a staker/asset pair.
    pub fn process_distribution(env: Env, staker: Address, asset: Symbol) -> i128 {
        YieldEngine::new(&env).process_distribution(&staker, &asset)
    }

    // -----------------------------------------------------------------------
    // Yield distribution & claiming system
    // -----------------------------------------------------------------------

    /// Claim a specific `amount` of accrued yield (partial claim).
    ///
    /// Requires authorization from `staker`. If `amount` exceeds the accrued
    /// yield, the full accrued amount is claimed. If a reserve exists for the
    /// asset and is insufficient, the claim is capped to the reserve balance.
    ///
    /// Returns the actual amount claimed.
    pub fn claim_yield_partial(
        env: Env,
        staker: Address,
        asset: Symbol,
        amount: i128,
    ) -> Result<i128, Error> {
        staker.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidClaimAmount);
        }
        let engine = YieldEngine::new(&env);
        // Verify a yield position exists.
        if engine.load_record(&staker, &asset).is_none() {
            return Err(Error::NoYieldPosition);
        }
        let claimed = engine
            .claim_yield_partial(&staker, &asset, amount)
            .map_err(|_| Error::NoYieldPosition)?;
        if claimed > 0 {
            env.events()
                .publish((symbol_short!("YLDPART"), staker, asset), claimed);
        }
        Ok(claimed)
    }

    /// Batch claim yield for multiple stakers on a single asset.
    ///
    /// Gas optimization: processes all stakers in one call. Each staker
    /// claims all their accrued yield. If distributions are paused, all
    /// claims return 0.
    ///
    /// Returns a `Vec` of `(staker, claimed_amount)` pairs.
    pub fn batch_claim(env: Env, stakers: Vec<Address>, asset: Symbol) -> Vec<(Address, i128)> {
        // Require auth for each staker.
        for i in 0..stakers.len() {
            stakers.get(i).unwrap().require_auth();
        }
        let results = YieldEngine::new(&env).batch_claim(&stakers, &asset);
        // Emit a summary event.
        let mut total_claimed: i128 = 0;
        for i in 0..results.len() {
            let (_, amount) = results.get(i).unwrap();
            total_claimed += amount;
        }
        if total_claimed > 0 {
            env.events()
                .publish((symbol_short!("BATCHYLD"), asset), total_claimed);
        }
        results
    }

    /// Fund the yield reserve for an asset.
    ///
    /// Admin-only. Increases the reserve balance used to back distributions.
    pub fn fund_reserve(env: Env, admin: Address, asset: Symbol, amount: i128) -> i128 {
        admin.require_auth();
        let _ = Self::assert_admin(&env, &admin);
        YieldEngine::new(&env).fund_reserve(&asset, amount)
    }

    /// Return the current yield reserve balance for an asset.
    pub fn reserve_balance(env: Env, asset: Symbol) -> i128 {
        YieldEngine::new(&env).reserve_balance(&asset)
    }

    /// Withdraw from the yield reserve.
    ///
    /// Admin-only. Reduces the reserve balance and returns the new balance.
    pub fn withdraw_reserve(env: Env, admin: Address, asset: Symbol, amount: i128) -> i128 {
        admin.require_auth();
        let _ = Self::assert_admin(&env, &admin);
        YieldEngine::new(&env).withdraw_reserve(&asset, amount)
    }

    /// Pause all yield distributions globally.
    ///
    /// Admin-only. When paused, `process_distribution` and `batch_claim`
    /// return 0 without modifying state. On-demand `claim_yield` and
    /// `claim_yield_partial` continue to work (they draw from accrued yield
    /// directly).
    pub fn pause_distributions(env: Env, admin: Address) -> Symbol {
        admin.require_auth();
        let _ = Self::assert_admin(&env, &admin);
        YieldEngine::new(&env).set_paused(true);
        symbol_short!("paused")
    }

    /// Resume yield distributions after a pause.
    pub fn unpause_distributions(env: Env, admin: Address) -> Symbol {
        admin.require_auth();
        let _ = Self::assert_admin(&env, &admin);
        YieldEngine::new(&env).set_paused(false);
        symbol_short!("active")
    }

    /// Whether distributions are currently paused.
    pub fn distributions_paused(env: Env) -> bool {
        YieldEngine::new(&env).is_paused()
    }

    /// Full distribution history for a `(staker, asset)` pair.
    pub fn distribution_history(
        env: Env,
        staker: Address,
        asset: Symbol,
    ) -> Vec<YieldDistributionRecord> {
        YieldEngine::new(&env).distribution_history(&staker, &asset)
    }

    /// Distribution history filtered by time range `[from_ts, to_ts]`
    /// (inclusive).
    pub fn distribution_history_range(
        env: Env,
        staker: Address,
        asset: Symbol,
        from_ts: u64,
        to_ts: u64,
    ) -> Vec<YieldDistributionRecord> {
        YieldEngine::new(&env).distribution_history_range(&staker, &asset, from_ts, to_ts)
    }

    /// Distribution history filtered by type (Claim, Scheduled, BatchClaim).
    pub fn distribution_history_by_type(
        env: Env,
        staker: Address,
        asset: Symbol,
        dist_type: DistributionType,
    ) -> Vec<YieldDistributionRecord> {
        YieldEngine::new(&env).distribution_history_by_type(&staker, &asset, dist_type)
    }

    /// Total yield claimed by a staker for an asset across all distributions.
    pub fn total_yield_claimed(env: Env, staker: Address, asset: Symbol) -> i128 {
        YieldEngine::new(&env).total_yield_claimed(&staker, &asset)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers (not part of the contract's external interface)
// ---------------------------------------------------------------------------

impl StakingContract {
    /// Return Err(Error::Unauthorized) if `admin` does not match the stored
    /// admin address.
    fn assert_admin(env: &Env, admin: &Address) -> Result<(), Error> {
        let stored_admin: Address = env
            .storage()
            .persistent()
            .get(&YieldDataKey::Admin)
            .ok_or(Error::Unauthorized)?;
        if stored_admin != *admin {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    /// Read the staked balance for a `(staker, asset)` pair, defaulting to `0`.
    #[allow(dead_code)]
    fn balance_of(env: &Env, staker: &Address, asset: &Symbol) -> i128 {
        env.storage()
            .persistent()
            .get(&StakeDataKey::Balance(staker.clone(), asset.clone()))
            .unwrap_or_default()
    }

    /// Load the configured yield defaults, falling back to the built-in
    /// [`DEFAULT_APR`] / [`DEFAULT_MODE`] when unset.
    #[allow(dead_code)]
    fn load_config(env: &Env) -> StakingConfig {
        env.storage()
            .persistent()
            .get(&StakeDataKey::Config)
            .unwrap_or(StakingConfig {
                default_apr: DEFAULT_APR,
                default_mode: DEFAULT_MODE,
            })
    }

    // -------------------------------------------------------------------
    // Protocol-level totals maintenance
    // -------------------------------------------------------------------

    /// Update `TotalStaked(asset)` and the distinct-active-staker count on
    /// a stake, given the previous and new per-pair balance.
    ///
    /// # Counter transitions
    ///
    /// - The active-staker count is incremented exactly once when a staker
    ///   transitions from zero to positive balance for any asset (i.e. they
    ///   become an active staker for the first time).
    /// - `TotalStaked(asset)` is increased by the staked delta.
    fn update_totals_on_stake(
        env: &Env,
        staker: &Address,
        asset: &Symbol,
        previous_balance: i128,
        new_balance: i128,
    ) {
        assert!(
            new_balance >= previous_balance,
            "stake must not reduce a balance"
        );

        // TotalStaked(asset) += delta.
        let total_key = StakeDataKey::TotalStaked(asset.clone());
        let current_total: i128 = env
            .storage()
            .persistent()
            .get(&total_key)
            .unwrap_or_default();
        let delta = new_balance - previous_balance;
        env.storage().persistent().set(
            &total_key,
            &current_total
                .checked_add(delta)
                .expect("TotalStaked overflow"),
        );

        // ActiveStakerCount++ if this is the staker's first active position.
        if previous_balance == 0 && new_balance > 0 {
            let pos_key = StakeDataKey::StakerPositionCount(staker.clone());
            let prev_positions: u32 = env.storage().persistent().get(&pos_key).unwrap_or_default();
            let new_positions = prev_positions + 1;
            env.storage().persistent().set(&pos_key, &new_positions);
            if prev_positions == 0 {
                let count_key = StakeDataKey::ActiveStakerCount;
                let count: u32 = env
                    .storage()
                    .persistent()
                    .get(&count_key)
                    .unwrap_or_default();
                env.storage().persistent().set(&count_key, &(count + 1));
            }
        }
    }

    /// Update `TotalStaked(asset)` and the distinct-active-staker count on
    /// an unstake (or emergency unstake), given the previous and new
    /// per-pair balance.
    ///
    /// # Counter transitions
    ///
    /// - The active-staker count is decremented exactly once when a staker's
    ///   final active position is removed (their last non-zero balance across
    ///   all assets transitions to zero).
    /// - `TotalStaked(asset)` is decreased by the unstaked delta.
    fn update_totals_on_unstake(
        env: &Env,
        staker: &Address,
        asset: &Symbol,
        previous_balance: i128,
        new_balance: i128,
    ) {
        assert!(
            new_balance <= previous_balance,
            "unstake must not increase a balance"
        );

        // TotalStaked(asset) -= delta.
        let total_key = StakeDataKey::TotalStaked(asset.clone());
        let current_total: i128 = env
            .storage()
            .persistent()
            .get(&total_key)
            .unwrap_or_default();
        let delta = previous_balance - new_balance;
        env.storage().persistent().set(
            &total_key,
            &current_total
                .checked_sub(delta)
                .expect("TotalStaked underflow"),
        );

        // Decrement staker-position count when this pair's balance hits zero.
        // If their last active position is gone, also decrement the global
        // active-staker count.
        if previous_balance > 0 && new_balance == 0 {
            let pos_key = StakeDataKey::StakerPositionCount(staker.clone());
            let prev_positions: u32 = env.storage().persistent().get(&pos_key).unwrap_or_default();
            // prev_positions must be >= 1 for this path; saturating_sub avoids
            // panics from any (theoretical) state divergence.
            let new_positions = prev_positions.saturating_sub(1);
            env.storage().persistent().set(&pos_key, &new_positions);
            if new_positions == 0 {
                let count_key = StakeDataKey::ActiveStakerCount;
                let count: u32 = env
                    .storage()
                    .persistent()
                    .get(&count_key)
                    .unwrap_or_default();
                env.storage()
                    .persistent()
                    .set(&count_key, &count.saturating_sub(1));
            }
        }
    }
}

/// Balance-threshold alert helpers.
impl StakingContract {
    /// Compare `balance` against the persisted alert threshold for
    /// `(staker, asset)`. If the threshold is set and the balance is
    /// below it, publish an [`alerts::AlertEvent`].
    fn check_balance_threshold(env: &Env, staker: &Address, asset: &Symbol, balance: i128) {
        let threshold: i128 = match env
            .storage()
            .persistent()
            .get(&YieldDataKey::AlertThreshold)
        {
            Some(t) => t,
            None => return,
        };
        if balance < threshold {
            env.events().publish(
                (symbol_short!("ALERT"), staker.clone(), asset.clone()),
                alerts::AlertEvent {
                    staker: staker.clone(),
                    asset: asset.clone(),
                    kind: alerts::AlertKind::BalanceDrop,
                    severity: alerts::AlertSeverity::Critical,
                    fired_at: env.ledger().timestamp(),
                    threshold_value: threshold,
                    observed_value: balance,
                    label: soroban_sdk::String::from_str(env, "balance_below_threshold"),
                },
            );
        }
    }
}

/// Integration with the audit-log contract.
impl StakingContract {
    /// Configure the audit-log sink address. Admin-only.
    pub fn set_audit_sink(env: Env, admin: Address, sink: Address) -> Result<Symbol, Error> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;
        env.storage()
            .persistent()
            .set(&StakeDataKey::AuditSink, &sink);
        Ok(symbol_short!("ok"))
    }

    /// Read the audit-log sink address, if configured.
    pub fn get_audit_sink(env: Env) -> Option<Address> {
        env.storage().persistent().get(&StakeDataKey::AuditSink)
    }

    /// Append an audit event if a sink is configured. No-op otherwise.
    ///
    /// We use the asset symbol as the audit portfolio id and the staker's
    /// `(before, after)` balance as the state snapshot. The outcome is
    /// passed through verbatim from the caller.
    #[allow(clippy::too_many_arguments)]
    fn log_audit_if_configured(
        env: &Env,
        actor: &Address,
        asset: &Symbol,
        before_balance: i128,
        after_balance: i128,
        event_type: AuditEventType,
        perms: u32,
        outcome: Symbol,
        detail: &str,
    ) {
        let key = StakeDataKey::AuditSink;
        let sink: Option<Address> = env.storage().persistent().get(&key);
        if let Some(sink) = sink {
            let mut before = StateSnapshot::empty(env);
            before.push(asset.clone(), before_balance);
            let mut after = StateSnapshot::empty(env);
            after.push(asset.clone(), after_balance);
            let detail_str = soroban_sdk::String::from_str(env, detail);
            let logger = AuditLogger::new(env, &sink);
            let _ = logger.log_event(
                actor.clone(),
                event_type,
                asset.clone(),
                perms,
                before,
                after,
                outcome,
                detail_str,
            );
        }
    }
}

#[cfg(test)]
mod tests;
