//! Record types (storage-friendly) for the governance contract.
//!
//! Contains proposal lifecycle types, voting records, delegation tracking,
//! treasury management structures, timelock entries, and storage key enums.

use soroban_sdk::{contracttype, Address, Symbol, Vec};

// ---------------------------------------------------------------------------
// Proposal status
// ---------------------------------------------------------------------------

/// Lifecycle status of a governance proposal.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProposalStatus {
    /// Proposal has been submitted and is awaiting the voting period to start.
    Submitted = 0,
    /// Voting period is active.
    Voting = 1,
    /// Voting ended; proposal passed quorum and majority.
    Passed = 2,
    /// Voting ended; proposal did not meet quorum or majority.
    Defeated = 3,
    /// Timelock delay is active before execution.
    Timelock = 4,
    /// Proposal has been executed.
    Executed = 5,
    /// Proposal was cancelled before execution.
    Cancelled = 6,
    /// Proposal execution failed (reverted).
    ExecutionFailed = 7,
}

/// Type of action a proposal carries.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProposalActionType {
    /// Change a governance parameter (quorum, voting period, etc.).
    ParameterChange = 0,
    /// Treasury spending request.
    TreasurySpend = 1,
    /// Protocol upgrade (WASM hash + migration steps).
    ProtocolUpgrade = 2,
    /// Emergency action (pause/unpause).
    EmergencyAction = 3,
    /// Reward distribution configuration.
    RewardDistribution = 4,
    /// Custom free-form action.
    Custom = 99,
}

// ---------------------------------------------------------------------------
// Proposal
// ---------------------------------------------------------------------------

/// A governance proposal submitted by a token holder.
#[contracttype]
#[derive(Debug, Clone)]
pub struct Proposal {
    /// Monotonically increasing proposal id.
    pub proposal_id: u64,
    /// Address of the proposer.
    pub proposer: Address,
    /// Title of the proposal (short symbol).
    pub title: Symbol,
    /// Description / rationale (free-form symbol).
    pub description: Symbol,
    /// Type of action this proposal carries.
    pub action_type: ProposalActionType,
    /// Encoded action payload (version-specific; stored as a symbol for
    /// simplicity; real-world would use bytes).
    pub action_payload: Symbol,
    /// Current lifecycle status.
    pub status: ProposalStatus,
    /// Total tokens voted in favor.
    pub votes_for: i128,
    /// Total tokens voted against.
    pub votes_against: i128,
    /// Total tokens voted abstain.
    pub votes_abstain: i128,
    /// Number of distinct voters (for quorum counting).
    pub voter_count: u32,
    /// Ledger timestamp when the proposal was submitted.
    pub created_at: u64,
    /// Ledger timestamp when voting started.
    pub voting_starts: u64,
    /// Ledger timestamp when voting ended.
    pub voting_ends: u64,
    /// Ledger timestamp when timelock expires (0 if not yet timelocked).
    pub timelock_expiry: u64,
    /// Ledger timestamp when the proposal was executed (0 if not executed).
    pub executed_at: u64,
}

// ---------------------------------------------------------------------------
// Vote record
// ---------------------------------------------------------------------------

/// The direction a voter chose.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VoteDirection {
    For = 0,
    Against = 1,
    Abstain = 2,
}

/// A single vote cast on a proposal.
#[contracttype]
#[derive(Debug, Clone)]
pub struct VoteRecord {
    /// Proposal this vote is for.
    pub proposal_id: u64,
    /// Address of the voter (may be a delegate).
    pub voter: Address,
    /// Address of the token holder whose tokens are being used.
    /// If the voter is voting on their own behalf, this equals `voter`.
    pub token_holder: Address,
    /// Number of tokens backing this vote.
    pub weight: i128,
    /// Vote direction.
    pub direction: VoteDirection,
    /// Ledger timestamp when the vote was cast.
    pub timestamp: u64,
    /// Whether this vote was cast via delegation.
    pub is_delegated: bool,
}

// ---------------------------------------------------------------------------
// Delegation
// ---------------------------------------------------------------------------

/// A delegation record: one address delegates its voting power to another.
#[contracttype]
#[derive(Debug, Clone)]
pub struct Delegation {
    /// The address delegating its voting power (the delegator).
    pub delegator: Address,
    /// The address receiving the delegated voting power (the delegate).
    pub delegate: Address,
    /// Ledger timestamp when the delegation was created.
    pub created_at: u64,
    /// Whether this delegation is currently active.
    pub active: bool,
}

// ---------------------------------------------------------------------------
// Treasury
// ---------------------------------------------------------------------------

/// A request to withdraw funds from the treasury.
#[contracttype]
#[derive(Debug, Clone)]
pub struct TreasuryRequest {
    /// Monotonically increasing request id.
    pub request_id: u64,
    /// Address of the recipient.
    pub recipient: Address,
    /// Amount to withdraw.
    pub amount: i128,
    /// Symbol of the asset to withdraw.
    pub asset: Symbol,
    /// Human-readable reason.
    pub reason: Symbol,
    /// Addresses that have approved this withdrawal.
    pub approvals: Vec<Address>,
    /// Whether the withdrawal has been executed.
    pub executed: bool,
    /// Ledger timestamp when the request was created.
    pub created_at: u64,
}

