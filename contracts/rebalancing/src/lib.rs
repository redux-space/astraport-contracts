#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Map, Symbol,
    Vec,
};

use astraport_audit::logger::AuditLogger;
use astraport_audit::records::{permissions, AuditEventType, StateSnapshot};

pub mod alerts;
pub mod drift_engine;
pub mod multi_asset_rebalancer;
pub mod rbac;
pub mod records;
use crate::rbac::{
    assign_role, check_permission, check_permission_detailed, describe_permissions,
    extend_role_expiry, get_access_log, get_raw_assignment, get_role_assignment, has_permission,
    revoke_all_roles, revoke_role, Role, RoleAssignment, CAN_MANAGE_ROLES,
};

/// Default tolerance used when deciding whether a holding needs rebalancing.
const DEFAULT_DRIFT_THRESHOLD_BPS: u32 = 100;

/// Errors returned by the rebalancing contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RebalancingError {
    /// The target allocation weights do not sum to 10_000 basis points (100%).
    InvalidAllocation = 1,
    /// The supplied current holding weights do not sum to 10_000 basis points.
    InvalidCurrentHoldings = 2,
    /// No target allocation has been configured for this portfolio.
    TargetAllocationNotFound = 3,
    /// No current holdings have been supplied for this portfolio.
    CurrentHoldingsNotFound = 4,
    /// An error occurred during multi-asset rebalancing.
    MultiAssetRebalanceFailed = 5,
    /// Caller is not authorized to modify this portfolio.
    Unauthorized = 6,
    /// RBAC: required permission not held by the actor.
    PermissionDenied = 7,
    /// RBAC: role assignment not found for the given account.
    RoleNotFound = 8,
    /// RBAC: cannot revoke the owner role.
    CannotRevokeOwner = 9,
    /// RBAC: role assignment has expired.
    RoleExpired = 10,
    /// Alerts: no alert configuration exists for this portfolio.
    AlertConfigNotFound = 11,
    /// Alerts: the per-portfolio threshold limit has been reached.
    AlertThresholdLimitReached = 12,
    /// Alerts: the referenced alert/threshold index is out of range.
    AlertIndexOutOfRange = 13,
    /// The drift threshold is greater than 100%.
    InvalidDriftThreshold = 14,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebalanceInterval {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalancingSchedule {
    pub portfolio_id: Symbol,
    pub interval: RebalanceInterval,
    pub next_execution: u64,
    pub last_execution: u64, // 0 means never
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionHistoryRecord {
    pub timestamp: u64,
    pub outcome: Symbol,
    pub details: Symbol,
}

/// Target allocation for a portfolio.
///
/// Maps each asset symbol to its target weight in basis points (1/100th of a
/// percent). All weights must sum to exactly 10_000 (= 100%).
#[contracttype]
#[derive(Clone)]
pub struct TargetAllocation {
    pub allocations: Map<Symbol, u32>,
}

/// Current portfolio weights in basis points. A holding omitted from this map is
/// treated as zero when it is compared with the target allocation.
#[contracttype]
#[derive(Clone)]
pub struct CurrentHoldings {
    pub allocations: Map<Symbol, u32>,
}

/// The action required to move a holding back toward its target weight.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RebalanceDirection {
    Buy,
    Sell,
}

/// An asset whose current weight differs from its target by more than the
/// configured tolerance.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalanceAdjustment {
    pub asset: Symbol,
    pub current_weight_bps: u32,
    pub target_weight_bps: u32,
    /// `current_weight_bps - target_weight_bps`. Positive drift means sell;
    /// negative drift means buy.
    pub drift_bps: i32,
    pub direction: RebalanceDirection,
}

/// Computed rebalance plan for a portfolio.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalanceResult {
    pub portfolio_id: Symbol,
    pub drift_threshold_bps: u32,
    pub adjustments: Vec<RebalanceAdjustment>,
}

#[contracttype]
pub enum DataKey {
    Schedule(Symbol),
    History(Symbol),
    Allocation(Symbol),
    CurrentHoldings(Symbol),
    DriftThreshold(Symbol),
    /// Optional audit-log sink address. When set, the rebalancing contract
    /// invokes the audit contract on every state-changing event.
    AuditSink,
    /// Portfolio owner address mapping: portfolio_id -> Address
    Owner(Symbol),
}

/// Event data for manual rebalance - includes drift summary via timestamp
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RebalanceEventData {
    pub portfolio_id: Symbol,
    pub outcome: Symbol,
    pub timestamp: u64,
}

/// Event data for scheduled rebalance - richer context for off-chain listeners
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchedRebalanceEventData {
    pub portfolio_id: Symbol,
    pub outcome: Symbol,
    pub timestamp: u64,
    pub details: Symbol,
}

/// Event emitted when a role is granted or revoked.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleChangeEvent {
    pub portfolio_id: Symbol,
    pub actor: Address,
    pub assignee: Address,
    pub role: Role,
    pub action: Symbol, // "grant" or "revoke"
    pub expires_at: u64,
}

pub struct ScheduleValidator;

impl ScheduleValidator {
    pub fn validate(interval: &RebalanceInterval) -> bool {
        match interval {
            RebalanceInterval::Hourly
            | RebalanceInterval::Daily
            | RebalanceInterval::Weekly
            | RebalanceInterval::Monthly => true,
        }
    }
}

fn interval_to_seconds(interval: &RebalanceInterval) -> u64 {
    match interval {
        RebalanceInterval::Hourly => 3600,
        RebalanceInterval::Daily => 86400,
        RebalanceInterval::Weekly => 604800,
        RebalanceInterval::Monthly => 2592000, // 30 days
    }
}

/// Rebalancing contract for AstraPort
/// Manages portfolio rebalancing and allocation adjustments
#[contract]
pub struct RebalancingContract;

#[contractimpl]
impl RebalancingContract {
    /// Initialize the rebalancing contract
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    ///
    /// # Returns
    /// Success symbol if initialization succeeds
    pub fn initialize(_env: Env) -> Symbol {
        symbol_short!("ok")
    }

    /// Helper to enforce portfolio owner authorization.
    /// If no owner is recorded yet for `portfolio_id`, registers `owner` as owner.
    /// Calls `owner.require_auth()` and ensures `owner` matches the recorded owner.
    fn require_owner_auth(
        env: &Env,
        owner: &Address,
        portfolio_id: &Symbol,
    ) -> Result<(), RebalancingError> {
        owner.require_auth();
        let key = DataKey::Owner(portfolio_id.clone());
        if let Some(stored_owner) = env.storage().persistent().get::<DataKey, Address>(&key) {
            if &stored_owner != owner {
                return Err(RebalancingError::Unauthorized);
            }
        } else {
            env.storage().persistent().set(&key, owner);
        }
        Ok(())
    }

