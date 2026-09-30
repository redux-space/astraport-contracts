//! Comprehensive tests for the RBAC system.
//!
//! Covers:
//! - Role assignment and revocation
//! - Permission checking for all roles (Owner, Manager, Viewer, Liquidator)
//! - Role inheritance (Manager ⊇ Viewer)
//! - Granular permissions per function
//! - Time-limited role assignments with expiry
//! - Access logging for audit trails
//! - Unauthorized access rejection with clear errors
//! - Permission changes taking effect immediately
//! - Detailed permission check results
//! - Describe permissions
//! - Bulk revocation
//! - RolePermission helpers

#[cfg(test)]
mod tests {
    use crate::rbac::{
        assign_role, check_permission, check_permission_detailed, describe_permissions,
        extend_role_expiry, get_access_log, get_raw_assignment, get_role_assignment,
        has_permission, permission_contains, revoke_all_roles, revoke_role, AccessLogEntry, Role,
        RoleAssignment, ALL_PERMISSIONS, CAN_CONFIGURE, CAN_EXECUTE_REBALANCE, CAN_LIQUIDATE,
        CAN_MANAGE_ROLES, CAN_MANAGE_SCHEDULE, CAN_MODIFY_ALLOCATIONS, CAN_REBALANCE, CAN_VIEW,
    };
    use soroban_sdk::{symbol_short, testutils::Address as _, testutils::Ledger, Env, Symbol};

    // Helper: create a portfolio and set an owner.
    fn setup_portfolio(env: &Env) -> (Symbol, soroban_sdk::Address) {
        let portfolio = symbol_short!("port1");
        let owner = soroban_sdk::Address::generate(env);
        use crate::{RebalancingContract, TargetAllocation};
        let allocation = {
            let mut m = soroban_sdk::Map::new(env);
            m.set(symbol_short!("USDC"), 10_000u32);
            m
        };
        env.mock_all_auths();
        RebalancingContract::set_target_allocation(
            env.clone(),
            owner.clone(),
            portfolio.clone(),
            TargetAllocation {
                allocations: allocation.clone(),
            },
        )
        .unwrap();
        (portfolio, owner)
    }

    // ================================================================
    // Role type tests
    // ================================================================

    #[test]
    fn test_owner_role_has_all_permissions() {
        assert_eq!(Role::Owner.default_permissions(), ALL_PERMISSIONS);
    }

    #[test]
    fn test_manager_role_has_view_modify_rebalance_schedule_execute() {
        let perms = Role::Manager.default_permissions();
        assert!(perms & CAN_VIEW != 0);
        assert!(perms & CAN_MODIFY_ALLOCATIONS != 0);
        assert!(perms & CAN_REBALANCE != 0);
        assert!(perms & CAN_MANAGE_SCHEDULE != 0);
        assert!(perms & CAN_EXECUTE_REBALANCE != 0);
        assert_eq!(perms & CAN_LIQUIDATE, 0);
        assert_eq!(perms & CAN_MANAGE_ROLES, 0);
        assert_eq!(perms & CAN_CONFIGURE, 0);
    }

    #[test]
    fn test_viewer_role_has_only_view() {
        assert_eq!(Role::Viewer.default_permissions(), CAN_VIEW);
    }

    #[test]
    fn test_liquidator_role_has_view_and_liquidate() {
        let perms = Role::Liquidator.default_permissions();
        assert!(perms & CAN_VIEW != 0);
        assert!(perms & CAN_LIQUIDATE != 0);
        assert_eq!(perms & CAN_MODIFY_ALLOCATIONS, 0);
        assert_eq!(perms & CAN_REBALANCE, 0);
        assert_eq!(perms & CAN_MANAGE_SCHEDULE, 0);
        assert_eq!(perms & CAN_EXECUTE_REBALANCE, 0);
        assert_eq!(perms & CAN_MANAGE_ROLES, 0);
        assert_eq!(perms & CAN_CONFIGURE, 0);
    }

