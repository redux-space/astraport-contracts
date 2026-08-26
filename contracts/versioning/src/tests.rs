//! Unit and integration tests for the versioning contract.
//!
//! Tests cover:
//! - Initialization and admin enforcement.
//! - Version registration and querying.
//! - Multi-sig proposal, approval, and execution flow.
//! - Rollback mechanisms.
//! - Feature flag management and gradual rollout.
//! - Frozen version archival.
//! - Audit trail logging.
//! - Migration compatibility checks.

use super::*;
use crate::records::{FeatureFlagStatus, VersionStatus};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::testutils::Ledger;
use soroban_sdk::{symbol_short, Address, BytesN, Env, Symbol, Vec};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup() -> (Env, VersioningContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, VersioningContract);
    let client = VersioningContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let _ = client.initialize(&admin);
    (env, client, admin)
}

fn setup_with_signers(
    signer_count: u32,
    threshold: u32,
) -> (
    Env,
    VersioningContractClient<'static>,
    Address,
    Vec<Address>,
) {
    let (env, client, admin) = setup();
    let mut signers = Vec::new(&env);
    for _ in 0..signer_count {
        let s = Address::generate(&env);
        client.add_signer(&admin, &s);
        signers.push_back(s);
    }
    client.set_approval_threshold(&admin, &threshold);
    (env, client, admin, signers)
}

fn wasm_hash_for(env: &Env, seed: u8) -> BytesN<32> {
    let mut bytes = [0u8; 32];
    bytes[0] = seed;
    BytesN::from_array(env, &bytes)
}

fn migration_steps(env: &Env) -> Vec<Symbol> {
    let mut steps = Vec::new(env);
    steps.push_back(symbol_short!("step1"));
    steps.push_back(symbol_short!("step2"));
    steps
}

fn sym(env: &Env, s: &str) -> Symbol {
    Symbol::new(env, s)
}

fn add_v1(env: &Env, client: &VersioningContractClient<'_>, admin: &Address) -> u32 {
    client.add_version(
        admin,
        &sym(env, "1_0_0"),
        &wasm_hash_for(env, 1),
        &migration_steps(env),
        &sym(env, "v1"),
    )
}

fn add_v2(env: &Env, client: &VersioningContractClient<'_>, admin: &Address) -> u32 {
    client.add_version(
        admin,
        &sym(env, "2_0_0"),
        &wasm_hash_for(env, 2),
        &migration_steps(env),
        &sym(env, "v2"),
    )
}