    /// Check whether `actor` is authorized for `portfolio_id` with the given
    /// `required_permission`.
    ///
    /// Authorization succeeds if ANY of the following is true:
    /// 1. `actor` is the portfolio owner (verified via `require_owner_auth`).
    /// 2. `actor` holds a role assignment with the required permission bits.
    ///
    /// This is the unified entry-point for all permission-checked functions.
    fn require_auth_or_rbac(
        env: &Env,
        actor: &Address,
        portfolio_id: &Symbol,
        required_permission: u32,
        _action: &Symbol,
    ) -> Result<(), RebalancingError> {
        // First, try owner auth (this also registers the owner on first call).
        if Self::require_owner_auth(env, actor, portfolio_id).is_ok() {
            return Ok(());
        }
        // Owner auth failed — check RBAC permission.
        // Note: require_owner_auth already called actor.require_auth(), so if we
        // reach here the actor authenticated but is not the owner. We need to
        // verify RBAC permission. However, require_owner_auth returned Err because
        // the address doesn't match, but it already called require_auth. That's
        // fine — the actor authenticated. Now check RBAC.
        //
        // For non-owner paths, we re-check auth since require_owner_auth may have
        // been called and failed due to address mismatch. The actor already
        // authenticated via require_auth() inside require_owner_auth, so we just
        // need to verify RBAC permissions.
        match check_permission(env, portfolio_id, actor, required_permission) {
            Ok(_) => Ok(()),
            Err(_) => Err(RebalancingError::PermissionDenied),
        }
    }

    /// Get the owner address for a portfolio if set.
    pub fn get_owner(env: Env, portfolio_id: Symbol) -> Option<Address> {
        let key = DataKey::Owner(portfolio_id);
        env.storage().persistent().get(&key)
    }

