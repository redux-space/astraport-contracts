//! Unit and integration tests for the audit-log contract.
//!
//! Tests cover:
//! - Initialization and admin enforcement.
//! - Append-only logging and sequence numbering.
//! - Query filters (event type, portfolio, actor, time range, limit).
//! - Chain-hash integrity (golden + tamper detection).
//! - Retention policy enforcement.
//! - JSON/CSV export formatting.

use super::*;
use crate::checksum::{entry_payload, first_chain_hash};
use crate::log_query::LogQuery;
use crate::records::{permissions, AuditEventType, RetentionPolicy, StateSnapshot};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{symbol_short, Address, BytesN, Env, String, Symbol, Vec};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup() -> (Env, AuditContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, AuditContract);
    let client = AuditContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let _ = client.initialize(&admin);
    (env, client, admin)
}

fn staker(env: &Env) -> Address {
    Address::generate(env)
}

fn asset() -> Symbol {
    symbol_short!("XLM")
}

fn happy_snapshot(env: &Env, key: Symbol, value: i128) -> StateSnapshot {
    let mut s = StateSnapshot::empty(env);
    s.push(key, value);
    s
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

#[test]
fn test_initialize_sets_admin() {
    let (env, client, admin) = setup();
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_double_initialize_panics() {
    let (_env, client, admin) = setup();
    assert!(client.try_initialize(&admin).is_err());
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

#[test]
fn test_log_event_increments_sequence() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let seq1 = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &happy_snapshot(&env, asset(), 0),
        &happy_snapshot(&env, asset(), 100),
        &symbol_short!("ok"),
        &String::from_str(&env, "first"),
    );
    let seq2 = client.log_event(
        &s,
        &AuditEventType::Unstake,
        &a,
        &permissions::STAKER,
        &happy_snapshot(&env, asset(), 100),
        &happy_snapshot(&env, asset(), 50),
        &symbol_short!("ok"),
        &String::from_str(&env, "second"),
    );
    assert_eq!(seq1, 1);
    assert_eq!(seq2, 2);
}

#[test]
fn test_log_event_sets_immutable_fields() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let seq = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &happy_snapshot(&env, a.clone(), 100),
        &symbol_short!("ok"),
        &String::from_str(&env, "ok"),
    );
    let entry = client.query(&LogQuery::new(&env, 10)).get(0).unwrap();
    assert_eq!(entry.seq, seq);
    assert_eq!(entry.event_type, AuditEventType::Stake);
    assert_eq!(entry.actor, s);
    assert_eq!(entry.portfolio, a);
    assert_eq!(entry.permissions, permissions::STAKER);
    assert_eq!(entry.outcome, symbol_short!("ok"));
    assert_eq!(entry.detail, String::from_str(&env, "ok"));
    assert_eq!(entry.state_after.fields.get(0).unwrap().value, 100);
}

#[test]
fn test_log_event_stamps_ledger_timestamp() {
    let (env, client, _admin) = setup();
    env.ledger().set_timestamp(1_700_000_000);
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, ""),
    );
    let entry = client.query(&LogQuery::new(&env, 10)).get(0).unwrap();
    assert_eq!(entry.timestamp, 1_700_000_000);
}

#[test]
fn test_log_event_trusts_caller_auth() {
    // `log_event` no longer enforces `actor.require_auth()` itself; the
    // calling contract is responsible. With `mock_all_auths()` the call
    // succeeds and the entry is logged.
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, AuditContract);
    let client = AuditContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let _ = client.initialize(&admin);
    let s = staker(&env);
    let seq = client.log_event(
        &s,
        &AuditEventType::Stake,
        &asset(),
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, ""),
    );
    assert_eq!(seq, 1);
}

// ---------------------------------------------------------------------------
// Querying
// ---------------------------------------------------------------------------

#[test]
fn test_query_returns_all_when_unfiltered() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    for _ in 0..5 {
        let _ = client.log_event(
            &s,
            &AuditEventType::Stake,
            &a,
            &permissions::STAKER,
            &StateSnapshot::empty(&env),
            &StateSnapshot::empty(&env),
            &symbol_short!("ok"),
            &String::from_str(&env, ""),
        );
    }
    let res = client.query(&LogQuery::new(&env, 10));
    assert_eq!(res.len(), 5);
}

#[test]
fn test_query_by_event_type() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, ""),
    );
    let _ = client.log_event(
        &s,
        &AuditEventType::Unstake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, ""),
    );
    let staked = client.query(&LogQuery::new(&env, 10).event_type(AuditEventType::Stake));
    assert_eq!(staked.len(), 1);
    assert_eq!(staked.get(0).unwrap().event_type, AuditEventType::Stake);
}