// ---------------------------------------------------------------------------
// Timelock
// ---------------------------------------------------------------------------

/// Status of a timelock entry.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimelockStatus {
    /// Waiting for the delay to expire.
    Pending = 0,
    /// Delay expired; ready to execute.
    Ready = 1,
    /// Already executed.
    Executed = 2,
    /// Cancelled before execution.
    Cancelled = 3,
}

/// An entry in the timelock queue.
#[contracttype]
#[derive(Debug, Clone)]
pub struct TimelockEntry {
    /// The proposal id this timelock is for.
    pub proposal_id: u64,
    /// When the timelock delay was set.
    pub scheduled_at: u64,
    /// When the timelock expires and execution becomes possible.
    pub execute_after: u64,
    /// Current status.
    pub status: TimelockStatus,
}

// ---------------------------------------------------------------------------
// Governance config
// ---------------------------------------------------------------------------

/// Configuration parameters for the governance system.
#[contracttype]
#[derive(Debug, Clone)]
pub struct GovernanceConfig {
    /// Minimum tokens required to submit a proposal.
    pub proposal_threshold: i128,
    /// Duration of the voting period in seconds.
    pub voting_period: u64,
    /// Minimum fraction of total supply that must vote (in basis points, 0-10000).
    pub quorum_threshold_bps: u32,
    /// Minimum fraction of votes that must be "for" (in basis points, 0-10000).
    pub approval_threshold_bps: u32,
    /// Timelock delay in seconds after a proposal passes before execution.
    pub timelock_delay: u64,
    /// Minimum delay for emergency actions (shorter timelock).
    pub emergency_timelock_delay: u64,
    /// Basis points of treasury that can be withdrawn per request (cap).
    pub max_treasury_spend_bps: u32,
    /// Multi-sig threshold for treasury withdrawals.
    pub treasury_multisig_threshold: u32,
    /// Reward amount per governance participation (per vote or per proposal).
    pub reward_per_participation: i128,
    /// Maximum number of active proposals at any time.
    pub max_active_proposals: u32,
}

// ---------------------------------------------------------------------------
// Governance summary / snapshot
// ---------------------------------------------------------------------------

/// A snapshot of governance state for querying.
#[contracttype]
#[derive(Debug, Clone)]
pub struct GovernanceSummary {
    /// Total number of proposals ever created.
    pub total_proposals: u64,
    /// Number of proposals currently in Voting or Timelock status.
    pub active_proposals: u32,
    /// Total tokens currently deposited for voting power.
    pub total_voting_power: i128,
    /// Number of delegations currently active.
    pub active_delegations: u32,
    /// Current treasury balance.
    pub treasury_balance: i128,
    /// Whether the system is emergency-paused.
    pub emergency_paused: bool,
}

// ---------------------------------------------------------------------------
// Audit trail entry
// ---------------------------------------------------------------------------

/// A single entry in the governance audit trail.
#[contracttype]
#[derive(Debug, Clone)]
pub struct GovernanceAuditEntry {
    /// Monotonically increasing sequence id.
    pub seq: u64,
    /// Ledger timestamp.
    pub timestamp: u64,
    /// What action was taken.
    pub action: Symbol,
    /// Proposal id this action pertains to (0 if not proposal-related).
    pub proposal_id: u64,
    /// Actor who performed the action.
    pub actor: Address,
    /// Additional detail (free-form).
    pub detail: Symbol,
}

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

/// Storage keys for the governance contract.
#[contracttype]
#[derive(Debug, Clone)]
pub enum StorageKey {
    /// Admin address set during `initialize`.
    Admin,
    /// Guardian address (can pause/unpause).
    Guardian,
    /// Current governance configuration.
    Config,
    /// Total token supply snapshot (used for quorum calculations).
    TotalSupply,
    /// Proposal keyed by proposal id.
    Proposal(u64),
    /// Next proposal id to allocate.
    NextProposalId,
    /// All proposal ids (for enumeration).
    AllProposalIds,
    /// Vote record keyed by (proposal_id, voter_address).
    VoteRecord(u64, Address),
    /// Delegation from a delegator address.
    Delegation(Address),
    /// All active delegation delegator addresses.
    AllDelegators,
    /// Token holder voting power (deposited tokens).
    VotingPower(Address),
    /// Total deposited voting power across all holders.
    TotalVotingPower,
    /// Treasury balance for an asset.
    TreasuryBalance(Symbol),
    /// All assets held in treasury.
    TreasuryAssets,
    /// Treasury withdrawal request keyed by request id.
    TreasuryRequest(u64),
    /// Next treasury request id to allocate.
    NextTreasuryRequestId,
    /// Treasury multi-sig signer addresses.
    TreasurySigners,
    /// Timelock entry keyed by proposal id.
    TimelockEntry(u64),
    /// Audit trail entries.
    AuditTrail,
    /// Next audit trail sequence id.
    NextAuditSeq,
    /// Whether the system is emergency-paused.
    EmergencyPaused,
    /// Reward claiming: has address claimed for proposal.
    RewardClaimed(u64, Address),
    /// Total rewards distributed.
    TotalRewardsDistributed,
    /// Audit-log sink address (cross-contract integration).
    AuditSink,
}
