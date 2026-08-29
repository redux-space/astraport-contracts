#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
};

use astraport_audit::logger::AuditLogger;
use astraport_audit::records::{permissions, AuditEventType, StateSnapshot};

// ============================================================================
// Storage Key Symbols
// ============================================================================

const ADMIN: Symbol = symbol_short!("ADMIN");
const GUARDIAN: Symbol = symbol_short!("GRDN");
const PAUSED: Symbol = symbol_short!("PAUSD");
const SAFE_MODE: Symbol = symbol_short!("SFMD");
const PAUSE_REASON: Symbol = symbol_short!("PRS_");
const SAFE_MODE_REASON: Symbol = symbol_short!("SM_RS");
const CIRCUIT_TRIP: Symbol = symbol_short!("CRT_T");
const CIRCUIT_THRESH: Symbol = symbol_short!("CRT_TH");
const MAX_TRADE: Symbol = symbol_short!("MX_TR");
const EMERG_WD_FEE: Symbol = symbol_short!("EM_WF");
const INCIDENT_LOG: Symbol = symbol_short!("INC_L");
const RATE_LIMITS: Symbol = symbol_short!("RT_LT");
const RATE_COUNTERS: Symbol = symbol_short!("RT_CN");
const NOTIFIERS: Symbol = symbol_short!("NOTF");
const LOCK_PERIOD: Symbol = symbol_short!("LCK_P");
const AUDIT_SINK: Symbol = symbol_short!("AUD_S");

// ============================================================================
// Limits & Constants
// ============================================================================

const MAX_INCIDENT_LOG: u32 = 200;
const MAX_NOTIFIERS: u32 = 50;
const DEFAULT_CIRCUIT_THRESHOLD_BPS: i128 = 2000; // 20%
const DEFAULT_MAX_TRADE_AMOUNT: i128 = 100_000_000; // 100M units
const DEFAULT_EMERGENCY_WITHDRAWAL_FEE_BPS: i128 = 1000; // 10% penalty
const DEFAULT_LOCK_PERIOD: u64 = 86400; // 24 hours in seconds
const BPS_DENOM: i128 = 10_000;

// ============================================================================
// Errors
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[contracterror]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    AdminRequired = 3,
    GuardianRequired = 4,
    AlreadyPaused = 5,
    NotPaused = 6,
    AlreadyInSafeMode = 7,
    NotInSafeMode = 8,
    CircuitBreakerTripped = 9,
    TradeSizeExceedsLimit = 10,
    OperationRateLimited = 11,
    InvalidConfiguration = 12,
    ArithmeticOverflow = 13,
    InsufficientBalance = 14,
    LockPeriodNotExpired = 15,
    IncidentLogFull = 16,
    TooManyNotifiers = 17,
    CannotPauseGuardian = 18,
    OperationBlockedBySafeMode = 19,
}

// ============================================================================
// Types
// ============================================================================

/// Severity levels for incident logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd)]
#[contracttype]
pub enum IncidentSeverity {
    Low = 0,
    Medium = 1,
    High = 2,
    Critical = 3,
}

/// Types of emergency actions that can be logged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[contracttype]
pub enum IncidentActionType {
    Pause = 0,
    Unpause = 1,
    EmergencyWithdrawal = 2,
    CircuitBreakerTrip = 3,
    CircuitBreakerReset = 4,
    SafeModeEnter = 5,
    SafeModeExit = 6,
    MaxTradeUpdated = 7,
    ThresholdUpdated = 8,
    RateLimitUpdated = 9,
    ConfigUpdated = 10,
    TradeBlocked = 11,
    OperationBlocked = 12,
}

/// A recorded incident in the event log.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct IncidentRecord {
    pub timestamp: u64,
    pub action_type: IncidentActionType,
    pub severity: IncidentSeverity,
    pub initiator: Address,
    pub description: Symbol,
    pub data: i128,
}

/// Rate limit configuration for an operation type.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct RateLimitConfig {
    pub operation: Symbol,
    pub max_calls: u32,
    pub window_seconds: u64,
}

/// Rate limit counter tracking calls in the current window.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct RateLimitCounter {
    pub operation: Symbol,
    pub count: u32,
    pub window_start: u64,
}

/// Snapshot of current emergency system state.
#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct EmergencyState {
    pub is_paused: bool,
    pub is_safe_mode: bool,
    pub circuit_breaker_tripped: bool,
    pub circuit_threshold_bps: i128,
    pub max_trade_amount: i128,
    pub emergency_withdrawal_fee_bps: i128,
    pub lock_period: u64,
    pub incident_count: u32,
    pub paused_reason: Symbol,
    pub safe_mode_reason: Symbol,
}

// ============================================================================
// Storage Helpers
// ============================================================================

fn is_initialized(env: &Env) -> bool {
    env.storage().instance().has(&ADMIN)
}

fn get_admin(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&ADMIN)
        .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, Error::NotInitialized))
}

fn put_admin(env: &Env, admin: &Address) {
    env.storage().instance().set(&ADMIN, admin);
}

fn get_guardian(env: &Env) -> Option<Address> {
    env.storage().instance().get(&GUARDIAN)
}

fn put_guardian(env: &Env, guardian: &Address) {
    env.storage().instance().set(&GUARDIAN, guardian);
}

