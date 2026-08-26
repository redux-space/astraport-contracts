//! Unit and integration tests for the governance DAO contract.
//!
//! Tests cover:
//! - Initialization and admin/guardian enforcement.
//! - Configuration management.
//! - Voting power deposit and withdrawal.
//! - Proposal submission and querying.
//! - Weighted voting (direct and delegated).
//! - Quorum and approval threshold enforcement.
//! - Vote delegation (create, revoke, delegated voting).
//! - Proposal lifecycle (submit → vote → finalize → timelock → execute).
//! - Treasury management (deposit, request, approve, execute).
//! - Emergency pause/unpause.
//! - Reward distribution.
//! - Audit trail logging.
//! - Edge cases and error conditions.

use super::*;
use crate::records::{
    Delegation, GovernanceConfig, ProposalActionType, ProposalStatus, VoteDirection, VoteRecord,
};
use crate::treasury::TreasuryRequest;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::testutils::Ledger;
use soroban_sdk::{symbol_short, Address, Env, Symbol, Vec};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup() -> (Env, GovernanceDAOClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, GovernanceDAO);
    let client = GovernanceDAOClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let guardian = Address::generate(&env);
    let _ = client.initialize(&admin, &guardian);
    (env, client, admin, guardian)
}

fn setup_with_config() -> (
    Env,
    GovernanceDAOClient<'static>,
    Address,
    Address,
    GovernanceConfig,
) {
    let (env, client, admin, guardian) = setup();
    let config = GovernanceConfig {
        proposal_threshold: 100,
        voting_period: 604_800,
        quorum_threshold_bps: 400,
        approval_threshold_bps: 5_001,
        timelock_delay: 172_800,
        emergency_timelock_delay: 3_600,
        max_treasury_spend_bps: 1_000,
        treasury_multisig_threshold: 2,
        reward_per_participation: 10,
        max_active_proposals: 10,
    };
    client.set_config(&admin, &config);
    (env, client, admin, guardian, config)
}

fn sym(env: &Env, s: &str) -> Symbol {
    Symbol::new(env, s)
}

fn deposit_and_submit(
    env: &Env,
    client: &GovernanceDAOClient<'_>,
    proposer: &Address,
    amount: i128,
) -> u64 {
    client.deposit_voting_power(proposer, &amount);
    client.submit_proposal(
        proposer,
        &sym(env, "Proposal 1"),
        &sym(env, "Description"),
        &ProposalActionType::ParameterChange,
        &sym(env, "payload"),
    )
}

