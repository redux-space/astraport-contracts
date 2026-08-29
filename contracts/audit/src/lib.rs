//! # AstraPort Audit-Log Contract
//!
//! Immutable, append-only audit log for portfolio events with tamper
//! detection via a SHA-256 chain hash, flexible querying, retention
//! policies, and JSON/CSV export.
//!
//! ## Module overview
//!
//! - [`records`] — types: `AuditLog`, `AuditEventType`, `StateSnapshot`,
//!   `RetentionPolicy`, `StorageKey`, `permissions`.
//! - [`checksum`] — SHA-256 chain hashing for tamper detection.
//! - [`log_query`] — `LogQuery` filter builder.
//! - [`export`] — JSON / CSV formatters.
//! - [`logger`] — cross-contract `AuditLogger` client for callers
//!   (staking, rebalancing).
//!
//! ## Usage
//!
//! 1. `initialize(admin)` once on contract deployment.
//! 2. Other contracts call `audit::log_event` via the [`logger::AuditLogger`]
//!    helper. Each call returns a monotonically-increasing sequence id.
//! 3. Off-chain and on-chain monitoring services call `query` and
//!    `verify_integrity` to surface and audit events.

#![no_std]
#![allow(clippy::too_many_arguments)]

extern crate alloc;

use soroban_sdk::{
    contract, contracterror, contractimpl, symbol_short, Address, BytesN, Env, String, Symbol, Vec,
};

pub mod checksum;
pub mod export;
pub mod log_query;
pub mod logger;
pub mod records;
pub mod signing;

use crate::checksum::{chain_hash, entry_payload, first_chain_hash};
use crate::records::{
    AuditEventType, AuditLog, RetentionPolicy, StateSnapshot, StorageKey, BUCKET_SIZE,
};

/// Sentinel return symbol.
const OK: Symbol = symbol_short!("ok");

/// Errors returned by the audit contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Contract was not initialized.
    NotInitialized = 1,
    /// Caller is not the admin.
    Unauthorized = 2,
    /// `log_event` was called with an invalid event type for the requested
    /// permission set (currently unused, reserved for future use).
    InvalidEvent = 3,
    /// Prune failed because the retention policy is unbounded.
    NoRetentionPolicy = 4,
    /// Contract was already initialized.
    AlreadyInitialized = 5,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// Audit-log contract for AstraPort.
#[contract]
pub struct AuditContract;

/// Append-only audit log with chain-hash integrity, indexing, and export.
#[contractimpl]
impl AuditContract {
    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Initialize the contract with an admin address. May only be called once.
    pub fn initialize(env: Env, admin: Address) -> Result<Symbol, Error> {
        let key = StorageKey::Admin;
        if env.storage().persistent().has(&key) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().persistent().set(&key, &admin);
        env.storage().persistent().set(&StorageKey::NextSeq, &0u64);
        env.storage()
            .persistent()
            .set(&StorageKey::EntryCount, &0u64);
        env.storage().persistent().set(&StorageKey::FirstSeq, &1u64);
        Ok(OK)
    }

    /// Return the current admin. Errors if uninitialized.
    pub fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .persistent()
            .get(&StorageKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    // -----------------------------------------------------------------------
    // Retention policy
    // -----------------------------------------------------------------------

    /// Set the retention policy. Admin-only.
    pub fn set_retention_policy(
        env: Env,
        admin: Address,
        policy: RetentionPolicy,
    ) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;
        env.storage()
            .persistent()
            .set(&StorageKey::RetentionPolicy, &policy);
        Ok(OK)
    }