fn activate_v1(env: &Env, client: &VersioningContractClient<'_>, admin: &Address) -> u32 {
    let v1 = add_v1(env, client, admin);
    let p = client.propose_upgrade(admin, &v1);
    client.execute_upgrade(&p);
    v1
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

#[test]
fn test_initialize_sets_admin() {
    let (_env, client, admin) = setup();
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_double_initialize_panics() {
    let (_env, client, admin) = setup();
    assert_eq!(
        client.try_initialize(&admin),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn test_initial_state() {
    let (_env, client, _admin) = setup();
    assert_eq!(client.get_current_version(), 0);
    assert_eq!(client.get_approval_threshold(), 1);
    assert_eq!(client.get_version_count(), 0);
}

// ---------------------------------------------------------------------------
// Multi-sig management
// ---------------------------------------------------------------------------

#[test]
fn test_add_and_remove_signer() {
    let (_env, client, admin) = setup();
    let signer = Address::generate(&_env);

    client.add_signer(&admin, &signer);
    let signers = client.get_signers();
    assert_eq!(signers.len(), 1);

    client.remove_signer(&admin, &signer);
    assert_eq!(client.get_signers().len(), 0);
}

#[test]
fn test_add_duplicate_signer_noop() {
    let (_env, client, admin) = setup();
    let signer = Address::generate(&_env);
    client.add_signer(&admin, &signer);
    client.add_signer(&admin, &signer);
    assert_eq!(client.get_signers().len(), 1);
}

#[test]
fn test_set_approval_threshold() {
    let (_env, client, admin) = setup();
    let s1 = Address::generate(&_env);
    let s2 = Address::generate(&_env);
    client.add_signer(&admin, &s1);
    client.add_signer(&admin, &s2);
    client.set_approval_threshold(&admin, &3);
    assert_eq!(client.get_approval_threshold(), 3);
}

#[test]
fn test_set_threshold_too_high_fails() {
    let (_env, client, admin) = setup();
    assert_eq!(
        client.try_set_approval_threshold(&admin, &2),
        Err(Ok(Error::InsufficientApprovals))
    );
}

#[test]
fn test_add_signer_non_admin_fails() {
    let (env, client, _admin) = setup();
    let non_admin = Address::generate(&env);
    let signer = Address::generate(&env);
    assert_eq!(
        client.try_add_signer(&non_admin, &signer),
        Err(Ok(Error::Unauthorized))
    );
}

// ---------------------------------------------------------------------------
// Version management
// ---------------------------------------------------------------------------

#[test]
fn test_add_version() {
    let (env, client, admin) = setup();
    let sv = sym(&env, "1_0_0");
    let v1 = client.add_version(
        &admin,
        &sv,
        &wasm_hash_for(&env, 1),
        &migration_steps(&env),
        &sym(&env, "initial"),
    );
    assert_eq!(v1, 1);
    assert_eq!(client.get_version_count(), 1);

    let meta = client.get_version_metadata(&v1).unwrap();
    assert_eq!(meta.version_number, 1);
    assert_eq!(meta.semantic_version, sv);
    assert_eq!(meta.status, VersionStatus::Proposed);
}

#[test]
fn test_add_multiple_versions() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let v2 = add_v2(&env, &client, &admin);
    assert_eq!(v1, 1);
    assert_eq!(v2, 2);
    assert_eq!(client.get_version_count(), 2);
    let all = client.get_all_versions();
    assert_eq!(all.get(0), Some(v1));
    assert_eq!(all.get(1), Some(v2));
}

#[test]
fn test_add_version_non_admin_fails() {
    let (env, client, _admin) = setup();
    let non_admin = Address::generate(&env);
    assert_eq!(
        client.try_add_version(
            &non_admin,
            &sym(&env, "1_0_0"),
            &wasm_hash_for(&env, 1),
            &migration_steps(&env),
            &sym(&env, "v1"),
        ),
        Err(Ok(Error::Unauthorized))
    );
}

// ---------------------------------------------------------------------------
// Propose + approve + execute upgrade flow
// ---------------------------------------------------------------------------

#[test]
fn test_full_upgrade_flow_single_admin() {
    let (env, client, admin) = setup();
    env.ledger().set_timestamp(100);

    let v1 = add_v1(&env, &client, &admin);
    let proposal = client.propose_upgrade(&admin, &v1);
    let result = client.execute_upgrade(&proposal);
    assert_eq!(result, OK);
    assert_eq!(client.get_current_version(), 1);

    env.ledger().set_timestamp(200);
    let v2 = add_v2(&env, &client, &admin);
    let proposal2 = client.propose_upgrade(&admin, &v2);
    let result2 = client.execute_upgrade(&proposal2);
    assert_eq!(result2, OK);
    assert_eq!(client.get_current_version(), 2);

    assert_eq!(
        client.get_version_metadata(&v1).unwrap().status,
        VersionStatus::Superseded
    );
    assert_eq!(
        client.get_version_metadata(&v2).unwrap().status,
        VersionStatus::Active
    );
}

#[test]
fn test_propose_upgrade() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let proposal_id = client.propose_upgrade(&admin, &v1);
    assert_eq!(proposal_id, 1);

    let proposal = client.get_proposal(&proposal_id).unwrap();
    assert_eq!(proposal.target_version, 1);
    assert_eq!(proposal.proposer, admin);
    assert!(!proposal.executed);
    assert!(!proposal.rejected);
    assert_eq!(proposal.approvals.len(), 0);
}

#[test]
fn test_propose_upgrade_nonexistent_version_fails() {
    let (_env, client, admin) = setup();
    assert_eq!(
        client.try_propose_upgrade(&admin, &99),
        Err(Ok(Error::ProposalNotFound))
    );
}

#[test]
fn test_approve_upgrade() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let proposal_id = client.propose_upgrade(&admin, &v1);

    let signer = Address::generate(&env);
    client.add_signer(&admin, &signer);
    client.set_approval_threshold(&admin, &2);

    let result = client.approve_upgrade(&signer, &proposal_id);
    assert_eq!(result, OK);

    let proposal = client.get_proposal(&proposal_id).unwrap();
    assert_eq!(proposal.approvals.len(), 1);
}

#[test]
fn test_approve_duplicate_fails() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let proposal_id = client.propose_upgrade(&admin, &v1);

    let signer = Address::generate(&env);
    client.add_signer(&admin, &signer);

    client.approve_upgrade(&signer, &proposal_id);
    assert_eq!(
        client.try_approve_upgrade(&signer, &proposal_id),
        Err(Ok(Error::AlreadyApproved))
    );
}