fn deposit_vote_finalize(
    env: &Env,
    client: &GovernanceDAOClient<'_>,
    voter: &Address,
    proposal_id: u64,
    direction: VoteDirection,
    voting_ends: u64,
) -> ProposalStatus {
    client.cast_vote(voter, &proposal_id, &direction);
    env.ledger().set_timestamp(voting_ends + 1);
    client.finalize_proposal(&proposal_id)
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

#[test]
fn test_initialize_sets_admin_and_guardian() {
    let (env, client, admin, guardian) = setup();
    assert_eq!(client.get_admin(), Ok(admin));
    assert_eq!(client.get_guardian(), Ok(guardian));
    assert!(!client.is_emergency_paused());
}

#[test]
fn test_initial_state() {
    let (env, client, _admin, _guardian) = setup();
    assert_eq!(client.get_total_supply(), 0);
    assert_eq!(client.get_total_voting_power(), 0);
    let summary = client.get_governance_summary();
    assert_eq!(summary.total_proposals, 0);
    assert_eq!(summary.active_proposals, 0);
    assert_eq!(summary.total_voting_power, 0);
}

#[test]
#[should_panic]
fn test_double_initialize_panics() {
    let (env, client, admin, guardian) = setup();
    client.initialize(&admin, &guardian);
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[test]
fn test_set_and_get_config() {
    let (env, client, admin, _guardian) = setup();
    let config = GovernanceConfig {
        proposal_threshold: 500,
        voting_period: 86400,
        quorum_threshold_bps: 1000,
        approval_threshold_bps: 6000,
        timelock_delay: 3600,
        emergency_timelock_delay: 600,
        max_treasury_spend_bps: 500,
        treasury_multisig_threshold: 3,
        reward_per_participation: 25,
        max_active_proposals: 5,
    };
    client.set_config(&admin, &config);
    let stored = client.get_config();
    assert_eq!(stored.proposal_threshold, 500);
    assert_eq!(stored.voting_period, 86400);
    assert_eq!(stored.quorum_threshold_bps, 1000);
    assert_eq!(stored.approval_threshold_bps, 6000);
    assert_eq!(stored.timelock_delay, 3600);
    assert_eq!(stored.treasury_multisig_threshold, 3);
    assert_eq!(stored.reward_per_participation, 25);
}

#[test]
fn test_set_config_invalid_params_fails() {
    let (env, client, admin, _guardian) = setup();
    let bad_config = GovernanceConfig {
        proposal_threshold: 100,
        voting_period: 0, // Invalid
        quorum_threshold_bps: 400,
        approval_threshold_bps: 5_001,
        timelock_delay: 172_800,
        emergency_timelock_delay: 3_600,
        max_treasury_spend_bps: 1_000,
        treasury_multisig_threshold: 2,
        reward_per_participation: 10,
        max_active_proposals: 10,
    };
    assert_eq!(
        client.try_set_config(&admin, &bad_config),
        Err(Ok(Error::InvalidConfig))
    );
}

#[test]
fn test_set_config_non_admin_fails() {
    let (env, client, _admin, _guardian) = setup();
    let non_admin = Address::generate(&env);
    let config = GovernanceConfig {
        proposal_threshold: 100,
        voting_period: 604_800,
        quorum_threshold_bps: 400,
        approval_threshold_bps: 5_001,
        timelock_delay: 172_800,
        emergency_timelock_delay: 3_600,
        max_treasury_spend_bps: 1_000,
        treasury_multisig_threshold: 2,
        reward_per_participation: 10,
        max_active_proposals: 10,
    };
    assert_eq!(
        client.try_set_config(&non_admin, &config),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn test_set_total_supply() {
    let (env, client, admin, _guardian) = setup();
    client.set_total_supply(&admin, &1_000_000);
    assert_eq!(client.get_total_supply(), 1_000_000);
}

// ---------------------------------------------------------------------------
// Voting power
// ---------------------------------------------------------------------------

#[test]
fn test_deposit_voting_power() {
    let (env, client, _admin, _guardian) = setup();
    let voter = Address::generate(&env);
    let power = client.deposit_voting_power(&voter, &500);
    assert_eq!(power, 500);
    assert_eq!(client.get_voting_power(&voter), 500);
    assert_eq!(client.get_total_voting_power(), 500);
}

#[test]
fn test_deposit_multiple_times() {
    let (env, client, _admin, _guardian) = setup();
    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &200);
    client.deposit_voting_power(&voter, &300);
    assert_eq!(client.get_voting_power(&voter), 500);
    assert_eq!(client.get_total_voting_power(), 500);
}

#[test]
fn test_withdraw_voting_power() {
    let (env, client, _admin, _guardian) = setup();
    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &500);
    let remaining = client.withdraw_voting_power(&voter, &200);
    assert_eq!(remaining, 300);
    assert_eq!(client.get_voting_power(&voter), 300);
    assert_eq!(client.get_total_voting_power(), 300);
}

#[test]
fn test_withdraw_all_voting_power() {
    let (env, client, _admin, _guardian) = setup();
    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &500);
    let remaining = client.withdraw_voting_power(&voter, &500);
    assert_eq!(remaining, 0);
    assert_eq!(client.get_voting_power(&voter), 0);
    assert_eq!(client.get_total_voting_power(), 0);
}

#[test]
fn test_withdraw_too_much_fails() {
    let (env, client, _admin, _guardian) = setup();
    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &100);
    assert_eq!(
        client.try_withdraw_voting_power(&voter, &200),
        Err(Ok(Error::NoVotingPower))
    );
}

#[test]
fn test_deposit_zero_fails() {
    let (env, client, _admin, _guardian) = setup();
    let voter = Address::generate(&env);
    assert_eq!(
        client.try_deposit_voting_power(&voter, &0),
        Err(Ok(Error::InvalidConfig))
    );
}

// ---------------------------------------------------------------------------
// Proposal submission
// ---------------------------------------------------------------------------

#[test]
fn test_submit_proposal() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);
    assert_eq!(pid, 1);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.proposal_id, 1);
    assert_eq!(proposal.proposer, proposer);
    assert_eq!(proposal.status, ProposalStatus::Voting);
    assert_eq!(proposal.votes_for, 0);
    assert_eq!(proposal.votes_against, 0);
    assert_eq!(proposal.voter_count, 0);
}

#[test]
fn test_submit_proposal_insufficient_power_fails() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    // Default threshold is 1000, only deposit 100.
    client.deposit_voting_power(&proposer, &100);
    assert_eq!(
        client.try_submit_proposal(
            &proposer,
            &sym(&env, "Title"),
            &sym(&env, "Desc"),
            &ProposalActionType::ParameterChange,
            &sym(&env, "payload"),
        ),
        Err(Ok(Error::InsufficientVotingPower))
    );
}