    #[test]
    fn test_role_labels() {
        assert_eq!(Role::Owner.label(), "owner");
        assert_eq!(Role::Manager.label(), "manager");
        assert_eq!(Role::Viewer.label(), "viewer");
        assert_eq!(Role::Liquidator.label(), "liquidator");
    }

    // ================================================================
    // Permission helpers
    // ================================================================

    #[test]
    fn test_permission_contains_exact() {
        assert!(permission_contains(CAN_VIEW, CAN_VIEW));
        assert!(permission_contains(ALL_PERMISSIONS, CAN_VIEW));
        assert!(permission_contains(ALL_PERMISSIONS, CAN_REBALANCE));
    }

    #[test]
    fn test_permission_contains_missing() {
        assert!(!permission_contains(CAN_VIEW, CAN_REBALANCE));
        assert!(!permission_contains(CAN_VIEW, CAN_MODIFY_ALLOCATIONS));
    }

    #[test]
    fn test_permission_contains_combined() {
        let held = CAN_VIEW | CAN_REBALANCE;
        assert!(permission_contains(held, CAN_VIEW));
        assert!(permission_contains(held, CAN_REBALANCE));
        assert!(!permission_contains(held, CAN_MODIFY_ALLOCATIONS));
    }

    #[test]
    fn test_describe_permissions_view_only() {
        let env = Env::default();
        let perms = describe_permissions(&env, CAN_VIEW);
        assert_eq!(perms.len(), 1);
        assert_eq!(perms.get(0).unwrap(), symbol_short!("VIEW"));
    }

    #[test]
    fn test_describe_permissions_multiple() {
        let env = Env::default();
        let perms = describe_permissions(&env, CAN_VIEW | CAN_REBALANCE);
        assert_eq!(perms.len(), 2);
    }

    #[test]
    fn test_describe_permissions_all() {
        let env = Env::default();
        let perms = describe_permissions(&env, ALL_PERMISSIONS);
        assert_eq!(perms.len(), 8); // All 8 permission types
    }

    // ================================================================
    // Role assignment and revocation
    // ================================================================