#[test]
fn test_query_filters_by_portfolio() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let xlm = symbol_short!("XLM");
    let usdc = symbol_short!("USDC");
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &xlm,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, ""),
    );
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &usdc,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, ""),
    );
    let only_xlm = client.query(&LogQuery::new(&env, 10).portfolio(xlm.clone()));
    assert_eq!(only_xlm.len(), 1);
    assert_eq!(only_xlm.get(0).unwrap().portfolio, xlm);
}

#[test]
fn test_query_limit_caps_results() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    for _ in 0..10 {
        let _ = client.log_event(
            &s,
            &AuditEventType::Stake,
            &a,
            &permissions::STAKER,
            &StateSnapshot::empty(&env),
            &StateSnapshot::empty(&env),
            &symbol_short!("ok"),
            &String::from_str(&env, ""),
        );
    }
    let res = client.query(&LogQuery::new(&env, 3));
    assert_eq!(res.len(), 3);
}

#[test]
fn test_query_range_by_timestamp() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    for i in 0..5u64 {
        env.ledger().set_timestamp(100 + i * 10);
        let _ = client.log_event(
            &s,
            &AuditEventType::Stake,
            &a,
            &permissions::STAKER,
            &StateSnapshot::empty(&env),
            &StateSnapshot::empty(&env),
            &symbol_short!("ok"),
            &String::from_str(&env, ""),
        );
    }
    let q = LogQuery::new(&env, 10).from_ts(110).to_ts(120);
    let res = client.query(&q);
    assert_eq!(res.len(), 2, "expected only 110 and 120 entries");
    assert_eq!(res.get(0).unwrap().timestamp, 110);
    assert_eq!(res.get(1).unwrap().timestamp, 120);
}

// ---------------------------------------------------------------------------
// Chain integrity
// ---------------------------------------------------------------------------

#[test]
fn test_chain_links_correctly() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "first"),
    );
    let head = client.integrity_head();
    let expected_payload = entry_payload(
        &env,
        1,
        env.ledger().timestamp(),
        AuditEventType::Stake as u32,
        permissions::STAKER,
        &s,
        &a,
        &symbol_short!("ok"),
        &String::from_str(&env, "first"),
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
    );
    let expected = first_chain_hash(&env, &expected_payload);
    assert_eq!(head, expected);
}

#[test]
fn test_full_recompute_integrity_returns_true_for_untampered_chain() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    for _ in 0..3 {
        let _ = client.log_event(
            &s,
            &AuditEventType::Stake,
            &a,
            &permissions::STAKER,
            &StateSnapshot::empty(&env),
            &StateSnapshot::empty(&env),
            &symbol_short!("ok"),
            &String::from_str(&env, ""),
        );
    }
    assert!(client.full_recompute_integrity());
}

#[test]
fn test_verify_integrity_stored_head() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, ""),
    );
    let head = client.integrity_head();
    assert!(client.verify_integrity(&head));
    let bogus = BytesN::from_array(&env, &[0u8; 32]);
    assert!(!client.verify_integrity(&bogus));
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

#[test]
fn test_prune_old_enforces_max_entries() {
    let (env, client, admin) = setup();
    let s = staker(&env);
    let a = asset();
    for _ in 0..5 {
        let _ = client.log_event(
            &s,
            &AuditEventType::Stake,
            &a,
            &permissions::STAKER,
            &StateSnapshot::empty(&env),
            &StateSnapshot::empty(&env),
            &symbol_short!("ok"),
            &String::from_str(&env, ""),
        );
    }
    let policy = RetentionPolicy {
        max_entries: 3,
        max_age_seconds: 0,
    };
    let _ = client.set_retention_policy(&admin, &policy);
    let pruned = client.prune_old(&admin);
    assert_eq!(pruned, 2);
    let res = client.query(&LogQuery::new(&env, 10));
    assert_eq!(res.len(), 3);
}

#[test]
fn test_prune_old_admin_only() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, ""),
    );
    let policy = RetentionPolicy {
        max_entries: 1,
        max_age_seconds: 0,
    };
    let _ = client.set_retention_policy(&_admin, &policy);
    let non_admin = Address::generate(&env);
    assert!(client.try_prune_old(&non_admin).is_err());
}