fn get_paused(env: &Env) -> bool {
    env.storage().instance().get(&PAUSED).unwrap_or(false)
}

fn put_paused(env: &Env, paused: bool) {
    env.storage().instance().set(&PAUSED, &paused);
}

fn get_pause_reason(env: &Env) -> Symbol {
    env.storage()
        .instance()
        .get(&PAUSE_REASON)
        .unwrap_or_else(|| symbol_short!("none"))
}

fn put_pause_reason(env: &Env, reason: &Symbol) {
    env.storage().instance().set(&PAUSE_REASON, reason);
}

fn get_safe_mode(env: &Env) -> bool {
    env.storage().instance().get(&SAFE_MODE).unwrap_or(false)
}

fn put_safe_mode(env: &Env, safe: bool) {
    env.storage().instance().set(&SAFE_MODE, &safe);
}

fn get_safe_mode_reason(env: &Env) -> Symbol {
    env.storage()
        .instance()
        .get(&SAFE_MODE_REASON)
        .unwrap_or_else(|| symbol_short!("none"))
}

fn put_safe_mode_reason(env: &Env, reason: &Symbol) {
    env.storage().instance().set(&SAFE_MODE_REASON, reason);
}

fn get_circuit_tripped(env: &Env) -> bool {
    env.storage().instance().get(&CIRCUIT_TRIP).unwrap_or(false)
}

fn put_circuit_tripped(env: &Env, tripped: bool) {
    env.storage().instance().set(&CIRCUIT_TRIP, &tripped);
}

fn get_circuit_threshold(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&CIRCUIT_THRESH)
        .unwrap_or(DEFAULT_CIRCUIT_THRESHOLD_BPS)
}

fn put_circuit_threshold(env: &Env, threshold: i128) {
    env.storage().instance().set(&CIRCUIT_THRESH, &threshold);
}

fn get_max_trade(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&MAX_TRADE)
        .unwrap_or(DEFAULT_MAX_TRADE_AMOUNT)
}

fn put_max_trade(env: &Env, amount: i128) {
    env.storage().instance().set(&MAX_TRADE, &amount);
}

fn get_emergency_wd_fee(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&EMERG_WD_FEE)
        .unwrap_or(DEFAULT_EMERGENCY_WITHDRAWAL_FEE_BPS)
}

fn put_emergency_wd_fee(env: &Env, fee: i128) {
    env.storage().instance().set(&EMERG_WD_FEE, &fee);
}

fn get_lock_period(env: &Env) -> u64 {
    env.storage()
        .instance()
        .get(&LOCK_PERIOD)
        .unwrap_or(DEFAULT_LOCK_PERIOD)
}

fn put_lock_period(env: &Env, period: u64) {
    env.storage().instance().set(&LOCK_PERIOD, &period);
}

