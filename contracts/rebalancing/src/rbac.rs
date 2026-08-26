//! # Role-Based Access Control (RBAC)
//!
//! Provides a comprehensive permission system for portfolio management.
//!
//! ## Roles
//!
//! | Role       | Description                                      | Default Permissions                                      |
//! |------------|--------------------------------------------------|-----------------------------------------------------------|
//! | Owner      | Full control over portfolio                      | ALL permissions                                           |
//! | Manager    | Can modify allocations and trigger rebalancing   | VIEW + MODIFY_ALLOCATIONS + REBALANCE + MANAGE_SCHEDULE + EXECUTE_REBALANCE |
//! | Viewer     | Read-only access to portfolio data               | VIEW                                                     |
//! | Liquidator | Emergency withdrawal only                        | VIEW + LIQUIDATE                                         |
//!
//! ## Permission Inheritance
//!
//! Manager ⊇ Viewer: Manager inherits all Viewer permissions.
//! Owner inherits all permissions from every other role.
//!
//! ## Time-Limited Roles
//!
//! Roles can be assigned with an optional expiry timestamp. Once the ledger
//! timestamp exceeds the expiry, the role is automatically considered revoked.
//! `expires_at = 0` means the role never expires.
//!
//! ## Permission Checking Flow
//!
//! 1. Load the `RoleAssignment` for `(portfolio_id, actor)`.
//! 2. If no assignment exists → deny (owner path is handled separately).
//! 3. If the assignment is expired → deny.
//! 4. Check `actor.permissions & required_permission == required_permission`.

use soroban_sdk::{contracttype, symbol_short, Address, Env, Symbol, Vec};

// ---------------------------------------------------------------------------
// Permission bitmask constants
// ---------------------------------------------------------------------------

/// Can read portfolio data (target allocation, current holdings, status, etc.).
pub const CAN_VIEW: u32 = 1 << 0;
/// Can modify target allocation, current holdings, drift threshold.
pub const CAN_MODIFY_ALLOCATIONS: u32 = 1 << 1;
/// Can trigger manual rebalancing.
pub const CAN_REBALANCE: u32 = 1 << 2;
/// Can set, update, or cancel rebalancing schedules.
pub const CAN_MANAGE_SCHEDULE: u32 = 1 << 3;
/// Can execute rebalance with a specific execution strategy.
pub const CAN_EXECUTE_REBALANCE: u32 = 1 << 4;
/// Can perform emergency liquidation withdrawals.
pub const CAN_LIQUIDATE: u32 = 1 << 5;
/// Can assign and revoke roles for other accounts.
pub const CAN_MANAGE_ROLES: u32 = 1 << 6;
/// Can configure system-level settings (audit sink, etc.).
pub const CAN_CONFIGURE: u32 = 1 << 7;

/// All permissions combined.
pub const ALL_PERMISSIONS: u32 = CAN_VIEW
    | CAN_MODIFY_ALLOCATIONS
    | CAN_REBALANCE
    | CAN_MANAGE_SCHEDULE
    | CAN_EXECUTE_REBALANCE
    | CAN_LIQUIDATE
    | CAN_MANAGE_ROLES
    | CAN_CONFIGURE;

// ---------------------------------------------------------------------------
// Role type
// ---------------------------------------------------------------------------

/// Distinct roles that can be assigned to accounts for a portfolio.
#[contracttype]
#[derive(Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// Full control. Assigned implicitly to the portfolio creator.
    Owner,
    /// Can modify allocations and trigger rebalancing.
    Manager,
    /// Read-only access.
    Viewer,
    /// Emergency withdrawal only.
    Liquidator,
}

impl Role {
    /// Return the default permissions bitmask for a role, accounting for
    /// inheritance (Manager includes Viewer, Owner includes everything).
    pub fn default_permissions(&self) -> u32 {
        match self {
            Role::Owner => ALL_PERMISSIONS,
            Role::Manager => {
                CAN_VIEW
                    | CAN_MODIFY_ALLOCATIONS
                    | CAN_REBALANCE
                    | CAN_MANAGE_SCHEDULE
                    | CAN_EXECUTE_REBALANCE
            }
            Role::Viewer => CAN_VIEW,
            Role::Liquidator => CAN_VIEW | CAN_LIQUIDATE,
        }
    }