#[test]
fn test_set_retention_policy_unbounded_default() {
    let (_env, client, _admin) = setup();
    let p = client.get_retention_policy();
    assert!(p.is_unbounded());
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

fn soroban_str_to_rust(s: &String) -> alloc::string::String {
    let len = s.len() as usize;
    if len == 0 {
        return alloc::string::String::new();
    }
    let mut buf = alloc::vec![0u8; len];
    s.copy_into_slice(&mut buf);
    alloc::string::String::from_utf8(buf).unwrap_or_default()
}

#[test]
fn test_export_jsonl_returns_one_per_entry() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "deposit"),
    );
    let rows = client.export_jsonl(&LogQuery::new(&env, 10));
    assert_eq!(rows.len(), 1);
    let r0 = rows.get(0).unwrap();
    let row = soroban_str_to_rust(&r0);
    assert!(row.contains("\"seq\":1"));
    assert!(row.contains("\"event_type\":\"Stake\""));
    assert!(row.contains("ok"));
    assert!(row.contains("\"detail\":\"deposit\""));
}

#[test]
fn test_export_csv_header_and_rows() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "deposit"),
    );
    let rows = client.export_csv(&LogQuery::new(&env, 10));
    assert_eq!(rows.len(), 2);
    let r0 = rows.get(0).unwrap();
    let header_str = soroban_str_to_rust(&r0);
    assert_eq!(header_str, export::CSV_HEADER);

    let r1 = rows.get(1).unwrap();
    let body = soroban_str_to_rust(&r1);
    assert!(body.contains("Stake"));
    assert!(body.contains("ok"));
    assert!(body.contains("deposit"));
}

#[test]
fn test_export_csv_escapes_special_characters() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "comma,with\"quotes"),
    );
    let rows = client.export_csv(&LogQuery::new(&env, 10));
    let body = soroban_str_to_rust(&rows.get(1).unwrap());
    // Field should be wrapped in quotes because it contains a comma.
    assert!(body.contains("\"comma,with\"\"quotes\""));
}

// ---------------------------------------------------------------------------
// Outcome filtering
// ---------------------------------------------------------------------------

#[test]
fn test_query_filters_by_outcome() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, ""),
    );
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("fail"),
        &String::from_str(&env, ""),
    );
    let ok_entries = client.query(&LogQuery::new(&env, 10).outcome(symbol_short!("ok")));
    assert_eq!(ok_entries.len(), 1);
    assert_eq!(ok_entries.get(0).unwrap().outcome, symbol_short!("ok"));
}

// ---------------------------------------------------------------------------
// New event types
// ---------------------------------------------------------------------------

#[test]
fn test_new_event_types_logging() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();

    // Test PortfolioCreated
    let seq = client.log_event(
        &s,
        &AuditEventType::PortfolioCreated,
        &a,
        &permissions::ADMIN,
        &StateSnapshot::empty(&env),
        &happy_snapshot(&env, symbol_short!("val"), 100),
        &symbol_short!("ok"),
        &String::from_str(&env, "created"),
    );
    assert_eq!(seq, 1);

    // Test GovernanceProposal
    let seq = client.log_event(
        &s,
        &AuditEventType::GovernanceProposal,
        &symbol_short!("gov"),
        &permissions::ADMIN,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "submitted"),
    );
    assert_eq!(seq, 2);

    // Test TradeExecution
    let seq = client.log_event(
        &s,
        &AuditEventType::TradeExecution,
        &symbol_short!("XLM_USDC"),
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "filled"),
    );
    assert_eq!(seq, 3);

    // Query by new event types
    let gov_entries =
        client.query(&LogQuery::new(&env, 10).event_type(AuditEventType::GovernanceProposal));
    assert_eq!(gov_entries.len(), 1);
    assert_eq!(
        gov_entries.get(0).unwrap().event_type,
        AuditEventType::GovernanceProposal
    );

    let trade_entries =
        client.query(&LogQuery::new(&env, 10).event_type(AuditEventType::TradeExecution));
    assert_eq!(trade_entries.len(), 1);
}

// ---------------------------------------------------------------------------
// Full export with state snapshots and hash
// ---------------------------------------------------------------------------

#[test]
fn test_export_jsonl_full_includes_hash() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &happy_snapshot(&env, a.clone(), 0),
        &happy_snapshot(&env, a.clone(), 100),
        &symbol_short!("ok"),
        &String::from_str(&env, "stake"),
    );
    let rows = client.export_jsonl_full(&LogQuery::new(&env, 10));
    assert_eq!(rows.len(), 1);
    let row = soroban_str_to_rust(&rows.get(0).unwrap());
    assert!(row.contains("\"hash\""));
    assert!(row.contains("\"state_before\""));
    assert!(row.contains("\"state_after\""));
}