    /// Read the current retention policy. Returns the unbounded default if
    /// not configured.
    pub fn get_retention_policy(env: Env) -> RetentionPolicy {
        env.storage()
            .persistent()
            .get(&StorageKey::RetentionPolicy)
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Logging
    // -----------------------------------------------------------------------

    /// Append a single audit event. Returns the assigned sequence id.
    ///
    /// Authentication is the responsibility of the caller: this entrypoint
    /// trusts the immediate caller to have already authorized `actor`.
    /// Calling contracts (e.g. the staking contract) must call
    /// `actor.require_auth()` themselves before invoking `log_event`.
    #[allow(clippy::too_many_arguments)]
    pub fn log_event(
        env: Env,
        actor: Address,
        event_type: AuditEventType,
        portfolio: Symbol,
        permissions_flags: u32,
        state_before: StateSnapshot,
        state_after: StateSnapshot,
        outcome: Symbol,
        detail: String,
    ) -> u64 {
        // Read chain head; first entry uses CHAIN_ORIGIN.
        let prev_hash: BytesN<32> = env
            .storage()
            .persistent()
            .get(&StorageKey::RollingChecksum)
            .unwrap_or_else(|| BytesN::from_array(&env, &records::CHAIN_ORIGIN));

        // Allocate the next sequence id.
        let seq: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::NextSeq)
            .unwrap_or(0)
            + 1;
        let timestamp = env.ledger().timestamp();

        let payload = entry_payload(
            &env,
            seq,
            timestamp,
            event_type as u32,
            permissions_flags,
            &actor,
            &portfolio,
            &outcome,
            &detail,
            &state_before,
            &state_after,
        );
        let hash = if seq == 1 {
            first_chain_hash(&env, &payload)
        } else {
            chain_hash(&env, &prev_hash, &payload)
        };

        let entry = AuditLog {
            seq,
            timestamp,
            event_type,
            actor,
            permissions: permissions_flags,
            portfolio,
            state_before,
            state_after,
            outcome,
            detail,
            hash: hash.clone(),
        };

        env.storage()
            .persistent()
            .set(&StorageKey::Entry(seq), &entry);
        env.storage().persistent().set(&StorageKey::NextSeq, &seq);
        env.storage()
            .persistent()
            .set(&StorageKey::RollingChecksum, &hash);

        // Update count + secondary indexes.
        let prev_count: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::EntryCount)
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&StorageKey::EntryCount, &(prev_count + 1));

        Self::append_index(
            &env,
            &StorageKey::IndexByType(event_type, Self::bucket(seq)),
            seq,
        );
        Self::append_index(
            &env,
            &StorageKey::IndexByActor(entry.actor.clone(), Self::bucket(seq)),
            seq,
        );
        Self::append_index(
            &env,
            &StorageKey::IndexByPortfolio(entry.portfolio.clone(), Self::bucket(seq)),
            seq,
        );
        Self::append_index(
            &env,
            &StorageKey::IndexByOutcome(entry.outcome.clone(), Self::bucket(seq)),
            seq,
        );

        seq
    }

    // -----------------------------------------------------------------------
    // Query
    // -----------------------------------------------------------------------

    /// Read entries matching the supplied [`LogQuery`]. Sorted by `seq`
    /// ascending. Capped by `q.limit`.
    pub fn query(env: Env, q: log_query::LogQuery) -> Vec<AuditLog> {
        let first_seq: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::FirstSeq)
            .unwrap_or(1);
        let next_seq: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::NextSeq)
            .unwrap_or(0);

        let mut out: Vec<AuditLog> = Vec::new(&env);
        let mut seq = first_seq;
        let limit = if q.limit == 0 { 50 } else { q.limit };

        while seq <= next_seq && out.len() < limit {
            let key = StorageKey::Entry(seq);
            if let Some(entry) = env.storage().persistent().get::<StorageKey, AuditLog>(&key) {
                if q.matches(&entry) {
                    out.push_back(entry);
                }
            }
            seq += 1;
        }

        out
    }

    // -----------------------------------------------------------------------
    // Integrity verification
    // -----------------------------------------------------------------------

    /// Verify that the recorded chain head matches the expected hash. Returns
    /// `true` on match; `false` if any tampering is detected or the chain
    /// is empty.
    pub fn verify_integrity(env: Env, expected_head: BytesN<32>) -> bool {
        let stored: Option<BytesN<32>> =
            env.storage().persistent().get(&StorageKey::RollingChecksum);
        match stored {
            Some(h) => h == expected_head,
            None => false,
        }
    }

    /// Return the recorded chain head (the most-recent entry's hash, or
    /// `None` if the chain is empty via a zero hash).
    pub fn integrity_head(env: Env) -> BytesN<32> {
        env.storage()
            .persistent()
            .get(&StorageKey::RollingChecksum)
            .unwrap_or_else(|| BytesN::from_array(&env, &records::CHAIN_ORIGIN))
    }

    /// Recompute the chain head from scratch over the retained entries and
    /// compare to the stored value. Useful for routine integrity audits.
    pub fn full_recompute_integrity(env: Env) -> bool {
        let first_seq: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::FirstSeq)
            .unwrap_or(1);
        let next_seq: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::NextSeq)
            .unwrap_or(0);

        let mut prev = BytesN::from_array(&env, &records::CHAIN_ORIGIN);
        let mut seq = first_seq;
        while seq <= next_seq {
            let key = StorageKey::Entry(seq);
            let entry: Option<AuditLog> = env.storage().persistent().get(&key);
            match entry {
                Some(e) => {
                    let payload = entry_payload(
                        &env,
                        e.seq,
                        e.timestamp,
                        e.event_type as u32,
                        e.permissions,
                        &e.actor,
                        &e.portfolio,
                        &e.outcome,
                        &e.detail,
                        &e.state_before,
                        &e.state_after,
                    );
                    prev = if seq == 1 {
                        first_chain_hash(&env, &payload)
                    } else {
                        chain_hash(&env, &prev, &payload)
                    };
                }
                None => return false, // gap ⇒ tampered
            }
            seq += 1;
        }
        let stored: BytesN<32> = env
            .storage()
            .persistent()
            .get(&StorageKey::RollingChecksum)
            .unwrap_or(prev.clone());
        stored == prev
    }

    // -----------------------------------------------------------------------
    // Retention
    // -----------------------------------------------------------------------

    /// Delete entries older than the retention policy. Admin-only.
    ///
    /// Returns the number of entries pruned.
    pub fn prune_old(env: Env, admin: Address) -> Result<u32, Error> {
        Self::assert_admin(&env, &admin)?;
        let policy = Self::get_retention_policy(env.clone());
        if policy.is_unbounded() {
            return Err(Error::NoRetentionPolicy);
        }

        let now = env.ledger().timestamp();
        let first_seq: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::FirstSeq)
            .unwrap_or(1);
        let next_seq: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::NextSeq)
            .unwrap_or(0);

        let age_limit = policy.max_age_seconds;
        let count_limit = policy.max_entries;

        // Pass 1 — count current size.
        let total: u64 = next_seq - first_seq + 1;

        // Compute target new floor.
        let mut prune_count: u32 = 0;
        let mut new_first = first_seq;

        if age_limit > 0 {
            let age_cutoff = now.saturating_sub(age_limit);
            let mut s = first_seq;
            while s <= next_seq {
                let entry: Option<AuditLog> = env.storage().persistent().get(&StorageKey::Entry(s));
                if let Some(e) = entry {
                    if e.timestamp < age_cutoff {
                        new_first = s + 1;
                        prune_count += 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
                s += 1;
            }
        }

        if count_limit > 0 {
            let mut remaining = total - prune_count as u64;
            while remaining > count_limit {
                new_first += 1;
                prune_count += 1;
                remaining -= 1;
            }
        }

        // Pass 2 — actually remove.
        let mut s = first_seq;
        while s < new_first {
            env.storage().persistent().remove(&StorageKey::Entry(s));
            s += 1;
        }

        if new_first != first_seq {
            env.storage()
                .persistent()
                .set(&StorageKey::FirstSeq, &new_first);
        }

        // Recompute the chain head from the new floor. If everything was pruned,
        // the head is reset to the chain origin; otherwise the head is the hash
        // of the lowest remaining entry.
        let mut prev = BytesN::from_array(&env, &records::CHAIN_ORIGIN);
        if new_first <= next_seq {
            let mut s = new_first;
            while s <= next_seq {
                let key = StorageKey::Entry(s);
                if let Some(e) = env.storage().persistent().get::<StorageKey, AuditLog>(&key) {
                    let payload = entry_payload(
                        &env,
                        e.seq,
                        e.timestamp,
                        e.event_type as u32,
                        e.permissions,
                        &e.actor,
                        &e.portfolio,
                        &e.outcome,
                        &e.detail,
                        &e.state_before,
                        &e.state_after,
                    );
                    prev = if s == 1 {
                        first_chain_hash(&env, &payload)
                    } else {
                        chain_hash(&env, &prev, &payload)
                    };
                }
                s += 1;
            }
        }
        env.storage()
            .persistent()
            .set(&StorageKey::RollingChecksum, &prev);

        Ok(prune_count)
    }

    // -----------------------------------------------------------------------
    // Exports
    // -----------------------------------------------------------------------

    /// Export the result of a [`LogQuery`] as a JSON-Lines `Vec<String>`.
    pub fn export_jsonl(env: Env, q: log_query::LogQuery) -> Vec<String> {
        let entries = Self::query(env.clone(), q);
        export::format_jsonl(&env, &entries)
    }

    /// Export the result of a [`LogQuery`] as CSV (header row first).
    pub fn export_csv(env: Env, q: log_query::LogQuery) -> Vec<String> {
        let entries = Self::query(env.clone(), q);
        export::format_csv(&env, &entries)
    }

    /// Export the result of a [`LogQuery`] as JSON-Lines including state
    /// snapshots and chain hashes.
    pub fn export_jsonl_full(env: Env, q: log_query::LogQuery) -> Vec<String> {
        let entries = Self::query(env.clone(), q);
        export::format_jsonl_full(&env, &entries)
    }

    /// Export the result of a [`LogQuery`] as CSV including state snapshots
    /// and chain hashes (header row first).
    pub fn export_csv_full(env: Env, q: log_query::LogQuery) -> Vec<String> {
        let entries = Self::query(env.clone(), q);
        export::format_csv_full(&env, &entries)
    }

    /// Compute the SHA-256 digest of audit entries for off-chain signing.
    ///
    /// Returns the digest that should be signed with ed25519 off-chain.
    pub fn compute_export_digest(env: Env, q: log_query::LogQuery) -> soroban_sdk::BytesN<32> {
        let entries = Self::query(env.clone(), q);
        signing::compute_digest(&env, &entries)
    }

    /// Verify a signed audit export against a known public key.
    ///
    /// Panics (aborting the transaction) if the signature is invalid.
    /// This is the secure default for Soroban contracts.
    pub fn verify_export(
        env: Env,
        export: signing::SignedExport,
        public_key: soroban_sdk::BytesN<32>,
    ) {
        signing::verify_export(&env, &export, &public_key)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers (not part of the contract's external interface)
// ---------------------------------------------------------------------------

impl AuditContract {
    fn assert_admin(env: &Env, admin: &Address) -> Result<(), Error> {
        let stored: Address = env
            .storage()
            .persistent()
            .get(&StorageKey::Admin)
            .ok_or(Error::NotInitialized)?;
        if *admin != stored {
            return Err(Error::Unauthorized);
        }
        admin.require_auth();
        Ok(())
    }

    /// Append one sequence id to a bucket's index.
    fn append_index(env: &Env, key: &StorageKey, seq: u64) {
        let mut bucket: Vec<u64> = env
            .storage()
            .persistent()
            .get(key)
            .unwrap_or_else(|| Vec::new(env));
        bucket.push_back(seq);
        env.storage().persistent().set(key, &bucket);
    }

    fn bucket(seq: u64) -> u32 {
        ((seq - 1) / BUCKET_SIZE as u64) as u32
    }
}

#[cfg(test)]
mod tests;
