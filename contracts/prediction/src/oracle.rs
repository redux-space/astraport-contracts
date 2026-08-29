//! Oracle-based market resolution and dispute mechanism.
//!
//! Handles submitting resolution data from oracle providers, confirming
//! resolutions, and managing the dispute lifecycle.

use soroban_sdk::{Env, Symbol};

use crate::types::{
    Dispute, DisputeStatus, Market, MarketStatus, OracleSource, PredictionDataKey,
    PredictionError, ResolutionData, DISPUTE_PERIOD_SECS,
};

// ---------------------------------------------------------------------------
// Oracle source management
// ---------------------------------------------------------------------------

/// Set the oracle source for a market.
pub fn set_oracle_source(
    env: &Env,
    market_id: u64,
    source: &OracleSource,
) -> Result<(), PredictionError> {
    env.storage().persistent().set(
        &PredictionDataKey::OracleSource(market_id),
        source,
    );
    Ok(())
}

/// Get the oracle source for a market.
pub fn get_oracle_source(env: &Env, market_id: u64) -> Option<OracleSource> {
    env.storage().persistent().get(&PredictionDataKey::OracleSource(market_id))
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Submit an oracle resolution for a market.
///
/// The resolution can only be submitted:
/// 1. By the configured oracle provider
/// 2. After the resolution window has closed
/// 3. When the market is in PendingResolution status
pub fn submit_resolution(
    env: &Env,
    market: &mut Market,
    oracle_provider: Symbol,
    resolved_outcome: u32,
) -> Result<ResolutionData, PredictionError> {
    // Validate the market state
    if market.status != MarketStatus::PendingResolution {
        return Err(PredictionError::MarketNotTradable);
    }

    let now = env.ledger().timestamp();
    if now < market.resolution_time {
        return Err(PredictionError::ResolutionWindowNotClosed);
    }

    // Validate outcome index
    if resolved_outcome >= market.outcomes.len() {
        return Err(PredictionError::InvalidOutcomeIndex);
    }

    // Check oracle source is configured and matches
    let source = get_oracle_source(env, market.market_id)
        .ok_or(PredictionError::OracleNotConfigured)?;

    if source.provider_id != oracle_provider {
        return Err(PredictionError::Unauthorized);
    }

    if !source.is_active {
        return Err(PredictionError::OracleNotConfigured);
    }

    // Create and store the resolution data
    let resolution = ResolutionData {
        market_id: market.market_id,
        resolved_outcome,
        oracle_provider,
        submitted_at: now,
        confirmed: true, // Auto-confirm if from the designated oracle
    };

    env.storage().persistent().set(
        &PredictionDataKey::ResolutionData(market.market_id),
        &resolution,
    );

    // Update market state
    market.status = MarketStatus::Resolved;
    market.resolved_outcome = Some(resolved_outcome);
    market.resolved_at = Some(now);

    Ok(resolution)
}

/// Get the resolution data for a market.
pub fn get_resolution_data(env: &Env, market_id: u64) -> Option<ResolutionData> {
    env.storage().persistent().get(&PredictionDataKey::ResolutionData(market_id))
}

/// Check if a market can be resolved.
pub fn can_resolve(market: &Market, now: u64) -> bool {
    market.status == MarketStatus::PendingResolution && now >= market.resolution_time
}

// ---------------------------------------------------------------------------
// Dispute mechanism
// ---------------------------------------------------------------------------

/// File a dispute against a market resolution.
///
/// The disputer must stake a bond (collateral). The dispute can be filed
/// within the dispute period after resolution.
pub fn file_dispute(
    env: &Env,
    market: &Market,
    disputer: &soroban_sdk::Address,
    claimed_outcome: u32,
    evidence: Symbol,
    bond_amount: i128,
) -> Result<Dispute, PredictionError> {
    // Can only dispute resolved markets
    if market.status != MarketStatus::Resolved {
        return Err(PredictionError::MarketNotResolved);
    }

    let now = env.ledger().timestamp();
    let resolved_at = market.resolved_at.unwrap_or(now);

    // Must be within the dispute period
    let dispute_deadline = resolved_at
        .checked_add(DISPUTE_PERIOD_SECS)
        .unwrap_or(u64::MAX);
    if now > dispute_deadline {
        return Err(PredictionError::DisputePeriodExpired);
    }

    // Validate claimed outcome
    if claimed_outcome >= market.outcomes.len() {
        return Err(PredictionError::InvalidOutcomeIndex);
    }

    // Check if there's already a pending dispute
    if let Some(existing) = get_dispute(env, market.market_id) {
        if existing.status == DisputeStatus::Pending {
            return Err(PredictionError::AlreadyExists);
        }
    }

    // Bond must be at least 1% of total collateral
    let min_bond = market
        .total_collateral
        .checked_mul(100)
        .unwrap_or(0)
        .checked_div(10_000)
        .unwrap_or(1);
    if bond_amount < min_bond {
        return Err(PredictionError::InsufficientBalance);
    }

    let dispute = Dispute {
        market_id: market.market_id,
        disputer: disputer.clone(),
        claimed_outcome,
        evidence,
        filed_at: now,
        status: DisputeStatus::Pending,
        bond_amount,
        resolved_at: None,
    };

    env.storage().persistent().set(
        &PredictionDataKey::Dispute(market.market_id),
        &dispute,
    );

    Ok(dispute)
}

/// Resolve a dispute. Admin only.
///
/// If the dispute is accepted, the market resolution is overturned.
/// If rejected, the original resolution stands and the bond is forfeited.
pub fn resolve_dispute(
    env: &Env,
    market: &mut Market,
    accepted: bool,
) -> Result<Dispute, PredictionError> {
    let mut dispute = get_dispute(env, market.market_id)
        .ok_or(PredictionError::NoDispute)?;

    if dispute.status != DisputeStatus::Pending {
        return Err(PredictionError::InvalidMarketParams);
    }

    let now = env.ledger().timestamp();
    dispute.status = if accepted {
        DisputeStatus::Accepted
    } else {
        DisputeStatus::Rejected
    };
    dispute.resolved_at = Some(now);

    env.storage().persistent().set(
        &PredictionDataKey::Dispute(market.market_id),
        &dispute,
    );

    if accepted {
        // Overturn the resolution — update market to new outcome
        market.resolved_outcome = Some(dispute.claimed_outcome);
        market.resolved_at = Some(now);

        // Update resolution data
        let mut resolution = get_resolution_data(env, market.market_id)
            .ok_or(PredictionError::MarketNotResolved)?;
        resolution.resolved_outcome = dispute.claimed_outcome;
        resolution.confirmed = true;

        env.storage().persistent().set(
            &PredictionDataKey::ResolutionData(market.market_id),
            &resolution,
        );
    }

    Ok(dispute)
}

/// Get the dispute for a market.
pub fn get_dispute(env: &Env, market_id: u64) -> Option<Dispute> {
    env.storage().persistent().get(&PredictionDataKey::Dispute(market_id))
}

/// Check if a market can be disputed.
pub fn can_dispute(market: &Market, now: u64) -> bool {
    if market.status != MarketStatus::Resolved {
        return false;
    }
    let resolved_at = market.resolved_at.unwrap_or(0);
    let dispute_deadline = resolved_at
        .checked_add(DISPUTE_PERIOD_SECS)
        .unwrap_or(u64::MAX);
    now <= dispute_deadline
}

// ---------------------------------------------------------------------------
// Market state transitions
// ---------------------------------------------------------------------------

/// Transition a market from Active to PendingResolution.
///
/// Called when the trading window ends.
pub fn transition_to_pending_resolution(
    env: &Env,
    market: &mut Market,
) -> Result<(), PredictionError> {
    if market.status != MarketStatus::Active {
        return Err(PredictionError::InvalidMarketParams);
    }

    let now = env.ledger().timestamp();
    if now < market.trading_end_time {
        return Err(PredictionError::ResolutionWindowNotClosed);
    }

    market.status = MarketStatus::PendingResolution;
    Ok(())
}

/// Early close a market (admin/creator only).
///
/// Sets all outcome token reserves to zero and resolves with no winning outcome.
pub fn early_close_market(
    env: &Env,
    market: &mut Market,
) -> Result<(), PredictionError> {
    if !market.allow_early_close {
        return Err(PredictionError::InvalidMarketParams);
    }

    if market.status != MarketStatus::Active {
        return Err(PredictionError::InvalidMarketParams);
    }

    market.status = MarketStatus::Closed;
    market.resolved_outcome = None;
    market.resolved_at = Some(env.ledger().timestamp());

    Ok(())
}

/// Cancel a market (admin only, only if no trades have occurred).
pub fn cancel_market(
    _env: &Env,
    market: &mut Market,
) -> Result<(), PredictionError> {
    if market.total_collateral > 0 {
        return Err(PredictionError::ActivePositionsExist);
    }

    if market.status != MarketStatus::Active {
        return Err(PredictionError::InvalidMarketParams);
    }

    market.status = MarketStatus::Cancelled;
    Ok(())
}
