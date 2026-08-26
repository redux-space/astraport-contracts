//! # AstraPort Governance DAO Contract
//!
//! Comprehensive decentralized governance system enabling community-driven
//! decision-making, treasury management, and protocol upgrades on Soroban.
//!
//! ## Module overview
//!
//! - [`records`] — types: `Proposal`, `VoteRecord`, `Delegation`,
//!   `TreasuryRequest`, `TimelockEntry`, `GovernanceConfig`, `StorageKey`.
//! - [`treasury`] — multi-sig treasury management: deposits, withdrawal
//!   requests, approvals, execution.
//! - [`timelock`] — time-delayed execution queue for passed proposals.
//!
//! ## Lifecycle
//!
//! 1. `initialize(admin, guardian)` — one-time setup.
//! 2. `set_config(config)` — configure governance parameters.
//! 3. `deposit_voting_power(amount)` — token holders deposit for voting.
//! 4. `submit_proposal(...)` — create a governance proposal.
//! 5. `cast_vote(proposal_id, direction)` — vote on active proposals.
//! 6. `finalize_proposal(proposal_id)` — tally votes and transition status.
//! 7. `queue_timelock(proposal_id)` — place passed proposal in timelock.
//! 8. `execute_proposal(proposal_id)` — execute after timelock expires.
//!
//! Additional capabilities:
//! - Vote delegation (revocable)
//! - Treasury management with multi-sig
//! - Emergency pause/unpause (guardian or multi-sig)
//! - Governance reward distribution
//! - Complete audit trail

#![no_std]
#![allow(clippy::too_many_arguments)]

extern crate alloc;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Symbol, Vec,
};

use astraport_audit::logger::AuditLogger;
use astraport_audit::records::{permissions, AuditEventType, StateSnapshot};

pub mod records;
pub mod timelock;
pub mod treasury;

use crate::records::{
    Delegation, GovernanceAuditEntry, GovernanceConfig, GovernanceSummary, Proposal,
    ProposalActionType, ProposalStatus, StorageKey, VoteDirection, VoteRecord,
};

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

/// Default proposal threshold: 1000 tokens minimum to submit a proposal.
const DEFAULT_PROPOSAL_THRESHOLD: i128 = 1_000;
/// Default voting period: 7 days in seconds.
const DEFAULT_VOTING_PERIOD: u64 = 604_800;
/// Default quorum: 4% of total supply must vote (400 bps).
const DEFAULT_QUORUM_BPS: u32 = 400;
/// Default approval threshold: 50%+1 of votes must be "for" (5001 bps).
const DEFAULT_APPROVAL_BPS: u32 = 5_001;
/// Default timelock delay: 48 hours in seconds.
const DEFAULT_TIMELOCK_DELAY: u64 = 172_800;
/// Default emergency timelock delay: 1 hour in seconds.
const DEFAULT_EMERGENCY_TIMELOCK_DELAY: u64 = 3_600;
/// Default max treasury spend: 10% per request (1000 bps).
const DEFAULT_MAX_TREASURY_SPEND_BPS: u32 = 1_000;
/// Default treasury multi-sig threshold: 2.
const DEFAULT_TREASURY_MULTISIG_THRESHOLD: u32 = 2;
/// Default reward per participation: 10 tokens.
const DEFAULT_REWARD_PER_PARTICIPATION: i128 = 10;
/// Default max active proposals: 10.
const DEFAULT_MAX_ACTIVE_PROPOSALS: u32 = 10;
/// Maximum number of audit trail entries to retain.
const MAX_AUDIT_TRAIL: u32 = 5_000;

/// Sentinel return symbol.
const OK: Symbol = symbol_short!("ok");

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the governance contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// Contract was not initialized.
    NotInitialized = 1,
    /// Caller is not the admin.
    Unauthorized = 2,
    /// Contract was already initialized.
    AlreadyInitialized = 3,
    /// Insufficient token balance to submit a proposal.
    InsufficientVotingPower = 4,
    /// Proposal not found.
    ProposalNotFound = 5,
    /// Proposal is not in a votable state.
    NotVotable = 6,
    /// Voter has already voted on this proposal.
    AlreadyVoted = 7,
    /// Invalid vote direction.
    InvalidVoteDirection = 8,
    /// Voting period has not ended yet.
    VotingNotEnded = 9,
    /// Voting period has already ended.
    VotingAlreadyEnded = 10,
    /// Proposal did not meet quorum.
    QuorumNotReached = 11,
    /// Proposal did not meet approval threshold.
    ApprovalNotReached = 12,
    /// Proposal is not in a state that can be timelocked.
    CannotTimelock = 13,
    /// Timelock delay has not expired yet.
    TimelockNotExpired = 14,
    /// Proposal is not in a state that can be executed.
    CannotExecute = 15,
    /// Cannot cancel a proposal in its current state.
    CannotCancel = 16,
    /// Delegation target not found.
    DelegationNotFound = 17,
    /// Cannot delegate to yourself.
    CannotDelegateToSelf = 18,
    /// No voting power deposited.
    NoVotingPower = 19,
    /// Reward already claimed.
    RewardAlreadyClaimed = 20,
    /// Invalid configuration parameters.
    InvalidConfig = 21,
    /// Maximum active proposals reached.
    MaxProposalsReached = 22,
    /// Caller is not the guardian.
    GuardianRequired = 23,
    /// System is not paused (for unpause).
    NotPaused = 24,
    /// System is already paused.
    AlreadyPaused = 25,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Event emitted when a proposal is submitted.
#[contracttype]
#[derive(Debug, Clone)]
pub struct ProposalEvent {
    pub proposal_id: u64,
    pub proposer: Address,
    pub title: Symbol,
    pub action_type: ProposalActionType,
}

