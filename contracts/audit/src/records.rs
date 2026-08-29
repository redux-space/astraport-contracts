//! Record types (storage-friendly) for the audit-log contract.
//!
//! Status of the on-chain hash: the user-facing spec asked for BLAKE3; we use
//! SHA-256 via the Soroban host (`env.crypto().sha256`) because BLAKE3 is not
//! available natively in Soroban 21.5.0 and any pure-WASM BLAKE3 implementation
//! would inflate gas/host-metering cost. SHA-256 is collision-resistant and
//! gives equivalent tamper-detection guarantees for this use case.

use soroban_sdk::{contracttype, Address, BytesN, String, Symbol, Vec};

/// Number of sequence ids packed into a single bucket index.
///
/// Smaller buckets keep each storage entry cheap to read/write. 250 fits inside
/// Soroban's hard cap on `Vec` size (the current host limit sits at ~500
/// elements), so each index bucket can grow up to `BUCKET_SIZE` entries before
/// the next bucket is created.
pub const BUCKET_SIZE: u32 = 250;

/// Chain origin (empty / genesis) hash. The first entry's hash is computed by
/// `SHA-256(CHAIN_ORIGIN || serialize(first_entry))`.
pub const CHAIN_ORIGIN: [u8; 32] = [0u8; 32];

/// Distinct event types that may be logged.
///
/// `Custom` is the catch-all variant for events that don't fit the predefined
/// categories. New variants should be appended (never re-ordered) for stable
/// serialization.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuditEventType {
    /// Portfolio was rebalanced (manual or scheduled execution).
    Rebalance = 0,
    /// Stake deposit (new principal added to a position).
    Stake = 1,
    /// Normal unstake (no penalty applied).
    Unstake = 2,
    /// Emergency unstake (penalty applied).
    EmergencyUnstake = 3,
    /// Yield accrued to a position.
    YieldAccrual = 4,
    /// External deposit into a portfolio.
    Deposit = 5,
    /// External withdrawal from a portfolio.
    Withdrawal = 6,
    /// Rebalance schedule created/updated/cancelled.
    ScheduleChange = 7,
    /// Admin-level configuration change (alert threshold, default APR, etc.).
    AdminAction = 8,
    /// Portfolio was created or initialized.
    PortfolioCreated = 9,
    /// Role granted or revoked (RBAC change).
    RoleChange = 10,
    /// Yield claimed by a staker.
    YieldClaim = 11,
    /// Governance proposal submitted.
    GovernanceProposal = 12,
    /// Governance vote cast.
    GovernanceVote = 13,
    /// Treasury withdrawal or spending action.
    TreasuryAction = 14,
    /// Emergency pause or unpause.
    EmergencyPause = 15,
    /// Trade executed (order matched or batch filled).
    TradeExecution = 16,
    /// Order placed on the order book.
    OrderPlaced = 17,
    /// Order cancelled from the order book.
    OrderCancelled = 18,
    /// Fee collected from a transaction.
    FeeCollection = 19,
    /// Catch-all for events that don't fit a predefined type.
    Custom = 99,
}

/// A `(key, value)` pair inside a [`StateSnapshot`].
///
/// `key` is typically a logical label (e.g. asset `Symbol`) and `value` is a
/// signed numeric measurement (e.g. balance, weight, total).
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldEntry {
    pub key: Symbol,
    pub value: i128,
}

/// Snapshot of relevant state just before or after a state-changing event.
///
/// Stored as a deterministically-ordered `Vec` so the chain hash is stable
/// (Soroban's `Map` iteration order is insertion-stable but verbose in tests).
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSnapshot {
    pub fields: Vec<FieldEntry>,
}

impl StateSnapshot {
    /// Build an empty snapshot.
    pub fn empty(env: &soroban_sdk::Env) -> Self {
        Self {
            fields: Vec::new(env),
        }
    }

