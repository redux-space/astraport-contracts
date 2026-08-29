//! Query builder for the audit log.
//!
//! `LogQuery` accumulates optional filters; `matches()` evaluates them against
//! a single `AuditLog`. The contract entrypoint `query()` walks the
//! append-only primary index and returns every entry that matches with a
//! cap of `limit`.

use soroban_sdk::testutils::Address as _;
use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol};

use crate::records::{AuditEventType, AuditLog};

/// Filter set for log queries.
///
/// `None` / default fields mean "any"; non-default narrows the search.
/// Boolean flags control which filters are active.
#[contracttype]
#[derive(Debug, Clone)]
pub struct LogQuery {
    /// Inclusive lower bound on `entry.timestamp`. `0` is treated as unset.
    pub from_ts: u64,
    /// Inclusive upper bound on `entry.timestamp`. `0` is treated as unset.
    pub to_ts: u64,
    /// Event-type filter. Ignored when `filter_event_type` is false.
    pub event_type: AuditEventType,
    /// Actor filter address. Only used when `filter_actor` is true.
    pub actor: Address,
    /// Portfolio filter symbol. Ignored when `filter_portfolio` is false.
    pub portfolio: Symbol,
    /// Outcome filter symbol. Only used when `filter_outcome` is true.
    pub outcome: Symbol,
    /// Whether `event_type` filtering is active.
    pub filter_event_type: bool,
    /// Whether `actor` filtering is active.
    pub filter_actor: bool,
    /// Whether `portfolio` filtering is active.
    pub filter_portfolio: bool,
    /// Whether `outcome` filtering is active.
    pub filter_outcome: bool,
    /// Maximum number of entries returned.
    pub limit: u32,
    /// Reserved for future use (e.g. cursor-based pagination).
    pub cursor: u64,
}

impl LogQuery {
    /// Build an empty query with a default limit.
    ///
    /// Uses `Address::generate` for the unused actor field placeholder.
    /// This placeholder is only stored in the struct and never used for
    /// comparison when `filter_actor` is false.
    pub fn new(env: &Env, limit: u32) -> Self {
        Self {
            from_ts: 0,
            to_ts: 0,
            event_type: AuditEventType::Custom,
            actor: Address::generate(env),
            portfolio: symbol_short!(""),
            outcome: symbol_short!(""),
            filter_event_type: false,
            filter_actor: false,
            filter_portfolio: false,
            filter_outcome: false,
            limit,
            cursor: 0,
        }
    }

    /// Restrict the query to events at or after this timestamp.
    pub fn from_ts(mut self, ts: u64) -> Self {
        self.from_ts = ts;
        self
    }

    /// Restrict the query to events at or before this timestamp.
    pub fn to_ts(mut self, ts: u64) -> Self {
        self.to_ts = ts;
        self
    }

    /// Restrict the query to a single event type.
    pub fn event_type(mut self, t: AuditEventType) -> Self {
        self.event_type = t;
        self.filter_event_type = true;
        self
    }

    /// Restrict the query to a single actor.
    pub fn actor(mut self, a: Address) -> Self {
        self.actor = a;
        self.filter_actor = true;
        self
    }

    /// Restrict the query to a single portfolio.
    pub fn portfolio(mut self, p: Symbol) -> Self {
        self.portfolio = p;
        self.filter_portfolio = true;
        self
    }

    /// Restrict the query to a single outcome symbol.
    pub fn outcome(mut self, o: Symbol) -> Self {
        self.outcome = o;
        self.filter_outcome = true;
        self
    }

    /// Override the maximum number of entries returned.
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = limit;
        self
    }

    /// Return `true` when `entry` satisfies every active filter.
    pub fn matches(&self, entry: &AuditLog) -> bool {
        if self.from_ts != 0 && entry.timestamp < self.from_ts {
            return false;
        }
        if self.to_ts != 0 && entry.timestamp > self.to_ts {
            return false;
        }
        if self.filter_event_type && entry.event_type != self.event_type {
            return false;
        }
        if self.filter_actor && entry.actor.to_string() != self.actor.to_string() {
            return false;
        }
        if self.filter_portfolio && entry.portfolio != self.portfolio {
            return false;
        }
        if self.filter_outcome && entry.outcome != self.outcome {
            return false;
        }
        true
    }
}