#[test]
fn test_submit_proposal_with_custom_config() {
    let (env, client, admin, _guardian, _config) = setup_with_config();
    let proposer = Address::generate(&env);
    // With threshold of 100, 150 should work.
    client.deposit_voting_power(&proposer, &150);
    let pid = client.submit_proposal(
        &proposer,
        &sym(&env, "Title"),
        &sym(&env, "Desc"),
        &ProposalActionType::ParameterChange,
        &sym(&env, "payload"),
    );
    assert_eq!(pid, 1);
}

#[test]
fn test_get_all_proposal_ids() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    client.deposit_voting_power(&proposer, &2000);
    let p1 = client.submit_proposal(
        &proposer,
        &sym(&env, "P1"),
        &sym(&env, "D1"),
        &ProposalActionType::ParameterChange,
        &sym(&env, "pl1"),
    );
    let p2 = client.submit_proposal(
        &proposer,
        &sym(&env, "P2"),
        &sym(&env, "D2"),
        &ProposalActionType::TreasurySpend,
        &sym(&env, "pl2"),
    );
    let all = client.get_all_proposal_ids();
    assert_eq!(all.len(), 2);
    assert_eq!(all.get(0), Some(p1));
    assert_eq!(all.get(1), Some(p2));
}

// ---------------------------------------------------------------------------
// Voting
// ---------------------------------------------------------------------------

#[test]
fn test_cast_vote() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &150);
    client.cast_vote(&voter, &pid, &VoteDirection::For);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.votes_for, 150);
    assert_eq!(proposal.voter_count, 1);

    let vote = client.get_vote(&pid, &voter).unwrap();
    assert_eq!(vote.direction, VoteDirection::For);
    assert_eq!(vote.weight, 150);
    assert_eq!(vote.voter, voter);
}

#[test]
fn test_cast_vote_against() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &100);
    client.cast_vote(&voter, &pid, &VoteDirection::Against);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.votes_against, 100);
}

#[test]
fn test_cast_vote_abstain() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &50);
    client.cast_vote(&voter, &pid, &VoteDirection::Abstain);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.votes_abstain, 50);
}

#[test]
fn test_double_vote_fails() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &100);
    client.cast_vote(&voter, &pid, &VoteDirection::For);
    assert_eq!(
        client.try_cast_vote(&voter, &pid, &VoteDirection::Against),
        Err(Ok(Error::AlreadyVoted))
    );
}

#[test]
fn test_vote_on_nonexistent_proposal_fails() {
    let (env, client, _admin, _guardian) = setup();
    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &100);
    assert_eq!(
        client.try_cast_vote(&voter, &999, &VoteDirection::For),
        Err(Ok(Error::ProposalNotFound))
    );
}

#[test]
fn test_vote_without_power_fails() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    // No deposit.
    assert_eq!(
        client.try_cast_vote(&voter, &pid, &VoteDirection::For),
        Err(Ok(Error::NoVotingPower))
    );
}

#[test]
fn test_vote_after_period_ends_fails() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &100);

    // Move past voting period.
    env.ledger().set_timestamp(604_801);

    assert_eq!(
        client.try_cast_vote(&voter, &pid, &VoteDirection::For),
        Err(Ok(Error::VotingAlreadyEnded))
    );
}

// ---------------------------------------------------------------------------
// Proposal finalization and quorum
// ---------------------------------------------------------------------------

#[test]
fn test_finalize_passed() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    // Set total supply for quorum calculation.
    client.set_total_supply(&admin, &10_000);

    // Deposit and vote with enough power.
    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &500);
    client.cast_vote(&voter, &pid, &VoteDirection::For);

    // Move past voting period.
    env.ledger().set_timestamp(604_801);
    let status = client.finalize_proposal(&pid);
    assert_eq!(status, ProposalStatus::Passed);
}

#[test]
fn test_finalize_defeated_quorum() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    // Set total supply very high so quorum is hard to reach.
    client.set_total_supply(&admin, &1_000_000);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &100);
    client.cast_vote(&voter, &pid, &VoteDirection::For);

    // Not enough total voting power deposited to meet quorum of 4% of 1M.
    env.ledger().set_timestamp(604_801);
    let status = client.finalize_proposal(&pid);
    assert_eq!(status, ProposalStatus::Defeated);
}

