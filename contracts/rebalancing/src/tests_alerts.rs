//! Comprehensive tests for the portfolio alert & monitoring system.
//!
//! Covers:
//! - Alert config CRUD (set/get, add/remove/update threshold, master switch)
//! - Threshold limit enforcement and error paths
//! - Drift breach firing (portfolio-wide and per-asset) via the contract client
//! - Low-balance and yield-underperformance alerts via supplied observations
//! - Custom exact-match metric
//! - Range-based bounds (lower_bound, upper_bound)
//! - Threshold-triggered actions
//! - Master switch and per-threshold `enabled` short-circuits
//! - Acknowledgment and pending-alert filtering
//! - Alert statistics tracking
//! - update_threshold() in-place replacement
//! - RBAC gating of config mutations (owner and `CAN_CONFIGURE` grantee vs stranger)
//! - Event emission on firing

#[cfg(test)]
mod tests {
    use crate::alerts::{
        AlertAction, AlertConfig, AlertSeverity, AlertStatistics, AlertThreshold, Comparison,
        MetricObservation, MetricType, MAX_THRESHOLDS_PER_CONFIG,
    };
    use crate::rbac::{Role, CAN_CONFIGURE};
    use crate::{
        CurrentHoldings, RebalancingContract, RebalancingContractClient, RebalancingError,
        TargetAllocation,
    };
    use soroban_sdk::{
        symbol_short, testutils::Address as _, testutils::Events, Address, Env, Map, String,
        Symbol, Vec,
    };

    // ----------------------------------------------------------------
    // Helpers
    // ----------------------------------------------------------------

