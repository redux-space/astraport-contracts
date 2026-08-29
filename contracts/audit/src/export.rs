//! Export formatters for the audit log.
//!
//! Soroban's `String` has a small mutator API surface — we build each
//! formatted output as a single `String::from_str(&env, ...)` from an
//! `alloc::string::String` (which Soroban contracts can use). Heavy CSV/JSON
//! readers (off-chain) parse the contract-side output, so we keep the
//! in-contract logic deliberately small.

use alloc::format;
use alloc::string::String as RustString;
use alloc::string::ToString;
use soroban_sdk::{Env, FromVal, String, Symbol, Vec};

use crate::records::{AuditEventType, AuditLog};

/// Helper: copy a Soroban `String` into a heap `alloc::string::String`.
fn soroban_str(s: &String) -> RustString {
    let len = s.len() as usize;
    let mut buf = alloc::vec![0u8; len];
    s.copy_into_slice(&mut buf);
    unsafe { RustString::from_utf8_unchecked(buf) }
}

/// Helper: convert a `Symbol` to a heap `alloc::string::String`.
fn symbol_to_rust(s: &soroban_sdk::Symbol) -> RustString {
    // Symbol::to_string() returns alloc::string::String via ToString trait.
    s.to_string()
}

/// CSV header shared by all exports. Column order is part of the contract
/// protocol and is pinned by tests.
pub const CSV_HEADER: &str =
    "seq,timestamp,event_type,actor,permissions,portfolio,outcome,detail,hash";

/// Extended CSV header including state snapshots.
pub const CSV_HEADER_FULL: &str = "seq,timestamp,event_type,actor,permissions,portfolio,outcome,detail,hash,state_before,state_after";

#[allow(dead_code)]
fn to_rust_str(s: &String) -> RustString {
    let len = s.len() as usize;
    if len == 0 {
        return RustString::new();
    }
    let mut buf = [0u8; 256];
    let slice_len = len.min(256);
    s.copy_into_slice(&mut buf[..slice_len]);
    RustString::from_utf8(buf[..slice_len].to_vec()).unwrap_or_default()
}

#[allow(dead_code)]
fn symbol_to_rust_str(env: &Env, s: &Symbol) -> RustString {
    let soroban_str = String::from_val(env, &s.to_val());
    to_rust_str(&soroban_str)
}

/// One JSON object per `AuditLog` (no surrounding array).
pub fn format_json_entry(env: &Env, entry: &AuditLog) -> String {
    let rs: RustString = format!(
        "{{\"seq\":{},\"timestamp\":{},\"event_type\":\"{}\",\"actor\":\"{:?}\",\"permissions\":{},\"portfolio\":\"{:?}\",\"outcome\":\"{:?}\",\"detail\":\"{}\",\"hash\":\"{}\"}}",
        entry.seq,
        entry.timestamp,
        event_type_name(entry.event_type),
        soroban_str(&entry.actor.to_string()),
        entry.permissions,
        symbol_to_rust(&entry.portfolio),
        symbol_to_rust(&entry.outcome),
        json_escape(&soroban_str(&entry.detail)),
        hex_encode_hash(env, &entry.hash),
    );
    String::from_str(env, &rs)
}

/// Extended JSON object including state snapshots.
pub fn format_json_entry_full(env: &Env, entry: &AuditLog) -> String {
    let rs: RustString = format!(
        "{{\"seq\":{},\"timestamp\":{},\"event_type\":\"{}\",\"actor\":\"{:?}\",\"permissions\":{},\"portfolio\":\"{:?}\",\"outcome\":\"{:?}\",\"detail\":\"{}\",\"hash\":\"{}\",\"state_before\":{},\"state_after\":{}}}",
        entry.seq,
        entry.timestamp,
        event_type_name(entry.event_type),
        soroban_str(&entry.actor.to_string()),
        entry.permissions,
        symbol_to_rust(&entry.portfolio),
        symbol_to_rust(&entry.outcome),
        json_escape(&soroban_str(&entry.detail)),
        hex_encode_hash(env, &entry.hash),
        snapshot_to_json(env, &entry.state_before),
        snapshot_to_json(env, &entry.state_after),
    );
    String::from_str(env, &rs)
}

/// One CSV row matching [`CSV_HEADER`].
pub fn format_csv_row(env: &Env, entry: &AuditLog) -> String {
    let rs: RustString = format!(
        "{},{},{},{:?},{},{:?},{:?},{},{}",
        entry.seq,
        entry.timestamp,
        event_type_name(entry.event_type),
        soroban_str(&entry.actor.to_string()),
        entry.permissions,
        symbol_to_rust(&entry.portfolio),
        symbol_to_rust(&entry.outcome),
        csv_escape(&soroban_str(&entry.detail)),
        hex_encode_hash(env, &entry.hash),
    );
    String::from_str(env, &rs)
}