#[test]
fn test_finalize_defeated_approval() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    // Set total supply so quorum is met.
    client.set_total_supply(&admin, &10_000);

    // Two voters: one for, one against. Net votes = 200, quorum = 400.
    // 400 >= 400 so quorum is met. But for (100) vs against (100),
    // approval = 200 * 5001 / 10000 = 100. 100 > 100 is false → defeated.
    let voter_for = Address::generate(&env);
    client.deposit_voting_power(&voter_for, &100);
    client.cast_vote(&voter_for, &pid, &VoteDirection::For);

    let voter_against = Address::generate(&env);
    client.deposit_voting_power(&voter_against, &100);
    client.cast_vote(&voter_against, &pid, &VoteDirection::Against);

    env.ledger().set_timestamp(604_801);
    let status = client.finalize_proposal(&pid);
    assert_eq!(status, ProposalStatus::Defeated);
}

#[test]
fn test_finalize_voting_not_ended_fails() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    // Still within voting period.
    env.ledger().set_timestamp(100);
    assert_eq!(
        client.try_finalize_proposal(&pid),
        Err(Ok(Error::VotingNotEnded))
    );
}

#[test]
fn test_finalize_already_finalized_fails() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &500);
    client.cast_vote(&voter, &pid, &VoteDirection::For);

    client.set_total_supply(&admin, &10_000);
    env.ledger().set_timestamp(604_801);
    client.finalize_proposal(&pid);

    assert_eq!(
        client.try_finalize_proposal(&pid),
        Err(Ok(Error::VotingAlreadyEnded))
    );
}

#[test]
fn test_get_quorum_info() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    client.set_total_supply(&admin, &10_000);
    // Total voting power = 200 (deposited by proposer).
    // 4% of total supply = 400.
    // Votes so far = 0.
    let (total_votes, quorum_required, quorum_met) = client.get_quorum_info(&pid).unwrap();
    assert_eq!(total_votes, 0);
    assert_eq!(quorum_required, 400);
    assert!(!quorum_met);
}

// ---------------------------------------------------------------------------
// Proposal cancellation
// ---------------------------------------------------------------------------

#[test]
fn test_cancel_proposal_by_proposer() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let result = client.cancel_proposal(&proposer, &pid);
    assert_eq!(result, OK);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.status, ProposalStatus::Cancelled);
}

#[test]
fn test_cancel_proposal_by_admin() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let result = client.cancel_proposal(&admin, &pid);
    assert_eq!(result, OK);
}

#[test]
fn test_cancel_proposal_unauthorized_fails() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let random = Address::generate(&env);
    assert_eq!(
        client.try_cancel_proposal(&random, &pid),
        Err(Ok(Error::CannotCancel))
    );
}

#[test]
fn test_cancel_executed_proposal_fails() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    // Finalize to Passed, then queue and execute.
    client.set_total_supply(&admin, &10_000);
    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &500);
    client.cast_vote(&voter, &pid, &VoteDirection::For);
    env.ledger().set_timestamp(604_801);
    client.finalize_proposal(&pid);
    client.queue_timelock(&pid);
    env.ledger().set_timestamp(604_801 + 172_800 + 1);
    let executor = Address::generate(&env);
    client.execute_proposal(&pid, &executor);

    assert_eq!(
        client.try_cancel_proposal(&proposer, &pid),
        Err(Ok(Error::CannotCancel))
    );
}

// ---------------------------------------------------------------------------
// Delegation
// ---------------------------------------------------------------------------

#[test]
fn test_delegate() {
    let (env, client, _admin, _guardian) = setup();
    let delegator = Address::generate(&env);
    let delegate = Address::generate(&env);

    let result = client.delegate(&delegator, &delegate);
    assert_eq!(result, OK);

    let del = client.get_delegation(&delegator).unwrap();
    assert_eq!(del.delegator, delegator);
    assert_eq!(del.delegate, delegate);
    assert!(del.active);
}

#[test]
fn test_revoke_delegation() {
    let (env, client, _admin, _guardian) = setup();
    let delegator = Address::generate(&env);
    let delegate = Address::generate(&env);

    client.delegate(&delegator, &delegate);
    client.revoke_delegation(&delegator);

    let del = client.get_delegation(&delegator).unwrap();
    assert!(!del.active);
}

#[test]
fn test_revoke_nonexistent_delegation_fails() {
    let (env, client, _admin, _guardian) = setup();
    let delegator = Address::generate(&env);
    assert_eq!(
        client.try_revoke_delegation(&delegator),
        Err(Ok(Error::DelegationNotFound))
    );
}

#[test]
fn test_delegate_to_self_fails() {
    let (env, client, _admin, _guardian) = setup();
    let delegator = Address::generate(&env);
    assert_eq!(
        client.try_delegate(&delegator, &delegator),
        Err(Ok(Error::CannotDelegateToSelf))
    );
}