    /// Human-readable label for the role.
    pub fn label(&self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Manager => "manager",
            Role::Viewer => "viewer",
            Role::Liquidator => "liquidator",
        }
    }
}

/// Decode a human-readable role label into the `Role` enum.
pub fn role_from_label(label: &str) -> Option<Role> {
    match label {
        "owner" => Some(Role::Owner),
        "manager" => Some(Role::Manager),
        "viewer" => Some(Role::Viewer),
        "liquidator" => Some(Role::Liquidator),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Permission helpers
// ---------------------------------------------------------------------------

/// Check whether a bitmask `held` contains ALL bits in `required`.
///
/// This is the fundamental permission predicate used by every check.
#[inline]
pub fn permission_contains(held: u32, required: u32) -> bool {
    held & required == required
}

/// Return a human-readable list of permission names present in `perms`.
pub fn describe_permissions(env: &Env, perms: u32) -> Vec<Symbol> {
    let mut out = Vec::new(env);
    if permission_contains(perms, CAN_VIEW) {
        out.push_back(symbol_short!("VIEW"));
    }
    if permission_contains(perms, CAN_MODIFY_ALLOCATIONS) {
        out.push_back(symbol_short!("MOD_ALLOC"));
    }
    if permission_contains(perms, CAN_REBALANCE) {
        out.push_back(symbol_short!("REBAL"));
    }
    if permission_contains(perms, CAN_MANAGE_SCHEDULE) {
        out.push_back(symbol_short!("MNG_SCHED"));
    }
    if permission_contains(perms, CAN_EXECUTE_REBALANCE) {
        out.push_back(symbol_short!("EXEC_RBL"));
    }
    if permission_contains(perms, CAN_LIQUIDATE) {
        out.push_back(symbol_short!("LIQUIDATE"));
    }
    if permission_contains(perms, CAN_MANAGE_ROLES) {
        out.push_back(symbol_short!("MNG_ROLES"));
    }
    if permission_contains(perms, CAN_CONFIGURE) {
        out.push_back(symbol_short!("CONFIGURE"));
    }
    out
}

// ---------------------------------------------------------------------------
// Stored role assignment
// ---------------------------------------------------------------------------

/// A single role assignment for an account on a portfolio.
///
/// `expires_at` is an optional ledger timestamp. If non-zero and the
/// current ledger time >= `expires_at`, the assignment is expired and
/// treated as revoked.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleAssignment {
    /// The account holding this role.
    pub address: Address,
    /// The assigned role.
    pub role: Role,
    /// Effective permissions bitmask (may be a subset of the role's defaults
    /// if the owner chose to restrict the grant).
    pub permissions: u32,
    /// Expiry timestamp (ledger seconds). `0` means no expiry.
    pub expires_at: u64,
    /// Ledger timestamp when this assignment was created.
    pub granted_at: u64,
    /// The address that granted this role (the portfolio owner or another
    /// account with `CAN_MANAGE_ROLES`).
    pub granted_by: Address,
}

impl RoleAssignment {
    /// Returns `true` if this assignment has expired relative to `now_ts`.
    pub fn is_expired(&self, now_ts: u64) -> bool {
        self.expires_at > 0 && now_ts >= self.expires_at
    }

    /// Returns `true` if this assignment grants ALL bits in `required`.
    pub fn has_permission(&self, required: u32) -> bool {
        permission_contains(self.permissions, required)
    }
}

/// Storage key for role assignments scoped to a portfolio.
#[contracttype]
#[derive(Clone, Debug)]
pub enum RbacStorageKey {
    /// Role assignment: (portfolio_id, assignee_address) -> RoleAssignment
    RoleAssignment(Symbol, Address),
    /// Access log entry index: portfolio_id -> u64 (next log index)
    AccessLogIndex(Symbol),
    /// A single access log entry: (portfolio_id, index) -> AccessLogEntry
    AccessLogEntry(Symbol, u64),
    /// Total count of active role assignments for a portfolio.
    /// Used for revocation auditing.
    RoleCount(Symbol),
}

// ---------------------------------------------------------------------------
// Access log (audit trail)
// ---------------------------------------------------------------------------

/// Logged permission check result for audit trails.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AccessLogEntry {
    /// Sequence number within the portfolio's log.
    pub index: u64,
    /// Ledger timestamp.
    pub timestamp: u64,
    /// The account that attempted access.
    pub actor: Address,
    /// The required permission that was checked.
    pub required_permission: u32,
    /// The permission bitmask the actor actually held.
    pub actor_permissions: u32,
    /// Whether access was granted.
    pub granted: bool,
    /// The function/resource that was accessed.
    pub action: Symbol,
}

