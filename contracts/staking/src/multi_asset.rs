//! Multi-asset staking facade.
//!
//! [`MultiAssetStaking`] sits between the public contract entry points and the
//! lower-level [`crate::engine::YieldEngine`]. It is responsible for:
//!
//! - Per-asset balance persistence (`StakeDataKey::Balance`).
//! - Opening / updating [`StakingPosition`]s on stake/unstake.
//! - Enforcing [`UnlockSchedule`] constraints on withdrawals.
//! - Maintaining the per-staker asset list (`StakeDataKey::StakerAssets`) so
//!   the portfolio view can iterate all positions.
//! - Aggregating positions into a [`PortfolioSnapshot`].
//!
//! The module purposefully contains **no financial math** — all yield
//! calculations are delegated to [`crate::engine::YieldEngine`] and the
//! underlying [`crate::compounding`] / [`crate::fixed_point`] layers.

use soroban_sdk::{Address, Env, Symbol, Vec};

use crate::engine::YieldEngine;
use crate::fixed_point as fp;
use crate::records::{
    AssetYieldRate, GraduatedUnlock, PortfolioSnapshot, StakeDataKey, StakingPosition,
    UnlockSchedule,
};
use crate::Error;

/// Facade that coordinates multi-asset staking operations.
///
/// Like [`crate::engine::YieldEngine`], this struct holds no state of its own
/// beyond a reference to the Soroban environment; all persistence goes through
/// `env.storage().persistent()`.
pub struct MultiAssetStaking<'a> {
    env: &'a Env,
}

impl<'a> MultiAssetStaking<'a> {
    /// Create a facade bound to `env`.
    pub fn new(env: &'a Env) -> Self {
        Self { env }
    }

    // -----------------------------------------------------------------------
    // Stake
    // -----------------------------------------------------------------------

    /// Add `amount` of `asset` to `staker`'s position.
    ///
    /// Steps:
    /// 1. Load existing balance and compute `new_balance`.
    /// 2. Enforce `max_stake` if configured.
    /// 3. Open or update a [`StakingPosition`].
    /// 4. Mirror the principal change into the [`YieldEngine`].
    /// 5. Persist balance and update the staker's asset list.
    ///
    /// Returns the new staked balance.
    pub fn stake(
        &self,
        staker: &Address,
        asset: &Symbol,
        amount: i128,
        config: &AssetYieldRate,
    ) -> Result<i128, Error> {
        let balance_key = StakeDataKey::Balance(staker.clone(), asset.clone());
        let current: i128 = self
            .env
            .storage()
            .persistent()
            .get(&balance_key)
            .unwrap_or(0);
        let new_balance = current + amount;

        // Enforce per-asset maximum stake.
        if config.max_stake > 0 && new_balance > config.max_stake {
            return Err(Error::ExceedsMaximumStake);
        }

        let now = self.env.ledger().timestamp();

        // Update or create the StakingPosition.
        let position = match self.load_position(staker, asset) {
            Some(mut pos) => {
                pos.principal = new_balance;
                // Refresh accrued yield from the engine checkpoint.
                pos.accrued_yield = self.checkpoint_accrued(staker, asset);
                pos
            }
            None => StakingPosition {
                staker: staker.clone(),
                asset: asset.clone(),
                principal: new_balance,
                apr: config.apr,
                mode: config.mode,
                opened_at: now,
                unlock_schedule: config.unlock_schedule.clone(),
                accrued_yield: 0,
                state: crate::records::StakingState::Active,
                locked: false,
            },
        };
        self.save_position(&position);

        // Synchronise principal in the yield engine (opens or updates).
        let engine = YieldEngine::new(self.env);
        if current == 0 {
            engine
                .open_position(staker, asset, new_balance, config.apr, config.mode)
                .map_err(|_| Error::InvalidStakeAmount)?;
        } else {
            engine
                .set_principal(staker, asset, new_balance)
                .map_err(|_| Error::InvalidStakeAmount)?;
        }

        // Persist balance.
        self.env
            .storage()
            .persistent()
            .set(&balance_key, &new_balance);

        // Track asset in staker's asset list.
        self.add_asset_to_staker(staker, asset);

        Ok(new_balance)
    }

    // -----------------------------------------------------------------------
    // Unstake
    // -----------------------------------------------------------------------