#[test]
fn test_export_csv_full_includes_state() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &happy_snapshot(&env, a.clone(), 0),
        &happy_snapshot(&env, a.clone(), 500),
        &symbol_short!("ok"),
        &String::from_str(&env, "deposit"),
    );
    let rows = client.export_csv_full(&LogQuery::new(&env, 10));
    assert_eq!(rows.len(), 2);
    let header = soroban_str_to_rust(&rows.get(0).unwrap());
    assert!(header.contains("hash"));
    assert!(header.contains("state_before"));
    assert!(header.contains("state_after"));
    let body = soroban_str_to_rust(&rows.get(1).unwrap());
    assert!(body.contains("XLM"));
}

// ---------------------------------------------------------------------------
// Signing / digest computation
// ---------------------------------------------------------------------------

#[test]
fn test_compute_export_digest() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    let _ = client.log_event(
        &s,
        &AuditEventType::Stake,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "test"),
    );
    let digest = client.compute_export_digest(&LogQuery::new(&env, 10));
    // Digest should be non-zero
    assert_ne!(digest, soroban_sdk::BytesN::from_array(&env, &[0u8; 32]));
}

#[test]
fn test_new_event_type_names() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let a = asset();
    // Log each new event type
    let seq = client.log_event(
        &s,
        &AuditEventType::PortfolioCreated,
        &a,
        &permissions::ADMIN,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "PortfolioCreated"),
    );
    assert_eq!(seq, 1);
    let seq = client.log_event(
        &s,
        &AuditEventType::RoleChange,
        &a,
        &permissions::ADMIN,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "RoleChange"),
    );
    assert_eq!(seq, 2);
    let seq = client.log_event(
        &s,
        &AuditEventType::YieldClaim,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "YieldClaim"),
    );
    assert_eq!(seq, 3);
    let seq = client.log_event(
        &s,
        &AuditEventType::GovernanceProposal,
        &a,
        &permissions::ADMIN,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "GovernanceProposal"),
    );
    assert_eq!(seq, 4);
    let seq = client.log_event(
        &s,
        &AuditEventType::GovernanceVote,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "GovernanceVote"),
    );
    assert_eq!(seq, 5);
    let seq = client.log_event(
        &s,
        &AuditEventType::TreasuryAction,
        &a,
        &permissions::ADMIN,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "TreasuryAction"),
    );
    assert_eq!(seq, 6);
    let seq = client.log_event(
        &s,
        &AuditEventType::EmergencyPause,
        &a,
        &permissions::ADMIN,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "EmergencyPause"),
    );
    assert_eq!(seq, 7);
    let seq = client.log_event(
        &s,
        &AuditEventType::TradeExecution,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "TradeExecution"),
    );
    assert_eq!(seq, 8);
    let seq = client.log_event(
        &s,
        &AuditEventType::OrderPlaced,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "OrderPlaced"),
    );
    assert_eq!(seq, 9);
    let seq = client.log_event(
        &s,
        &AuditEventType::OrderCancelled,
        &a,
        &permissions::STAKER,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "OrderCancelled"),
    );
    assert_eq!(seq, 10);
    let seq = client.log_event(
        &s,
        &AuditEventType::FeeCollection,
        &a,
        &permissions::ADMIN,
        &StateSnapshot::empty(&env),
        &StateSnapshot::empty(&env),
        &symbol_short!("ok"),
        &String::from_str(&env, "FeeCollection"),
    );
    assert_eq!(seq, 11);
    // Verify all logged
    let all = client.query(&LogQuery::new(&env, 50));
    assert_eq!(all.len(), 11);
}

#[test]
fn test_log_event_with_full_state_snapshots() {
    let (env, client, _admin) = setup();
    let s = staker(&env);
    let mut before = StateSnapshot::empty(&env);
    before.push(symbol_short!("XLM"), 1000);
    before.push(symbol_short!("USDC"), 5000);
    let mut after = StateSnapshot::empty(&env);
    after.push(symbol_short!("XLM"), 800);
    after.push(symbol_short!("USDC"), 6000);
    let seq = client.log_event(
        &s,
        &AuditEventType::Rebalance,
        &symbol_short!("PORT1"),
        &permissions::ADMIN,
        &before,
        &after,
        &symbol_short!("ok"),
        &String::from_str(&env, "rebalanced"),
    );
    assert_eq!(seq, 1);
    let entry = client.query(&LogQuery::new(&env, 10)).get(0).unwrap();
    assert_eq!(entry.state_before.fields.len(), 2);
    assert_eq!(entry.state_after.fields.len(), 2);
    assert_eq!(entry.state_after.fields.get(0).unwrap().value, 800);
    assert_eq!(entry.state_after.fields.get(1).unwrap().value, 6000);
}