#[test]
fn test_approve_non_signer_fails() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let proposal_id = client.propose_upgrade(&admin, &v1);

    let random = Address::generate(&env);
    assert_eq!(
        client.try_approve_upgrade(&random, &proposal_id),
        Err(Ok(Error::NotASigner))
    );
}

#[test]
fn test_execute_upgrade_insufficient_approvals_fails() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let proposal_id = client.propose_upgrade(&admin, &v1);

    let signer = Address::generate(&env);
    client.add_signer(&admin, &signer);
    client.set_approval_threshold(&admin, &2);

    assert_eq!(
        client.try_execute_upgrade(&proposal_id),
        Err(Ok(Error::InsufficientApprovals))
    );
}

#[test]
fn test_execute_upgrade_with_multi_sig() {
    let (env, client, admin, signers) = setup_with_signers(2, 3);

    let v1 = add_v1(&env, &client, &admin);
    let proposal_id = client.propose_upgrade(&admin, &v1);

    client.approve_upgrade(&admin, &proposal_id);
    client.approve_upgrade(&signers.get(0).unwrap(), &proposal_id);
    client.approve_upgrade(&signers.get(1).unwrap(), &proposal_id);

    assert!(client.is_proposal_approved(&proposal_id));

    let result = client.execute_upgrade(&proposal_id);
    assert_eq!(result, OK);
    assert_eq!(client.get_current_version(), 1);
}

#[test]
fn test_reject_upgrade() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let proposal_id = client.propose_upgrade(&admin, &v1);

    let result = client.reject_upgrade(&admin, &proposal_id);
    assert_eq!(result, OK);

    let proposal = client.get_proposal(&proposal_id).unwrap();
    assert!(proposal.rejected);
}

#[test]
fn test_execute_rejected_proposal_fails() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let proposal_id = client.propose_upgrade(&admin, &v1);
    client.reject_upgrade(&admin, &proposal_id);

    assert_eq!(
        client.try_execute_upgrade(&proposal_id),
        Err(Ok(Error::ProposalRejected))
    );
}

// ---------------------------------------------------------------------------
// Rollback
// ---------------------------------------------------------------------------

#[test]
fn test_rollback_to_previous_version() {
    let (env, client, admin) = setup();
    env.ledger().set_timestamp(100);

    let v1 = activate_v1(&env, &client, &admin);
    assert_eq!(client.get_current_version(), 1);

    env.ledger().set_timestamp(200);
    let v2 = add_v2(&env, &client, &admin);
    let p2 = client.propose_upgrade(&admin, &v2);
    client.execute_upgrade(&p2);
    assert_eq!(client.get_current_version(), 2);

    env.ledger().set_timestamp(300);
    let result = client.rollback(&admin, &v1);
    assert_eq!(result, OK);
    assert_eq!(client.get_current_version(), 1);

    assert_eq!(
        client.get_version_metadata(&v1).unwrap().status,
        VersionStatus::Active
    );
    assert_eq!(
        client.get_version_metadata(&v2).unwrap().status,
        VersionStatus::RolledBack
    );
}

#[test]
fn test_rollback_nonexistent_version_fails() {
    let (env, client, admin) = setup();
    let _v1 = activate_v1(&env, &client, &admin);

    assert_eq!(
        client.try_rollback(&admin, &99),
        Err(Ok(Error::ProposalNotFound))
    );
}

#[test]
fn test_rollback_no_versions_fails() {
    let (_env, client, admin) = setup();
    assert_eq!(client.try_rollback(&admin, &1), Err(Ok(Error::NoVersions)));
}

// ---------------------------------------------------------------------------
// Feature flags
// ---------------------------------------------------------------------------