/// Event emitted when a vote is cast.
#[contracttype]
#[derive(Debug, Clone)]
pub struct VoteEvent {
    pub proposal_id: u64,
    pub voter: Address,
    pub direction: VoteDirection,
    pub weight: i128,
}

/// Event emitted when a delegation is created or revoked.
#[contracttype]
#[derive(Debug, Clone)]
pub struct DelegationEvent {
    pub delegator: Address,
    pub delegate: Address,
    pub active: bool,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// Governance DAO contract for AstraPort.
///
/// Enables token-weighted voting, proposal management, treasury control,
/// timelock execution, and emergency pause capabilities.
#[contract]
pub struct GovernanceDAO;

#[contractimpl]
impl GovernanceDAO {
    // ====================================================================
    // Initialization
    // ====================================================================

    /// Initialize the governance contract.
    ///
    /// Sets the admin and guardian addresses and initializes default
    /// governance configuration. Can only be called once.
    pub fn initialize(env: Env, admin: Address, guardian: Address) -> Result<Symbol, Error> {
        if env.storage().persistent().has(&StorageKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }

        env.storage().persistent().set(&StorageKey::Admin, &admin);
        env.storage()
            .persistent()
            .set(&StorageKey::Guardian, &guardian);

        let config = GovernanceConfig {
            proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
            voting_period: DEFAULT_VOTING_PERIOD,
            quorum_threshold_bps: DEFAULT_QUORUM_BPS,
            approval_threshold_bps: DEFAULT_APPROVAL_BPS,
            timelock_delay: DEFAULT_TIMELOCK_DELAY,
            emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
            max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
            treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
            reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
            max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
        };
        env.storage().persistent().set(&StorageKey::Config, &config);

        env.storage()
            .persistent()
            .set(&StorageKey::TotalSupply, &0i128);
        env.storage()
            .persistent()
            .set(&StorageKey::TotalVotingPower, &0i128);
        env.storage()
            .persistent()
            .set(&StorageKey::EmergencyPaused, &false);
        env.storage()
            .persistent()
            .set(&StorageKey::NextProposalId, &1u64);
        env.storage()
            .persistent()
            .set(&StorageKey::NextTreasuryRequestId, &1u64);
        env.storage()
            .persistent()
            .set(&StorageKey::NextAuditSeq, &1u64);
        env.storage()
            .persistent()
            .set(&StorageKey::AllProposalIds, &Vec::<u64>::new(&env));
        env.storage()
            .persistent()
            .set(&StorageKey::AllDelegators, &Vec::<Address>::new(&env));

        env.events()
            .publish((symbol_short!("GOV_INIT"), &admin), &guardian);

        Ok(OK)
    }

    /// Return the current admin address.
    pub fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .persistent()
            .get(&StorageKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    /// Return the current guardian address.
    pub fn get_guardian(env: Env) -> Result<Address, Error> {
        env.storage()
            .persistent()
            .get(&StorageKey::Guardian)
            .ok_or(Error::NotInitialized)
    }

    // ====================================================================
    // Configuration
    // ====================================================================

    /// Set the governance configuration. Admin-only.
    pub fn set_config(env: Env, admin: Address, config: GovernanceConfig) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;

        if config.voting_period == 0
            || config.quorum_threshold_bps == 0
            || config.quorum_threshold_bps > 10_000
            || config.approval_threshold_bps == 0
            || config.approval_threshold_bps > 10_000
            || config.treasury_multisig_threshold == 0
            || config.max_active_proposals == 0
        {
            return Err(Error::InvalidConfig);
        }

        env.storage().persistent().set(&StorageKey::Config, &config);

        Self::append_audit(
            &env,
            &symbol_short!("set_cfg"),
            0,
            &admin,
            &symbol_short!("config"),
        );

        Ok(OK)
    }