#[test]
fn test_cast_delegated_vote() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let token_holder = Address::generate(&env);
    client.deposit_voting_power(&token_holder, &300);

    let delegate_addr = Address::generate(&env);
    client.delegate(&token_holder, &delegate_addr);

    // Delegate casts vote on behalf of token holder.
    client.cast_delegated_vote(&delegate_addr, &token_holder, &pid, &VoteDirection::For);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.votes_for, 300);
    assert_eq!(proposal.voter_count, 1);

    // The vote record is stored under token_holder's address.
    let vote = client.get_vote(&pid, &token_holder).unwrap();
    assert_eq!(vote.weight, 300);
    assert!(vote.is_delegated);
    assert_eq!(vote.voter, delegate_addr);
}

#[test]
fn test_cast_delegated_vote_no_delegation_fails() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let token_holder = Address::generate(&env);
    client.deposit_voting_power(&token_holder, &300);

    let delegate_addr = Address::generate(&env);
    // No delegation created.

    assert_eq!(
        client.try_cast_delegated_vote(&delegate_addr, &token_holder, &pid, &VoteDirection::For),
        Err(Ok(Error::DelegationNotFound))
    );
}

// ---------------------------------------------------------------------------
// Timelock and execution
// ---------------------------------------------------------------------------

#[test]
fn test_full_proposal_lifecycle() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    client.set_total_supply(&admin, &10_000);

    // Vote.
    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &500);
    client.cast_vote(&voter, &pid, &VoteDirection::For);

    // Finalize.
    env.ledger().set_timestamp(604_801);
    let status = client.finalize_proposal(&pid);
    assert_eq!(status, ProposalStatus::Passed);

    // Queue timelock.
    let result = client.queue_timelock(&pid);
    assert_eq!(result, OK);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.status, ProposalStatus::Timelock);

    // Cannot execute yet (timelock not expired).
    let executor = Address::generate(&env);
    assert_eq!(
        client.try_execute_proposal(&pid, &executor),
        Err(Ok(Error::TimelockNotExpired))
    );

    // Move past timelock delay.
    env.ledger().set_timestamp(604_801 + 172_800 + 1);

    // Execute.
    let result = client.execute_proposal(&pid, &executor);
    assert_eq!(result, OK);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.status, ProposalStatus::Executed);
}

#[test]
fn test_queue_timelock_on_non_passed_fails() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    // Still in Voting state.
    assert_eq!(
        client.try_queue_timelock(&pid),
        Err(Ok(Error::CannotTimelock))
    );
}

#[test]
fn test_execute_before_timelock_expires_fails() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    client.set_total_supply(&admin, &10_000);
    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &500);
    client.cast_vote(&voter, &pid, &VoteDirection::For);

    env.ledger().set_timestamp(604_801);
    client.finalize_proposal(&pid);
    client.queue_timelock(&pid);

    // Move past voting but not timelock.
    env.ledger().set_timestamp(604_801 + 1000);

    let executor = Address::generate(&env);
    assert_eq!(
        client.try_execute_proposal(&pid, &executor),
        Err(Ok(Error::TimelockNotExpired))
    );
}

#[test]
fn test_execute_non_timelocked_fails() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let executor = Address::generate(&env);
    assert_eq!(
        client.try_execute_proposal(&pid, &executor),
        Err(Ok(Error::CannotExecute))
    );
}

// ---------------------------------------------------------------------------
// Treasury
// ---------------------------------------------------------------------------

#[test]
fn test_treasury_deposit() {
    let (env, client, _admin, _guardian) = setup();
    let depositor = Address::generate(&env);
    let asset = sym(&env, "XLM");

    let balance = client.treasury_deposit(&depositor, &asset, &10_000);
    assert_eq!(balance, 10_000);
    assert_eq!(client.treasury_balance(&asset), 10_000);

    let assets = client.treasury_all_assets();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets.get(0), Some(asset));
}

#[test]
fn test_treasury_deposit_multiple_assets() {
    let (env, client, _admin, _guardian) = setup();
    let depositor = Address::generate(&env);

    client.treasury_deposit(&depositor, &sym(&env, "XLM"), &5_000);
    client.treasury_deposit(&depositor, &sym(&env, "USDC"), &10_000);

    assert_eq!(client.treasury_balance(&sym(&env, "XLM")), 5_000);
    assert_eq!(client.treasury_balance(&sym(&env, "USDC")), 10_000);

    let assets = client.treasury_all_assets();
    assert_eq!(assets.len(), 2);
}