    /// Remove `amount` of `asset` from `staker`'s position.
    ///
    /// Steps:
    /// 1. Verify sufficient balance.
    /// 2. Compute how much of the position has unlocked and validate the request.
    /// 3. Checkpoint accrued yield.
    /// 4. Reduce principal in the engine.
    /// 5. Update / remove the position and the staker's asset list.
    ///
    /// Returns the remaining staked balance.
    pub fn unstake(&self, staker: &Address, asset: &Symbol, amount: i128) -> Result<i128, Error> {
        let balance_key = StakeDataKey::Balance(staker.clone(), asset.clone());
        let current: i128 = self
            .env
            .storage()
            .persistent()
            .get(&balance_key)
            .unwrap_or(0);

        if amount > current {
            return Err(Error::InsufficientBalance);
        }

        // Enforce unlock schedule.
        if let Some(pos) = self.load_position(staker, asset) {
            let now = self.env.ledger().timestamp();
            let unlocked = self.compute_unlocked_amount(&pos, now);
            if amount > unlocked {
                return Err(Error::ExceedsUnlockedAmount);
            }
        }

        let new_balance = current - amount;
        let engine = YieldEngine::new(self.env);

        if new_balance == 0 {
            // Full exit: persist a zero principal. The engine record stays so
            // yield history is preserved; the position and asset-list entry are
            // removed from the staking layer.
            engine
                .set_principal(staker, asset, 0)
                .map_err(|_| Error::InsufficientBalance)?;
            self.env.storage().persistent().remove(&balance_key);
            self.remove_position(staker, asset);
            self.remove_asset_from_staker(staker, asset);
        } else {
            // Partial exit: checkpoint first, then update principal.
            let accrued = self.checkpoint_accrued(staker, asset);
            engine
                .set_principal(staker, asset, new_balance)
                .map_err(|_| Error::InsufficientBalance)?;

            if let Some(mut pos) = self.load_position(staker, asset) {
                pos.principal = new_balance;
                pos.accrued_yield = accrued;
                self.save_position(&pos);
            }
            self.env
                .storage()
                .persistent()
                .set(&balance_key, &new_balance);
        }

        Ok(new_balance)
    }

    // -----------------------------------------------------------------------
    // Portfolio aggregation
    // -----------------------------------------------------------------------

    /// Build a [`PortfolioSnapshot`] for all active positions of `staker`.
    ///
    /// This is a read-only computation that does **not** mutate storage.
    pub fn portfolio_snapshot(&self, staker: &Address) -> PortfolioSnapshot {
        let assets = self.staker_assets(staker);
        let engine = YieldEngine::new(self.env);
        let _now = self.env.ledger().timestamp();

        let mut total_principal: i128 = 0;
        let mut total_yield: i128 = 0;
        let mut weighted_apr_sum: i128 = 0;
        let mut positions: Vec<StakingPosition> = Vec::new(self.env);

        for i in 0..assets.len() {
            let asset = assets.get(i).unwrap();
            if let Some(mut pos) = self.load_position(staker, &asset) {
                // Read live yield from the engine (non-mutating).
                let live_yield = engine.current_yield(staker, &asset).unwrap_or(0);
                pos.accrued_yield = live_yield;

                total_principal = total_principal.saturating_add(pos.principal);
                total_yield = total_yield.saturating_add(live_yield);

                // Accumulate weighted APR sum: apr * principal (fixed-point
                // principal is a raw i128, so we treat it as a weight).
                let w = fp::mul(pos.apr, pos.principal).unwrap_or(0);
                weighted_apr_sum = weighted_apr_sum.saturating_add(w);

                positions.push_back(pos);
            }
        }

        // Weighted-average APR: sum(apr_i * principal_i) / sum(principal_i).
        let weighted_avg_apr = if total_principal > 0 {
            fp::div(weighted_apr_sum, total_principal).unwrap_or(0)
        } else {
            0
        };

        PortfolioSnapshot {
            total_principal,
            total_accrued_yield: total_yield,
            asset_count: positions.len(),
            weighted_avg_apr,
            positions,
        }
    }

    /// Return the total live yield across all active positions for `staker`.
    pub fn portfolio_yield(&self, staker: &Address) -> i128 {
        let assets = self.staker_assets(staker);
        let engine = YieldEngine::new(self.env);
        let mut total: i128 = 0;
        for i in 0..assets.len() {
            let asset = assets.get(i).unwrap();
            let y = engine.current_yield(staker, &asset).unwrap_or(0);
            total = total.saturating_add(y);
        }
        total
    }

    // -----------------------------------------------------------------------
    // Unlock schedule helpers
    // -----------------------------------------------------------------------

    /// Compute how much of a position's principal has unlocked as of `now`.
    ///
    /// | Schedule | Unlocked amount |
    /// |---|---|
    /// | `Immediate` | full `principal` |
    /// | `Cliff(ts)` | `principal` if `now >= ts`, else `0` |
    /// | `Graduated` | tranched fraction of `principal` |
    pub fn compute_unlocked_amount(&self, pos: &StakingPosition, now: u64) -> i128 {
        match &pos.unlock_schedule {
            UnlockSchedule::Immediate => pos.principal,
            UnlockSchedule::Cliff(unlock_ts) => {
                if now >= *unlock_ts {
                    pos.principal
                } else {
                    0
                }
            }
            UnlockSchedule::Graduated(g) => self.graduated_unlocked(pos.principal, g, now),
        }
    }