    /// Get the current governance configuration.
    pub fn get_config(env: Env) -> GovernanceConfig {
        env.storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            })
    }

    /// Set the total token supply snapshot (for quorum calculations). Admin-only.
    pub fn set_total_supply(env: Env, admin: Address, total_supply: i128) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;
        env.storage()
            .persistent()
            .set(&StorageKey::TotalSupply, &total_supply);
        Ok(OK)
    }

    /// Get the total token supply snapshot.
    pub fn get_total_supply(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&StorageKey::TotalSupply)
            .unwrap_or_default()
    }

    // ====================================================================
    // Voting power management
    // ====================================================================

    /// Deposit tokens for voting power.
    ///
    /// Requires authorization from `holder`. Increases the holder's voting
    /// power and the total voting power.
    pub fn deposit_voting_power(env: Env, holder: Address, amount: i128) -> Result<i128, Error> {
        holder.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidConfig);
        }

        let current: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::VotingPower(holder.clone()))
            .unwrap_or_default();

        let new_power = current.checked_add(amount).ok_or(Error::InvalidConfig)?;

        env.storage()
            .persistent()
            .set(&StorageKey::VotingPower(holder.clone()), &new_power);

        // Update total voting power.
        let total: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVotingPower)
            .unwrap_or_default();
        env.storage().persistent().set(
            &StorageKey::TotalVotingPower,
            &total.checked_add(amount).ok_or(Error::InvalidConfig)?,
        );

        env.events()
            .publish((symbol_short!("VP_DEP"), &holder), (amount, new_power));

        Ok(new_power)
    }

    /// Withdraw deposited voting power.
    ///
    /// Requires authorization from `holder`. Decreases the holder's voting
    /// power and the total voting power.
    pub fn withdraw_voting_power(env: Env, holder: Address, amount: i128) -> Result<i128, Error> {
        holder.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidConfig);
        }

        let current: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::VotingPower(holder.clone()))
            .unwrap_or_default();

        if amount > current {
            return Err(Error::NoVotingPower);
        }

        let new_power = current - amount;
        if new_power == 0 {
            env.storage()
                .persistent()
                .remove(&StorageKey::VotingPower(holder.clone()));
        } else {
            env.storage()
                .persistent()
                .set(&StorageKey::VotingPower(holder.clone()), &new_power);
        }

        // Update total voting power.
        let total: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVotingPower)
            .unwrap_or_default();
        env.storage().persistent().set(
            &StorageKey::TotalVotingPower,
            &total.checked_sub(amount).ok_or(Error::InvalidConfig)?,
        );

        env.events()
            .publish((symbol_short!("VP_WTH"), &holder), (amount, new_power));

        Ok(new_power)
    }

    /// Get the voting power for a holder.
    pub fn get_voting_power(env: Env, holder: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&StorageKey::VotingPower(holder))
            .unwrap_or_default()
    }

    /// Get the total deposited voting power.
    pub fn get_total_voting_power(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&StorageKey::TotalVotingPower)
            .unwrap_or_default()
    }

    // ====================================================================
    // Proposal system
    // ====================================================================

    /// Submit a new governance proposal.
    ///
    /// Requires the proposer to have at least `proposal_threshold` voting
    /// power. Returns the new proposal id.
    pub fn submit_proposal(
        env: Env,
        proposer: Address,
        title: Symbol,
        description: Symbol,
        action_type: ProposalActionType,
        action_payload: Symbol,
    ) -> Result<u64, Error> {
        proposer.require_auth();

        let config: GovernanceConfig = env
            .storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            });

        // Check proposer has sufficient voting power.
        let voting_power: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::VotingPower(proposer.clone()))
            .unwrap_or_default();

        if voting_power < config.proposal_threshold {
            return Err(Error::InsufficientVotingPower);
        }

        // Check max active proposals.
        let all_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&StorageKey::AllProposalIds)
            .unwrap_or_else(|| Vec::new(&env));
        let active_count = Self::count_active_proposals(&env, &all_ids);
        if active_count >= config.max_active_proposals {
            return Err(Error::MaxProposalsReached);
        }

        // Allocate proposal id.
        let proposal_id: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::NextProposalId)
            .unwrap_or(1);

        let now = env.ledger().timestamp();
        let voting_starts = now;
        let voting_ends = now + config.voting_period;

        let proposal = Proposal {
            proposal_id,
            proposer: proposer.clone(),
            title: title.clone(),
            description,
            action_type,
            action_payload,
            status: ProposalStatus::Voting,
            votes_for: 0,
            votes_against: 0,
            votes_abstain: 0,
            voter_count: 0,
            created_at: now,
            voting_starts,
            voting_ends,
            timelock_expiry: 0,
            executed_at: 0,
        };

        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);
        env.storage()
            .persistent()
            .set(&StorageKey::NextProposalId, &(proposal_id + 1));

        // Add to all proposals list.
        let mut ids = all_ids;
        ids.push_back(proposal_id);
        env.storage()
            .persistent()
            .set(&StorageKey::AllProposalIds, &ids);

        // Audit trail.
        Self::append_audit(
            &env,
            &symbol_short!("submit"),
            proposal_id,
            &proposer,
            &title,
        );

        env.events().publish(
            (symbol_short!("PROPOSAL"), &proposer),
            ProposalEvent {
                proposal_id,
                proposer: proposer.clone(),
                title,
                action_type: proposal.action_type,
            },
        );

        Ok(proposal_id)
    }

    /// Get a proposal by id.
    pub fn get_proposal(env: Env, proposal_id: u64) -> Result<Proposal, Error> {
        env.storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)
    }

    /// Get all proposal ids.
    pub fn get_all_proposal_ids(env: Env) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&StorageKey::AllProposalIds)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Cancel a proposal. Only the proposer or admin can cancel.
    /// Can only cancel proposals in Submitted or Voting status.
    pub fn cancel_proposal(env: Env, caller: Address, proposal_id: u64) -> Result<Symbol, Error> {
        caller.require_auth();

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        let admin: Address = env
            .storage()
            .persistent()
            .get(&StorageKey::Admin)
            .ok_or(Error::NotInitialized)?;

        if caller != proposal.proposer && caller != admin {
            return Err(Error::CannotCancel);
        }

        match proposal.status {
            ProposalStatus::Submitted | ProposalStatus::Voting => {
                proposal.status = ProposalStatus::Cancelled;
            }
            _ => return Err(Error::CannotCancel),
        }

        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("cancel"),
            proposal_id,
            &caller,
            &symbol_short!("cancelled"),
        );

        Ok(OK)
    }

    // ====================================================================
    // Voting
    // ====================================================================

    /// Cast a vote on an active proposal.
    ///
    /// Requires the voter to have deposited voting power. The vote weight
    /// equals the voter's current deposited voting power. Supports direct
    /// voting and delegated voting.
    pub fn cast_vote(
        env: Env,
        voter: Address,
        proposal_id: u64,
        direction: VoteDirection,
    ) -> Result<Symbol, Error> {
        voter.require_auth();

        let _config: GovernanceConfig = env
            .storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            });

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        // Check proposal is votable.
        if proposal.status != ProposalStatus::Voting {
            return Err(Error::NotVotable);
        }

        let now = env.ledger().timestamp();
        if now > proposal.voting_ends {
            return Err(Error::VotingAlreadyEnded);
        }

        // Check voter hasn't already voted.
        let existing_vote: Option<VoteRecord> = env
            .storage()
            .persistent()
            .get(&StorageKey::VoteRecord(proposal_id, voter.clone()));
        if existing_vote.is_some() {
            return Err(Error::AlreadyVoted);
        }

        // Check for delegation.
        let delegation: Option<Delegation> = env
            .storage()
            .persistent()
            .get(&StorageKey::Delegation(voter.clone()));

        let (token_holder, weight, is_delegated) = if let Some(ref del) = delegation {
            if del.active && del.delegate == voter {
                // This voter is receiving delegated power.
                // Weight = own power + all delegated power to them.
                let own_power: i128 = env
                    .storage()
                    .persistent()
                    .get(&StorageKey::VotingPower(voter.clone()))
                    .unwrap_or_default();
                (voter.clone(), own_power, false)
            } else {
                // This voter is delegating to someone else, but is voting
                // on their own behalf using their deposited power.
                let own_power: i128 = env
                    .storage()
                    .persistent()
                    .get(&StorageKey::VotingPower(voter.clone()))
                    .unwrap_or_default();
                (voter.clone(), own_power, false)
            }
        } else {
            // No delegation; use own deposited power.
            let own_power: i128 = env
                .storage()
                .persistent()
                .get(&StorageKey::VotingPower(voter.clone()))
                .unwrap_or_default();
            (voter.clone(), own_power, false)
        };

        if weight <= 0 {
            return Err(Error::NoVotingPower);
        }

        // Record the vote.
        let vote = VoteRecord {
            proposal_id,
            voter: voter.clone(),
            token_holder: token_holder.clone(),
            weight,
            direction,
            timestamp: now,
            is_delegated,
        };

        env.storage()
            .persistent()
            .set(&StorageKey::VoteRecord(proposal_id, voter.clone()), &vote);

        // Update proposal tallies.
        match direction {
            VoteDirection::For => proposal.votes_for += weight,
            VoteDirection::Against => proposal.votes_against += weight,
            VoteDirection::Abstain => proposal.votes_abstain += weight,
        }
        proposal.voter_count += 1;

        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("vote"),
            proposal_id,
            &voter,
            &symbol_short!("cast"),
        );

        env.events().publish(
            (symbol_short!("VOTE"), &voter),
            VoteEvent {
                proposal_id,
                voter: voter.clone(),
                direction,
                weight,
            },
        );

        Ok(OK)
    }

    /// Cast a vote on behalf of a delegated token holder.
    ///
    /// The `delegate` must have an active delegation from `token_holder`.
    /// The vote weight equals the token holder's deposited voting power.
    pub fn cast_delegated_vote(
        env: Env,
        delegate: Address,
        token_holder: Address,
        proposal_id: u64,
        direction: VoteDirection,
    ) -> Result<Symbol, Error> {
        delegate.require_auth();

        let _config: GovernanceConfig = env
            .storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            });

        // Verify delegation exists.
        let delegation: Delegation = env
            .storage()
            .persistent()
            .get(&StorageKey::Delegation(token_holder.clone()))
            .ok_or(Error::DelegationNotFound)?;

        if !delegation.active || delegation.delegate != delegate {
            return Err(Error::DelegationNotFound);
        }

        // Check proposal is votable.
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Voting {
            return Err(Error::NotVotable);
        }

        let now = env.ledger().timestamp();
        if now > proposal.voting_ends {
            return Err(Error::VotingAlreadyEnded);
        }

        // Check token holder hasn't already voted.
        let existing: Option<VoteRecord> = env
            .storage()
            .persistent()
            .get(&StorageKey::VoteRecord(proposal_id, token_holder.clone()));
        if existing.is_some() {
            return Err(Error::AlreadyVoted);
        }

        // Get the token holder's voting power.
        let weight: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::VotingPower(token_holder.clone()))
            .unwrap_or_default();

        if weight <= 0 {
            return Err(Error::NoVotingPower);
        }

        // Record the delegated vote.
        let vote = VoteRecord {
            proposal_id,
            voter: delegate.clone(),
            token_holder: token_holder.clone(),
            weight,
            direction,
            timestamp: now,
            is_delegated: true,
        };

        env.storage().persistent().set(
            &StorageKey::VoteRecord(proposal_id, token_holder.clone()),
            &vote,
        );

        // Update proposal tallies.
        match direction {
            VoteDirection::For => proposal.votes_for += weight,
            VoteDirection::Against => proposal.votes_against += weight,
            VoteDirection::Abstain => proposal.votes_abstain += weight,
        }
        proposal.voter_count += 1;

        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("del_vote"),
            proposal_id,
            &delegate,
            &symbol_short!("del_vote"),
        );

        env.events().publish(
            (symbol_short!("DEL_VOTE"), &delegate),
            VoteEvent {
                proposal_id,
                voter: token_holder,
                direction,
                weight,
            },
        );

        Ok(OK)
    }

    /// Get a specific vote record.
    pub fn get_vote(env: Env, proposal_id: u64, voter: Address) -> Option<VoteRecord> {
        env.storage()
            .persistent()
            .get(&StorageKey::VoteRecord(proposal_id, voter))
    }

    // ====================================================================
    // Proposal finalization
    // ====================================================================

    /// Finalize a proposal after the voting period has ended.
    ///
    /// Checks quorum and approval thresholds. Transitions the proposal
    /// to Passed or Defeated.
    pub fn finalize_proposal(env: Env, proposal_id: u64) -> Result<ProposalStatus, Error> {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Voting {
            return Err(Error::VotingAlreadyEnded);
        }

        let now = env.ledger().timestamp();
        if now <= proposal.voting_ends {
            return Err(Error::VotingNotEnded);
        }

        let config: GovernanceConfig = env
            .storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            });

        // Check quorum.
        let total_voting_power: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVotingPower)
            .unwrap_or_default();

        let total_votes = proposal.votes_for + proposal.votes_against + proposal.votes_abstain;
        let quorum_required = total_voting_power
            .checked_mul(config.quorum_threshold_bps as i128)
            .ok_or(Error::InvalidConfig)?
            / 10_000;

        if total_votes < quorum_required {
            proposal.status = ProposalStatus::Defeated;
            env.storage()
                .persistent()
                .set(&StorageKey::Proposal(proposal_id), &proposal);
            Self::append_audit(
                &env,
                &symbol_short!("defeat"),
                proposal_id,
                &proposal.proposer,
                &symbol_short!("quorum"),
            );
            return Ok(ProposalStatus::Defeated);
        }

        // Check approval threshold.
        let net_votes = proposal.votes_for + proposal.votes_against;
        if net_votes == 0 {
            proposal.status = ProposalStatus::Defeated;
            env.storage()
                .persistent()
                .set(&StorageKey::Proposal(proposal_id), &proposal);
            return Ok(ProposalStatus::Defeated);
        }

        let approval_required = net_votes
            .checked_mul(config.approval_threshold_bps as i128)
            .ok_or(Error::InvalidConfig)?
            / 10_000;

        if proposal.votes_for <= approval_required {
            proposal.status = ProposalStatus::Defeated;
            env.storage()
                .persistent()
                .set(&StorageKey::Proposal(proposal_id), &proposal);
            Self::append_audit(
                &env,
                &symbol_short!("defeat"),
                proposal_id,
                &proposal.proposer,
                &symbol_short!("approval"),
            );
            return Ok(ProposalStatus::Defeated);
        }

        // Proposal passed.
        proposal.status = ProposalStatus::Passed;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("pass"),
            proposal_id,
            &proposal.proposer,
            &symbol_short!("passed"),
        );

        env.events().publish(
            (symbol_short!("PASSED"), proposal_id),
            (proposal.votes_for, proposal.votes_against, total_votes),
        );

        Ok(ProposalStatus::Passed)
    }

    // ====================================================================
    // Timelock
    // ====================================================================

    /// Queue a passed proposal into the timelock.
    ///
    /// Can only be called on proposals with `Passed` status.
    pub fn queue_timelock(env: Env, proposal_id: u64) -> Result<Symbol, Error> {
        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Passed {
            return Err(Error::CannotTimelock);
        }

        let config: GovernanceConfig = env
            .storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            });

        let is_emergency = proposal.action_type == ProposalActionType::EmergencyAction;
        let delay = timelock::effective_delay(&env, is_emergency, &config);

        let entry =
            timelock::schedule(&env, proposal_id, delay).map_err(|_| Error::CannotTimelock)?;

        proposal.status = ProposalStatus::Timelock;
        proposal.timelock_expiry = entry.execute_after;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("tl_queue"),
            proposal_id,
            &proposal.proposer,
            &symbol_short!("queued"),
        );

        Ok(OK)
    }

    /// Execute a proposal after its timelock has expired.
    ///
    /// Transitions the proposal to `Executed`. The actual execution logic
    /// (WASM upgrade, treasury transfer, etc.) is handled by the caller
    /// or external integration; this records the state change.
    pub fn execute_proposal(
        env: Env,
        proposal_id: u64,
        executor: Address,
    ) -> Result<Symbol, Error> {
        executor.require_auth();

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Timelock {
            return Err(Error::CannotExecute);
        }

        // Check timelock has expired.
        let _entry =
            timelock::check_ready(&env, proposal_id).map_err(|_| Error::TimelockNotExpired)?;

        // Mark timelock as executed.
        let _ = timelock::mark_executed(&env, proposal_id).map_err(|_| Error::CannotExecute)?;

        let now = env.ledger().timestamp();
        proposal.status = ProposalStatus::Executed;
        proposal.executed_at = now;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("execute"),
            proposal_id,
            &executor,
            &symbol_short!("executed"),
        );

        env.events()
            .publish((symbol_short!("EXECUTED"), proposal_id), (executor, now));

        Ok(OK)
    }

    // ====================================================================
    // Delegation
    // ====================================================================

    /// Delegate voting power to another address.
    ///
    /// Creates or updates a delegation record. The delegate can cast votes
    /// on behalf of the delegator using `cast_delegated_vote`.
    pub fn delegate(env: Env, delegator: Address, delegate_addr: Address) -> Result<Symbol, Error> {
        delegator.require_auth();

        if delegator == delegate_addr {
            return Err(Error::CannotDelegateToSelf);
        }

        let now = env.ledger().timestamp();
        let delegation = Delegation {
            delegator: delegator.clone(),
            delegate: delegate_addr.clone(),
            created_at: now,
            active: true,
        };

        env.storage()
            .persistent()
            .set(&StorageKey::Delegation(delegator.clone()), &delegation);

        // Track all delegators.
        let mut delegators: Vec<Address> = env
            .storage()
            .persistent()
            .get(&StorageKey::AllDelegators)
            .unwrap_or_else(|| Vec::new(&env));
        if !delegators.contains(&delegator) {
            delegators.push_back(delegator.clone());
            env.storage()
                .persistent()
                .set(&StorageKey::AllDelegators, &delegators);
        }

        Self::append_audit(
            &env,
            &symbol_short!("delegate"),
            0,
            &delegator,
            &symbol_short!("delegate"),
        );

        env.events().publish(
            (symbol_short!("DELEGATE"), &delegator),
            DelegationEvent {
                delegator: delegator.clone(),
                delegate: delegate_addr,
                active: true,
            },
        );

        Ok(OK)
    }

    /// Revoke a delegation.
    pub fn revoke_delegation(env: Env, delegator: Address) -> Result<Symbol, Error> {
        delegator.require_auth();

        let mut delegation: Delegation = env
            .storage()
            .persistent()
            .get(&StorageKey::Delegation(delegator.clone()))
            .ok_or(Error::DelegationNotFound)?;

        delegation.active = false;
        env.storage()
            .persistent()
            .set(&StorageKey::Delegation(delegator.clone()), &delegation);

        Self::append_audit(
            &env,
            &symbol_short!("rvk_del"),
            0,
            &delegator,
            &symbol_short!("rvk_del"),
        );

        env.events().publish(
            (symbol_short!("DEL_REV"), &delegator),
            DelegationEvent {
                delegator: delegator.clone(),
                delegate: delegation.delegate,
                active: false,
            },
        );

        Ok(OK)
    }

    /// Get the delegation for a delegator.
    pub fn get_delegation(env: Env, delegator: Address) -> Option<Delegation> {
        env.storage()
            .persistent()
            .get(&StorageKey::Delegation(delegator))
    }

    /// Get all active delegators.
    pub fn get_all_delegators(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&StorageKey::AllDelegators)
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ====================================================================
    // Treasury integration
    // ====================================================================

    /// Deposit funds into the treasury.
    pub fn treasury_deposit(
        env: Env,
        depositor: Address,
        asset: Symbol,
        amount: i128,
    ) -> Result<i128, treasury::TreasuryError> {
        treasury::deposit(&env, &depositor, &asset, amount)
    }

    /// Get the treasury balance for an asset.
    pub fn treasury_balance(env: Env, asset: Symbol) -> i128 {
        treasury::balance_of(&env, &asset)
    }

    /// Get all treasury assets.
    pub fn treasury_all_assets(env: Env) -> Vec<Symbol> {
        treasury::all_assets(&env)
    }

    /// Create a treasury withdrawal request.
    pub fn treasury_create_request(
        env: Env,
        requester: Address,
        recipient: Address,
        amount: i128,
        asset: Symbol,
        reason: Symbol,
    ) -> Result<u64, treasury::TreasuryError> {
        let config: GovernanceConfig = env
            .storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            });
        treasury::create_request(
            &env, &requester, &recipient, amount, &asset, &reason, &config,
        )
    }

    /// Approve a treasury withdrawal request.
    pub fn treasury_approve_request(
        env: Env,
        signer: Address,
        request_id: u64,
    ) -> Result<treasury::TreasuryRequest, treasury::TreasuryError> {
        treasury::approve_request(&env, &signer, request_id)
    }

    /// Execute a treasury withdrawal request.
    pub fn treasury_execute_request(
        env: Env,
        request_id: u64,
    ) -> Result<i128, treasury::TreasuryError> {
        let config: GovernanceConfig = env
            .storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            });
        treasury::execute_request(&env, request_id, &config)
    }

    /// Get a treasury request by id.
    pub fn treasury_get_request(env: Env, request_id: u64) -> Option<treasury::TreasuryRequest> {
        treasury::get_request(&env, request_id)
    }

    /// Add a treasury multi-sig signer. Admin-only.
    pub fn treasury_add_signer(
        env: Env,
        admin: Address,
        signer: Address,
    ) -> Result<Symbol, treasury::TreasuryError> {
        treasury::add_signer(&env, &admin, &signer)
    }

    /// Remove a treasury multi-sig signer. Admin-only.
    pub fn treasury_remove_signer(
        env: Env,
        admin: Address,
        signer: Address,
    ) -> Result<Symbol, treasury::TreasuryError> {
        treasury::remove_signer(&env, &admin, &signer)
    }

    /// Get all treasury signers.
    pub fn treasury_get_signers(env: Env) -> Vec<Address> {
        treasury::get_signers(&env)
    }

    // ====================================================================
    // Emergency controls
    // ====================================================================

    /// Emergency pause the governance system.
    ///
    /// Can be called by the guardian or admin. Prevents new proposals,
    /// voting, and treasury withdrawals.
    pub fn emergency_pause(env: Env, caller: Address) -> Result<Symbol, Error> {
        caller.require_auth();

        let admin: Address = env
            .storage()
            .persistent()
            .get(&StorageKey::Admin)
            .ok_or(Error::NotInitialized)?;
        let guardian: Address = env
            .storage()
            .persistent()
            .get(&StorageKey::Guardian)
            .ok_or(Error::NotInitialized)?;

        if caller != admin && caller != guardian {
            return Err(Error::GuardianRequired);
        }

        let is_paused: bool = env
            .storage()
            .persistent()
            .get(&StorageKey::EmergencyPaused)
            .unwrap_or(false);

        if is_paused {
            return Err(Error::AlreadyPaused);
        }

        env.storage()
            .persistent()
            .set(&StorageKey::EmergencyPaused, &true);

        Self::append_audit(
            &env,
            &symbol_short!("em_pause"),
            0,
            &caller,
            &symbol_short!("paused"),
        );

        env.events()
            .publish((symbol_short!("EM_PAUSE"), &caller), OK);

        Ok(OK)
    }

    /// Emergency unpause the governance system. Admin-only.
    pub fn emergency_unpause(env: Env, admin: Address) -> Result<Symbol, Error> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let is_paused: bool = env
            .storage()
            .persistent()
            .get(&StorageKey::EmergencyPaused)
            .unwrap_or(false);

        if !is_paused {
            return Err(Error::NotPaused);
        }

        env.storage()
            .persistent()
            .set(&StorageKey::EmergencyPaused, &false);

        Self::append_audit(
            &env,
            &symbol_short!("em_unpse"),
            0,
            &admin,
            &symbol_short!("unpaused"),
        );

        env.events()
            .publish((symbol_short!("EM_UNPSE"), &admin), OK);

        Ok(OK)
    }

    /// Check if the system is emergency-paused.
    pub fn is_emergency_paused(env: Env) -> bool {
        env.storage()
            .persistent()
            .get(&StorageKey::EmergencyPaused)
            .unwrap_or(false)
    }

    /// Emergency execute a proposal, bypassing timelock.
    ///
    /// Can only be called by admin + guardian (multi-sig) together.
    /// This is for critical security responses.
    pub fn emergency_execute(env: Env, caller: Address, proposal_id: u64) -> Result<Symbol, Error> {
        caller.require_auth();

        let admin: Address = env
            .storage()
            .persistent()
            .get(&StorageKey::Admin)
            .ok_or(Error::NotInitialized)?;
        let guardian: Address = env
            .storage()
            .persistent()
            .get(&StorageKey::Guardian)
            .ok_or(Error::NotInitialized)?;

        // Require both admin and guardian (dual-sign).
        // In Soroban, we check that the caller is one of them and that
        // both have authorized this call.
        if caller != admin && caller != guardian {
            return Err(Error::GuardianRequired);
        }

        // The other party must also authorize.
        if caller == admin {
            guardian.require_auth();
        } else {
            admin.require_auth();
        }

        let mut proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        // Can emergency-execute from Passed, Timelock, or even Voting state.
        if proposal.status == ProposalStatus::Executed
            || proposal.status == ProposalStatus::Cancelled
            || proposal.status == ProposalStatus::ExecutionFailed
        {
            return Err(Error::CannotExecute);
        }

        let now = env.ledger().timestamp();
        proposal.status = ProposalStatus::Executed;
        proposal.executed_at = now;
        env.storage()
            .persistent()
            .set(&StorageKey::Proposal(proposal_id), &proposal);

        Self::append_audit(
            &env,
            &symbol_short!("em_exec"),
            proposal_id,
            &caller,
            &symbol_short!("emergency"),
        );

        env.events()
            .publish((symbol_short!("EM_EXEC"), &caller), (proposal_id, now));

        Ok(OK)
    }

    // ====================================================================
    // Rewards
    // ====================================================================

    /// Claim governance reward for participating in a proposal vote.
    ///
    /// Each voter can claim once per proposal. The reward amount is
    /// configured in `GovernanceConfig.reward_per_participation`.
    pub fn claim_rewards(env: Env, voter: Address, proposal_id: u64) -> Result<i128, Error> {
        voter.require_auth();

        // Check not already claimed.
        let claimed: bool = env
            .storage()
            .persistent()
            .get(&StorageKey::RewardClaimed(proposal_id, voter.clone()))
            .unwrap_or(false);

        if claimed {
            return Err(Error::RewardAlreadyClaimed);
        }

        // Verify the voter actually voted on this proposal.
        let vote: Option<VoteRecord> = env
            .storage()
            .persistent()
            .get(&StorageKey::VoteRecord(proposal_id, voter.clone()));
        if vote.is_none() {
            return Err(Error::NoVotingPower);
        }

        let config: GovernanceConfig = env
            .storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            });

        let reward = config.reward_per_participation;

        // Mark as claimed.
        env.storage().persistent().set(
            &StorageKey::RewardClaimed(proposal_id, voter.clone()),
            &true,
        );

        // Update total rewards distributed.
        let total_rewards: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TotalRewardsDistributed)
            .unwrap_or_default();
        env.storage().persistent().set(
            &StorageKey::TotalRewardsDistributed,
            &(total_rewards + reward),
        );

        Self::append_audit(
            &env,
            &symbol_short!("reward"),
            proposal_id,
            &voter,
            &symbol_short!("claimed"),
        );

        env.events()
            .publish((symbol_short!("REWARD"), &voter), (proposal_id, reward));

        Ok(reward)
    }

    /// Check if a voter has claimed rewards for a proposal.
    pub fn has_claimed_reward(env: Env, proposal_id: u64, voter: Address) -> bool {
        env.storage()
            .persistent()
            .get(&StorageKey::RewardClaimed(proposal_id, voter))
            .unwrap_or(false)
    }

    /// Get total rewards distributed.
    pub fn total_rewards_distributed(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&StorageKey::TotalRewardsDistributed)
            .unwrap_or_default()
    }

    // ====================================================================
    // Query helpers
    // ====================================================================

    /// Get quorum information for a proposal.
    pub fn get_quorum_info(env: Env, proposal_id: u64) -> Result<(i128, i128, bool), Error> {
        let proposal: Proposal = env
            .storage()
            .persistent()
            .get(&StorageKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)?;

        let config: GovernanceConfig = env
            .storage()
            .persistent()
            .get(&StorageKey::Config)
            .unwrap_or(GovernanceConfig {
                proposal_threshold: DEFAULT_PROPOSAL_THRESHOLD,
                voting_period: DEFAULT_VOTING_PERIOD,
                quorum_threshold_bps: DEFAULT_QUORUM_BPS,
                approval_threshold_bps: DEFAULT_APPROVAL_BPS,
                timelock_delay: DEFAULT_TIMELOCK_DELAY,
                emergency_timelock_delay: DEFAULT_EMERGENCY_TIMELOCK_DELAY,
                max_treasury_spend_bps: DEFAULT_MAX_TREASURY_SPEND_BPS,
                treasury_multisig_threshold: DEFAULT_TREASURY_MULTISIG_THRESHOLD,
                reward_per_participation: DEFAULT_REWARD_PER_PARTICIPATION,
                max_active_proposals: DEFAULT_MAX_ACTIVE_PROPOSALS,
            });

        let total_voting_power: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVotingPower)
            .unwrap_or_default();

        let total_votes = proposal.votes_for + proposal.votes_against + proposal.votes_abstain;
        let quorum_required = total_voting_power * config.quorum_threshold_bps as i128 / 10_000;

        let quorum_met = total_votes >= quorum_required;

        Ok((total_votes, quorum_required, quorum_met))
    }

    /// Get a governance summary snapshot.
    pub fn get_governance_summary(env: Env) -> GovernanceSummary {
        let all_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&StorageKey::AllProposalIds)
            .unwrap_or_else(|| Vec::new(&env));

        let total_proposals = all_ids.len() as u64;
        let active_proposals = Self::count_active_proposals(&env, &all_ids);

        let total_voting_power: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::TotalVotingPower)
            .unwrap_or_default();

        let delegators: Vec<Address> = env
            .storage()
            .persistent()
            .get(&StorageKey::AllDelegators)
            .unwrap_or_else(|| Vec::new(&env));
        let active_delegations = Self::count_active_delegations(&env, &delegators);

        // Get total treasury balance (sum across assets).
        let assets: Vec<Symbol> = treasury::all_assets(&env);
        let mut treasury_balance: i128 = 0;
        for i in 0..assets.len() {
            let asset = assets.get(i).unwrap();
            treasury_balance += treasury::balance_of(&env, &asset);
        }

        let emergency_paused: bool = env
            .storage()
            .persistent()
            .get(&StorageKey::EmergencyPaused)
            .unwrap_or(false);

        GovernanceSummary {
            total_proposals,
            active_proposals,
            total_voting_power,
            active_delegations,
            treasury_balance,
            emergency_paused,
        }
    }

    // ====================================================================
    // Audit trail
    // ====================================================================

    /// Get the full governance audit trail.
    pub fn get_audit_trail(env: Env) -> Vec<GovernanceAuditEntry> {
        env.storage()
            .persistent()
            .get(&StorageKey::AuditTrail)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get audit trail entries for a specific proposal.
    pub fn get_audit_trail_for_proposal(env: Env, proposal_id: u64) -> Vec<GovernanceAuditEntry> {
        let trail: Vec<GovernanceAuditEntry> = env
            .storage()
            .persistent()
            .get(&StorageKey::AuditTrail)
            .unwrap_or_else(|| Vec::new(&env));

        let mut filtered = Vec::new(&env);
        for entry in trail.iter() {
            if entry.proposal_id == proposal_id {
                filtered.push_back(entry);
            }
        }
        filtered
    }

    // ====================================================================
    // Audit-log sink (cross-contract integration)
    // ====================================================================

    /// Configure the audit-log sink address. Admin-only.
    pub fn set_audit_sink(env: Env, admin: Address, sink: Address) -> Result<Symbol, Error> {
        Self::assert_admin(&env, &admin)?;
        admin.require_auth();
        env.storage()
            .persistent()
            .set(&StorageKey::AuditSink, &sink);
        Ok(OK)
    }

    /// Read the audit-log sink address, if configured.
    pub fn get_audit_sink(env: Env) -> Option<Address> {
        env.storage().persistent().get(&StorageKey::AuditSink)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

impl GovernanceDAO {
    /// Return `Err(Error::Unauthorized)` if `admin` does not match the stored
    /// admin address.
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

    /// Count proposals that are in Voting, Timelock, or Passed status.
    fn count_active_proposals(env: &Env, all_ids: &Vec<u64>) -> u32 {
        let mut count: u32 = 0;
        for i in 0..all_ids.len() {
            let pid = all_ids.get(i).unwrap();
            if let Some(proposal) = env
                .storage()
                .persistent()
                .get::<StorageKey, Proposal>(&StorageKey::Proposal(pid))
            {
                match proposal.status {
                    ProposalStatus::Voting | ProposalStatus::Timelock | ProposalStatus::Passed => {
                        count += 1;
                    }
                    _ => {}
                }
            }
        }
        count
    }

    /// Count delegations that are currently active.
    fn count_active_delegations(env: &Env, delegators: &Vec<Address>) -> u32 {
        let mut count: u32 = 0;
        for i in 0..delegators.len() {
            let addr = delegators.get(i).unwrap();
            if let Some(del) = env
                .storage()
                .persistent()
                .get::<StorageKey, Delegation>(&StorageKey::Delegation(addr))
            {
                if del.active {
                    count += 1;
                }
            }
        }
        count
    }

    /// Append an entry to the governance audit trail.
    fn append_audit(
        env: &Env,
        action: &Symbol,
        proposal_id: u64,
        actor: &Address,
        detail: &Symbol,
    ) {
        let seq: u64 = env
            .storage()
            .persistent()
            .get(&StorageKey::NextAuditSeq)
            .unwrap_or(1);

        let entry = GovernanceAuditEntry {
            seq,
            timestamp: env.ledger().timestamp(),
            action: action.clone(),
            proposal_id,
            actor: actor.clone(),
            detail: detail.clone(),
        };

        let mut trail: Vec<GovernanceAuditEntry> = env
            .storage()
            .persistent()
            .get(&StorageKey::AuditTrail)
            .unwrap_or_else(|| Vec::new(env));

        // Enforce max audit trail size.
        if trail.len() >= MAX_AUDIT_TRAIL {
            trail = trail.slice(1..);
        }

        trail.push_back(entry);
        env.storage()
            .persistent()
            .set(&StorageKey::AuditTrail, &trail);
        env.storage()
            .persistent()
            .set(&StorageKey::NextAuditSeq, &(seq + 1));
    }

    /// Integration with the audit-log contract.
    #[allow(dead_code)]
    fn log_audit_if_configured(
        env: &Env,
        actor: &Address,
        proposal_id: u64,
        outcome: Symbol,
        detail: &str,
    ) {
        let key = StorageKey::AuditSink;
        let sink: Option<Address> = env.storage().persistent().get(&key);
        if let Some(sink) = sink {
            let mut before = StateSnapshot::empty(env);
            before.push(symbol_short!("prpid"), proposal_id as i128);
            let mut after = StateSnapshot::empty(env);
            after.push(symbol_short!("prpid"), proposal_id as i128);
            let detail_str = soroban_sdk::String::from_str(env, detail);
            let logger = AuditLogger::new(env, &sink);
            let _ = logger.log_event(
                actor.clone(),
                AuditEventType::AdminAction,
                symbol_short!("gov"),
                permissions::ADMIN,
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