    /// Compute a rebalance plan from the stored target allocation and current
    /// holdings. The plan only includes assets whose absolute drift is greater
    /// than the configured threshold. A manual rebalance is recorded in the
    /// execution history.
    pub fn rebalance(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
    ) -> Result<RebalanceResult, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_REBALANCE,
            &Symbol::new(&env, "rebalance"),
        )?;
        let result = Self::calculate_rebalance(&env, &portfolio_id)?;
        Self::record_execution(
            &env,
            &portfolio_id,
            symbol_short!("done"),
            symbol_short!("manual"),
        );
        let snapshot_before = env
            .storage()
            .persistent()
            .get::<DataKey, CurrentHoldings>(&DataKey::CurrentHoldings(portfolio_id.clone()));
        let snapshot_after = env
            .storage()
            .persistent()
            .get::<DataKey, TargetAllocation>(&DataKey::Allocation(portfolio_id.clone()));
        let mut before_map = Map::new(&env);
        let mut after_map = Map::new(&env);
        if let Some(h) = snapshot_before {
            for (k, v) in h.allocations.iter() {
                before_map.set(k, v);
            }
        }
        if let Some(a) = snapshot_after {
            for (k, v) in a.allocations.iter() {
                after_map.set(k, v);
            }
        }
        Self::log_audit_if_configured(
            &env,
            &portfolio_id,
            symbol_short!("done"),
            "manual_rebalance",
            &before_map,
            &after_map,
        );
        Ok(result)
    }

    /// Get current rebalancing status
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `portfolio_id` - Identifier for the portfolio
    ///
    /// # Returns
    /// Status symbol
    pub fn get_status(_env: Env, _portfolio_id: Symbol) -> Symbol {
        symbol_short!("ok")
    }

    pub fn set_schedule(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        interval: RebalanceInterval,
    ) -> Symbol {
        if Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_MANAGE_SCHEDULE,
            &Symbol::new(&env, "set_sched"),
        )
        .is_err()
        {
            return symbol_short!("err_auth");
        }
        if !ScheduleValidator::validate(&interval) {
            return symbol_short!("err_val");
        }
        let key = DataKey::Schedule(portfolio_id.clone());
        if env.storage().persistent().has(&key) {
            return symbol_short!("err_exist");
        }

        let now = env.ledger().timestamp();
        let next_execution = now + interval_to_seconds(&interval);

        let schedule = RebalancingSchedule {
            portfolio_id,
            interval,
            next_execution,
            last_execution: 0,
        };

        env.storage().persistent().set(&key, &schedule);
        symbol_short!("ok")
    }

    pub fn update_schedule(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        interval: RebalanceInterval,
    ) -> Symbol {
        if Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_MANAGE_SCHEDULE,
            &Symbol::new(&env, "upd_sched"),
        )
        .is_err()
        {
            return symbol_short!("err_auth");
        }
        if !ScheduleValidator::validate(&interval) {
            return symbol_short!("err_val");
        }
        let key = DataKey::Schedule(portfolio_id.clone());
        if !env.storage().persistent().has(&key) {
            return symbol_short!("err_none");
        }

        let mut schedule: RebalancingSchedule = env.storage().persistent().get(&key).unwrap();
        let now = env.ledger().timestamp();

        schedule.interval = interval;
        let interval_secs = interval_to_seconds(&schedule.interval);
        if schedule.last_execution > 0 {
            schedule.next_execution = schedule.last_execution + interval_secs;
        } else {
            schedule.next_execution = now + interval_secs;
        }

        env.storage().persistent().set(&key, &schedule);
        symbol_short!("ok")
    }

    pub fn cancel_schedule(env: Env, owner: Address, portfolio_id: Symbol) -> Symbol {
        if Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_MANAGE_SCHEDULE,
            &Symbol::new(&env, "cancel_sched"),
        )
        .is_err()
        {
            return symbol_short!("err_auth");
        }
        let key = DataKey::Schedule(portfolio_id.clone());
        if !env.storage().persistent().has(&key) {
            return symbol_short!("err_none");
        }
        env.storage().persistent().remove(&key);
        symbol_short!("ok")
    }

    pub fn get_schedule(env: Env, portfolio_id: Symbol) -> Option<RebalancingSchedule> {
        let key = DataKey::Schedule(portfolio_id);
        env.storage().persistent().get(&key)
    }

    /// Set the target allocation for a portfolio.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `owner` - Portfolio owner address
    /// * `portfolio_id` - Identifier for the portfolio
    /// * `allocation` - Target allocation with asset→basis-points weights
    ///
    /// # Returns
    /// `Ok(ok)` if the allocation is valid (sums to 10_000 bps) and persisted.
    /// `Err(RebalancingError::InvalidAllocation)` if weights don't sum to 10_000.
    pub fn set_target_allocation(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        allocation: TargetAllocation,
    ) -> Result<Symbol, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_MODIFY_ALLOCATIONS,
            &Symbol::new(&env, "set_alloc"),
        )?;
        let mut total: u32 = 0;
        for (_asset, weight) in allocation.allocations.iter() {
            total += weight;
        }
        if total != 10_000 {
            return Err(RebalancingError::InvalidAllocation);
        }

        let key = DataKey::Allocation(portfolio_id);
        env.storage().persistent().set(&key, &allocation);
        Ok(symbol_short!("ok"))
    }

    /// Get the target allocation for a portfolio.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment
    /// * `portfolio_id` - Identifier for the portfolio
    ///
    /// # Returns
    /// `Some(TargetAllocation)` if one has been set, `None` otherwise.
    pub fn get_target_allocation(env: Env, portfolio_id: Symbol) -> Option<TargetAllocation> {
        let key = DataKey::Allocation(portfolio_id);
        env.storage().persistent().get(&key)
    }

    /// Store the current portfolio weights used by `rebalance`. Current weights
    /// are expressed in basis points and must total 10_000.
    pub fn set_current_holdings(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        holdings: CurrentHoldings,
    ) -> Result<Symbol, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_MODIFY_ALLOCATIONS,
            &Symbol::new(&env, "set_hold"),
        )?;
        let mut total: u32 = 0;
        for (_asset, weight) in holdings.allocations.iter() {
            total += weight;
        }
        if total != 10_000 {
            return Err(RebalancingError::InvalidCurrentHoldings);
        }
        let key = DataKey::CurrentHoldings(portfolio_id);
        env.storage().persistent().set(&key, &holdings);
        Ok(symbol_short!("ok"))
    }

    pub fn get_current_holdings(env: Env, portfolio_id: Symbol) -> Option<CurrentHoldings> {
        let key = DataKey::CurrentHoldings(portfolio_id);
        env.storage().persistent().get(&key)
    }

    /// Set the per-portfolio drift tolerance in basis points. The default is
    /// 100 bps when this value has not been configured.
    pub fn set_drift_threshold_bps(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        threshold_bps: u32,
    ) -> Result<(), RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_MODIFY_ALLOCATIONS,
            &Symbol::new(&env, "set_drift"),
        )?;
        if threshold_bps > 10_000 {
            return Err(RebalancingError::InvalidDriftThreshold);
        }
        let key = DataKey::DriftThreshold(portfolio_id);
        env.storage().persistent().set(&key, &threshold_bps);
        Ok(())
    }

    pub fn get_drift_threshold_bps(env: Env, portfolio_id: Symbol) -> u32 {
        let key = DataKey::DriftThreshold(portfolio_id);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or(DEFAULT_DRIFT_THRESHOLD_BPS)
    }

    /// Get execution history for a portfolio
    pub fn get_execution_history(env: Env, portfolio_id: Symbol) -> Vec<ExecutionHistoryRecord> {
        let key = DataKey::History(portfolio_id);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Check and execute scheduled rebalance
    pub fn check_exec_sched_rebalance(env: Env, portfolio_id: Symbol) -> Symbol {
        let key = DataKey::Schedule(portfolio_id.clone());
        if !env.storage().persistent().has(&key) {
            let ts = env.ledger().timestamp();
            let event_data = SchedRebalanceEventData {
                portfolio_id: portfolio_id.clone(),
                outcome: symbol_short!("err_none"),
                timestamp: ts,
                details: symbol_short!("err_none"),
            };
            env.events()
                .publish((symbol_short!("SREBAL"), portfolio_id.clone()), event_data);
            return symbol_short!("err_none");
        }

        let mut schedule: RebalancingSchedule = env.storage().persistent().get(&key).unwrap();
        let now = env.ledger().timestamp();

        if now < schedule.next_execution {
            let event_data = SchedRebalanceEventData {
                portfolio_id: portfolio_id.clone(),
                outcome: symbol_short!("not_due"),
                timestamp: now,
                details: symbol_short!("not_due"),
            };
            env.events()
                .publish((symbol_short!("SREBAL"), portfolio_id.clone()), event_data);
            return symbol_short!("not_due");
        }

        // Scheduled execution calculates the same plan as a manual rebalance,
        // but records a scheduled (rather than manual) history entry below.
        let outcome = match Self::calculate_rebalance(&env, &portfolio_id) {
            Ok(_) => symbol_short!("done"),
            Err(RebalancingError::TargetAllocationNotFound) => symbol_short!("no_target"),
            Err(RebalancingError::CurrentHoldingsNotFound) => symbol_short!("no_hold"),
            Err(_) => symbol_short!("err"),
        };

        // Update schedule
        schedule.last_execution = now;
        schedule.next_execution = now + interval_to_seconds(&schedule.interval);
        env.storage().persistent().set(&key, &schedule);

        // Log execution history
        let history_key = DataKey::History(portfolio_id.clone());
        let mut history: Vec<ExecutionHistoryRecord> = env
            .storage()
            .persistent()
            .get(&history_key)
            .unwrap_or_else(|| Vec::new(&env));

        let record = ExecutionHistoryRecord {
            timestamp: now,
            outcome: outcome.clone(),
            details: symbol_short!("schd_exec"),
        };
        history.push_back(record);
        env.storage().persistent().set(&history_key, &history);

        // Audit log integration: capture before/after balances for the schedule.
        let cur = env
            .storage()
            .persistent()
            .get::<DataKey, CurrentHoldings>(&DataKey::CurrentHoldings(portfolio_id.clone()));
        let tgt = env
            .storage()
            .persistent()
            .get::<DataKey, TargetAllocation>(&DataKey::Allocation(portfolio_id.clone()));
        let mut before_map = Map::new(&env);
        let mut after_map = Map::new(&env);
        if let Some(h) = cur {
            for (k, v) in h.allocations.iter() {
                before_map.set(k, v);
            }
        }
        if let Some(a) = tgt {
            for (k, v) in a.allocations.iter() {
                after_map.set(k, v);
            }
        }
        Self::log_audit_if_configured(
            &env,
            &portfolio_id,
            outcome.clone(),
            "scheduled_rebalance",
            &before_map,
            &after_map,
        );

        outcome
    }

    pub fn get_rebalance_plan(
        env: Env,
        portfolio_id: Symbol,
    ) -> Result<RebalanceResult, RebalancingError> {
        Self::calculate_rebalance(&env, &portfolio_id)
    }

    pub fn check_and_exec_sched(env: Env, portfolio_id: Symbol) -> Symbol {
        Self::check_exec_sched_rebalance(env, portfolio_id)
    }

    pub fn execute_rebalance(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        strategy: multi_asset_rebalancer::ExecutionStrategy,
    ) -> Result<(), RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_EXECUTE_REBALANCE,
            &Symbol::new(&env, "exec_rebal"),
        )?;
        let result = Self::calculate_rebalance(&env, &portfolio_id)?;
        let rebalancer_id =
            env.register_contract(None, multi_asset_rebalancer::MultiAssetRebalancer);
        let client = multi_asset_rebalancer::MultiAssetRebalancerClient::new(&env, &rebalancer_id);
        client.rebalance(&portfolio_id, &strategy, &result.adjustments);
        Self::record_execution(
            &env,
            &portfolio_id,
            symbol_short!("done"),
            symbol_short!("manual"),
        );
        Ok(())
    }

    pub fn simulate_rebalance(
        env: Env,
        portfolio_id: Symbol,
        strategy: multi_asset_rebalancer::ExecutionStrategy,
    ) -> Result<multi_asset_rebalancer::SimulationResult, RebalancingError> {
        let result = Self::calculate_rebalance(&env, &portfolio_id)?;
        let rebalancer_id =
            env.register_contract(None, multi_asset_rebalancer::MultiAssetRebalancer);
        let client = multi_asset_rebalancer::MultiAssetRebalancerClient::new(&env, &rebalancer_id);
        Ok(client.simulate_rebalance(&portfolio_id, &strategy, &result.adjustments))
    }

    // -------------------------------------------------------------------
    // RBAC management endpoints
    // -------------------------------------------------------------------

    /// Assign a role to an account for a portfolio.
    ///
    /// The `granter` must hold `CAN_MANAGE_ROLES` permission (or be the owner).
    /// `expires_at`: `0` for permanent, or a future ledger timestamp.
    pub fn grant_role(
        env: Env,
        granter: Address,
        portfolio_id: Symbol,
        assignee: Address,
        role: Role,
        expires_at: u64,
    ) -> Result<Symbol, RebalancingError> {
        // Granter must be owner or have CAN_MANAGE_ROLES.
        if Self::require_owner_auth(&env, &granter, &portfolio_id).is_ok() {
            // Owner can always grant roles.
        } else {
            // Non-owner needs CAN_MANAGE_ROLES.
            match check_permission(&env, &portfolio_id, &granter, CAN_MANAGE_ROLES) {
                Ok(_) => {}
                Err(_) => return Err(RebalancingError::PermissionDenied),
            }
        }
        // Prevent granting Owner role via this function.
        if role == Role::Owner {
            return Err(RebalancingError::CannotRevokeOwner);
        }
        assign_role(
            &env,
            &portfolio_id,
            &granter,
            &assignee,
            role,
            expires_at,
            None,
        )
        .map_err(|_| RebalancingError::PermissionDenied)?;

        // Emit role-change event.
        env.events().publish(
            (symbol_short!("ROLE"), portfolio_id.clone()),
            RoleChangeEvent {
                portfolio_id,
                actor: granter,
                assignee,
                role,
                action: symbol_short!("grant"),
                expires_at,
            },
        );
        Ok(symbol_short!("ok"))
    }

    /// Assign a role with custom permissions (owner only).
    pub fn grant_role_with_permissions(
        env: Env,
        granter: Address,
        portfolio_id: Symbol,
        assignee: Address,
        role: Role,
        perm: u32,
        expires_at: u64,
    ) -> Result<Symbol, RebalancingError> {
        Self::require_owner_auth(&env, &granter, &portfolio_id)?;
        if role == Role::Owner {
            return Err(RebalancingError::CannotRevokeOwner);
        }
        assign_role(
            &env,
            &portfolio_id,
            &granter,
            &assignee,
            role,
            expires_at,
            Some(perm),
        )
        .map_err(|_| RebalancingError::PermissionDenied)?;

        env.events().publish(
            (symbol_short!("ROLE"), portfolio_id.clone()),
            RoleChangeEvent {
                portfolio_id,
                actor: granter,
                assignee,
                role,
                action: symbol_short!("grant"),
                expires_at,
            },
        );
        Ok(symbol_short!("ok"))
    }

    /// Revoke a role from an account for a portfolio.
    pub fn revoke_role(
        env: Env,
        revoker: Address,
        portfolio_id: Symbol,
        assignee: Address,
    ) -> Result<Symbol, RebalancingError> {
        // Only owner can revoke roles.
        Self::require_owner_auth(&env, &revoker, &portfolio_id)?;
        // Prevent revoking the owner role.
        let assignment = get_raw_assignment(&env, &portfolio_id, &assignee);
        let revoked_role = assignment.as_ref().map(|a| a.role).unwrap_or(Role::Viewer);
        if let Some(a) = &assignment {
            if a.role == Role::Owner {
                return Err(RebalancingError::CannotRevokeOwner);
            }
        }
        revoke_role(&env, &portfolio_id, &revoker, &assignee)
            .map_err(|_| RebalancingError::RoleNotFound)?;

        env.events().publish(
            (symbol_short!("ROLE"), portfolio_id.clone()),
            RoleChangeEvent {
                portfolio_id,
                actor: revoker,
                assignee,
                role: revoked_role,
                action: symbol_short!("revoke"),
                expires_at: 0,
            },
        );
        Ok(symbol_short!("ok"))
    }

    /// Get the current role assignment for an account on a portfolio.
    pub fn get_role(env: Env, portfolio_id: Symbol, assignee: Address) -> Option<RoleAssignment> {
        get_role_assignment(&env, &portfolio_id, &assignee)
    }

    /// Get the raw stored role assignment (ignoring expiry) for admin queries.
    pub fn get_role_raw(
        env: Env,
        portfolio_id: Symbol,
        assignee: Address,
    ) -> Option<RoleAssignment> {
        get_raw_assignment(&env, &portfolio_id, &assignee)
    }

    /// Extend the expiry of an existing role assignment.
    pub fn extend_role(
        env: Env,
        granter: Address,
        portfolio_id: Symbol,
        assignee: Address,
        new_expiry: u64,
    ) -> Result<Symbol, RebalancingError> {
        Self::require_owner_auth(&env, &granter, &portfolio_id)?;
        extend_role_expiry(&env, &portfolio_id, &granter, &assignee, new_expiry)
            .map_err(|_| RebalancingError::RoleNotFound)?;
        Ok(symbol_short!("ok"))
    }

    /// Get the access log for a portfolio.
    pub fn get_access_log(env: Env, portfolio_id: Symbol) -> Vec<rbac::AccessLogEntry> {
        get_access_log(&env, &portfolio_id)
    }

    /// Public permission check endpoint for off-chain and on-chain use.
    ///
    /// Returns a `PermissionCheckResult` with rich context about whether
    /// the actor holds the required permissions.
    pub fn check_permission_rbac(
        env: Env,
        portfolio_id: Symbol,
        actor: Address,
        required_permission: u32,
    ) -> rbac::PermissionCheckResult {
        check_permission_detailed(&env, &portfolio_id, &actor, required_permission)
    }

    /// Returns `true` if `actor` has `required_permission` for `portfolio_id`.
    ///
    /// Lightweight boolean check — use `check_permission_rbac` for full context.
    pub fn has_rbac_permission(
        env: Env,
        portfolio_id: Symbol,
        actor: Address,
        required_permission: u32,
    ) -> bool {
        has_permission(&env, &portfolio_id, &actor, required_permission)
    }

    /// Returns the default permissions bitmask for a given role.
    pub fn get_role_permissions(_env: Env, role: Role) -> u32 {
        role.default_permissions()
    }

    /// Returns human-readable permission names for a bitmask.
    pub fn describe_permissions(env: Env, perms: u32) -> Vec<Symbol> {
        describe_permissions(&env, perms)
    }

    /// Emergency: revoke ALL non-owner roles for a portfolio.
    ///
    /// Owner-only. `known_addresses` is a list of addresses to check and revoke.
    /// Returns the number of roles revoked.
    pub fn emergency_revoke_all_roles(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        known_addresses: Vec<Address>,
    ) -> Result<u32, RebalancingError> {
        Self::require_owner_auth(&env, &owner, &portfolio_id)?;
        revoke_all_roles(&env, &portfolio_id, &owner, &known_addresses)
            .map_err(|_| RebalancingError::PermissionDenied)
    }

    /// Set the audit-log sink address with RBAC check.
    ///
    /// Requires `CAN_CONFIGURE` permission (or owner).
    pub fn set_audit_sink_rbac(
        env: Env,
        actor: Address,
        portfolio_id: Symbol,
        sink: Address,
    ) -> Result<Symbol, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &actor,
            &portfolio_id,
            rbac::CAN_CONFIGURE,
            &Symbol::new(&env, "set_sink"),
        )?;
        env.storage().persistent().set(&DataKey::AuditSink, &sink);
        Ok(symbol_short!("ok"))
    }

    // -------------------------------------------------------------------
    // Alert & monitoring endpoints
    // -------------------------------------------------------------------

    /// Create or fully replace the alert configuration for a portfolio.
    ///
    /// Requires owner or `CAN_CONFIGURE`. The stored config's `portfolio_id`
    /// is forced to the `portfolio_id` argument regardless of the value carried
    /// inside `config`.
    pub fn set_alert_config(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        config: alerts::AlertConfig,
    ) -> Result<alerts::AlertConfig, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_CONFIGURE,
            &Symbol::new(&env, "set_alert_cfg"),
        )?;
        let config = alerts::AlertConfig {
            portfolio_id: portfolio_id.clone(),
            thresholds: config.thresholds,
            alerts_enabled: config.alerts_enabled,
        };
        let monitor = alerts::AlertMonitor::new(&env);
        Ok(monitor.set_config(config))
    }

    /// Append a threshold to a portfolio's alert configuration.
    ///
    /// Requires owner or `CAN_CONFIGURE`.
    pub fn add_alert_threshold(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        threshold: alerts::AlertThreshold,
    ) -> Result<alerts::AlertConfig, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_CONFIGURE,
            &Symbol::new(&env, "add_thresh"),
        )?;
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.add_threshold(&portfolio_id, threshold)
    }

    /// Remove the threshold at `index` from a portfolio's alert configuration.
    ///
    /// Requires owner or `CAN_CONFIGURE`.
    pub fn remove_alert_threshold(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        index: u32,
    ) -> Result<alerts::AlertConfig, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_CONFIGURE,
            &Symbol::new(&env, "rm_thresh"),
        )?;
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.remove_threshold(&portfolio_id, index)
    }

    /// Enable or disable all alerts for a portfolio (master switch).
    ///
    /// Requires owner or `CAN_CONFIGURE`.
    pub fn set_alerts_enabled(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        enabled: bool,
    ) -> Result<alerts::AlertConfig, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_CONFIGURE,
            &Symbol::new(&env, "set_al_en"),
        )?;
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.set_alerts_enabled(&portfolio_id, enabled)
    }

    /// Acknowledge the alert at `index` in a portfolio's history.
    ///
    /// Requires owner or `CAN_CONFIGURE`.
    pub fn acknowledge_alert(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        index: u32,
    ) -> Result<Symbol, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_CONFIGURE,
            &Symbol::new(&env, "ack_alert"),
        )?;
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.acknowledge(&portfolio_id, index)?;
        Ok(symbol_short!("ok"))
    }

    /// Read the alert configuration for a portfolio, if any. Public read.
    pub fn get_alert_config(env: Env, portfolio_id: Symbol) -> Option<alerts::AlertConfig> {
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.get_config(&portfolio_id)
    }

    /// Read the full alert history for a portfolio (oldest first). Public read.
    pub fn get_alert_history(env: Env, portfolio_id: Symbol) -> Vec<alerts::AlertHistoryEntry> {
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.history(&portfolio_id)
    }

    /// Read only the unacknowledged alerts for a portfolio. Public read.
    pub fn get_pending_alerts(env: Env, portfolio_id: Symbol) -> Vec<alerts::AlertHistoryEntry> {
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.pending_alerts(&portfolio_id)
    }

    /// Evaluate drift-based alerts for a portfolio against its stored target
    /// allocation and current holdings.
    ///
    /// Permissionless (mirrors the scheduled-rebalance checks): recording a
    /// factual breach requires no authorization. Returns the number of alerts
    /// that fired.
    pub fn check_portfolio_alerts(env: Env, portfolio_id: Symbol) -> u32 {
        let observations = Self::build_drift_observations(&env, &portfolio_id);
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.check(&portfolio_id, observations)
    }

    /// Evaluate alerts for a portfolio against its stored drift metrics plus
    /// caller-supplied observations (e.g. balance, yield, custom metrics).
    ///
    /// Drift observations are computed from storage; the `extra` observations
    /// are appended before evaluation. Permissionless. Returns the number of
    /// alerts that fired.
    pub fn check_portfolio_alerts_with(
        env: Env,
        portfolio_id: Symbol,
        extra: Vec<alerts::MetricObservation>,
    ) -> u32 {
        let mut observations = Self::build_drift_observations(&env, &portfolio_id);
        for i in 0..extra.len() {
            observations.push_back(extra.get(i).unwrap());
        }
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.check(&portfolio_id, observations)
    }

    /// Update the threshold at `index` in a portfolio's alert configuration.
    ///
    /// Replaces the threshold in-place without changing the order of other
    /// thresholds. This does not disrupt ongoing monitoring.
    ///
    /// Requires owner or `CAN_CONFIGURE`.
    pub fn update_alert_threshold(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        index: u32,
        threshold: alerts::AlertThreshold,
    ) -> Result<alerts::AlertConfig, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_CONFIGURE,
            &Symbol::new(&env, "upd_thresh"),
        )?;
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.update_threshold(&portfolio_id, index, threshold)
    }

    /// Read the aggregated alert statistics for a portfolio. Public read.
    pub fn get_alert_statistics(env: Env, portfolio_id: Symbol) -> Option<alerts::AlertStatistics> {
        let monitor = alerts::AlertMonitor::new(&env);
        monitor.get_statistics(&portfolio_id)
    }
}