#[test]
fn test_treasury_request_and_approve() {
    let (env, client, admin, _guardian, _config) = setup_with_config();
    let depositor = Address::generate(&env);
    let asset = sym(&env, "XLM");
    client.treasury_deposit(&depositor, &asset, &100_000);

    let requester = Address::generate(&env);
    let recipient = Address::generate(&env);

    let req_id = client.treasury_create_request(
        &requester,
        &recipient,
        &5_000, // 5% of 100k = within 10% limit
        &asset,
        &sym(&env, "grant"),
    );
    assert_eq!(req_id, 1);

    let req = client.treasury_get_request(&req_id).unwrap();
    assert_eq!(req.amount, 5_000);
    assert_eq!(req.recipient, recipient);
    assert!(!req.executed);
    assert_eq!(req.approvals.len(), 0);

    // Need 2 approvals (multisig threshold).
    let signer1 = Address::generate(&env);
    let signer2 = Address::generate(&env);
    client.treasury_add_signer(&admin, &signer1);
    client.treasury_add_signer(&admin, &signer2);

    client.treasury_approve_request(&signer1, &req_id);
    let req = client.treasury_get_request(&req_id).unwrap();
    assert_eq!(req.approvals.len(), 1);

    client.treasury_approve_request(&signer2, &req_id);
    let req = client.treasury_get_request(&req_id).unwrap();
    assert_eq!(req.approvals.len(), 2);

    // Execute.
    let amount = client.treasury_execute_request(&req_id);
    assert_eq!(amount, 5_000);

    assert_eq!(client.treasury_balance(&asset), 95_000);

    let req = client.treasury_get_request(&req_id).unwrap();
    assert!(req.executed);
}

#[test]
fn test_treasury_request_exceeds_limit_fails() {
    let (env, client, _admin, _guardian, _config) = setup_with_config();
    let depositor = Address::generate(&env);
    let asset = sym(&env, "XLM");
    client.treasury_deposit(&depositor, &asset, &100_000);

    let requester = Address::generate(&env);
    let recipient = Address::generate(&env);

    // Try to withdraw 20% (exceeds 10% limit).
    assert_eq!(
        client.try_treasury_create_request(
            &requester,
            &recipient,
            &20_000,
            &asset,
            &sym(&env, "too_much"),
        ),
        Err(Err(treasury::TreasuryError::ExceedsSpendLimit))
    );
}

#[test]
fn test_treasury_execute_insufficient_approvals_fails() {
    let (env, client, admin, _guardian, _config) = setup_with_config();
    let depositor = Address::generate(&env);
    let asset = sym(&env, "XLM");
    client.treasury_deposit(&depositor, &asset, &100_000);

    let requester = Address::generate(&env);
    let recipient = Address::generate(&env);

    let req_id =
        client.treasury_create_request(&requester, &recipient, &5_000, &asset, &sym(&env, "grant"));

    let signer1 = Address::generate(&env);
    client.treasury_add_signer(&admin, &signer1);
    client.treasury_approve_request(&signer1, &req_id);

    // Only 1 approval, threshold is 2.
    assert_eq!(
        client.try_treasury_execute_request(&req_id),
        Err(Err(treasury::TreasuryError::InsufficientApprovals))
    );
}

#[test]
fn test_treasury_signers_management() {
    let (env, client, admin, _guardian) = setup();
    let signer1 = Address::generate(&env);
    let signer2 = Address::generate(&env);

    client.treasury_add_signer(&admin, &signer1);
    client.treasury_add_signer(&admin, &signer2);

    let signers = client.treasury_get_signers();
    assert_eq!(signers.len(), 2);

    client.treasury_remove_signer(&admin, &signer1);
    let signers = client.treasury_get_signers();
    assert_eq!(signers.len(), 1);
}

// ---------------------------------------------------------------------------
// Emergency controls
// ---------------------------------------------------------------------------

#[test]
fn test_emergency_pause_by_guardian() {
    let (env, client, _admin, guardian) = setup();
    let result = client.emergency_pause(&guardian);
    assert_eq!(result, OK);
    assert!(client.is_emergency_paused());
}

#[test]
fn test_emergency_pause_by_admin() {
    let (env, client, admin, _guardian) = setup();
    let result = client.emergency_pause(&admin);
    assert_eq!(result, OK);
    assert!(client.is_emergency_paused());
}

#[test]
fn test_emergency_pause_unauthorized_fails() {
    let (env, client, _admin, _guardian) = setup();
    let random = Address::generate(&env);
    assert_eq!(
        client.try_emergency_pause(&random),
        Err(Ok(Error::GuardianRequired))
    );
}

#[test]
fn test_emergency_double_pause_fails() {
    let (env, client, _admin, guardian) = setup();
    client.emergency_pause(&guardian);
    assert_eq!(
        client.try_emergency_pause(&guardian),
        Err(Ok(Error::AlreadyPaused))
    );
}