/// Result of a permission check with rich context.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PermissionCheckResult {
    /// Whether access was granted.
    pub granted: bool,
    /// The permission bitmask the actor held (0 if no assignment).
    pub held_permissions: u32,
    /// The permission bits that were required.
    pub required_permission: u32,
    /// Missing permission bits (0 if granted).
    pub missing_permissions: u32,
    /// The role of the actor, if any.
    pub role: Role,
    /// Whether a role is assigned.
    pub has_role: bool,
    /// Whether the assignment has expired.
    pub expired: bool,
}

// ---------------------------------------------------------------------------
// Permission checking
// ---------------------------------------------------------------------------

/// Check whether an `actor` address has the required `permission` bits for a
/// given `portfolio_id`. Returns `Ok(permissions_held)` if granted, or
/// `Err(held_permissions)` if denied.
///
/// Permission checking logic:
/// 1. Load the `RoleAssignment` for `(portfolio_id, actor)`.
/// 2. If no assignment exists → deny (owner is handled at the call site).
/// 3. If the assignment is expired → deny.
/// 4. Check `actor.permissions & required_permission == required_permission`.
pub fn check_permission(
    env: &Env,
    portfolio_id: &Symbol,
    actor: &Address,
    required_permission: u32,
) -> Result<u32, u32> {
    let key = RbacStorageKey::RoleAssignment(portfolio_id.clone(), actor.clone());
    if let Some(assignment) = env
        .storage()
        .persistent()
        .get::<RbacStorageKey, RoleAssignment>(&key)
    {
        // Check expiry.
        if assignment.is_expired(env.ledger().timestamp()) {
            return Err(0);
        }
        if assignment.has_permission(required_permission) {
            return Ok(assignment.permissions);
        } else {
            return Err(assignment.permissions);
        }
    }
    Err(0)
}

/// Rich permission check returning a `PermissionCheckResult` with full context.
pub fn check_permission_detailed(
    env: &Env,
    portfolio_id: &Symbol,
    actor: &Address,
    required_permission: u32,
) -> PermissionCheckResult {
    let key = RbacStorageKey::RoleAssignment(portfolio_id.clone(), actor.clone());
    if let Some(assignment) = env
        .storage()
        .persistent()
        .get::<RbacStorageKey, RoleAssignment>(&key)
    {
        let expired = assignment.is_expired(env.ledger().timestamp());
        let granted = !expired && assignment.has_permission(required_permission);
        let missing = if granted {
            0
        } else {
            required_permission & !assignment.permissions
        };
        PermissionCheckResult {
            granted,
            held_permissions: assignment.permissions,
            required_permission,
            missing_permissions: missing,
            role: assignment.role,
            has_role: true,
            expired,
        }
    } else {
        PermissionCheckResult {
            granted: false,
            held_permissions: 0,
            required_permission,
            missing_permissions: required_permission,
            role: Role::Viewer,
            has_role: false,
            expired: false,
        }
    }
}

/// Convenience: check whether `actor` holds `required_permission` (boolean).
pub fn has_permission(
    env: &Env,
    portfolio_id: &Symbol,
    actor: &Address,
    required_permission: u32,
) -> bool {
    check_permission(env, portfolio_id, actor, required_permission).is_ok()
}