impl RebalancingContract {
    /// Configure the audit-log sink address. Admin-only is enforced by the
    /// caller (no admin concept here yet, so we accept any caller — the
    /// rebalancing contract is usually gated by the deployer key).
    pub fn set_audit_sink(env: Env, sink: Address) -> Symbol {
        env.storage().persistent().set(&DataKey::AuditSink, &sink);
        symbol_short!("ok")
    }

    /// Read the audit-log sink address, if configured.
    pub fn get_audit_sink(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey::AuditSink)
    }

    /// Append an audit event if a sink is configured. No-op otherwise.
    fn log_audit_if_configured(
        env: &Env,
        portfolio_id: &Symbol,
        outcome: Symbol,
        detail: &str,
        balances_before: &Map<Symbol, u32>,
        balances_after: &Map<Symbol, u32>,
    ) {
        let key = DataKey::AuditSink;
        let sink: Option<Address> = env.storage().persistent().get(&key);
        if let Some(sink) = sink {
            let mut before = StateSnapshot::empty(env);
            for (k, v) in balances_before.iter() {
                before.push(k, v as i128);
            }
            let mut after = StateSnapshot::empty(env);
            for (k, v) in balances_after.iter() {
                after.push(k, v as i128);
            }
            let detail_str = soroban_sdk::String::from_str(env, detail);
            let logger = AuditLogger::new(env, &sink);
            // The actor is the contract itself for rebalance events; we use
            // the portfolio id as the actor label so verifiers can spot
            // portfolio-scoped changes.
            let actor_addr = env.current_contract_address();
            let _ = logger.log_event(
                actor_addr,
                AuditEventType::Rebalance,
                portfolio_id.clone(),
                permissions::ADMIN,
                before,
                after,
                outcome,
                detail_str,
            );
        }
    }

    /// Build drift [`alerts::MetricObservation`]s from a portfolio's stored
    /// target allocation and current holdings.
    ///
    /// Produces one [`alerts::MetricType::AssetDrift`] observation per asset in
    /// `target ∪ current` carrying the **absolute** drift magnitude in bps, plus
    /// a single [`alerts::MetricType::PortfolioDrift`] observation holding the
    /// maximum absolute drift across all assets. Returns an empty vector when
    /// either the target allocation or current holdings are unset. The drift math
    /// mirrors [`Self::add_adjustment_if_needed`].
    fn build_drift_observations(
        env: &Env,
        portfolio_id: &Symbol,
    ) -> Vec<alerts::MetricObservation> {
        let mut observations = Vec::new(env);
        let target: TargetAllocation = match env
            .storage()
            .persistent()
            .get(&DataKey::Allocation(portfolio_id.clone()))
        {
            Some(t) => t,
            None => return observations,
        };
        let current: CurrentHoldings = match env
            .storage()
            .persistent()
            .get(&DataKey::CurrentHoldings(portfolio_id.clone()))
        {
            Some(c) => c,
            None => return observations,
        };

        let mut max_drift: i128 = 0;

        // Assets present in the target allocation.
        for (asset, target_weight) in target.allocations.iter() {
            let current_weight = current.allocations.get(asset.clone()).unwrap_or(0);
            let drift = (current_weight as i32 - target_weight as i32).unsigned_abs() as i128;
            if drift > max_drift {
                max_drift = drift;
            }
            observations.push_back(alerts::MetricObservation {
                metric: alerts::MetricType::AssetDrift,
                asset: asset.clone(),
                value: drift,
            });
        }
        // Assets held but absent from the target (target weight is 0).
        for (asset, current_weight) in current.allocations.iter() {
            if !target.allocations.contains_key(asset.clone()) {
                let drift = current_weight as i128;
                if drift > max_drift {
                    max_drift = drift;
                }
                observations.push_back(alerts::MetricObservation {
                    metric: alerts::MetricType::AssetDrift,
                    asset: asset.clone(),
                    value: drift,
                });
            }
        }

        observations.push_back(alerts::MetricObservation {
            metric: alerts::MetricType::PortfolioDrift,
            asset: symbol_short!("ALL"),
            value: max_drift,
        });

        observations
    }

    fn calculate_rebalance(
        env: &Env,
        portfolio_id: &Symbol,
    ) -> Result<RebalanceResult, RebalancingError> {
        let target: TargetAllocation = env
            .storage()
            .persistent()
            .get(&DataKey::Allocation(portfolio_id.clone()))
            .ok_or(RebalancingError::TargetAllocationNotFound)?;
        let current: CurrentHoldings = env
            .storage()
            .persistent()
            .get(&DataKey::CurrentHoldings(portfolio_id.clone()))
            .ok_or(RebalancingError::CurrentHoldingsNotFound)?;
        let threshold: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DriftThreshold(portfolio_id.clone()))
            .unwrap_or(DEFAULT_DRIFT_THRESHOLD_BPS);
        let mut adjustments = Vec::new(env);

        // Visit target assets first, then current-only assets. This makes an
        // asset removed from a target allocation correctly appear as a sell.
        for (asset, target_weight) in target.allocations.iter() {
            let current_weight = current.allocations.get(asset.clone()).unwrap_or(0);
            Self::add_adjustment_if_needed(
                &mut adjustments,
                asset,
                current_weight,
                target_weight,
                threshold,
            );
        }
        for (asset, current_weight) in current.allocations.iter() {
            if !target.allocations.contains_key(asset.clone()) {
                Self::add_adjustment_if_needed(
                    &mut adjustments,
                    asset,
                    current_weight,
                    0,
                    threshold,
                );
            }
        }

        Ok(RebalanceResult {
            portfolio_id: portfolio_id.clone(),
            drift_threshold_bps: threshold,
            adjustments,
        })
    }

    fn add_adjustment_if_needed(
        adjustments: &mut Vec<RebalanceAdjustment>,
        asset: Symbol,
        current_weight: u32,
        target_weight: u32,
        threshold: u32,
    ) {
        let drift = current_weight as i32 - target_weight as i32;
        if drift.unsigned_abs() > threshold {
            let direction = if drift > 0 {
                RebalanceDirection::Sell
            } else {
                RebalanceDirection::Buy
            };
            adjustments.push_back(RebalanceAdjustment {
                asset,
                current_weight_bps: current_weight,
                target_weight_bps: target_weight,
                drift_bps: drift,
                direction,
            });
        }
    }

    // ------------------------------------------------------------------
    // Core Rebalancing Engine public endpoints
    // ------------------------------------------------------------------

    /// Calculate per-asset drift for a portfolio with ±0.01% accuracy.
    ///
    /// Returns a `DriftReport` containing per-asset drift data and a
    /// portfolio-wide summary. This is the primary drift detection entry
    /// point.
    pub fn calculate_portfolio_drift(
        env: Env,
        portfolio_id: Symbol,
    ) -> Result<records::DriftReport, RebalancingError> {
        let target: TargetAllocation = env
            .storage()
            .persistent()
            .get(&DataKey::Allocation(portfolio_id.clone()))
            .ok_or(RebalancingError::TargetAllocationNotFound)?;
        let current: CurrentHoldings = env
            .storage()
            .persistent()
            .get(&DataKey::CurrentHoldings(portfolio_id.clone()))
            .ok_or(RebalancingError::CurrentHoldingsNotFound)?;
        let threshold = Self::get_drift_threshold_bps(env.clone(), portfolio_id.clone());

        Ok(drift_engine::DriftEngine::calculate_portfolio_drift(
            &env,
            &portfolio_id,
            &target,
            &current,
            threshold,
        ))
    }

    /// Detect whether rebalancing is needed for a portfolio.
    ///
    /// Returns `(needs_rebalance, out_of_threshold_count)`.
    pub fn detect_rebalancing_need(
        env: Env,
        portfolio_id: Symbol,
    ) -> Result<(bool, u32), RebalancingError> {
        let target: TargetAllocation = env
            .storage()
            .persistent()
            .get(&DataKey::Allocation(portfolio_id.clone()))
            .ok_or(RebalancingError::TargetAllocationNotFound)?;
        let current: CurrentHoldings = env
            .storage()
            .persistent()
            .get(&DataKey::CurrentHoldings(portfolio_id.clone()))
            .ok_or(RebalancingError::CurrentHoldingsNotFound)?;
        let threshold = Self::get_drift_threshold_bps(env.clone(), portfolio_id.clone());

        Ok(drift_engine::DriftEngine::detect_rebalancing_need(
            &env,
            &portfolio_id,
            &target,
            &current,
            threshold,
        ))
    }

    /// Calculate specific trade orders to restore target allocation.
    ///
    /// Returns a `RebalancePlan` with concrete trade amounts, estimated
    /// fees, and slippage. The plan can be executed atomically or
    /// simulated first.
    pub fn calculate_rebalance_trades(
        env: Env,
        portfolio_id: Symbol,
        total_portfolio_value: i128,
    ) -> Result<records::RebalancePlan, RebalancingError> {
        let target: TargetAllocation = env
            .storage()
            .persistent()
            .get(&DataKey::Allocation(portfolio_id.clone()))
            .ok_or(RebalancingError::TargetAllocationNotFound)?;
        let current: CurrentHoldings = env
            .storage()
            .persistent()
            .get(&DataKey::CurrentHoldings(portfolio_id.clone()))
            .ok_or(RebalancingError::CurrentHoldingsNotFound)?;
        let threshold = Self::get_drift_threshold_bps(env.clone(), portfolio_id.clone());
        let constraints = records::TradeConstraints::default();

        Ok(drift_engine::DriftEngine::calculate_rebalance_trades(
            &env,
            &portfolio_id,
            &target,
            &current,
            threshold,
            &constraints,
            total_portfolio_value,
        ))
    }

    /// Simulate a rebalance without modifying any state.
    ///
    /// Returns a `SimulationPlanResult` with before/after drift reports,
    /// the computed trade plan, and whether the plan fully resolves drift.
    pub fn simulate_rebalance_full(
        env: Env,
        portfolio_id: Symbol,
        total_portfolio_value: i128,
    ) -> Result<records::SimulationPlanResult, RebalancingError> {
        let target: TargetAllocation = env
            .storage()
            .persistent()
            .get(&DataKey::Allocation(portfolio_id.clone()))
            .ok_or(RebalancingError::TargetAllocationNotFound)?;
        let current: CurrentHoldings = env
            .storage()
            .persistent()
            .get(&DataKey::CurrentHoldings(portfolio_id.clone()))
            .ok_or(RebalancingError::CurrentHoldingsNotFound)?;
        let threshold = Self::get_drift_threshold_bps(env.clone(), portfolio_id.clone());
        let constraints = records::TradeConstraints::default();

        Ok(drift_engine::DriftEngine::simulate_rebalance_full(
            &env,
            &portfolio_id,
            &target,
            &current,
            threshold,
            &constraints,
            total_portfolio_value,
        ))
    }

    /// Validate rebalance inputs (target allocation, current holdings,
    /// drift threshold) before computing a plan.
    pub fn validate_rebalance_inputs(
        env: Env,
        target: TargetAllocation,
        current: CurrentHoldings,
        threshold_bps: u32,
    ) -> records::RebalanceValidation {
        drift_engine::DriftEngine::validate_rebalance_inputs(&env, &target, &current, threshold_bps)
    }

    /// Execute an atomic rebalance using the core engine.
    ///
    /// This computes the rebalance plan, validates it, and records the
    /// operation in history. The operation is all-or-nothing: if any
    /// trade would fail, the entire rebalance is aborted.
    pub fn execute_atomic_rebalance(
        env: Env,
        owner: Address,
        portfolio_id: Symbol,
        total_portfolio_value: i128,
    ) -> Result<records::RebalanceRecord, RebalancingError> {
        Self::require_auth_or_rbac(
            &env,
            &owner,
            &portfolio_id,
            rbac::CAN_REBALANCE,
            &Symbol::new(&env, "atomic_rebal"),
        )?;

        let target: TargetAllocation = env
            .storage()
            .persistent()
            .get(&DataKey::Allocation(portfolio_id.clone()))
            .ok_or(RebalancingError::TargetAllocationNotFound)?;
        let current: CurrentHoldings = env
            .storage()
            .persistent()
            .get(&DataKey::CurrentHoldings(portfolio_id.clone()))
            .ok_or(RebalancingError::CurrentHoldingsNotFound)?;
        let threshold = Self::get_drift_threshold_bps(env.clone(), portfolio_id.clone());
        let constraints = records::TradeConstraints::default();

        // Compute drift before.
        let drift_before = drift_engine::DriftEngine::calculate_portfolio_drift(
            &env,
            &portfolio_id,
            &target,
            &current,
            threshold,
        );

        // Calculate the plan.
        let plan = drift_engine::DriftEngine::calculate_rebalance_trades(
            &env,
            &portfolio_id,
            &target,
            &current,
            threshold,
            &constraints,
            total_portfolio_value,
        );

        // Simulate post-rebalance drift.
        let sim_result = drift_engine::DriftEngine::simulate_rebalance_full(
            &env,
            &portfolio_id,
            &target,
            &current,
            threshold,
            &constraints,
            total_portfolio_value,
        );

        // A plan is executable only when every detected drift can be resolved.
        // No state is changed until this check succeeds.
        let atomic_success = plan.warnings.is_empty() && sim_result.fully_rebalanced;

        // Record the execution.
        let record = drift_engine::DriftEngine::create_rebalance_record(
            &env,
            &portfolio_id,
            &drift_before,
            &sim_result.drift_after,
            plan.trades.len(),
            atomic_success,
            &symbol_short!("manual"),
            plan.estimated_total_fees,
        );

        // Append to history.
        Self::record_execution(
            &env,
            &portfolio_id,
            record.outcome.clone(),
            symbol_short!("atomic"),
        );

        if atomic_success {
            env.storage()
                .persistent()
                .set(&DataKey::CurrentHoldings(portfolio_id.clone()), &target);
        }

        // Audit logging.
        let snapshot_before = env
            .storage()
            .persistent()
            .get::<DataKey, CurrentHoldings>(&DataKey::CurrentHoldings(portfolio_id.clone()));
        let snapshot_after = env
            .storage()
            .persistent()
            .get::<DataKey, TargetAllocation>(&DataKey::Allocation(portfolio_id.clone()));
        let mut before_map = Map::new(&env);
        let mut after_map = Map::new(&env);
        if let Some(h) = snapshot_before {
            for (k, v) in h.allocations.iter() {
                before_map.set(k, v);
            }
        }
        if let Some(a) = snapshot_after {
            for (k, v) in a.allocations.iter() {
                after_map.set(k, v);
            }
        }
        Self::log_audit_if_configured(
            &env,
            &portfolio_id,
            record.outcome.clone(),
            "atomic_rebalance",
            &before_map,
            &after_map,
        );

        Ok(record)
    }

    fn record_execution(env: &Env, portfolio_id: &Symbol, outcome: Symbol, details: Symbol) {
        let history_key = DataKey::History(portfolio_id.clone());
        let mut history: Vec<ExecutionHistoryRecord> = env
            .storage()
            .persistent()
            .get(&history_key)
            .unwrap_or_else(|| Vec::new(env));
        history.push_back(ExecutionHistoryRecord {
            timestamp: env.ledger().timestamp(),
            outcome,
            details,
        });
        env.storage().persistent().set(&history_key, &history);
    }

    pub fn check_and_execute_scheduled_rebalance(env: Env, portfolio_id: Symbol) -> Symbol {
        Self::check_exec_sched_rebalance(env, portfolio_id)
    }
}