    /// Append a `(key, value)` field. Keep call-sites simple and deterministic.
    pub fn push(&mut self, key: Symbol, value: i128) {
        self.fields.push_back(FieldEntry { key, value });
    }
}

/// One immutable audit-log entry.
///
/// `hash` is the SHA-256 chain hash:
/// `SHA-256(prev_hash || canonical_bytes(entry_excluding_hash))`. The hash
/// binds this entry to every prior entry in the chain (Merkle-style) so any
/// in-place tampering breaks verification downstream.
#[contracttype]
#[derive(Debug, Clone)]
pub struct AuditLog {
    /// Monotonically increasing sequence id assigned at log time.
    pub seq: u64,
    /// Ledger timestamp (seconds) at time of log.
    pub timestamp: u64,
    /// What happened.
    pub event_type: AuditEventType,
    /// Caller / signer that initiated the event. May be the contract itself
    /// when logging from a system path.
    pub actor: Address,
    /// Bitmask of permissions the actor asserted (admin, staker, treasury,
    /// etc.). Interpretation is policy-defined; we record the raw bitmask so
    /// off-chain consumers can decode it.
    pub permissions: u32,
    /// Portfolio or logical scope the event affects (e.g. portfolio id, or
    /// `GLOBAL` for whole-system events).
    pub portfolio: Symbol,
    /// State immediately before the event. Empty when not applicable.
    pub state_before: StateSnapshot,
    /// State immediately after the event. Empty when not applicable.
    pub state_after: StateSnapshot,
    /// Outcome symbol (e.g. `"ok"`, `"fail"`, `"PEN_APPL"`). Free-form but
    /// should be one of a known set per event type to keep queries consistent.
    pub outcome: Symbol,
    /// Optional human-readable detail.
    pub detail: String,
    /// SHA-256 chain hash binding this entry to the previous entry's hash.
    pub hash: BytesN<32>,
}

/// Admin-configurable retention policy.
///
/// Either field `== 0` means "no cap". When at least one cap is configured,
/// the admin-triggered `prune_old` enforces it.
#[contracttype]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Maximum number of entries to retain. 0 = no cap.
    pub max_entries: u64,
    /// Maximum age (seconds) of any retained entry. 0 = no cap.
    pub max_age_seconds: u64,
}

impl RetentionPolicy {
    pub fn is_unbounded(&self) -> bool {
        self.max_entries == 0 && self.max_age_seconds == 0
    }
}

/// Standard permission bitflags. Hosts MAY log these; the contract simply
/// stores the raw `u32` so off-chain consumers and the contract itself can
/// share a vocabulary.
pub mod permissions {
    pub const NONE: u32 = 0;
    pub const STAKER: u32 = 1 << 0;
    pub const ADMIN: u32 = 1 << 1;
    pub const TREASURY: u32 = 1 << 2;
    pub const SYSTEM: u32 = 1 << 3;
}

/// Storage keys for the audit-log contract.
#[contracttype]
#[derive(Debug, Clone)]
pub enum StorageKey {
    /// Admin address set by `initialize`.
    Admin,
    /// Retention policy.
    RetentionPolicy,
    /// Current tail sequence (next id to be assigned).
    NextSeq,
    /// Head hash of the entry chain.
    RollingChecksum,
    /// Floor sequence id still stored (entries below are pruned).
    FirstSeq,
    /// Total number of entries currently retained.
    EntryCount,
    /// An audit entry, keyed by sequence id.
    Entry(u64),
    /// Bucket index for an event-type secondary lookup.
    /// `bucket = seq / BUCKET_SIZE`.
    IndexByType(AuditEventType, u32),
    /// Bucket index for an actor secondary lookup.
    IndexByActor(Address, u32),
    /// Bucket index for a portfolio secondary lookup.
    IndexByPortfolio(Symbol, u32),
    /// Index of every logged sequence id (1:1 with `Entry(seq)`).
    AllSeqs,
    /// Bucket index for an outcome secondary lookup.
    IndexByOutcome(Symbol, u32),
}