/// Extended CSV row including state snapshots.
pub fn format_csv_row_full(env: &Env, entry: &AuditLog) -> String {
    let rs: RustString = format!(
        "{},{},{},{:?},{},{:?},{:?},{},{},{},{}",
        entry.seq,
        entry.timestamp,
        event_type_name(entry.event_type),
        soroban_str(&entry.actor.to_string()),
        entry.permissions,
        symbol_to_rust(&entry.portfolio),
        symbol_to_rust(&entry.outcome),
        csv_escape(&soroban_str(&entry.detail)),
        hex_encode_hash(env, &entry.hash),
        csv_escape(&snapshot_to_compact(env, &entry.state_before)),
        csv_escape(&snapshot_to_compact(env, &entry.state_after)),
    );
    String::from_str(env, &rs)
}

/// JSON-Lines batch exporter: one `String` per audit entry, no header.
/// We use JSONL rather than a top-level JSON array because Soroban strings
/// have a per-call size budget.
pub fn format_jsonl(env: &Env, entries: &Vec<AuditLog>) -> Vec<String> {
    let mut out = Vec::new(env);
    for entry in entries.iter() {
        out.push_back(format_json_entry(env, &entry));
    }
    out
}

/// Extended JSON-Lines exporter including state snapshots.
pub fn format_jsonl_full(env: &Env, entries: &Vec<AuditLog>) -> Vec<String> {
    let mut out = Vec::new(env);
    for entry in entries.iter() {
        out.push_back(format_json_entry_full(env, &entry));
    }
    out
}

/// CSV batch exporter: first row is the header, subsequent rows are entries.
pub fn format_csv(env: &Env, entries: &Vec<AuditLog>) -> Vec<String> {
    let mut out = Vec::new(env);
    out.push_back(String::from_str(env, CSV_HEADER));
    for entry in entries.iter() {
        out.push_back(format_csv_row(env, &entry));
    }
    out
}

/// Extended CSV exporter including state snapshots.
pub fn format_csv_full(env: &Env, entries: &Vec<AuditLog>) -> Vec<String> {
    let mut out = Vec::new(env);
    out.push_back(String::from_str(env, CSV_HEADER_FULL));
    for entry in entries.iter() {
        out.push_back(format_csv_row_full(env, &entry));
    }
    out
}

// ---- internals ----

/// Hex-encode a 32-byte hash for human-readable output.
fn hex_encode_hash(_env: &Env, hash: &soroban_sdk::BytesN<32>) -> RustString {
    let bytes = hash.to_array();
    let mut out = RustString::with_capacity(64);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes.iter() {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Convert a StateSnapshot to a compact string representation.
fn snapshot_to_compact(_env: &Env, snapshot: &crate::records::StateSnapshot) -> RustString {
    let mut out = RustString::from("{");
    let mut first = true;
    for entry in snapshot.fields.iter() {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&symbol_to_rust(&entry.key));
        out.push(':');
        out.push_str(&RustString::from(entry.value.to_string()));
    }
    out.push('}');
    out
}

/// Convert a StateSnapshot to a JSON array string.
fn snapshot_to_json(_env: &Env, snapshot: &crate::records::StateSnapshot) -> RustString {
    let mut out = RustString::from("[");
    let mut first = true;
    for entry in snapshot.fields.iter() {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str("{\"key\":\"");
        out.push_str(&symbol_to_rust(&entry.key));
        out.push_str("\",\"value\":");
        out.push_str(&RustString::from(entry.value.to_string()));
        out.push('}');
    }
    out.push(']');
    out
}

fn event_type_name(t: AuditEventType) -> &'static str {
    match t {
        AuditEventType::Rebalance => "Rebalance",
        AuditEventType::Stake => "Stake",
        AuditEventType::Unstake => "Unstake",
        AuditEventType::EmergencyUnstake => "EmergencyUnstake",
        AuditEventType::YieldAccrual => "YieldAccrual",
        AuditEventType::Deposit => "Deposit",
        AuditEventType::Withdrawal => "Withdrawal",
        AuditEventType::ScheduleChange => "ScheduleChange",
        AuditEventType::AdminAction => "AdminAction",
        AuditEventType::PortfolioCreated => "PortfolioCreated",
        AuditEventType::RoleChange => "RoleChange",
        AuditEventType::YieldClaim => "YieldClaim",
        AuditEventType::GovernanceProposal => "GovernanceProposal",
        AuditEventType::GovernanceVote => "GovernanceVote",
        AuditEventType::TreasuryAction => "TreasuryAction",
        AuditEventType::EmergencyPause => "EmergencyPause",
        AuditEventType::TradeExecution => "TradeExecution",
        AuditEventType::OrderPlaced => "OrderPlaced",
        AuditEventType::OrderCancelled => "OrderCancelled",
        AuditEventType::FeeCollection => "FeeCollection",
        AuditEventType::Custom => "Custom",
    }
}

/// Escape a string for JSON output.
fn json_escape(s: &str) -> RustString {
    let mut out = RustString::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

/// Escape a string for CSV output (wrap in quotes if needed; double inner quotes).
fn csv_escape(s: &str) -> RustString {
    let needs_quoting = s.contains(',') || s.contains('"') || s.contains('\n');
    if !needs_quoting {
        return RustString::from(s);
    }
    let mut out = RustString::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push_str("\"\"");
        } else {
            out.push(c);
        }
    }
    out.push('"');
    out
}