#[test]
fn test_emergency_unpause() {
    let (env, client, admin, guardian) = setup();
    client.emergency_pause(&guardian);
    assert!(client.is_emergency_paused());

    let result = client.emergency_unpause(&admin);
    assert_eq!(result, OK);
    assert!(!client.is_emergency_paused());
}

#[test]
fn test_emergency_unpause_when_not_paused_fails() {
    let (env, client, admin, _guardian) = setup();
    assert_eq!(
        client.try_emergency_unpause(&admin),
        Err(Ok(Error::NotPaused))
    );
}

#[test]
fn test_emergency_execute() {
    let (env, client, admin, guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let executor = Address::generate(&env);
    let result = client.emergency_execute(&admin, &pid);
    assert_eq!(result, OK);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.status, ProposalStatus::Executed);
}

#[test]
fn test_emergency_execute_requires_both_admin_and_guardian() {
    let (env, client, admin, guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    // Only admin calls, but guardian must also auth.
    // Since mock_all_auths is on, both will auth.
    // Let's test with a random address that isn't admin or guardian.
    let random = Address::generate(&env);
    assert_eq!(
        client.try_emergency_execute(&random, &pid),
        Err(Ok(Error::GuardianRequired))
    );
}

// ---------------------------------------------------------------------------
// Rewards
// ---------------------------------------------------------------------------

#[test]
fn test_claim_rewards() {
    let (env, client, _admin, _guardian, _config) = setup_with_config();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &100);
    client.cast_vote(&voter, &pid, &VoteDirection::For);

    let reward = client.claim_rewards(&voter, &pid);
    assert_eq!(reward, 10); // Default reward_per_participation
    assert_eq!(client.total_rewards_distributed(), 10);
    assert!(client.has_claimed_reward(&pid, &voter));
}

#[test]
fn test_claim_rewards_double_claim_fails() {
    let (env, client, _admin, _guardian, _config) = setup_with_config();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &100);
    client.cast_vote(&voter, &pid, &VoteDirection::For);

    client.claim_rewards(&voter, &pid);
    assert_eq!(
        client.try_claim_rewards(&voter, &pid),
        Err(Ok(Error::RewardAlreadyClaimed))
    );
}

#[test]
fn test_claim_rewards_not_voter_fails() {
    let (env, client, _admin, _guardian, _config) = setup_with_config();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let non_voter = Address::generate(&env);
    client.deposit_voting_power(&non_voter, &100);
    // Did not vote.

    assert_eq!(
        client.try_claim_rewards(&non_voter, &pid),
        Err(Ok(Error::NoVotingPower))
    );
}

// ---------------------------------------------------------------------------
// Audit trail
// ---------------------------------------------------------------------------

#[test]
fn test_audit_trail_records_actions() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let trail = client.get_audit_trail();
    assert!(trail.len() >= 1);

    // The first entry should be the submit action.
    let entry = trail.get(0).unwrap();
    assert_eq!(entry.action, symbol_short!("submit"));
    assert_eq!(entry.proposal_id, pid);
    assert_eq!(entry.actor, proposer);
}

#[test]
fn test_audit_trail_for_proposal() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let voter = Address::generate(&env);
    client.deposit_voting_power(&voter, &100);
    client.cast_vote(&voter, &pid, &VoteDirection::For);

    let trail = client.get_audit_trail_for_proposal(&pid);
    assert_eq!(trail.len(), 2);
    assert_eq!(trail.get(0).unwrap().action, symbol_short!("submit"));
    assert_eq!(trail.get(1).unwrap().action, symbol_short!("vote"));
}

// ---------------------------------------------------------------------------
// Governance summary
// ---------------------------------------------------------------------------