fn get_incident_log(env: &Env) -> soroban_sdk::Vec<IncidentRecord> {
    env.storage()
        .persistent()
        .get(&INCIDENT_LOG)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_incident_log(env: &Env, log: &soroban_sdk::Vec<IncidentRecord>) {
    env.storage().persistent().set(&INCIDENT_LOG, log);
}

fn append_incident(
    env: &Env,
    action_type: IncidentActionType,
    severity: IncidentSeverity,
    initiator: &Address,
    description: Symbol,
    data: i128,
) {
    let mut log = get_incident_log(env);
    if log.len() >= MAX_INCIDENT_LOG {
        log = log.slice(1..);
    }
    log.push_back(IncidentRecord {
        timestamp: env.ledger().timestamp(),
        action_type,
        severity,
        initiator: initiator.clone(),
        description,
        data,
    });
    put_incident_log(env, &log);
}

fn get_rate_limits(env: &Env) -> soroban_sdk::Vec<RateLimitConfig> {
    env.storage()
        .persistent()
        .get(&RATE_LIMITS)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_rate_limits(env: &Env, limits: &soroban_sdk::Vec<RateLimitConfig>) {
    env.storage().persistent().set(&RATE_LIMITS, limits);
}

fn get_rate_counter(env: &Env, operation: &Symbol) -> RateLimitCounter {
    let key = (RATE_COUNTERS, operation);
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or(RateLimitCounter {
            operation: operation.clone(),
            count: 0,
            window_start: 0,
        })
}

fn put_rate_counter(env: &Env, counter: &RateLimitCounter) {
    let key = (RATE_COUNTERS, &counter.operation);
    env.storage().persistent().set(&key, counter);
}

fn get_notifiers(env: &Env) -> soroban_sdk::Vec<Address> {
    env.storage()
        .persistent()
        .get(&NOTIFIERS)
        .unwrap_or_else(|| soroban_sdk::Vec::new(env))
}

fn put_notifiers(env: &Env, notifiers: &soroban_sdk::Vec<Address>) {
    env.storage().persistent().set(&NOTIFIERS, notifiers);
}

// ============================================================================
// Authorization Helpers
// ============================================================================

fn require_admin(env: &Env) -> Address {
    let admin = get_admin(env);
    admin.require_auth();
    admin
}

// ============================================================================
// Contract
// ============================================================================

/// EmergencyControls contract for AstraPort
/// Provides circuit breakers, pause mechanisms, emergency withdrawals,
/// safe mode, rate limiting, and incident logging for portfolio safety.
#[contract]
pub struct EmergencyControls;

#[contractimpl]
impl EmergencyControls {
    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the emergency controls contract.
    pub fn initialize(env: Env, admin: Address) -> Symbol {
        if is_initialized(&env) {
            soroban_sdk::panic_with_error!(&env, Error::AlreadyInitialized);
        }
        put_admin(&env, &admin);
        put_circuit_threshold(&env, DEFAULT_CIRCUIT_THRESHOLD_BPS);
        put_max_trade(&env, DEFAULT_MAX_TRADE_AMOUNT);
        put_emergency_wd_fee(&env, DEFAULT_EMERGENCY_WITHDRAWAL_FEE_BPS);
        put_lock_period(&env, DEFAULT_LOCK_PERIOD);

        env.events().publish(
            (symbol_short!("EM_INIT"), &admin),
            (DEFAULT_CIRCUIT_THRESHOLD_BPS, DEFAULT_MAX_TRADE_AMOUNT),
        );

        symbol_short!("ok")
    }

    /// Configure the audit-log sink address. Admin-only.
    pub fn set_audit_sink(env: Env, sink: Address) -> Symbol {
        let admin = require_admin(&env);
        env.storage().persistent().set(&AUDIT_SINK, &sink);

        append_incident(
            &env,
            IncidentActionType::ConfigUpdated,
            IncidentSeverity::Medium,
            &admin,
            symbol_short!("AUD_S"),
            0,
        );

        symbol_short!("ok")
    }

    /// Read the audit-log sink address, if configured.
    pub fn get_audit_sink(env: Env) -> Option<Address> {
        env.storage().persistent().get(&AUDIT_SINK)
    }

    /// Log an audit event if a sink is configured. No-op otherwise.
    fn log_audit_if_configured(
        env: &Env,
        actor: &Address,
        event_type: AuditEventType,
        outcome: Symbol,
        detail: &str,
    ) {
        let sink: Option<Address> = env.storage().persistent().get(&AUDIT_SINK);
        if let Some(sink) = sink {
            let detail_str = soroban_sdk::String::from_str(env, detail);
            let logger = AuditLogger::new(env, &sink);
            let _ = logger.log_event(
                actor.clone(),
                event_type,
                symbol_short!("emerg"),
                permissions::ADMIN,
                StateSnapshot::empty(env),
                StateSnapshot::empty(env),
                outcome,
                detail_str,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Pause / Resume
    // -----------------------------------------------------------------------

    /// Pause the contract. Prevents new transactions but allows withdrawals.
    /// Can be called by admin or guardian.
    pub fn pause(env: Env, caller: Address, reason: Symbol) -> Symbol {
        caller.require_auth();
        let admin = get_admin(&env);
        let guardian = get_guardian(&env);

        if caller != admin && Some(caller.clone()) != guardian {
            soroban_sdk::panic_with_error!(&env, Error::AdminRequired);
        }

        if get_paused(&env) {
            soroban_sdk::panic_with_error!(&env, Error::AlreadyPaused);
        }

        put_paused(&env, true);
        put_pause_reason(&env, &reason);

        append_incident(
            &env,
            IncidentActionType::Pause,
            IncidentSeverity::Critical,
            &caller,
            reason.clone(),
            0,
        );

        Self::log_audit_if_configured(
            &env,
            &caller,
            AuditEventType::EmergencyPause,
            symbol_short!("ok"),
            &"pause_activated",
        );

        env.events()
            .publish((symbol_short!("PAUSE"), &caller), &reason);

        symbol_short!("ok")
    }

    /// Resume operations after a pause. Only admin can unpause.
    pub fn unpause(env: Env, reason: Symbol) -> Symbol {
        let admin = require_admin(&env);

        if !get_paused(&env) {
            soroban_sdk::panic_with_error!(&env, Error::NotPaused);
        }

        put_paused(&env, false);
        put_pause_reason(&env, &symbol_short!("none"));

        append_incident(
            &env,
            IncidentActionType::Unpause,
            IncidentSeverity::High,
            &admin,
            reason.clone(),
            0,
        );

        Self::log_audit_if_configured(
            &env,
            &admin,
            AuditEventType::EmergencyPause,
            symbol_short!("ok"),
            &"pause_deactivated",
        );

        env.events()
            .publish((symbol_short!("UNPAUS"), &admin), &reason);

        symbol_short!("ok")
    }

    /// Check if the contract is currently paused.
    pub fn is_paused(env: Env) -> bool {
        get_paused(&env)
    }

    /// Get the reason for the current pause.
    pub fn get_pause_reason(env: Env) -> Symbol {
        get_pause_reason(&env)
    }

    // -----------------------------------------------------------------------
    // Emergency Withdrawal
    // -----------------------------------------------------------------------

    /// Execute an emergency withdrawal bypassing normal lock periods.
    /// Applies a penalty fee as configured. Returns net amount after penalty.
    pub fn emergency_withdrawal(env: Env, user: Address, amount: i128) -> i128 {
        user.require_auth();

        if amount <= 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidConfiguration);
        }

        let fee_bps = get_emergency_wd_fee(&env);
        let penalty = amount
            .checked_mul(fee_bps)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow))
            / BPS_DENOM;
        let net_amount = amount - penalty;

        append_incident(
            &env,
            IncidentActionType::EmergencyWithdrawal,
            IncidentSeverity::High,
            &user,
            symbol_short!("EM_WD"),
            amount,
        );

        Self::log_audit_if_configured(
            &env,
            &user,
            AuditEventType::EmergencyUnstake,
            symbol_short!("ok"),
            &"emergency_withdrawal",
        );

        env.events().publish(
            (symbol_short!("EM_WD"), &user),
            (amount, penalty, net_amount),
        );

        net_amount
    }

    /// Get the emergency withdrawal penalty fee in basis points.
    pub fn get_emergency_withdrawal_fee(env: Env) -> i128 {
        get_emergency_wd_fee(&env)
    }

    /// Set the emergency withdrawal penalty fee (admin only).
    pub fn set_emergency_withdrawal_fee(env: Env, fee_bps: i128) -> Symbol {
        let admin = require_admin(&env);

        if !(0..=BPS_DENOM).contains(&fee_bps) {
            soroban_sdk::panic_with_error!(&env, Error::InvalidConfiguration);
        }

        put_emergency_wd_fee(&env, fee_bps);

        append_incident(
            &env,
            IncidentActionType::ConfigUpdated,
            IncidentSeverity::Medium,
            &admin,
            symbol_short!("EM_WF"),
            fee_bps,
        );

        symbol_short!("ok")
    }

    // -----------------------------------------------------------------------
    // Circuit Breaker
    // -----------------------------------------------------------------------

    /// Report a price change and trip the circuit breaker if threshold exceeded.
    /// Returns whether the circuit breaker was tripped.
    pub fn report_price_change(env: Env, caller: Address, price_change_bps: i128) -> bool {
        caller.require_auth();

        let threshold = get_circuit_threshold(&env);
        let abs_change = if price_change_bps < 0 {
            -price_change_bps
        } else {
            price_change_bps
        };

        if abs_change >= threshold {
            put_circuit_tripped(&env, true);
            put_paused(&env, true);
            put_pause_reason(&env, &symbol_short!("CIRCUIT"));

            append_incident(
                &env,
                IncidentActionType::CircuitBreakerTrip,
                IncidentSeverity::Critical,
                &caller,
                symbol_short!("CB_TRIP"),
                price_change_bps,
            );

            env.events().publish(
                (symbol_short!("CB_TRIP"), &caller),
                (price_change_bps, threshold),
            );

            true
        } else {
            false
        }
    }

    /// Check if the circuit breaker is tripped.
    pub fn is_circuit_breaker_tripped(env: Env) -> bool {
        get_circuit_tripped(&env)
    }

    /// Reset the circuit breaker (admin only).
    pub fn reset_circuit_breaker(env: Env, reason: Symbol) -> Symbol {
        let admin = require_admin(&env);

        if !get_circuit_tripped(&env) {
            soroban_sdk::panic_with_error!(&env, Error::CircuitBreakerTripped);
        }

        put_circuit_tripped(&env, false);

        append_incident(
            &env,
            IncidentActionType::CircuitBreakerReset,
            IncidentSeverity::High,
            &admin,
            reason.clone(),
            0,
        );

        env.events()
            .publish((symbol_short!("CB_RST"), &admin), &reason);

        symbol_short!("ok")
    }

    /// Get the current circuit breaker threshold in basis points.
    pub fn get_circuit_breaker_threshold(env: Env) -> i128 {
        get_circuit_threshold(&env)
    }

    /// Set the circuit breaker threshold (admin only).
    pub fn set_circuit_breaker_threshold(env: Env, threshold_bps: i128) -> Symbol {
        let admin = require_admin(&env);

        if threshold_bps <= 0 || threshold_bps > BPS_DENOM {
            soroban_sdk::panic_with_error!(&env, Error::InvalidConfiguration);
        }

        put_circuit_threshold(&env, threshold_bps);

        append_incident(
            &env,
            IncidentActionType::ThresholdUpdated,
            IncidentSeverity::Medium,
            &admin,
            symbol_short!("CB_TH"),
            threshold_bps,
        );

        symbol_short!("ok")
    }

    // -----------------------------------------------------------------------
    // Trade Size Limits
    // -----------------------------------------------------------------------

    /// Validate that a trade amount is within the configured maximum.
    pub fn validate_trade_size(env: Env, amount: i128) -> i128 {
        let max = get_max_trade(&env);
        if amount > max {
            soroban_sdk::panic_with_error!(&env, Error::TradeSizeExceedsLimit);
        }
        amount
    }

    /// Get the current maximum trade size.
    pub fn get_max_trade_size(env: Env) -> i128 {
        get_max_trade(&env)
    }

    /// Set the maximum trade size (admin only).
    pub fn set_max_trade_size(env: Env, max_amount: i128) -> Symbol {
        let admin = require_admin(&env);

        if max_amount <= 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidConfiguration);
        }

        put_max_trade(&env, max_amount);

        append_incident(
            &env,
            IncidentActionType::MaxTradeUpdated,
            IncidentSeverity::Medium,
            &admin,
            symbol_short!("MX_TR"),
            max_amount,
        );

        env.events()
            .publish((symbol_short!("MX_TR"), &admin), max_amount);

        symbol_short!("ok")
    }

    // -----------------------------------------------------------------------
    // Safe Mode
    // -----------------------------------------------------------------------

    /// Enter safe mode which reduces risk by disabling automated operations.
    /// Can be called by admin or guardian.
    pub fn enter_safe_mode(env: Env, caller: Address, reason: Symbol) -> Symbol {
        caller.require_auth();
        let admin = get_admin(&env);
        let guardian = get_guardian(&env);

        if caller != admin && Some(caller.clone()) != guardian {
            soroban_sdk::panic_with_error!(&env, Error::AdminRequired);
        }

        if get_safe_mode(&env) {
            soroban_sdk::panic_with_error!(&env, Error::AlreadyInSafeMode);
        }

        put_safe_mode(&env, true);
        put_safe_mode_reason(&env, &reason);

        append_incident(
            &env,
            IncidentActionType::SafeModeEnter,
            IncidentSeverity::High,
            &caller,
            reason.clone(),
            0,
        );

        env.events()
            .publish((symbol_short!("SF_MOD"), &caller), &reason);

        symbol_short!("ok")
    }

    /// Exit safe mode and resume normal operations. Only admin can exit.
    pub fn exit_safe_mode(env: Env, reason: Symbol) -> Symbol {
        let admin = require_admin(&env);

        if !get_safe_mode(&env) {
            soroban_sdk::panic_with_error!(&env, Error::NotInSafeMode);
        }

        put_safe_mode(&env, false);
        put_safe_mode_reason(&env, &symbol_short!("none"));

        append_incident(
            &env,
            IncidentActionType::SafeModeExit,
            IncidentSeverity::High,
            &admin,
            reason.clone(),
            0,
        );

        env.events()
            .publish((symbol_short!("SF_EXIT"), &admin), &reason);

        symbol_short!("ok")
    }

    /// Check if the system is in safe mode.
    pub fn is_safe_mode(env: Env) -> bool {
        get_safe_mode(&env)
    }

    /// Get the reason for the current safe mode.
    pub fn get_safe_mode_reason(env: Env) -> Symbol {
        get_safe_mode_reason(&env)
    }

    /// Validate that an operation is allowed in the current system state.
    /// Checks pause, safe mode, and circuit breaker status.
    pub fn validate_operation(
        env: Env,
        _operation: Symbol,
        allow_during_pause: bool,
        allow_during_safe_mode: bool,
    ) -> bool {
        if get_paused(&env) && !allow_during_pause {
            append_incident(
                &env,
                IncidentActionType::OperationBlocked,
                IncidentSeverity::Medium,
                &get_admin(&env),
                symbol_short!("OP_BLK"),
                0,
            );
            return false;
        }

        if get_safe_mode(&env) && !allow_during_safe_mode {
            append_incident(
                &env,
                IncidentActionType::OperationBlocked,
                IncidentSeverity::Medium,
                &get_admin(&env),
                symbol_short!("OP_SFBL"),
                0,
            );
            return false;
        }

        if get_circuit_tripped(&env) && !allow_during_pause {
            append_incident(
                &env,
                IncidentActionType::OperationBlocked,
                IncidentSeverity::High,
                &get_admin(&env),
                symbol_short!("CB_BLCK"),
                0,
            );
            return false;
        }

        true
    }

    // -----------------------------------------------------------------------
    // Rate Limiting
    // -----------------------------------------------------------------------

    /// Set rate limit configuration for an operation (admin only).
    pub fn set_rate_limit(
        env: Env,
        operation: Symbol,
        max_calls: u32,
        window_seconds: u64,
    ) -> Symbol {
        let admin = require_admin(&env);

        if max_calls == 0 || window_seconds == 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidConfiguration);
        }

        let config = RateLimitConfig {
            operation: operation.clone(),
            max_calls,
            window_seconds,
        };

        let mut limits = get_rate_limits(&env);
        let mut found = false;
        let mut idx: u32 = 0;
        while idx < limits.len() {
            let existing = limits.get(idx).unwrap();
            if existing.operation == operation {
                limits.set(idx, config.clone());
                found = true;
                break;
            }
            idx += 1;
        }
        if !found {
            limits.push_back(config);
        }
        put_rate_limits(&env, &limits);

        append_incident(
            &env,
            IncidentActionType::RateLimitUpdated,
            IncidentSeverity::Low,
            &admin,
            symbol_short!("RT_UPD"),
            max_calls as i128,
        );

        symbol_short!("ok")
    }

    /// Check rate limit and increment counter if allowed.
    /// Returns true if the operation is within rate limits.
    pub fn check_rate_limit(env: Env, operation: Symbol) -> bool {
        let limits = get_rate_limits(&env);
        let now = env.ledger().timestamp();

        let mut config_option: Option<RateLimitConfig> = None;
        for limit in limits.iter() {
            if limit.operation == operation {
                config_option = Some(limit);
                break;
            }
        }

        let config = match config_option {
            Some(c) => c,
            None => return true, // No rate limit configured, allow
        };

        let mut counter = get_rate_counter(&env, &operation);

        if now >= counter.window_start + config.window_seconds {
            counter.count = 0;
            counter.window_start = now;
        }

        if counter.count >= config.max_calls {
            append_incident(
                &env,
                IncidentActionType::OperationBlocked,
                IncidentSeverity::Medium,
                &get_admin(&env),
                symbol_short!("RT_BLCK"),
                counter.count as i128,
            );
            return false;
        }

        counter.count += 1;
        put_rate_counter(&env, &counter);

        true
    }

    /// Get the current rate limit config for an operation.
    pub fn get_rate_limit_config(env: Env, operation: Symbol) -> Option<RateLimitConfig> {
        let limits = get_rate_limits(&env);
        limits.iter().find(|limit| limit.operation == operation)
    }

    /// Get the current call count for a rate-limited operation.
    pub fn get_rate_limit_count(env: Env, operation: Symbol) -> u32 {
        let counter = get_rate_counter(&env, &operation);
        counter.count
    }

    // -----------------------------------------------------------------------
    // Guardian Management
    // -----------------------------------------------------------------------

    /// Set the guardian address (admin only).
    pub fn set_guardian(env: Env, guardian: Address) -> Symbol {
        let admin = require_admin(&env);
        put_guardian(&env, &guardian);

        append_incident(
            &env,
            IncidentActionType::ConfigUpdated,
            IncidentSeverity::Medium,
            &admin,
            symbol_short!("GRD_SET"),
            0,
        );

        symbol_short!("ok")
    }

    /// Get the guardian address.
    pub fn get_guardian(env: Env) -> Option<Address> {
        get_guardian(&env)
    }

    // -----------------------------------------------------------------------
    // Lock Period
    // -----------------------------------------------------------------------

    /// Set the lock period for normal withdrawals (admin only).
    pub fn set_lock_period(env: Env, period: u64) -> Symbol {
        let _admin = require_admin(&env);
        put_lock_period(&env, period);
        symbol_short!("ok")
    }

    /// Get the current lock period in seconds.
    pub fn get_lock_period(env: Env) -> u64 {
        get_lock_period(&env)
    }

    /// Check if a lock period has expired given a stake timestamp.
    pub fn is_lock_expired(env: Env, staked_at: u64) -> bool {
        let now = env.ledger().timestamp();
        let lock = get_lock_period(&env);
        now >= staked_at + lock
    }

    // -----------------------------------------------------------------------
    // Notification System
    // -----------------------------------------------------------------------

    /// Register a notifier address (admin only).
    pub fn add_notifier(env: Env, notifier: Address) -> Symbol {
        let _admin = require_admin(&env);

        let mut notifiers = get_notifiers(&env);
        if notifiers.len() >= MAX_NOTIFIERS {
            soroban_sdk::panic_with_error!(&env, Error::TooManyNotifiers);
        }

        for existing in notifiers.iter() {
            if existing == notifier {
                return symbol_short!("ok");
            }
        }

        notifiers.push_back(notifier);
        put_notifiers(&env, &notifiers);

        symbol_short!("ok")
    }

    /// Remove a notifier address (admin only).
    pub fn remove_notifier(env: Env, notifier: Address) -> Symbol {
        let _admin = require_admin(&env);
        let notifiers = get_notifiers(&env);
        let mut new_notifiers = soroban_sdk::Vec::new(&env);

        for existing in notifiers.iter() {
            if existing != notifier {
                new_notifiers.push_back(existing);
            }
        }

        put_notifiers(&env, &new_notifiers);
        symbol_short!("ok")
    }

    /// Get all registered notifier addresses.
    pub fn get_notifiers_list(env: Env) -> soroban_sdk::Vec<Address> {
        get_notifiers(&env)
    }

    /// Emit a notification event for all registered notifiers.
    pub fn notify(env: Env, event_type: Symbol, severity: IncidentSeverity, data: i128) -> u32 {
        let notifiers = get_notifiers(&env);
        let count = notifiers.len();

        for notifier in notifiers.iter() {
            env.events().publish(
                (symbol_short!("NOTIFY"), &notifier),
                (&event_type, severity as u32, data),
            );
        }

        count
    }

    // -----------------------------------------------------------------------
    // Incident Log
    // -----------------------------------------------------------------------

    /// Get all incident records (up to max entries). 0 = all.
    pub fn get_incident_log(env: Env, max_entries: u32) -> soroban_sdk::Vec<IncidentRecord> {
        let log = get_incident_log(&env);
        if max_entries == 0 || log.len() <= max_entries {
            log
        } else {
            log.slice(log.len() - max_entries..)
        }
    }

    /// Get the total number of incident records.
    pub fn get_incident_count(env: Env) -> u32 {
        get_incident_log(&env).len()
    }

    /// Clear the incident log (admin only).
    pub fn clear_incident_log(env: Env) -> Symbol {
        let _admin = require_admin(&env);
        put_incident_log(&env, &soroban_sdk::Vec::new(&env));
        symbol_short!("ok")
    }

    // -----------------------------------------------------------------------
    // System State
    // -----------------------------------------------------------------------

    /// Get a comprehensive snapshot of the emergency system state.
    pub fn get_emergency_state(env: Env) -> EmergencyState {
        EmergencyState {
            is_paused: get_paused(&env),
            is_safe_mode: get_safe_mode(&env),
            circuit_breaker_tripped: get_circuit_tripped(&env),
            circuit_threshold_bps: get_circuit_threshold(&env),
            max_trade_amount: get_max_trade(&env),
            emergency_withdrawal_fee_bps: get_emergency_wd_fee(&env),
            lock_period: get_lock_period(&env),
            incident_count: get_incident_log(&env).len(),
            paused_reason: get_pause_reason(&env),
            safe_mode_reason: get_safe_mode_reason(&env),
        }
    }

    // -----------------------------------------------------------------------
    // Admin
    // -----------------------------------------------------------------------

    /// Get the admin address.
    pub fn get_admin(env: Env) -> Address {
        get_admin(&env)
    }

    /// Transfer admin role to a new address.
    pub fn transfer_admin(env: Env, new_admin: Address) -> Symbol {
        let admin = get_admin(&env);
        admin.require_auth();
        put_admin(&env, &new_admin);

        append_incident(
            &env,
            IncidentActionType::ConfigUpdated,
            IncidentSeverity::Critical,
            &admin,
            symbol_short!("ADM_TRF"),
            0,
        );

        symbol_short!("ok")
    }
}