    fn client(env: &Env) -> RebalancingContractClient<'_> {
        let id = env.register_contract(None, RebalancingContract);
        RebalancingContractClient::new(env, &id)
    }

    fn weights(env: &Env, entries: &[(Symbol, u32)]) -> Map<Symbol, u32> {
        let mut result = Map::new(env);
        for (asset, weight) in entries.iter() {
            result.set(asset.clone(), *weight);
        }
        result
    }

    fn empty_config(env: &Env, portfolio: &Symbol, enabled: bool) -> AlertConfig {
        AlertConfig {
            portfolio_id: portfolio.clone(),
            thresholds: Vec::new(env),
            alerts_enabled: enabled,
        }
    }

    fn threshold(
        env: &Env,
        metric: MetricType,
        comparison: Comparison,
        trigger: i128,
        asset: Symbol,
    ) -> AlertThreshold {
        AlertThreshold {
            metric,
            comparison,
            trigger_value: trigger,
            severity: AlertSeverity::Warning,
            asset,
            label: String::from_str(env, "t"),
            enabled: true,
            lower_bound: None,
            upper_bound: None,
            action: AlertAction::None,
        }
    }

    fn threshold_with_bounds(
        env: &Env,
        metric: MetricType,
        asset: Symbol,
        lower: Option<i128>,
        upper: Option<i128>,
    ) -> AlertThreshold {
        AlertThreshold {
            metric,
            comparison: Comparison::Above,
            trigger_value: 0,
            severity: AlertSeverity::Warning,
            asset,
            label: String::from_str(env, "range"),
            enabled: true,
            lower_bound: lower,
            upper_bound: upper,
            action: AlertAction::None,
        }
    }

    fn threshold_with_action(
        env: &Env,
        metric: MetricType,
        comparison: Comparison,
        trigger: i128,
        asset: Symbol,
        action: AlertAction,
    ) -> AlertThreshold {
        AlertThreshold {
            metric,
            comparison,
            trigger_value: trigger,
            severity: AlertSeverity::Critical,
            asset,
            label: String::from_str(env, "act"),
            enabled: true,
            lower_bound: None,
            upper_bound: None,
            action,
        }
    }

    fn obs(metric: MetricType, asset: Symbol, value: i128) -> MetricObservation {
        MetricObservation {
            metric,
            asset,
            value,
        }
    }

    fn setup_drift_portfolio(
        env: &Env,
        client: &RebalancingContractClient<'_>,
        owner: &Address,
        portfolio: &Symbol,
        target: &[(Symbol, u32)],
        current: &[(Symbol, u32)],
    ) {
        client.set_target_allocation(
            owner,
            portfolio,
            &TargetAllocation {
                allocations: weights(env, target),
            },
        );
        client.set_current_holdings(
            owner,
            portfolio,
            &CurrentHoldings {
                allocations: weights(env, current),
            },
        );
        client.set_alert_config(owner, portfolio, &empty_config(env, portfolio, true));
    }

    // ----------------------------------------------------------------
    // Config CRUD
    // ----------------------------------------------------------------

    #[test]
    fn test_set_and_get_alert_config() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        assert!(client.get_alert_config(&portfolio).is_none());
        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));

        let got = client.get_alert_config(&portfolio).unwrap();
        assert_eq!(got.portfolio_id, portfolio);
        assert_eq!(got.thresholds.len(), 0);
        assert!(got.alerts_enabled);
    }

    #[test]
    fn test_set_alert_config_forces_portfolio_id() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        let mut cfg = empty_config(&env, &symbol_short!("other"), true);
        cfg.portfolio_id = symbol_short!("other");
        let stored = client.set_alert_config(&owner, &portfolio, &cfg);
        assert_eq!(stored.portfolio_id, portfolio);
        assert!(client.get_alert_config(&symbol_short!("other")).is_none());
    }

    #[test]
    fn test_add_threshold_appends() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        let t = threshold(
            &env,
            MetricType::PortfolioDrift,
            Comparison::Above,
            100,
            symbol_short!("ALL"),
        );
        let cfg = client.add_alert_threshold(&owner, &portfolio, &t);
        assert_eq!(cfg.thresholds.len(), 1);
        assert_eq!(
            client
                .get_alert_config(&portfolio)
                .unwrap()
                .thresholds
                .len(),
            1
        );
    }

    #[test]
    fn test_add_threshold_no_config_errors() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_target_allocation(
            &owner,
            &portfolio,
            &TargetAllocation {
                allocations: weights(&env, &[(symbol_short!("USDC"), 10_000)]),
            },
        );
        let t = threshold(
            &env,
            MetricType::PortfolioDrift,
            Comparison::Above,
            100,
            symbol_short!("ALL"),
        );
        let res = client.try_add_alert_threshold(&owner, &portfolio, &t);
        assert_eq!(res, Err(Ok(RebalancingError::AlertConfigNotFound)));
    }

    #[test]
    fn test_add_threshold_limit_enforced() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        for _ in 0..MAX_THRESHOLDS_PER_CONFIG {
            client.add_alert_threshold(
                &owner,
                &portfolio,
                &threshold(&env, MetricType::Custom, Comparison::Equal, 1, symbol_short!("ALL")),
            );
        }
        let res = client.try_add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(&env, MetricType::Custom, Comparison::Equal, 1, symbol_short!("ALL")),
        );
        assert_eq!(res, Err(Ok(RebalancingError::AlertThresholdLimitReached)));
    }

    #[test]
    fn test_remove_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                111,
                symbol_short!("ALL"),
            ),
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                222,
                symbol_short!("ALL"),
            ),
        );

        let cfg = client.remove_alert_threshold(&owner, &portfolio, &0);
        assert_eq!(cfg.thresholds.len(), 1);
        assert_eq!(cfg.thresholds.get(0).unwrap().trigger_value, 222);

        let res = client.try_remove_alert_threshold(&owner, &portfolio, &5);
        assert_eq!(res, Err(Ok(RebalancingError::AlertIndexOutOfRange)));
    }

    // ----------------------------------------------------------------
    // update_threshold
    // ----------------------------------------------------------------

    #[test]
    fn test_update_threshold_replaces_in_place() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                100,
                symbol_short!("ALL"),
            ),
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(&env, MetricType::Balance, Comparison::Below, 500, symbol_short!("ALL")),
        );

        // Update index 0 to have a different trigger value.
        let new_t = threshold(
            &env,
            MetricType::PortfolioDrift,
            Comparison::Above,
            999,
            symbol_short!("ALL"),
        );
        let cfg = client.update_alert_threshold(&owner, &portfolio, &0, &new_t);
        assert_eq!(cfg.thresholds.len(), 2);
        assert_eq!(cfg.thresholds.get(0).unwrap().trigger_value, 999);
        // Index 1 is unchanged.
        assert_eq!(cfg.thresholds.get(1).unwrap().trigger_value, 500);
    }

    #[test]
    fn test_update_threshold_no_config_errors() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        let t = threshold(&env, MetricType::Custom, Comparison::Equal, 1, symbol_short!("ALL"));
        let res = client.try_update_alert_threshold(&owner, &portfolio, &0, &t);
        assert_eq!(res, Err(Ok(RebalancingError::AlertConfigNotFound)));
    }

    #[test]
    fn test_update_threshold_out_of_range_errors() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        let t = threshold(&env, MetricType::Custom, Comparison::Equal, 1, symbol_short!("ALL"));
        let res = client.try_update_alert_threshold(&owner, &portfolio, &5, &t);
        assert_eq!(res, Err(Ok(RebalancingError::AlertIndexOutOfRange)));
    }

    #[test]
    fn test_update_threshold_reflects_in_next_check() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        // Set up a drift portfolio where max abs drift = 300.
        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        // Add a threshold with trigger at 500 — won't fire (300 < 500).
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                500,
                symbol_short!("ALL"),
            ),
        );
        assert_eq!(client.check_portfolio_alerts(&portfolio), 0);

        // Update the threshold to 200 — now 300 > 200, so it fires.
        let updated = threshold(
            &env,
            MetricType::PortfolioDrift,
            Comparison::Above,
            200,
            symbol_short!("ALL"),
        );
        client.update_alert_threshold(&owner, &portfolio, &0, &updated);
        assert_eq!(client.check_portfolio_alerts(&portfolio), 1);
    }

    // ----------------------------------------------------------------
    // Drift monitoring
    // ----------------------------------------------------------------

    #[test]
    fn test_portfolio_drift_breach_fires_and_records() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                200,
                symbol_short!("ALL"),
            ),
        );

        let fired = client.check_portfolio_alerts(&portfolio);
        assert_eq!(fired, 1);

        let hist = client.get_alert_history(&portfolio);
        assert_eq!(hist.len(), 1);
        let e = hist.get(0).unwrap();
        assert_eq!(e.metric, MetricType::PortfolioDrift);
        assert_eq!(e.asset, symbol_short!("ALL"));
        assert_eq!(e.observed_value, 300);
        assert_eq!(e.threshold_value, 200);
        assert_eq!(e.severity, AlertSeverity::Warning);
        assert!(!e.acknowledged);
    }

    #[test]
    fn test_no_breach_when_within_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                400,
                symbol_short!("ALL"),
            ),
        );

        assert_eq!(client.check_portfolio_alerts(&portfolio), 0);
        assert_eq!(client.get_alert_history(&portfolio).len(), 0);
    }

    #[test]
    fn test_asset_drift_targets_specific_asset() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 5_000),
                (symbol_short!("XLM"), 3_000),
                (symbol_short!("BTC"), 2_000),
            ],
            &[
                (symbol_short!("USDC"), 5_400),
                (symbol_short!("XLM"), 2_800),
                (symbol_short!("BTC"), 1_800),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::AssetDrift,
                Comparison::Above,
                300,
                symbol_short!("USDC"),
            ),
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::AssetDrift,
                Comparison::Above,
                300,
                symbol_short!("XLM"),
            ),
        );

        let fired = client.check_portfolio_alerts(&portfolio);
        assert_eq!(fired, 1);
        let e = client.get_alert_history(&portfolio).get(0).unwrap();
        assert_eq!(e.metric, MetricType::AssetDrift);
        assert_eq!(e.asset, symbol_short!("USDC"));
        assert_eq!(e.observed_value, 400);
    }

    // ----------------------------------------------------------------
    // Balance / yield / custom via supplied observations
    // ----------------------------------------------------------------

    #[test]
    fn test_low_balance_alert_below() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::Balance,
                Comparison::Below,
                1_000,
                symbol_short!("USDC"),
            ),
        );

        let mut extra = Vec::new(&env);
        extra.push_back(obs(MetricType::Balance, symbol_short!("USDC"), 500));

        let fired = client.check_portfolio_alerts_with(&portfolio, &extra);
        assert_eq!(fired, 1);
        let e = client.get_alert_history(&portfolio).get(0).unwrap();
        assert_eq!(e.metric, MetricType::Balance);
        assert_eq!(e.observed_value, 500);
    }

    #[test]
    fn test_yield_underperformance_below() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::Yield,
                Comparison::Below,
                500,
                symbol_short!("XLM"),
            ),
        );

        let mut extra = Vec::new(&env);
        extra.push_back(obs(MetricType::Yield, symbol_short!("XLM"), 300));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &extra), 1);

        let mut healthy = Vec::new(&env);
        healthy.push_back(obs(MetricType::Yield, symbol_short!("XLM"), 800));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &healthy), 0);
    }

    #[test]
    fn test_custom_equal_match() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::Custom,
                Comparison::Equal,
                42,
                symbol_short!("FLAG"),
            ),
        );

        let mut miss = Vec::new(&env);
        miss.push_back(obs(MetricType::Custom, symbol_short!("FLAG"), 43));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &miss), 0);

        let mut hit = Vec::new(&env);
        hit.push_back(obs(MetricType::Custom, symbol_short!("FLAG"), 42));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &hit), 1);
    }

    // ----------------------------------------------------------------
    // Range-based bounds
    // ----------------------------------------------------------------

    #[test]
    fn test_range_both_bounds_fires_when_below_lower() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold_with_bounds(
                &env,
                MetricType::Balance,
                symbol_short!("USDC"),
                Some(1_000),
                Some(10_000),
            ),
        );

        let mut extra = Vec::new(&env);
        extra.push_back(obs(MetricType::Balance, symbol_short!("USDC"), 500));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &extra), 1);
    }

    #[test]
    fn test_range_both_bounds_fires_when_above_upper() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold_with_bounds(
                &env,
                MetricType::Balance,
                symbol_short!("USDC"),
                Some(1_000),
                Some(10_000),
            ),
        );

        let mut extra = Vec::new(&env);
        extra.push_back(obs(
            MetricType::Balance,
            symbol_short!("USDC"),
            15_000,
        ));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &extra), 1);
    }

    #[test]
    fn test_range_both_bounds_no_fire_within_range() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold_with_bounds(
                &env,
                MetricType::Balance,
                symbol_short!("USDC"),
                Some(1_000),
                Some(10_000),
            ),
        );

        let mut extra = Vec::new(&env);
        extra.push_back(obs(MetricType::Balance, symbol_short!("USDC"), 5_000));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &extra), 0);
    }

    #[test]
    fn test_range_both_bounds_no_fire_on_boundary() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold_with_bounds(
                &env,
                MetricType::Balance,
                symbol_short!("USDC"),
                Some(1_000),
                Some(10_000),
            ),
        );

        // Exactly at the lower bound — should NOT fire.
        let mut at_lower = Vec::new(&env);
        at_lower.push_back(obs(MetricType::Balance, symbol_short!("USDC"), 1_000));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &at_lower), 0);

        // Exactly at the upper bound — should NOT fire.
        let mut at_upper = Vec::new(&env);
        at_upper.push_back(obs(
            MetricType::Balance,
            symbol_short!("USDC"),
            10_000,
        ));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &at_upper), 0);
    }

    #[test]
    fn test_range_only_lower_bound() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold_with_bounds(
                &env,
                MetricType::Yield,
                symbol_short!("XLM"),
                Some(500),
                None,
            ),
        );

        // Below lower bound — fires.
        let mut below = Vec::new(&env);
        below.push_back(obs(MetricType::Yield, symbol_short!("XLM"), 200));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &below), 1);

        // Above lower bound — does not fire.
        let mut above = Vec::new(&env);
        above.push_back(obs(MetricType::Yield, symbol_short!("XLM"), 800));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &above), 0);
    }

    #[test]
    fn test_range_only_upper_bound() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold_with_bounds(
                &env,
                MetricType::AssetDrift,
                symbol_short!("BTC"),
                None,
                Some(200),
            ),
        );

        // Above upper bound — fires.
        let mut above = Vec::new(&env);
        above.push_back(obs(MetricType::AssetDrift, symbol_short!("BTC"), 350));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &above), 1);

        // Below upper bound — does not fire.
        let mut below = Vec::new(&env);
        below.push_back(obs(MetricType::AssetDrift, symbol_short!("BTC"), 100));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &below), 0);
    }

    #[test]
    fn test_range_overrides_comparison() {
        // When bounds are set, comparison/trigger_value should be ignored.
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        // Set Comparison::Above with trigger_value=1000, but bounds [100, 500].
        // Value 300 is within bounds, so should NOT fire regardless of comparison.
        let mut t = threshold_with_bounds(
            &env,
            MetricType::Balance,
            symbol_short!("USDC"),
            Some(100),
            Some(500),
        );
        t.comparison = Comparison::Above;
        t.trigger_value = 100;
        client.add_alert_threshold(&owner, &portfolio, &t);

        let mut in_range = Vec::new(&env);
        in_range.push_back(obs(MetricType::Balance, symbol_short!("USDC"), 300));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &in_range), 0);
    }

    // ----------------------------------------------------------------
    // Alert actions
    // ----------------------------------------------------------------

    #[test]
    fn test_action_recorded_in_history() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold_with_action(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                200,
                symbol_short!("ALL"),
                AlertAction::EmergencyRebalance,
            ),
        );

        assert_eq!(client.check_portfolio_alerts(&portfolio), 1);
        let e = client.get_alert_history(&portfolio).get(0).unwrap();
        assert_eq!(e.action, AlertAction::EmergencyRebalance);
    }

    #[test]
    fn test_action_notify_variant() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold_with_action(
                &env,
                MetricType::Balance,
                Comparison::Below,
                1_000,
                symbol_short!("USDC"),
                AlertAction::Notify,
            ),
        );

        let mut extra = Vec::new(&env);
        extra.push_back(obs(MetricType::Balance, symbol_short!("USDC"), 500));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &extra), 1);
        let e = client.get_alert_history(&portfolio).get(0).unwrap();
        assert_eq!(e.action, AlertAction::Notify);
    }

    #[test]
    fn test_action_custom_variant() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold_with_action(
                &env,
                MetricType::Yield,
                Comparison::Below,
                100,
                symbol_short!("ETH"),
                AlertAction::Custom(symbol_short!("STK_MORE")),
            ),
        );

        let mut extra = Vec::new(&env);
        extra.push_back(obs(MetricType::Yield, symbol_short!("ETH"), 50));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &extra), 1);
        let e = client.get_alert_history(&portfolio).get(0).unwrap();
        assert_eq!(e.action, AlertAction::Custom(symbol_short!("STK_MORE")));
    }

    #[test]
    fn test_action_none_is_default() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                200,
                symbol_short!("ALL"),
            ),
        );

        assert_eq!(client.check_portfolio_alerts(&portfolio), 1);
        let e = client.get_alert_history(&portfolio).get(0).unwrap();
        assert_eq!(e.action, AlertAction::None);
    }

    // ----------------------------------------------------------------
    // Short-circuits
    // ----------------------------------------------------------------

    #[test]
    fn test_master_switch_disables_all_alerts() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                100,
                symbol_short!("ALL"),
            ),
        );

        client.set_alerts_enabled(&owner, &portfolio, &false);
        assert_eq!(client.check_portfolio_alerts(&portfolio), 0);

        client.set_alerts_enabled(&owner, &portfolio, &true);
        assert_eq!(client.check_portfolio_alerts(&portfolio), 1);
    }

    #[test]
    fn test_disabled_threshold_is_skipped() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        let mut t = threshold(
            &env,
            MetricType::PortfolioDrift,
            Comparison::Above,
            100,
            symbol_short!("ALL"),
        );
        t.enabled = false;
        client.add_alert_threshold(&owner, &portfolio, &t);

        assert_eq!(client.check_portfolio_alerts(&portfolio), 0);
    }

    // ----------------------------------------------------------------
    // Acknowledge / pending
    // ----------------------------------------------------------------

    #[test]
    fn test_acknowledge_and_pending() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                100,
                symbol_short!("ALL"),
            ),
        );
        client.check_portfolio_alerts(&portfolio);

        assert_eq!(client.get_pending_alerts(&portfolio).len(), 1);

        client.acknowledge_alert(&owner, &portfolio, &0);
        assert_eq!(client.get_pending_alerts(&portfolio).len(), 0);
        assert!(
            client
                .get_alert_history(&portfolio)
                .get(0)
                .unwrap()
                .acknowledged
        );
    }

    #[test]
    fn test_acknowledge_out_of_range_errors() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        let res = client.try_acknowledge_alert(&owner, &portfolio, &0);
        assert_eq!(res, Err(Ok(RebalancingError::AlertIndexOutOfRange)));
    }

    // ----------------------------------------------------------------
    // Statistics
    // ----------------------------------------------------------------

    #[test]
    fn test_statistics_initially_none() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let portfolio = symbol_short!("port1");

        assert!(client.get_alert_statistics(&portfolio).is_none());
    }

    #[test]
    fn test_statistics_updated_on_fire() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                200,
                symbol_short!("ALL"),
            ),
        );

        client.check_portfolio_alerts(&portfolio);

        let stats = client.get_alert_statistics(&portfolio).unwrap();
        assert_eq!(stats.total_fired, 1);
        assert_eq!(stats.warning_fired, 1);
        assert_eq!(stats.info_fired, 0);
        assert_eq!(stats.critical_fired, 0);
        assert!(stats.last_fired_at > 0);
        assert_eq!(stats.unique_metrics_triggered, 1);
    }

    #[test]
    fn test_statistics_accumulate_across_checks() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                200,
                symbol_short!("ALL"),
            ),
        );

        // Fire twice.
        client.check_portfolio_alerts(&portfolio);
        client.check_portfolio_alerts(&portfolio);

        let stats = client.get_alert_statistics(&portfolio).unwrap();
        assert_eq!(stats.total_fired, 2);
        assert_eq!(stats.warning_fired, 2);
    }

    #[test]
    fn test_statistics_no_fire_means_no_stats() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        // Threshold above 500 won't fire (max drift = 300).
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                500,
                symbol_short!("ALL"),
            ),
        );

        client.check_portfolio_alerts(&portfolio);
        assert!(client.get_alert_statistics(&portfolio).is_none());
    }

    #[test]
    fn test_statistics_severity_breakdown() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );

        // Add three thresholds with different severities, all breach at drift=300.
        let mut t_info = threshold(
            &env,
            MetricType::PortfolioDrift,
            Comparison::Above,
            200,
            symbol_short!("ALL"),
        );
        t_info.severity = AlertSeverity::Info;
        client.add_alert_threshold(&owner, &portfolio, &t_info);

        let mut t_warn = threshold(
            &env,
            MetricType::PortfolioDrift,
            Comparison::Above,
            250,
            symbol_short!("ALL"),
        );
        t_warn.severity = AlertSeverity::Warning;
        client.add_alert_threshold(&owner, &portfolio, &t_warn);

        let mut t_crit = threshold(
            &env,
            MetricType::PortfolioDrift,
            Comparison::Above,
            100,
            symbol_short!("ALL"),
        );
        t_crit.severity = AlertSeverity::Critical;
        client.add_alert_threshold(&owner, &portfolio, &t_crit);

        client.check_portfolio_alerts(&portfolio);

        let stats = client.get_alert_statistics(&portfolio).unwrap();
        assert_eq!(stats.total_fired, 3);
        assert_eq!(stats.info_fired, 1);
        assert_eq!(stats.warning_fired, 1);
        assert_eq!(stats.critical_fired, 1);
    }

    #[test]
    fn test_statistics_multiple_thresholds_same_metric() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );

        // Two thresholds for the same metric — both fire.
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                200,
                symbol_short!("ALL"),
            ),
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                250,
                symbol_short!("ALL"),
            ),
        );

        let fired = client.check_portfolio_alerts(&portfolio);
        assert_eq!(fired, 2);

        let stats = client.get_alert_statistics(&portfolio).unwrap();
        assert_eq!(stats.total_fired, 2);
    }

    // ----------------------------------------------------------------
    // Multiple thresholds per portfolio
    // ----------------------------------------------------------------

    #[test]
    fn test_multiple_thresholds_fires_independently() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));

        // PortfolioDrift threshold — will fire (drift = 300 > 200).
        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                200,
                symbol_short!("ALL"),
            ),
        );

        // Balance threshold — will fire (500 < 1000).
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::Balance,
                Comparison::Below,
                1_000,
                symbol_short!("USDC"),
            ),
        );

        let mut extra = Vec::new(&env);
        extra.push_back(obs(MetricType::Balance, symbol_short!("USDC"), 500));

        let fired = client.check_portfolio_alerts_with(&portfolio, &extra);
        assert_eq!(fired, 2);

        let hist = client.get_alert_history(&portfolio);
        assert_eq!(hist.len(), 2);
    }

    // ----------------------------------------------------------------
    // RBAC gating & events
    // ----------------------------------------------------------------

    #[test]
    fn test_rbac_stranger_denied_grantee_allowed() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let stranger = Address::generate(&env);
        let grantee = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));

        let t = threshold(&env, MetricType::Custom, Comparison::Equal, 1, symbol_short!("ALL"));
        let res = client.try_add_alert_threshold(&stranger, &portfolio, &t);
        assert_eq!(res, Err(Ok(RebalancingError::PermissionDenied)));

        client.grant_role_with_permissions(
            &owner,
            &portfolio,
            &grantee,
            &Role::Manager,
            &CAN_CONFIGURE,
            &0,
        );
        let cfg = client.add_alert_threshold(&grantee, &portfolio, &t);
        assert_eq!(cfg.thresholds.len(), 1);
    }

    #[test]
    fn test_event_emitted_on_fire() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        setup_drift_portfolio(
            &env,
            &client,
            &owner,
            &portfolio,
            &[
                (symbol_short!("USDC"), 6_000),
                (symbol_short!("XLM"), 4_000),
            ],
            &[
                (symbol_short!("USDC"), 6_300),
                (symbol_short!("XLM"), 3_700),
            ],
        );
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::PortfolioDrift,
                Comparison::Above,
                100,
                symbol_short!("ALL"),
            ),
        );

        let before = env.events().all().len();
        client.check_portfolio_alerts(&portfolio);
        assert!(env.events().all().len() > before);
    }

    // ----------------------------------------------------------------
    // Edge cases: zero threshold values, no observations
    // ----------------------------------------------------------------

    #[test]
    fn test_no_observations_yields_zero_fires() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(&env, MetricType::Balance, Comparison::Below, 1_000, symbol_short!("ALL")),
        );

        // No observations supplied — threshold has no matching observation.
        let empty = Vec::new(&env);
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &empty), 0);
    }

    #[test]
    fn test_zero_trigger_value_equal_match() {
        let env = Env::default();
        env.mock_all_auths();
        let client = client(&env);
        let owner = Address::generate(&env);
        let portfolio = symbol_short!("port1");

        client.set_alert_config(&owner, &portfolio, &empty_config(&env, &portfolio, true));
        client.add_alert_threshold(
            &owner,
            &portfolio,
            &threshold(
                &env,
                MetricType::Custom,
                Comparison::Equal,
                0,
                symbol_short!("ZERO"),
            ),
        );

        let mut hit = Vec::new(&env);
        hit.push_back(obs(MetricType::Custom, symbol_short!("ZERO"), 0));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &hit), 1);

        let mut miss = Vec::new(&env);
        miss.push_back(obs(MetricType::Custom, symbol_short!("ZERO"), 1));
        assert_eq!(client.check_portfolio_alerts_with(&portfolio, &miss), 0);
    }
}