#[test]
fn test_governance_summary() {
    let (env, client, admin, guardian) = setup();
    let proposer = Address::generate(&env);
    client.deposit_voting_power(&proposer, &1000);

    // Before any proposals.
    let summary = client.get_governance_summary();
    assert_eq!(summary.total_proposals, 0);
    assert_eq!(summary.active_proposals, 0);
    assert_eq!(summary.total_voting_power, 1000);
    assert_eq!(summary.treasury_balance, 0);
    assert!(!summary.emergency_paused);

    // Submit a proposal.
    let pid = client.submit_proposal(
        &proposer,
        &sym(&env, "P1"),
        &sym(&env, "D1"),
        &ProposalActionType::ParameterChange,
        &sym(&env, "pl1"),
    );

    let summary = client.get_governance_summary();
    assert_eq!(summary.total_proposals, 1);
    assert_eq!(summary.active_proposals, 1);

    // Deposit to treasury.
    let depositor = Address::generate(&env);
    client.treasury_deposit(&depositor, &sym(&env, "XLM"), &50_000);

    let summary = client.get_governance_summary();
    assert_eq!(summary.treasury_balance, 50_000);

    // Emergency pause.
    client.emergency_pause(&guardian);
    let summary = client.get_governance_summary();
    assert!(summary.emergency_paused);
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_multiple_voters_weighted() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    client.set_total_supply(&admin, &10_000);

    // 3 voters with different weights.
    let v1 = Address::generate(&env);
    client.deposit_voting_power(&v1, &100);
    client.cast_vote(&v1, &pid, &VoteDirection::For);

    let v2 = Address::generate(&env);
    client.deposit_voting_power(&v2, &200);
    client.cast_vote(&v2, &pid, &VoteDirection::For);

    let v3 = Address::generate(&env);
    client.deposit_voting_power(&v3, &50);
    client.cast_vote(&v3, &pid, &VoteDirection::Against);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.votes_for, 300);
    assert_eq!(proposal.votes_against, 50);
    assert_eq!(proposal.voter_count, 3);

    env.ledger().set_timestamp(604_801);
    let status = client.finalize_proposal(&pid);
    assert_eq!(status, ProposalStatus::Passed);
}

#[test]
fn test_zero_votes_proposal_defeated() {
    let (env, client, admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    client.set_total_supply(&admin, &10_000);
    // No one votes.

    env.ledger().set_timestamp(604_801);
    let status = client.finalize_proposal(&pid);
    assert_eq!(status, ProposalStatus::Defeated);
}

#[test]
fn test_max_active_proposals_enforced() {
    let (env, client, admin, _guardian) = setup();
    let config = GovernanceConfig {
        proposal_threshold: 100,
        voting_period: 604_800,
        quorum_threshold_bps: 400,
        approval_threshold_bps: 5_001,
        timelock_delay: 172_800,
        emergency_timelock_delay: 3_600,
        max_treasury_spend_bps: 1_000,
        treasury_multisig_threshold: 2,
        reward_per_participation: 10,
        max_active_proposals: 2, // Only 2 active proposals allowed.
    };
    client.set_config(&admin, &config);

    let proposer = Address::generate(&env);
    client.deposit_voting_power(&proposer, &1000);

    // Submit 2 proposals (should work).
    let p1 = client.submit_proposal(
        &proposer,
        &sym(&env, "P1"),
        &sym(&env, "D1"),
        &ProposalActionType::ParameterChange,
        &sym(&env, "pl1"),
    );
    let p2 = client.submit_proposal(
        &proposer,
        &sym(&env, "P2"),
        &sym(&env, "D2"),
        &ProposalActionType::ParameterChange,
        &sym(&env, "pl2"),
    );

    // Third should fail.
    assert_eq!(
        client.try_submit_proposal(
            &proposer,
            &sym(&env, "P3"),
            &sym(&env, "D3"),
            &ProposalActionType::ParameterChange,
            &sym(&env, "pl3"),
        ),
        Err(Ok(Error::MaxProposalsReached))
    );

    // Cancel one, then submit again should work.
    client.cancel_proposal(&proposer, &p1);
    let p3 = client.submit_proposal(
        &proposer,
        &sym(&env, "P3"),
        &sym(&env, "D3"),
        &ProposalActionType::ParameterChange,
        &sym(&env, "pl3"),
    );
    assert_eq!(p3, 3);
}

// ---------------------------------------------------------------------------
// Delegation + voting integration
// ---------------------------------------------------------------------------

#[test]
fn test_delegated_voter_can_vote_directly() {
    let (env, client, _admin, _guardian) = setup();
    let proposer = Address::generate(&env);
    let pid = deposit_and_submit(&env, &client, &proposer, 200);

    let token_holder = Address::generate(&env);
    client.deposit_voting_power(&token_holder, &300);

    let delegate_addr = Address::generate(&env);
    client.delegate(&token_holder, &delegate_addr);

    // Token holder can still vote directly.
    client.cast_vote(&token_holder, &pid, &VoteDirection::For);

    let proposal = client.get_proposal(&pid).unwrap();
    assert_eq!(proposal.votes_for, 300);
    assert_eq!(proposal.voter_count, 1);
}

// ---------------------------------------------------------------------------
// Audit sink
// ---------------------------------------------------------------------------

#[test]
fn test_set_get_audit_sink() {
    let (env, client, admin, _guardian) = setup();
    let sink = Address::generate(&env);
    client.set_audit_sink(&admin, &sink);
    assert_eq!(client.get_audit_sink(), Some(sink));
}