    #[test]
    fn test_assign_and_get_role() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);

        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();

        let assignment = get_role_assignment(&env, &portfolio, &manager).unwrap();
        assert_eq!(assignment.role, Role::Manager);
        assert_eq!(assignment.permissions, Role::Manager.default_permissions());
        assert_eq!(assignment.granted_by, owner);
        assert_eq!(assignment.expires_at, 0);
    }

    #[test]
    fn test_revoke_role() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);

        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();
        assert!(get_role_assignment(&env, &portfolio, &viewer).is_some());

        revoke_role(&env, &portfolio, &owner, &viewer).unwrap();
        assert!(get_role_assignment(&env, &portfolio, &viewer).is_none());
    }

    #[test]
    fn test_revoke_nonexistent_role_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let nobody = soroban_sdk::Address::generate(&env);

        let result = revoke_role(&env, &portfolio, &owner, &nobody);
        assert!(result.is_err());
    }

    // ================================================================
    // Permission checking
    // ================================================================

    #[test]
    fn test_manager_can_modify_allocations() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &manager, CAN_MODIFY_ALLOCATIONS).is_ok());
    }

    #[test]
    fn test_manager_can_rebalance() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &manager, CAN_REBALANCE).is_ok());
    }

    #[test]
    fn test_manager_cannot_manage_roles() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &manager, CAN_MANAGE_ROLES).is_err());
    }

    #[test]
    fn test_manager_cannot_liquidate() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &manager, CAN_LIQUIDATE).is_err());
    }

    #[test]
    fn test_viewer_can_view() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &viewer, CAN_VIEW).is_ok());
    }

    #[test]
    fn test_viewer_cannot_modify_allocations() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &viewer, CAN_MODIFY_ALLOCATIONS).is_err());
    }

    #[test]
    fn test_viewer_cannot_rebalance() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &viewer, CAN_REBALANCE).is_err());
    }

    #[test]
    fn test_liquidator_can_liquidate() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let liq = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &liq, Role::Liquidator, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &liq, CAN_LIQUIDATE).is_ok());
    }

    #[test]
    fn test_liquidator_cannot_rebalance() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let liq = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &liq, Role::Liquidator, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &liq, CAN_REBALANCE).is_err());
    }

    #[test]
    fn test_liquidator_can_view() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let liq = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &liq, Role::Liquidator, 0, None).unwrap();
        assert!(check_permission(&env, &portfolio, &liq, CAN_VIEW).is_ok());
    }

    #[test]
    fn test_unassigned_account_has_no_permissions() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, _owner) = setup_portfolio(&env);
        let stranger = soroban_sdk::Address::generate(&env);
        assert!(check_permission(&env, &portfolio, &stranger, CAN_VIEW).is_err());
    }

    // ================================================================
    // has_permission convenience
    // ================================================================

    #[test]
    fn test_has_permission_true() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();
        assert!(has_permission(&env, &portfolio, &viewer, CAN_VIEW));
    }

    #[test]
    fn test_has_permission_false() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();
        assert!(!has_permission(&env, &portfolio, &viewer, CAN_REBALANCE));
    }

    // ================================================================
    // Detailed permission check
    // ================================================================

    #[test]
    fn test_detailed_check_granted() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();

        let result = check_permission_detailed(&env, &portfolio, &manager, CAN_REBALANCE);
        assert!(result.granted);
        assert_eq!(result.role, Role::Manager);
        assert!(!result.expired);
        assert_eq!(result.missing_permissions, 0);
    }

    #[test]
    fn test_detailed_check_denied() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();

        let result = check_permission_detailed(&env, &portfolio, &viewer, CAN_REBALANCE);
        assert!(!result.granted);
        assert_eq!(result.role, Role::Viewer);
        assert_eq!(result.missing_permissions, CAN_REBALANCE);
    }

    #[test]
    fn test_detailed_check_no_role() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, _owner) = setup_portfolio(&env);
        let stranger = soroban_sdk::Address::generate(&env);

        let result = check_permission_detailed(&env, &portfolio, &stranger, CAN_VIEW);
        assert!(!result.granted);
        assert_eq!(result.role, Role::Viewer);
        assert_eq!(result.held_permissions, 0);
    }

    #[test]
    fn test_detailed_check_expired() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);

        let mut ledger = env.ledger().get();
        ledger.timestamp = 100;
        env.ledger().set(ledger);

        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 150, None).unwrap();

        let mut ledger = env.ledger().get();
        ledger.timestamp = 200;
        env.ledger().set(ledger);

        let result = check_permission_detailed(&env, &portfolio, &manager, CAN_REBALANCE);
        assert!(!result.granted);
        assert!(result.expired);
    }

    // ================================================================
    // Role inheritance (Manager ⊇ Viewer)
    // ================================================================

    #[test]
    fn test_manager_inherits_viewer_permissions() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();

        assert!(check_permission(&env, &portfolio, &manager, CAN_VIEW).is_ok());
        assert!(check_permission(&env, &portfolio, &manager, CAN_MODIFY_ALLOCATIONS).is_ok());
        assert!(check_permission(&env, &portfolio, &manager, CAN_REBALANCE).is_ok());
        assert!(check_permission(&env, &portfolio, &manager, CAN_MANAGE_SCHEDULE).is_ok());
        assert!(check_permission(&env, &portfolio, &manager, CAN_EXECUTE_REBALANCE).is_ok());
    }

    // ================================================================
    // Time-limited role assignments
    // ================================================================

    #[test]
    fn test_time_limited_role_not_yet_expired() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);

        let mut ledger = env.ledger().get();
        ledger.timestamp = 100;
        env.ledger().set(ledger);

        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 200, None).unwrap();
        assert!(check_permission(&env, &portfolio, &manager, CAN_REBALANCE).is_ok());
    }

    #[test]
    fn test_time_limited_role_expired() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);

        let mut ledger = env.ledger().get();
        ledger.timestamp = 100;
        env.ledger().set(ledger);

        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 150, None).unwrap();

        let mut ledger = env.ledger().get();
        ledger.timestamp = 200;
        env.ledger().set(ledger);

        assert!(check_permission(&env, &portfolio, &manager, CAN_REBALANCE).is_err());
        assert!(get_role_assignment(&env, &portfolio, &manager).is_none());
    }

    #[test]
    fn test_permanent_role_never_expires() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);

        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();

        let mut ledger = env.ledger().get();
        ledger.timestamp = 999_999_999;
        env.ledger().set(ledger);

        assert!(check_permission(&env, &portfolio, &viewer, CAN_VIEW).is_ok());
    }

    #[test]
    fn test_extend_role_expiry() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);

        let mut ledger = env.ledger().get();
        ledger.timestamp = 100;
        env.ledger().set(ledger);

        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 150, None).unwrap();
        extend_role_expiry(&env, &portfolio, &owner, &manager, 300).unwrap();

        let mut ledger = env.ledger().get();
        ledger.timestamp = 200;
        env.ledger().set(ledger);

        assert!(check_permission(&env, &portfolio, &manager, CAN_REBALANCE).is_ok());
    }

    #[test]
    fn test_extend_role_to_permanent() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);

        let mut ledger = env.ledger().get();
        ledger.timestamp = 100;
        env.ledger().set(ledger);

        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 150, None).unwrap();
        extend_role_expiry(&env, &portfolio, &owner, &viewer, 0).unwrap();

        let mut ledger = env.ledger().get();
        ledger.timestamp = 200;
        env.ledger().set(ledger);

        assert!(check_permission(&env, &portfolio, &viewer, CAN_VIEW).is_ok());
    }

    // ================================================================
    // Access logging
    // ================================================================

    #[test]
    fn test_access_log_records_granted() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();

        let action = Symbol::new(&env, "rebalance");
        let _ = crate::rbac::assert_permission(&env, &portfolio, &manager, CAN_REBALANCE, &action);

        let log = get_access_log(&env, &portfolio);
        assert_eq!(log.len(), 1);
        let entry = log.get(0).unwrap();
        assert_eq!(entry.actor, manager);
        assert_eq!(entry.required_permission, CAN_REBALANCE);
        assert!(entry.granted);
    }

    #[test]
    fn test_access_log_records_denied() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let viewer = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();

        let action = Symbol::new(&env, "rebalance");
        let result =
            crate::rbac::assert_permission(&env, &portfolio, &viewer, CAN_REBALANCE, &action);
        assert!(result.is_err());

        let log = get_access_log(&env, &portfolio);
        assert_eq!(log.len(), 1);
        assert!(!log.get(0).unwrap().granted);
    }

    #[test]
    fn test_access_log_multiple_entries() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        let viewer = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();
        assign_role(&env, &portfolio, &owner, &viewer, Role::Viewer, 0, None).unwrap();

        let action = Symbol::new(&env, "rebalance");
        let _ = crate::rbac::assert_permission(&env, &portfolio, &manager, CAN_REBALANCE, &action);
        let _ = crate::rbac::assert_permission(&env, &portfolio, &viewer, CAN_REBALANCE, &action);

        let log = get_access_log(&env, &portfolio);
        assert_eq!(log.len(), 2);
        assert!(log.get(0).unwrap().granted);
        assert!(!log.get(1).unwrap().granted);
    }

    // ================================================================
    // Custom permissions override
    // ================================================================

    #[test]
    fn test_custom_permissions_override() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);

        let custom_perms = CAN_VIEW | CAN_REBALANCE;
        assign_role(
            &env,
            &portfolio,
            &owner,
            &manager,
            Role::Manager,
            0,
            Some(custom_perms),
        )
        .unwrap();

        assert!(check_permission(&env, &portfolio, &manager, CAN_VIEW).is_ok());
        assert!(check_permission(&env, &portfolio, &manager, CAN_REBALANCE).is_ok());
        assert!(check_permission(&env, &portfolio, &manager, CAN_MODIFY_ALLOCATIONS).is_err());
    }

    // ================================================================
    // Role overwrite
    // ================================================================

    #[test]
    fn test_reassign_role_overwrites_previous() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let account = soroban_sdk::Address::generate(&env);

        assign_role(&env, &portfolio, &owner, &account, Role::Viewer, 0, None).unwrap();
        assert_eq!(
            get_role_assignment(&env, &portfolio, &account)
                .unwrap()
                .role,
            Role::Viewer
        );

        assign_role(&env, &portfolio, &owner, &account, Role::Manager, 0, None).unwrap();
        let assignment = get_role_assignment(&env, &portfolio, &account).unwrap();
        assert_eq!(assignment.role, Role::Manager);
        assert!(check_permission(&env, &portfolio, &account, CAN_REBALANCE).is_ok());
    }

    // ================================================================
    // Raw assignment query (ignores expiry)
    // ================================================================

    #[test]
    fn test_get_raw_assignment_shows_expired_roles() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);

        let mut ledger = env.ledger().get();
        ledger.timestamp = 100;
        env.ledger().set(ledger);

        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 150, None).unwrap();

        let mut ledger = env.ledger().get();
        ledger.timestamp = 200;
        env.ledger().set(ledger);

        assert!(get_role_assignment(&env, &portfolio, &manager).is_none());

        let raw = get_raw_assignment(&env, &portfolio, &manager).unwrap();
        assert_eq!(raw.role, Role::Manager);
        assert_eq!(raw.expires_at, 150);
    }

    // ================================================================
    // Permission bitmask combination tests
    // ================================================================

    #[test]
    fn test_multiple_permission_check() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();

        let combined = CAN_VIEW | CAN_MODIFY_ALLOCATIONS | CAN_REBALANCE;
        let held = check_permission(&env, &portfolio, &manager, combined).unwrap();
        assert!(held & combined == combined);
    }

    #[test]
    fn test_partial_permission_check_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);
        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();

        let combined = CAN_VIEW | CAN_LIQUIDATE;
        assert!(check_permission(&env, &portfolio, &manager, combined).is_err());
    }

    // ================================================================
    // RoleAssignment helper methods
    // ================================================================

    #[test]
    fn test_role_assignment_is_expired() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);

        let mut ledger = env.ledger().get();
        ledger.timestamp = 100;
        env.ledger().set(ledger);

        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 150, None).unwrap();
        let assignment = get_raw_assignment(&env, &portfolio, &manager).unwrap();

        assert!(!assignment.is_expired(100));
        assert!(!assignment.is_expired(149));
        assert!(assignment.is_expired(150));
        assert!(assignment.is_expired(200));
    }

    #[test]
    fn test_role_assignment_has_permission() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let manager = soroban_sdk::Address::generate(&env);

        assign_role(&env, &portfolio, &owner, &manager, Role::Manager, 0, None).unwrap();
        let assignment = get_raw_assignment(&env, &portfolio, &manager).unwrap();

        assert!(assignment.has_permission(CAN_VIEW));
        assert!(assignment.has_permission(CAN_REBALANCE));
        assert!(!assignment.has_permission(CAN_LIQUIDATE));
        assert!(!assignment.has_permission(CAN_MANAGE_ROLES));
    }

    // ================================================================
    // Edge cases
    // ================================================================

    #[test]
    fn test_assign_role_with_zero_permissions() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let nobody = soroban_sdk::Address::generate(&env);

        assign_role(&env, &portfolio, &owner, &nobody, Role::Manager, 0, Some(0)).unwrap();

        assert!(check_permission(&env, &portfolio, &nobody, CAN_VIEW).is_err());
        assert!(check_permission(&env, &portfolio, &nobody, CAN_REBALANCE).is_err());
    }

    #[test]
    fn test_extend_nonexistent_role_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let (portfolio, owner) = setup_portfolio(&env);
        let stranger = soroban_sdk::Address::generate(&env);

        let result = extend_role_expiry(&env, &portfolio, &owner, &stranger, 300);
        assert!(result.is_err());
    }
}