#[test]
fn test_set_and_get_feature_flag() {
    let (env, client, admin) = setup();
    let flag_name = sym(&env, "new_ui");
    let result = client.set_feature_flag(
        &admin,
        &flag_name,
        &FeatureFlagStatus::Enabled,
        &0,
        &1,
        &sym(&env, "new_ui_d"),
    );
    assert_eq!(result, OK);

    let flag = client.get_feature_flag(&flag_name).unwrap();
    assert_eq!(flag.status, FeatureFlagStatus::Enabled);
    assert_eq!(flag.min_version, 1);
}

#[test]
fn test_feature_flag_enabled_for_all_versions_above_min() {
    let (env, client, admin) = setup();
    let flag_name = sym(&env, "feat_a");
    client.set_feature_flag(
        &admin,
        &flag_name,
        &FeatureFlagStatus::Enabled,
        &0,
        &2,
        &sym(&env, "feat_a"),
    );

    assert!(!client.is_feature_enabled(&flag_name, &1, &0));
    assert!(client.is_feature_enabled(&flag_name, &2, &0));
    assert!(client.is_feature_enabled(&flag_name, &3, &42));
}

#[test]
fn test_feature_flag_disabled() {
    let (env, client, admin) = setup();
    let flag_name = sym(&env, "feat_b");
    client.set_feature_flag(
        &admin,
        &flag_name,
        &FeatureFlagStatus::Disabled,
        &0,
        &1,
        &sym(&env, "feat_b"),
    );
    assert!(!client.is_feature_enabled(&flag_name, &5, &0));
}

#[test]
fn test_feature_flag_gradual_rollout() {
    let (env, client, admin) = setup();
    let flag_name = sym(&env, "feat_c");
    client.set_feature_flag(
        &admin,
        &flag_name,
        &FeatureFlagStatus::GradualRollout,
        &50,
        &1,
        &sym(&env, "feat_c"),
    );

    assert!(client.is_feature_enabled(&flag_name, &1, &0));
    assert!(client.is_feature_enabled(&flag_name, &1, &49));
    assert!(!client.is_feature_enabled(&flag_name, &1, &50));
    assert!(!client.is_feature_enabled(&flag_name, &1, &99));
    assert!(!client.is_feature_enabled(&flag_name, &1, &150));
}

#[test]
fn test_feature_flag_nonexistent_returns_false() {
    let (env, client, _admin) = setup();
    let flag_name = sym(&env, "nope");
    assert!(!client.is_feature_enabled(&flag_name, &1, &0));
}

#[test]
fn test_feature_flag_invalid_rollout_percentage_fails() {
    let (env, client, admin) = setup();
    assert_eq!(
        client.try_set_feature_flag(
            &admin,
            &sym(&env, "bad"),
            &FeatureFlagStatus::GradualRollout,
            &101,
            &1,
            &sym(&env, "bad"),
        ),
        Err(Ok(Error::InvalidRolloutPercentage))
    );
}

#[test]
fn test_get_all_feature_flags() {
    let (env, client, admin) = setup();
    client.set_feature_flag(
        &admin,
        &sym(&env, "f1"),
        &FeatureFlagStatus::Enabled,
        &0,
        &1,
        &sym(&env, "f1"),
    );
    client.set_feature_flag(
        &admin,
        &sym(&env, "f2"),
        &FeatureFlagStatus::Disabled,
        &0,
        &1,
        &sym(&env, "f2"),
    );
    let flags = client.get_all_feature_flags();
    assert_eq!(flags.len(), 2);
}

#[test]
fn test_update_feature_flag() {
    let (env, client, admin) = setup();
    let flag_name = sym(&env, "flip");
    client.set_feature_flag(
        &admin,
        &flag_name,
        &FeatureFlagStatus::Disabled,
        &0,
        &1,
        &sym(&env, "flip"),
    );
    assert!(!client.is_feature_enabled(&flag_name, &1, &0));

    client.set_feature_flag(
        &admin,
        &flag_name,
        &FeatureFlagStatus::Enabled,
        &0,
        &1,
        &sym(&env, "flip"),
    );
    assert!(client.is_feature_enabled(&flag_name, &1, &0));
}

// ---------------------------------------------------------------------------
// Frozen versions
// ---------------------------------------------------------------------------