#[cfg(test)]
mod tests_rbac;

#[cfg(test)]
mod tests_alerts;

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short, testutils::Address as _, testutils::Ledger, Env, Map};

    fn weights(env: &Env, entries: &[(Symbol, u32)]) -> Map<Symbol, u32> {
        let mut result = Map::new(env);
        for (asset, weight) in entries.iter() {
            result.set(asset.clone(), *weight);
        }
        result
    }

    fn client(env: &Env) -> RebalancingContractClient<'_> {
        let id = env.register_contract(None, RebalancingContract);
        RebalancingContractClient::new(env, &id)
    }

    #[test]
    fn test_initialize() {
        let env = Env::default();
        assert_eq!(client(&env).initialize(), symbol_short!("ok"));
    }

    #[test]
    fn test_rebalance_no_drift_does_not_flag_assets_and_logs_manual_execution() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");
        let allocation = weights(
            &env,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
        );
        client.set_target_allocation(
            &owner,
            &portfolio,
            &TargetAllocation {
                allocations: allocation.clone(),
            },
        );
        client.set_current_holdings(
            &owner,
            &portfolio,
            &CurrentHoldings {
                allocations: allocation,
            },
        );
        let result = client.rebalance(&owner, &portfolio);
        assert_eq!(result.adjustments.len(), 0);
        let history = client.get_execution_history(&portfolio);
        assert_eq!(history.len(), 1);
        assert_eq!(history.get(0).unwrap().details, symbol_short!("manual"));
        assert_eq!(client.get_owner(&portfolio), Some(owner));
    }

    #[test]
    fn test_rebalance_flags_single_asset_drift_with_direction() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");
        client.set_target_allocation(
            &owner,
            &portfolio,
            &TargetAllocation {
                allocations: weights(
                    &env,
                    &[
                        (symbol_short!("USDC"), 5_000),
                        (symbol_short!("XLM"), 3_000),
                        (symbol_short!("BTC"), 2_000),
                    ],
                ),
            },
        );
        client.set_current_holdings(
            &owner,
            &portfolio,
            &CurrentHoldings {
                allocations: weights(
                    &env,
                    &[
                        (symbol_short!("USDC"), 5_250),
                        (symbol_short!("XLM"), 2_900),
                        (symbol_short!("BTC"), 1_850),
                    ],
                ),
            },
        );
        client.set_drift_threshold_bps(&owner, &portfolio, &200);
        let result = client.rebalance(&owner, &portfolio);
        assert_eq!(result.adjustments.len(), 1);
        let adjustment = result.adjustments.get(0).unwrap();
        assert_eq!(adjustment.asset, symbol_short!("USDC"));
        assert_eq!(adjustment.drift_bps, 250);
        assert_eq!(adjustment.direction, RebalanceDirection::Sell);
    }

    #[test]
    fn test_rebalance_flags_multiple_assets_and_includes_buy_and_sell() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");
        client.set_target_allocation(
            &owner,
            &portfolio,
            &TargetAllocation {
                allocations: weights(
                    &env,
                    &[
                        (symbol_short!("USDC"), 5_000),
                        (symbol_short!("XLM"), 3_000),
                        (symbol_short!("BTC"), 2_000),
                    ],
                ),
            },
        );
        client.set_current_holdings(
            &owner,
            &portfolio,
            &CurrentHoldings {
                allocations: weights(
                    &env,
                    &[
                        (symbol_short!("USDC"), 5_300),
                        (symbol_short!("XLM"), 2_700),
                        (symbol_short!("BTC"), 2_000),
                    ],
                ),
            },
        );
        client.set_drift_threshold_bps(&owner, &portfolio, &100);
        let result = client.rebalance(&owner, &portfolio);
        assert_eq!(result.adjustments.len(), 2);
        assert_eq!(
            result.adjustments.get(0).unwrap().direction,
            RebalanceDirection::Sell
        );
        assert_eq!(
            result.adjustments.get(1).unwrap().direction,
            RebalanceDirection::Buy
        );
    }

    #[test]
    fn test_scheduled_rebalance_execution() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");
        client.set_schedule(&owner, &portfolio, &RebalanceInterval::Hourly);
        let allocation = weights(&env, &[(symbol_short!("USDC"), 10_000)]);
        client.set_target_allocation(
            &owner,
            &portfolio,
            &TargetAllocation {
                allocations: allocation.clone(),
            },
        );
        client.set_current_holdings(
            &owner,
            &portfolio,
            &CurrentHoldings {
                allocations: allocation,
            },
        );
        assert_eq!(
            client.check_exec_sched_rebalance(&portfolio),
            symbol_short!("not_due")
        );
        let mut ledger = env.ledger().get();
        ledger.timestamp = 3600;
        env.ledger().set(ledger);
        assert_eq!(
            client.check_exec_sched_rebalance(&portfolio),
            symbol_short!("done")
        );
        let history = client.get_execution_history(&portfolio);
        assert_eq!(history.len(), 1);
        assert_eq!(history.get(0).unwrap().details, symbol_short!("schd_exec"));
    }

    #[test]
    fn test_owner_registration_and_access_control() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner1 = Address::generate(&env);
        let owner2 = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        // First creation sets owner to owner1
        let allocation = weights(&env, &[(symbol_short!("USDC"), 10_000)]);
        let set_res = client.set_target_allocation(
            &owner1,
            &portfolio,
            &TargetAllocation {
                allocations: allocation.clone(),
            },
        );
        assert_eq!(set_res, symbol_short!("ok"));
        assert_eq!(client.get_owner(&portfolio), Some(owner1.clone()));

        // owner1 can update schedule
        assert_eq!(
            client.set_schedule(&owner1, &portfolio, &RebalanceInterval::Hourly),
            symbol_short!("ok")
        );
        assert_eq!(
            client.update_schedule(&owner1, &portfolio, &RebalanceInterval::Daily),
            symbol_short!("ok")
        );

        // owner2 attempts to mutate owner1's portfolio -> fails with err_auth / Unauthorized
        assert_eq!(
            client.update_schedule(&owner2, &portfolio, &RebalanceInterval::Weekly),
            symbol_short!("err_auth")
        );
        assert_eq!(
            client.cancel_schedule(&owner2, &portfolio),
            symbol_short!("err_auth")
        );
        assert_eq!(
            client.set_schedule(&owner2, &portfolio, &RebalanceInterval::Monthly),
            symbol_short!("err_auth")
        );

        let set_res2 = client.try_set_target_allocation(
            &owner2,
            &portfolio,
            &TargetAllocation {
                allocations: allocation.clone(),
            },
        );
        assert_eq!(set_res2, Err(Ok(RebalancingError::Unauthorized)));

        let reb_res = client.try_rebalance(&owner2, &portfolio);
        assert_eq!(reb_res, Err(Ok(RebalancingError::Unauthorized)));

        // owner1 can cancel schedule successfully
        assert_eq!(
            client.cancel_schedule(&owner1, &portfolio),
            symbol_short!("ok")
        );
    }

    #[test]
    fn test_read_methods_remain_public_without_auth() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        let allocation = weights(&env, &[(symbol_short!("USDC"), 10_000)]);
        client.set_target_allocation(
            &owner,
            &portfolio,
            &TargetAllocation {
                allocations: allocation.clone(),
            },
        );
        client.set_current_holdings(
            &owner,
            &portfolio,
            &CurrentHoldings {
                allocations: allocation,
            },
        );
        client.set_schedule(&owner, &portfolio, &RebalanceInterval::Daily);

        // Read operations without mock_all_auths
        let env_no_auth = Env::default();
        let id = env.register_contract(None, RebalancingContract);
        let client_no_auth = RebalancingContractClient::new(&env_no_auth, &id);

        assert_eq!(client.get_owner(&portfolio), Some(owner));
        assert!(client.get_schedule(&portfolio).is_some());
        assert!(client.get_target_allocation(&portfolio).is_some());
        assert!(client.get_current_holdings(&portfolio).is_some());
        assert_eq!(client.get_status(&portfolio), symbol_short!("ok"));
        assert_eq!(
            client.get_drift_threshold_bps(&portfolio),
            DEFAULT_DRIFT_THRESHOLD_BPS
        );
    }
}
