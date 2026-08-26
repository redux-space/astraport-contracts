//! Timelock queue management for the governance contract.
//!
//! Provides time-delayed execution of passed proposals, with separate delays
//! for normal and emergency actions.

use soroban_sdk::{symbol_short, Address, Env, Symbol, Vec};

use crate::records::{GovernanceConfig, StorageKey, TimelockEntry, TimelockStatus};

/// Sentinel return symbol.
const OK: Symbol = symbol_short!("ok");

/// Errors related to timelock operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[soroban_sdk::contracterror]
pub enum TimelockError {
    /// Timelock entry not found.
    EntryNotFound = 1,
    /// Timelock delay has not yet expired.
    DelayNotExpired = 2,
    /// Entry has already been executed.
    AlreadyExecuted = 3,
    /// Entry was cancelled.
    AlreadyCancelled = 4,
    /// Invalid timelock delay.
    InvalidDelay = 5,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Schedule a timelock for a passed proposal.
///
/// Sets the timelock expiry to `now + delay_seconds` and stores the entry.
/// Returns the [`TimelockEntry`].
pub fn schedule(
    env: &Env,
    proposal_id: u64,
    delay_seconds: u64,
) -> Result<TimelockEntry, TimelockError> {
    if delay_seconds == 0 {
        return Err(TimelockError::InvalidDelay);
    }

    let now = env.ledger().timestamp();
    let execute_after = now + delay_seconds;

    let entry = TimelockEntry {
        proposal_id,
        scheduled_at: now,
        execute_after,
        status: TimelockStatus::Pending,
    };

    env.storage()
        .persistent()
        .set(&StorageKey::TimelockEntry(proposal_id), &entry);

    env.events().publish(
        (symbol_short!("TL_SCHD"), proposal_id),
        (delay_seconds, execute_after),
    );

    Ok(entry)
}

/// Check if a timelock entry's delay has expired and mark it as Ready.
///
/// Returns the updated [`TimelockEntry`].
pub fn check_ready(env: &Env, proposal_id: u64) -> Result<TimelockEntry, TimelockError> {
    let mut entry: TimelockEntry = env
        .storage()
        .persistent()
        .get(&StorageKey::TimelockEntry(proposal_id))
        .ok_or(TimelockError::EntryNotFound)?;

    if entry.status == TimelockStatus::Executed {
        return Err(TimelockError::AlreadyExecuted);
    }
    if entry.status == TimelockStatus::Cancelled {
        return Err(TimelockError::AlreadyCancelled);
    }

    let now = env.ledger().timestamp();
    if now < entry.execute_after {
        return Err(TimelockError::DelayNotExpired);
    }

    if entry.status != TimelockStatus::Ready {
        entry.status = TimelockStatus::Ready;
        env.storage()
            .persistent()
            .set(&StorageKey::TimelockEntry(proposal_id), &entry);
    }

    Ok(entry)
}

/// Mark a timelock entry as executed.
pub fn mark_executed(env: &Env, proposal_id: u64) -> Result<TimelockEntry, TimelockError> {
    let mut entry: TimelockEntry = env
        .storage()
        .persistent()
        .get(&StorageKey::TimelockEntry(proposal_id))
        .ok_or(TimelockError::EntryNotFound)?;

    if entry.status == TimelockStatus::Executed {
        return Err(TimelockError::AlreadyExecuted);
    }
    if entry.status == TimelockStatus::Cancelled {
        return Err(TimelockError::AlreadyCancelled);
    }

    entry.status = TimelockStatus::Executed;
    env.storage()
        .persistent()
        .set(&StorageKey::TimelockEntry(proposal_id), &entry);

    env.events()
        .publish((symbol_short!("TL_EXEC"), proposal_id), OK);

    Ok(entry)
}

/// Cancel a timelock entry. Admin-only.
pub fn cancel(
    env: &Env,
    admin: &Address,
    proposal_id: u64,
) -> Result<TimelockEntry, TimelockError> {
    admin.require_auth();

    let mut entry: TimelockEntry = env
        .storage()
        .persistent()
        .get(&StorageKey::TimelockEntry(proposal_id))
        .ok_or(TimelockError::EntryNotFound)?;

    if entry.status == TimelockStatus::Executed {
        return Err(TimelockError::AlreadyExecuted);
    }
    if entry.status == TimelockStatus::Cancelled {
        return Err(TimelockError::AlreadyCancelled);
    }

    entry.status = TimelockStatus::Cancelled;
    env.storage()
        .persistent()
        .set(&StorageKey::TimelockEntry(proposal_id), &entry);

    env.events()
        .publish((symbol_short!("TL_CANCEL"), proposal_id), OK);

    Ok(entry)
}

/// Get a timelock entry by proposal id.
pub fn get_entry(env: &Env, proposal_id: u64) -> Option<TimelockEntry> {
    env.storage()
        .persistent()
        .get(&StorageKey::TimelockEntry(proposal_id))
}

/// Get the effective delay for a proposal action type.
///
/// Uses the emergency timelock delay for emergency actions, and the standard
/// timelock delay for all others.
pub fn effective_delay(_env: &Env, is_emergency: bool, config: &GovernanceConfig) -> u64 {
    if is_emergency {
        config.emergency_timelock_delay
    } else {
        config.timelock_delay
    }
}

/// Get all pending timelock entries (for querying).
pub fn get_pending_entries(env: &Env, all_proposal_ids: &Vec<u64>) -> Vec<TimelockEntry> {
    let mut pending = Vec::new(env);
    for i in 0..all_proposal_ids.len() {
        let pid = all_proposal_ids.get(i).unwrap();
        if let Some(entry) = get_entry(env, pid) {
            if entry.status == TimelockStatus::Pending || entry.status == TimelockStatus::Ready {
                pending.push_back(entry);
            }
        }
    }
    pending
}