#[test]
fn test_freeze_version() {
    let (env, client, admin) = setup();

    let v1 = activate_v1(&env, &client, &admin);

    let _v2 = add_v2(&env, &client, &admin);

    let result = client.freeze_version(&admin, &v1);
    assert_eq!(result, OK);

    let meta = client.get_version_metadata(&v1).unwrap();
    assert_eq!(meta.status, VersionStatus::Frozen);
    assert!(client.is_version_frozen(&v1));

    let frozen = client.get_frozen_versions();
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen.get(0), Some(v1));
}

#[test]
fn test_freeze_active_version_fails() {
    let (env, client, admin) = setup();
    let v1 = activate_v1(&env, &client, &admin);

    assert_eq!(
        client.try_freeze_version(&admin, &v1),
        Err(Ok(Error::CannotFreezeActiveVersion))
    );
}

#[test]
fn test_freeze_already_frozen_fails() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    client.freeze_version(&admin, &v1);

    assert_eq!(
        client.try_freeze_version(&admin, &v1),
        Err(Ok(Error::AlreadyFrozen))
    );
}

// ---------------------------------------------------------------------------
// Audit trail
// ---------------------------------------------------------------------------

#[test]
fn test_audit_trail_records_actions() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);

    let trail = client.get_audit_trail();
    assert_eq!(trail.len(), 1);
    let entry = trail.get(0).unwrap();
    assert_eq!(entry.action, symbol_short!("add_ver"));
    assert_eq!(entry.version_number, v1);
    assert_eq!(entry.actor, admin);
}

#[test]
fn test_audit_trail_filters_by_version() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let v2 = add_v2(&env, &client, &admin);

    let trail_v1 = client.get_audit_trail_for_version(&v1);
    assert_eq!(trail_v1.len(), 1);

    let trail_v2 = client.get_audit_trail_for_version(&v2);
    assert_eq!(trail_v2.len(), 1);
}

#[test]
fn test_full_upgrade_flow_records_audit_trail() {
    let (env, client, admin) = setup();
    let v1 = add_v1(&env, &client, &admin);
    let proposal_id = client.propose_upgrade(&admin, &v1);

    let signer = Address::generate(&env);
    client.add_signer(&admin, &signer);
    client.approve_upgrade(&signer, &proposal_id);
    client.execute_upgrade(&proposal_id);

    let trail = client.get_audit_trail();
    assert_eq!(trail.len(), 4);
    assert_eq!(trail.get(0).unwrap().action, symbol_short!("add_ver"));
    assert_eq!(trail.get(1).unwrap().action, symbol_short!("propose"));
    assert_eq!(trail.get(2).unwrap().action, symbol_short!("approve"));
    assert_eq!(trail.get(3).unwrap().action, symbol_short!("upgrade"));
}

// ---------------------------------------------------------------------------
// Migration records
// ---------------------------------------------------------------------------

#[test]
fn test_migration_record_stored_after_upgrade() {
    let (env, client, admin) = setup();
    let _v1 = activate_v1(&env, &client, &admin);

    let v2 = add_v2(&env, &client, &admin);
    let p2 = client.propose_upgrade(&admin, &v2);
    client.execute_upgrade(&p2);

    let record = client.get_migration_record(&1, &2).unwrap();
    assert_eq!(record.from_version, 1);
    assert_eq!(record.to_version, 2);
    assert!(record.success);
}

// ---------------------------------------------------------------------------
// Backward compatibility check
// ---------------------------------------------------------------------------

#[test]
fn test_check_backward_compatibility_valid() {
    let (env, client, admin) = setup();
    let _v1 = activate_v1(&env, &client, &admin);

    let _v2 = add_v2(&env, &client, &admin);

    assert!(client.check_backward_compatibility(&1, &2));
}

#[test]
fn test_check_backward_compatibility_same_version_fails() {
    let (_env, client, _admin) = setup();
    assert!(!client.check_backward_compatibility(&1, &1));
}

// ---------------------------------------------------------------------------
// Audit sink
// ---------------------------------------------------------------------------

#[test]
fn test_set_get_audit_sink() {
    let (_env, client, admin) = setup();
    let sink = Address::generate(&_env);
    client.set_audit_sink(&admin, &sink);
    assert_eq!(client.get_audit_sink(), Some(sink));
}