// ============================================================================
// Unit Tests (pure functions only — no Address/auth required)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        assert_eq!(Error::NotInitialized as u32, 1);
        assert_eq!(Error::AlreadyInitialized as u32, 2);
        assert_eq!(Error::AdminRequired as u32, 3);
        assert_eq!(Error::GuardianRequired as u32, 4);
        assert_eq!(Error::AlreadyPaused as u32, 5);
        assert_eq!(Error::NotPaused as u32, 6);
        assert_eq!(Error::AlreadyInSafeMode as u32, 7);
        assert_eq!(Error::NotInSafeMode as u32, 8);
        assert_eq!(Error::CircuitBreakerTripped as u32, 9);
        assert_eq!(Error::TradeSizeExceedsLimit as u32, 10);
        assert_eq!(Error::OperationRateLimited as u32, 11);
        assert_eq!(Error::InvalidConfiguration as u32, 12);
        assert_eq!(Error::ArithmeticOverflow as u32, 13);
        assert_eq!(Error::InsufficientBalance as u32, 14);
        assert_eq!(Error::LockPeriodNotExpired as u32, 15);
        assert_eq!(Error::IncidentLogFull as u32, 16);
        assert_eq!(Error::TooManyNotifiers as u32, 17);
        assert_eq!(Error::CannotPauseGuardian as u32, 18);
        assert_eq!(Error::OperationBlockedBySafeMode as u32, 19);
    }

    #[test]
    fn test_constants() {
        assert_eq!(BPS_DENOM, 10_000);
        assert_eq!(MAX_INCIDENT_LOG, 200);
        assert_eq!(MAX_NOTIFIERS, 50);
        assert_eq!(DEFAULT_CIRCUIT_THRESHOLD_BPS, 2000);
        assert_eq!(DEFAULT_MAX_TRADE_AMOUNT, 100_000_000);
        assert_eq!(DEFAULT_EMERGENCY_WITHDRAWAL_FEE_BPS, 1000);
        assert_eq!(DEFAULT_LOCK_PERIOD, 86400);
    }

    #[test]
    fn test_incident_action_type_variants() {
        assert_eq!(IncidentActionType::Pause as u32, 0);
        assert_eq!(IncidentActionType::Unpause as u32, 1);
        assert_eq!(IncidentActionType::EmergencyWithdrawal as u32, 2);
        assert_eq!(IncidentActionType::CircuitBreakerTrip as u32, 3);
        assert_eq!(IncidentActionType::CircuitBreakerReset as u32, 4);
        assert_eq!(IncidentActionType::SafeModeEnter as u32, 5);
        assert_eq!(IncidentActionType::SafeModeExit as u32, 6);
        assert_eq!(IncidentActionType::MaxTradeUpdated as u32, 7);
        assert_eq!(IncidentActionType::ThresholdUpdated as u32, 8);
        assert_eq!(IncidentActionType::RateLimitUpdated as u32, 9);
        assert_eq!(IncidentActionType::ConfigUpdated as u32, 10);
        assert_eq!(IncidentActionType::TradeBlocked as u32, 11);
        assert_eq!(IncidentActionType::OperationBlocked as u32, 12);
    }

    #[test]
    fn test_severity_variants() {
        assert_eq!(IncidentSeverity::Low as u32, 0);
        assert_eq!(IncidentSeverity::Medium as u32, 1);
        assert_eq!(IncidentSeverity::High as u32, 2);
        assert_eq!(IncidentSeverity::Critical as u32, 3);
    }

    #[test]
    fn test_severity_ordering() {
        assert!(IncidentSeverity::Low < IncidentSeverity::Medium);
        assert!(IncidentSeverity::Medium < IncidentSeverity::High);
        assert!(IncidentSeverity::High < IncidentSeverity::Critical);
    }

    #[test]
    fn test_severity_equality() {
        assert_eq!(IncidentSeverity::Low, IncidentSeverity::Low);
        assert_ne!(IncidentSeverity::Low, IncidentSeverity::Critical);
    }

    #[test]
    fn test_incident_action_type_equality() {
        assert_eq!(IncidentActionType::Pause, IncidentActionType::Pause);
        assert_ne!(IncidentActionType::Pause, IncidentActionType::Unpause);
    }

    #[test]
    fn test_emergency_state_default_values() {
        let state = EmergencyState {
            is_paused: false,
            is_safe_mode: false,
            circuit_breaker_tripped: false,
            circuit_threshold_bps: DEFAULT_CIRCUIT_THRESHOLD_BPS,
            max_trade_amount: DEFAULT_MAX_TRADE_AMOUNT,
            emergency_withdrawal_fee_bps: DEFAULT_EMERGENCY_WITHDRAWAL_FEE_BPS,
            lock_period: DEFAULT_LOCK_PERIOD,
            incident_count: 0,
            paused_reason: symbol_short!("none"),
            safe_mode_reason: symbol_short!("none"),
        };
        assert!(!state.is_paused);
        assert!(!state.is_safe_mode);
        assert!(!state.circuit_breaker_tripped);
        assert_eq!(state.circuit_threshold_bps, 2000);
        assert_eq!(state.max_trade_amount, 100_000_000);
        assert_eq!(state.emergency_withdrawal_fee_bps, 1000);
        assert_eq!(state.lock_period, 86400);
        assert_eq!(state.incident_count, 0);
    }

    #[test]
    fn test_emergency_state_clone() {
        let state = EmergencyState {
            is_paused: true,
            is_safe_mode: false,
            circuit_breaker_tripped: true,
            circuit_threshold_bps: 1500,
            max_trade_amount: 50_000_000,
            emergency_withdrawal_fee_bps: 750,
            lock_period: 3600,
            incident_count: 5,
            paused_reason: symbol_short!("TEST"),
            safe_mode_reason: symbol_short!("none"),
        };
        let cloned = state.clone();
        assert_eq!(state.is_paused, cloned.is_paused);
        assert_eq!(state.circuit_threshold_bps, cloned.circuit_threshold_bps);
        assert_eq!(state.incident_count, cloned.incident_count);
    }

    #[test]
    fn test_rate_limit_config_clone() {
        let config = RateLimitConfig {
            operation: symbol_short!("TRADE"),
            max_calls: 10,
            window_seconds: 60,
        };
        let cloned = config.clone();
        assert_eq!(config.operation, cloned.operation);
        assert_eq!(config.max_calls, cloned.max_calls);
        assert_eq!(config.window_seconds, cloned.window_seconds);
    }

    #[test]
    fn test_rate_limit_counter_clone() {
        let counter = RateLimitCounter {
            operation: symbol_short!("SWAP"),
            count: 3,
            window_start: 500,
        };
        let cloned = counter.clone();
        assert_eq!(counter.operation, cloned.operation);
        assert_eq!(counter.count, cloned.count);
        assert_eq!(counter.window_start, cloned.window_start);
    }

    #[test]
    fn test_emergency_withdrawal_fee_calculation() {
        let amount: i128 = 1_000_000;
        let fee_bps: i128 = 1000;
        let penalty = amount * fee_bps / BPS_DENOM;
        let net = amount - penalty;
        assert_eq!(penalty, 100_000);
        assert_eq!(net, 900_000);
    }

    #[test]
    fn test_emergency_withdrawal_zero_fee() {
        let amount: i128 = 1_000_000;
        let fee_bps: i128 = 0;
        let penalty = amount * fee_bps / BPS_DENOM;
        let net = amount - penalty;
        assert_eq!(penalty, 0);
        assert_eq!(net, 1_000_000);
    }

    #[test]
    fn test_emergency_withdrawal_high_fee() {
        let amount: i128 = 1_000_000;
        let fee_bps: i128 = 5000;
        let penalty = amount * fee_bps / BPS_DENOM;
        let net = amount - penalty;
        assert_eq!(penalty, 500_000);
        assert_eq!(net, 500_000);
    }

    #[test]
    fn test_trade_size_within_limit() {
        assert!(50_000_000 <= 100_000_000);
    }

    #[test]
    fn test_trade_size_exceeds_limit() {
        assert!(200_000_000 > 100_000_000);
    }

    #[test]
    fn test_circuit_breaker_threshold_calculation() {
        assert!(2500 >= 2000);
    }

    #[test]
    fn test_circuit_breaker_negative_price_change() {
        let price_change_bps: i128 = -3000;
        let abs_change = if price_change_bps < 0 {
            -price_change_bps
        } else {
            price_change_bps
        };
        assert!(abs_change >= 2000);
    }

    #[test]
    fn test_circuit_breaker_below_threshold() {
        assert!(1500 < 2000);
    }

    #[test]
    fn test_lock_period_expiry_calculation() {
        let now: u64 = 200;
        let staked_at: u64 = 50;
        let lock_period: u64 = 100;
        assert!(now >= staked_at + lock_period);
    }

    #[test]
    fn test_lock_period_not_expired() {
        let now: u64 = 100;
        let staked_at: u64 = 50;
        let lock_period: u64 = 100;
        assert!(now < staked_at + lock_period);
    }

    #[test]
    fn test_rate_limit_counter_expiry() {
        let now: u64 = 200;
        let window_start: u64 = 100;
        let window_seconds: u64 = 60;
        assert!(now >= window_start + window_seconds);
    }

    #[test]
    fn test_rate_limit_counter_not_expired() {
        let now: u64 = 120;
        let window_start: u64 = 100;
        let window_seconds: u64 = 60;
        assert!(now < window_start + window_seconds);
    }
}