    /// Compute the graduated unlocked amount.
    ///
    /// Tranches are numbered 0, 1, 2, …  Tranche `k` unlocks at
    /// `start_ts + k * interval_seconds`. Each tranche releases
    /// `tranche_pct_bps / 10_000` of the original principal. The final
    /// tranche unlocks any remainder, ensuring 100% eventually unlocks.
    fn graduated_unlocked(&self, principal: i128, g: &GraduatedUnlock, now: u64) -> i128 {
        if now < g.start_ts || g.interval_seconds == 0 || g.tranche_pct_bps == 0 {
            return 0;
        }
        // Number of completed tranches.
        let elapsed = now - g.start_ts;
        let tranches_elapsed = (elapsed / g.interval_seconds) as i128 + 1; // +1: first tranche at start_ts

        // Tranches needed to unlock 100%.
        let bps = g.tranche_pct_bps as i128;
        let total_tranches = if bps >= 10_000 {
            1
        } else {
            (10_000 + bps - 1) / bps // ceiling division
        };

        let tranches_done = tranches_elapsed.min(total_tranches);

        if tranches_done >= total_tranches {
            // All tranches completed.
            return principal;
        }

        // unlocked = principal * (tranches_done * bps) / 10_000
        // Use i128 arithmetic; principal is a raw token amount, not fixed-point.
        principal.saturating_mul(tranches_done).saturating_mul(bps) / 10_000
    }

    // -----------------------------------------------------------------------
    // Persistence helpers
    // -----------------------------------------------------------------------

    /// Load a [`StakingPosition`] from storage, if present.
    pub fn load_position(&self, staker: &Address, asset: &Symbol) -> Option<StakingPosition> {
        self.env
            .storage()
            .persistent()
            .get(&StakeDataKey::Position(staker.clone(), asset.clone()))
    }

    /// Persist a [`StakingPosition`].
    fn save_position(&self, pos: &StakingPosition) {
        self.env.storage().persistent().set(
            &StakeDataKey::Position(pos.staker.clone(), pos.asset.clone()),
            pos,
        );
    }

    /// Remove a [`StakingPosition`] from storage.
    fn remove_position(&self, staker: &Address, asset: &Symbol) {
        self.env
            .storage()
            .persistent()
            .remove(&StakeDataKey::Position(staker.clone(), asset.clone()));
    }

    /// Checkpoint the yield engine and return the current `accrued_yield`.
    ///
    /// Used internally before mutating principal so no yield is lost at
    /// boundaries.
    fn checkpoint_accrued(&self, staker: &Address, asset: &Symbol) -> i128 {
        YieldEngine::new(self.env)
            .accrue(staker, asset)
            .map(|r| r.accrued_yield)
            .unwrap_or(0)
    }

    // -----------------------------------------------------------------------
    // Staker-asset list helpers
    // -----------------------------------------------------------------------

    /// Load the list of asset symbols a staker is currently staked in.
    fn staker_assets(&self, staker: &Address) -> Vec<Symbol> {
        self.env
            .storage()
            .persistent()
            .get(&StakeDataKey::StakerAssets(staker.clone()))
            .unwrap_or_else(|| Vec::new(self.env))
    }

    /// Add `asset` to `staker`'s asset list if not already present.
    fn add_asset_to_staker(&self, staker: &Address, asset: &Symbol) {
        let key = StakeDataKey::StakerAssets(staker.clone());
        let mut list: Vec<Symbol> = self
            .env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(self.env));

        // Linear scan — the list is expected to be small (≤ ~20 assets).
        for i in 0..list.len() {
            if list.get(i).as_ref() == Some(asset) {
                return; // already present
            }
        }
        list.push_back(asset.clone());
        self.env.storage().persistent().set(&key, &list);
    }

    /// Remove `asset` from `staker`'s asset list.
    fn remove_asset_from_staker(&self, staker: &Address, asset: &Symbol) {
        let key = StakeDataKey::StakerAssets(staker.clone());
        let list: Vec<Symbol> = match self.env.storage().persistent().get(&key) {
            Some(l) => l,
            None => return,
        };

        let mut updated: Vec<Symbol> = Vec::new(self.env);
        for i in 0..list.len() {
            let a = list.get(i).unwrap();
            if &a != asset {
                updated.push_back(a);
            }
        }
        self.env.storage().persistent().set(&key, &updated);
    }
}