/// Convenience: assert that `actor` has `required_permission`, logging the
/// access attempt. Returns `Ok(())` on success, or `Err(held_permissions)`.
pub fn assert_permission(
    env: &Env,
    portfolio_id: &Symbol,
    actor: &Address,
    required_permission: u32,
    action: &Symbol,
) -> Result<(), u32> {
    let result = check_permission(env, portfolio_id, actor, required_permission);
    let (granted, actor_perms) = match result {
        Ok(perms) => (true, perms),
        Err(perms) => (false, perms),
    };

    log_access(
        env,
        portfolio_id,
        actor,
        required_permission,
        actor_perms,
        granted,
        action,
    );

    result?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Access logging
// ---------------------------------------------------------------------------

/// Append an access log entry for the given portfolio.
pub fn log_access(
    env: &Env,
    portfolio_id: &Symbol,
    actor: &Address,
    required_permission: u32,
    actor_permissions: u32,
    granted: bool,
    action: &Symbol,
) {
    let index_key = RbacStorageKey::AccessLogIndex(portfolio_id.clone());
    let index: u64 = env.storage().persistent().get(&index_key).unwrap_or(0u64);
    let next_index = index + 1;

    let entry = AccessLogEntry {
        index: next_index,
        timestamp: env.ledger().timestamp(),
        actor: actor.clone(),
        required_permission,
        actor_permissions,
        granted,
        action: action.clone(),
    };

    let entry_key = RbacStorageKey::AccessLogEntry(portfolio_id.clone(), next_index);
    env.storage().persistent().set(&entry_key, &entry);
    env.storage().persistent().set(&index_key, &next_index);
}

// ---------------------------------------------------------------------------
// Role management
// ---------------------------------------------------------------------------

/// Assign a role to an `assignee` for a `portfolio_id`.
///
/// - `granter` must hold `CAN_MANAGE_ROLES` (or be the portfolio owner).
/// - `expires_at`: `0` for permanent, or a future ledger timestamp.
/// - `permissions_override`: `None` uses the role's default permissions;
///   `Some(bits)` applies a custom subset.
///
/// If the assignee already has a role, it is overwritten.
pub fn assign_role(
    env: &Env,
    portfolio_id: &Symbol,
    granter: &Address,
    assignee: &Address,
    role: Role,
    expires_at: u64,
    permissions_override: Option<u32>,
) -> Result<(), u32> {
    assert_permission(
        env,
        portfolio_id,
        granter,
        CAN_MANAGE_ROLES,
        &Symbol::new(env, "assign_role"),
    )?;

    let permissions = permissions_override.unwrap_or_else(|| role.default_permissions());
    let now = env.ledger().timestamp();

    let assignment = RoleAssignment {
        address: assignee.clone(),
        role,
        permissions,
        expires_at,
        granted_at: now,
        granted_by: granter.clone(),
    };

    let key = RbacStorageKey::RoleAssignment(portfolio_id.clone(), assignee.clone());
    env.storage().persistent().set(&key, &assignment);

    // Update role count for the portfolio.
    let count_key = RbacStorageKey::RoleCount(portfolio_id.clone());
    let prev_had = env.storage().persistent().has(&key);
    if !prev_had {
        let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
        env.storage().persistent().set(&count_key, &(count + 1));
    }

    Ok(())
}

/// Revoke a role from `assignee` for a `portfolio_id`.
///
/// - `revoker` must hold `CAN_MANAGE_ROLES` (or be the portfolio owner).
pub fn revoke_role(
    env: &Env,
    portfolio_id: &Symbol,
    revoker: &Address,
    assignee: &Address,
) -> Result<(), u32> {
    assert_permission(
        env,
        portfolio_id,
        revoker,
        CAN_MANAGE_ROLES,
        &Symbol::new(env, "revoke_role"),
    )?;

    let key = RbacStorageKey::RoleAssignment(portfolio_id.clone(), assignee.clone());
    if env.storage().persistent().has(&key) {
        env.storage().persistent().remove(&key);

        // Decrement role count.
        let count_key = RbacStorageKey::RoleCount(portfolio_id.clone());
        let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
        if count > 0 {
            env.storage().persistent().set(&count_key, &(count - 1));
        }

        Ok(())
    } else {
        Err(0)
    }
}

/// Revoke ALL non-owner roles for a portfolio. Owner-only emergency function.
pub fn revoke_all_roles(
    env: &Env,
    portfolio_id: &Symbol,
    revoker: &Address,
    known_addresses: &Vec<Address>,
) -> Result<u32, u32> {
    assert_permission(
        env,
        portfolio_id,
        revoker,
        CAN_MANAGE_ROLES,
        &Symbol::new(env, "revoke_all"),
    )?;

    let mut revoked: u32 = 0;
    for addr in known_addresses.iter() {
        let key = RbacStorageKey::RoleAssignment(portfolio_id.clone(), addr.clone());
        if let Some(assignment) = env
            .storage()
            .persistent()
            .get::<RbacStorageKey, RoleAssignment>(&key)
        {
            // Never revoke the owner.
            if assignment.role != Role::Owner {
                env.storage().persistent().remove(&key);
                revoked += 1;
            }
        }
    }

    // Reset the role count (only owner may remain).
    let count_key = RbacStorageKey::RoleCount(portfolio_id.clone());
    let owner_count: u32 = if revoked > 0 { 0 } else { 0 };
    env.storage().persistent().set(&count_key, &owner_count);

    Ok(revoked)
}

/// Read the current role assignment for an account on a portfolio.
/// Returns `None` if no assignment exists or if it has expired.
pub fn get_role_assignment(
    env: &Env,
    portfolio_id: &Symbol,
    assignee: &Address,
) -> Option<RoleAssignment> {
    let key = RbacStorageKey::RoleAssignment(portfolio_id.clone(), assignee.clone());
    let assignment: RoleAssignment = env.storage().persistent().get(&key)?;

    if assignment.is_expired(env.ledger().timestamp()) {
        return None;
    }
    Some(assignment)
}

/// Read the raw stored assignment without expiry checking (for admin queries).
pub fn get_raw_assignment(
    env: &Env,
    portfolio_id: &Symbol,
    assignee: &Address,
) -> Option<RoleAssignment> {
    let key = RbacStorageKey::RoleAssignment(portfolio_id.clone(), assignee.clone());
    env.storage().persistent().get(&key)
}

/// Retrieve the access log for a portfolio.
pub fn get_access_log(env: &Env, portfolio_id: &Symbol) -> Vec<AccessLogEntry> {
    let index_key = RbacStorageKey::AccessLogIndex(portfolio_id.clone());
    let count: u64 = env.storage().persistent().get(&index_key).unwrap_or(0);
    let mut log = Vec::new(env);
    for i in 1..=count {
        let entry_key = RbacStorageKey::AccessLogEntry(portfolio_id.clone(), i);
        if let Some(entry) = env
            .storage()
            .persistent()
            .get::<RbacStorageKey, AccessLogEntry>(&entry_key)
        {
            log.push_back(entry);
        }
    }
    log
}

/// Extend the expiry of an existing role assignment.
///
/// `new_expiry`: `0` to make permanent, or a future ledger timestamp.
pub fn extend_role_expiry(
    env: &Env,
    portfolio_id: &Symbol,
    granter: &Address,
    assignee: &Address,
    new_expiry: u64,
) -> Result<(), u32> {
    assert_permission(
        env,
        portfolio_id,
        granter,
        CAN_MANAGE_ROLES,
        &Symbol::new(env, "extend_expiry"),
    )?;

    let key = RbacStorageKey::RoleAssignment(portfolio_id.clone(), assignee.clone());
    let mut assignment: RoleAssignment = env.storage().persistent().get(&key).ok_or(0u32)?;

    assignment.expires_at = new_expiry;
    env.storage().persistent().set(&key, &assignment);
    Ok(())
}
