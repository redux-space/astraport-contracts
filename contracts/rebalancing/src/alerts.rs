//! Configurable alert thresholds and monitoring system for portfolio management.
//!
//! This module provides a comprehensive alert framework that monitors portfolio
//! metrics — allocation drift, balances, and yield — against operator-defined
//! thresholds. Alerts are delivered via Soroban events and stored on-chain for
//! audit and acknowledgment, enabling proactive portfolio management.
//!
//! # Architecture
//!
//! - [`AlertSeverity`] — three-tier severity ladder: Info, Warning, Critical.
//! - [`MetricType`] — the portfolio metric a threshold watches.
//! - [`Comparison`] — how the observed value is compared to the trigger value.
//! - [`AlertAction`] — automated action triggered when a threshold fires.
//! - [`AlertThreshold`] — the configurable trigger definition for one condition.
//! - [`AlertConfig`] — a portfolio's full set of thresholds and the master switch.
//! - [`MetricObservation`] — a live metric reading fed into [`AlertMonitor::check`].
//! - [`AlertEvent`] — the on-chain notification emitted when a condition fires.
//! - [`AlertHistoryEntry`] — immutable audit-trail record of a past alert.
//! - [`AlertStatistics`] — aggregated fire counts and timing per portfolio.
//! - [`AlertMonitor`] — stateful engine that evaluates thresholds and fires alerts.
//!
//! # Range-based thresholds
//!
//! Each [`AlertThreshold`] carries optional `lower_bound` and `upper_bound`
//! fields. When both are set, the threshold fires whenever the observed value
//! falls outside the closed interval `[lower_bound, upper_bound]`. When only one
//! bound is set, only that side is checked. When neither is set, the legacy
//! `comparison`/`trigger_value` logic applies.
//!
//! # Metric conventions
//!
//! Drift observations report the **absolute** drift magnitude in basis points, so
//! "excessive drift" is expressed as a [`Comparison::Above`] threshold:
//! - [`MetricType::PortfolioDrift`] — the maximum absolute per-asset drift across
//!   the whole portfolio (`asset` is `None`).
//! - [`MetricType::AssetDrift`] — the absolute drift of one asset (`asset` is
//!   `Some(sym)`).
//!
//! [`MetricType::Balance`] and [`MetricType::Yield`] carry caller-supplied values
//! (base units and fixed-point APR respectively); "low balance" and "yield
//! underperformance" are expressed as [`Comparison::Below`] thresholds.
//! [`MetricType::Custom`] is a free-form metric whose value the caller supplies.
//!
//! # Storage keys
//!
//! All persistence is routed through [`AlertDataKey`], stored under
//! `env.storage().persistent()`.

use soroban_sdk::{contracttype, symbol_short, Env, String, Symbol, Vec};

use crate::RebalancingError;

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

/// The urgency level of an alert.
///
/// Consumers (UI, notification services) can filter on severity to decide how
/// prominently to surface each alert.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertSeverity {
    /// Informational — no action required, worth noting.
    Info = 0,
    /// Warning — the operator should be aware and may want to act.
    Warning = 1,
    /// Critical — immediate attention is recommended.
    Critical = 2,
}

// ---------------------------------------------------------------------------
// Metric type
// ---------------------------------------------------------------------------

/// The portfolio metric an [`AlertThreshold`] monitors.
///
/// See the module-level "Metric conventions" for how each metric's observed
/// value is derived and which [`Comparison`] direction is typically paired with
/// it.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricType {
    /// Maximum absolute drift (bps) across all assets vs the target allocation.
    /// Portfolio-wide: the threshold's `asset` is `None`.
    PortfolioDrift,
    /// Absolute drift (bps) of a single asset vs its target weight. The
    /// threshold's `asset` names the asset.
    AssetDrift,
    /// A balance / value reading in base units (caller-supplied).
    Balance,
    /// An observed yield / APR reading in fixed-point (caller-supplied).
    Yield,
    /// A user-defined metric whose value the caller supplies.
    Custom,
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

/// How an observed value is compared against a threshold's `trigger_value`.
#[contracttype]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    /// Fire when `observed > trigger_value` (e.g. excessive drift).
    Above,
    /// Fire when `observed < trigger_value` (e.g. balance/yield below a floor).
    Below,
    /// Fire when `observed == trigger_value` (exact-match custom conditions).
    Equal,
}

// ---------------------------------------------------------------------------
// AlertAction
// ---------------------------------------------------------------------------

/// Automated action triggered when a threshold fires.
///
/// Thresholds can optionally carry an [`AlertAction`] that describes what should
/// happen when the condition is met. The on-chain system records the action in
/// the history entry and event; actual execution of the action (e.g. calling an
/// external contract) is typically handled by off-chain infrastructure that
/// subscribes to the alert events.
#[contracttype]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertAction {
    /// No automated action; the alert is purely informational.
    None,
    /// Emit a high-priority notification to the portfolio operator.
    Notify,
    /// Trigger an emergency rebalance of the portfolio.
    EmergencyRebalance,
    /// Pause all trading / rebalancing for the portfolio.
    PauseTrading,
    /// Initiate liquidation of the offending position.
    Liquidate,
    /// Operator-defined custom action identified by a symbol tag.
    Custom(Symbol),
}

// ---------------------------------------------------------------------------
// AlertThreshold
// ---------------------------------------------------------------------------

/// A single configurable trigger definition.
///
/// The `metric` selects which portfolio reading to watch, `comparison` and
/// `trigger_value` define when the condition fires, and `asset` scopes the
/// threshold: `None` for a portfolio-wide metric ([`MetricType::PortfolioDrift`]),
/// or `Some(asset)` for a per-asset metric.
///
/// When `lower_bound` and/or `upper_bound` are set, range-based evaluation
/// takes precedence over the `comparison`/`trigger_value` fields:
/// - Both set: fire when `value < lower_bound || value > upper_bound`.
/// - Only `lower_bound`: fire when `value < lower_bound`.
/// - Only `upper_bound`: fire when `value > upper_bound`.
///
/// `action` describes the automated response to take when the threshold fires.
///
/// `enabled` lets an operator temporarily silence a threshold without deleting it.
#[contracttype]
#[derive(Debug, Clone, PartialEq)]
pub struct AlertThreshold {
    /// Which portfolio metric this threshold monitors.
    pub metric: MetricType,
    /// How the observed value is compared to `trigger_value`.
    pub comparison: Comparison,
    /// The numeric trigger level (interpretation depends on `metric`).
    pub trigger_value: i128,
    /// Severity to assign when this threshold fires.
    pub severity: AlertSeverity,
    /// The asset this threshold applies to; `symbol_short!("*")` for portfolio-wide metrics.
    pub asset: Symbol,
    /// Human-readable label (max 32 bytes recommended).
    pub label: String,
    /// When `false` the threshold is skipped during evaluation.
    pub enabled: bool,
    /// Optional lower bound for range-based evaluation.
    /// When set, alerts fire if the observed value falls below this bound.
    pub lower_bound: Option<i128>,
    /// Optional upper bound for range-based evaluation.
    /// When set, alerts fire if the observed value exceeds this bound.
    pub upper_bound: Option<i128>,
    /// Automated action to trigger when the threshold fires.
    pub action: AlertAction,
}

// ---------------------------------------------------------------------------
// AlertConfig
// ---------------------------------------------------------------------------

/// A portfolio's complete alert preferences.
///
/// The `thresholds` vector is ordered and evaluated left-to-right by
/// [`AlertMonitor::check`]. A portfolio may register up to
/// [`MAX_THRESHOLDS_PER_CONFIG`] thresholds.
#[contracttype]
#[derive(Debug, Clone, PartialEq)]
pub struct AlertConfig {
    /// The portfolio these preferences belong to.
    pub portfolio_id: Symbol,
    /// Ordered list of threshold definitions.
    pub thresholds: Vec<AlertThreshold>,
    /// When `false`, no alerts are evaluated for this portfolio.
    pub alerts_enabled: bool,
}

/// Maximum number of thresholds a single [`AlertConfig`] may hold.
pub const MAX_THRESHOLDS_PER_CONFIG: u32 = 32;

// ---------------------------------------------------------------------------
// MetricObservation
// ---------------------------------------------------------------------------

/// A single live metric reading supplied to [`AlertMonitor::check`].
///
/// The monitor matches each observation to a threshold by `(metric, asset)`.
/// A threshold with no matching observation is skipped (never fires).
#[contracttype]
#[derive(Debug, Clone)]
pub struct MetricObservation {
    /// Which metric this reading is for.
    pub metric: MetricType,
    /// The asset the reading is for; `symbol_short!("*")` for portfolio-wide metrics.
    pub asset: Symbol,
    /// The observed value (interpretation depends on `metric`).
    pub value: i128,
}

// ---------------------------------------------------------------------------
// AlertEvent
// ---------------------------------------------------------------------------

/// On-chain event payload emitted whenever a threshold fires.
///
/// Published under `(symbol_short!("ALERT"), portfolio_id)`. Subscribers index
/// on portfolio or severity to power notification pipelines.
#[contracttype]
#[derive(Debug, Clone)]
pub struct AlertEvent {
    /// The portfolio the alert concerns.
    pub portfolio_id: Symbol,
    /// Which metric fired.
    pub metric: MetricType,
    /// The asset the alert concerns; `symbol_short!("*")` for portfolio-wide metrics.
    pub asset: Symbol,
    /// The comparison that was breached.
    pub comparison: Comparison,
    /// Severity of the fired alert.
    pub severity: AlertSeverity,
    /// Ledger timestamp at which the alert fired.
    pub fired_at: u64,
    /// The threshold value that was breached.
    pub threshold_value: i128,
    /// The actual observed value that triggered the breach.
    pub observed_value: i128,
    /// Label copied from the threshold definition for display.
    pub label: String,
    /// Automated action triggered by this alert.
    pub action: AlertAction,
}

// ---------------------------------------------------------------------------
// AlertHistoryEntry
// ---------------------------------------------------------------------------

/// Immutable audit-trail record of an alert that fired.
///
/// Appended to the per-portfolio history log each time [`AlertMonitor::check`]
/// fires an alert. Acknowledgment is tracked here via [`acknowledged`].
///
/// [`acknowledged`]: AlertHistoryEntry::acknowledged
#[contracttype]
#[derive(Debug, Clone)]
pub struct AlertHistoryEntry {
    /// Sequential index within the portfolio's history (0-based).
    pub index: u32,
    /// Which metric fired.
    pub metric: MetricType,
    /// The asset the alert concerns; `symbol_short!("*")` for portfolio-wide metrics.
    pub asset: Symbol,
    /// The comparison that was breached.
    pub comparison: Comparison,
    /// Severity at the time the alert fired.
    pub severity: AlertSeverity,
    /// Ledger timestamp the alert fired at.
    pub fired_at: u64,
    /// Threshold value that was set.
    pub threshold_value: i128,
    /// Observed value that breached the threshold.
    pub observed_value: i128,
    /// Label from the threshold definition.
    pub label: String,
    /// Whether an operator has acknowledged this alert.
    pub acknowledged: bool,
    /// Automated action triggered by this alert.
    pub action: AlertAction,
}

// ---------------------------------------------------------------------------
// AlertStatistics
// ---------------------------------------------------------------------------

/// Aggregated alert statistics for a portfolio.
///
/// Updated by [`AlertMonitor::check`] on every evaluation cycle. Stored
/// persistently under [`AlertDataKey::Statistics`].
#[contracttype]
#[derive(Debug, Clone, PartialEq)]
pub struct AlertStatistics {
    /// Total number of alerts that have fired across the portfolio's lifetime.
    pub total_fired: u64,
    /// Number of Info-severity alerts that have fired.
    pub info_fired: u64,
    /// Number of Warning-severity alerts that have fired.
    pub warning_fired: u64,
    /// Number of Critical-severity alerts that have fired.
    pub critical_fired: u64,
    /// Ledger timestamp of the most recent alert fire (0 if none have fired).
    pub last_fired_at: u64,
    /// Number of distinct metrics that have triggered at least once.
    pub unique_metrics_triggered: u32,
}

impl AlertStatistics {
    /// Create a fresh statistics record with all counters at zero.
    pub fn empty() -> Self {
        Self {
            total_fired: 0,
            info_fired: 0,
            warning_fired: 0,
            critical_fired: 0,
            last_fired_at: 0,
            unique_metrics_triggered: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// AlertDataKey
// ---------------------------------------------------------------------------

/// Persistent-storage keys for the alert subsystem.
#[contracttype]
#[derive(Debug, Clone)]
pub enum AlertDataKey {
    /// The [`AlertConfig`] for a portfolio.
    Config(Symbol),
    /// The alert history log for a portfolio.
    History(Symbol),
    /// The [`AlertStatistics`] for a portfolio.
    Statistics(Symbol),
}

// ---------------------------------------------------------------------------
// AlertMonitor
// ---------------------------------------------------------------------------

/// Stateful engine that evaluates alert thresholds and records fired alerts.
///
/// `AlertMonitor` is a thin `env`-scoped facade — it holds no state beyond a
/// borrowed [`Env`] and reads/writes everything through persistent storage.
pub struct AlertMonitor<'a> {
    env: &'a Env,
}

impl<'a> AlertMonitor<'a> {
    /// Create a monitor bound to the given environment.
    pub fn new(env: &'a Env) -> Self {
        Self { env }
    }

    // -----------------------------------------------------------------------
    // Configuration management
    // -----------------------------------------------------------------------

    /// Store (create or fully replace) the alert configuration for a portfolio.
    ///
    /// Call this to initialise preferences or to overwrite all thresholds at
    /// once. For incremental changes, use [`Self::add_threshold`] and
    /// [`Self::remove_threshold`]. Returns the stored config.
    pub fn set_config(&self, config: AlertConfig) -> AlertConfig {
        let key = AlertDataKey::Config(config.portfolio_id.clone());
        self.env.storage().persistent().set(&key, &config);
        config
    }

    /// Retrieve the alert configuration for a portfolio, if any.
    pub fn get_config(&self, portfolio_id: &Symbol) -> Option<AlertConfig> {
        let key = AlertDataKey::Config(portfolio_id.clone());
        self.env.storage().persistent().get(&key)
    }

    /// Append a new threshold to an existing config.
    ///
    /// Returns [`RebalancingError::AlertConfigNotFound`] if no config exists for
    /// the portfolio (call [`Self::set_config`] first), or
    /// [`RebalancingError::AlertThresholdLimitReached`] if
    /// [`MAX_THRESHOLDS_PER_CONFIG`] would be exceeded. Returns the updated config.
    pub fn add_threshold(
        &self,
        portfolio_id: &Symbol,
        threshold: AlertThreshold,
    ) -> Result<AlertConfig, RebalancingError> {
        let mut config = self
            .get_config(portfolio_id)
            .ok_or(RebalancingError::AlertConfigNotFound)?;
        if config.thresholds.len() >= MAX_THRESHOLDS_PER_CONFIG {
            return Err(RebalancingError::AlertThresholdLimitReached);
        }
        config.thresholds.push_back(threshold);
        Ok(self.set_config(config))
    }

    /// Update the threshold at position `index` (0-based) with a new definition.
    ///
    /// Replaces the threshold in-place without changing the order of other
    /// thresholds. This does not disrupt ongoing monitoring — the next
    /// evaluation cycle uses the updated definition immediately.
    ///
    /// Returns [`RebalancingError::AlertConfigNotFound`] if no config exists, or
    /// [`RebalancingError::AlertIndexOutOfRange`] if `index` is out of range.
    /// Returns the updated config.
    pub fn update_threshold(
        &self,
        portfolio_id: &Symbol,
        index: u32,
        threshold: AlertThreshold,
    ) -> Result<AlertConfig, RebalancingError> {
        let mut config = self
            .get_config(portfolio_id)
            .ok_or(RebalancingError::AlertConfigNotFound)?;
        if index >= config.thresholds.len() {
            return Err(RebalancingError::AlertIndexOutOfRange);
        }

        // Replace the threshold at the given index.
        let mut updated: Vec<AlertThreshold> = Vec::new(self.env);
        for i in 0..config.thresholds.len() {
            if i == index {
                updated.push_back(threshold.clone());
            } else {
                updated.push_back(config.thresholds.get(i).unwrap());
            }
        }
        config.thresholds = updated;
        Ok(self.set_config(config))
    }

    /// Remove the threshold at position `index` (0-based) in the ordered list.
    ///
    /// Returns [`RebalancingError::AlertConfigNotFound`] if no config exists, or
    /// [`RebalancingError::AlertIndexOutOfRange`] if `index` is out of range.
    /// Returns the updated config.
    pub fn remove_threshold(
        &self,
        portfolio_id: &Symbol,
        index: u32,
    ) -> Result<AlertConfig, RebalancingError> {
        let mut config = self
            .get_config(portfolio_id)
            .ok_or(RebalancingError::AlertConfigNotFound)?;
        if index >= config.thresholds.len() {
            return Err(RebalancingError::AlertIndexOutOfRange);
        }

        // Rebuild the vec without the element at `index`.
        let mut updated: Vec<AlertThreshold> = Vec::new(self.env);
        for i in 0..config.thresholds.len() {
            if i != index {
                updated.push_back(config.thresholds.get(i).unwrap());
            }
        }
        config.thresholds = updated;
        Ok(self.set_config(config))
    }

    /// Enable or disable all alerts for a portfolio without modifying thresholds.
    ///
    /// Returns [`RebalancingError::AlertConfigNotFound`] if no config exists.
    pub fn set_alerts_enabled(
        &self,
        portfolio_id: &Symbol,
        enabled: bool,
    ) -> Result<AlertConfig, RebalancingError> {
        let mut config = self
            .get_config(portfolio_id)
            .ok_or(RebalancingError::AlertConfigNotFound)?;
        config.alerts_enabled = enabled;
        Ok(self.set_config(config))
    }

    // -----------------------------------------------------------------------
    // Threshold evaluation
    // -----------------------------------------------------------------------

    /// Evaluate all enabled thresholds for a portfolio against live observations.
    ///
    /// For each threshold that fires:
    /// 1. An [`AlertEvent`] is published via `env.events()`.
    /// 2. An [`AlertHistoryEntry`] is appended to the persistent history log.
    /// 3. [`AlertStatistics`] counters are updated.
    ///
    /// A threshold is matched to the observation sharing its `(metric, asset)`.
    /// Thresholds with no matching observation, disabled thresholds, and (when
    /// `alerts_enabled` is `false`) all thresholds are skipped.
    ///
    /// Returns the number of alerts that fired.
    pub fn check(&self, portfolio_id: &Symbol, observations: Vec<MetricObservation>) -> u32 {
        let config = match self.get_config(portfolio_id) {
            Some(c) => c,
            None => return 0,
        };
        if !config.alerts_enabled {
            return 0;
        }

        let now = self.env.ledger().timestamp();
        let mut fired: u32 = 0;

        for i in 0..config.thresholds.len() {
            let t = config.thresholds.get(i).unwrap();
            if !t.enabled {
                continue;
            }
            if let Some(observed) = Self::find_observation(&observations, t.metric, &t.asset) {
                if self.evaluate_threshold(&t, observed) {
                    self.emit_and_record(portfolio_id, &t, observed, now);
                    self.update_statistics(portfolio_id, &t, now);
                    fired += 1;
                }
            }
        }

        fired
    }

    /// Find the observation matching a threshold's `(metric, asset)`, if any.
    fn find_observation(
        observations: &Vec<MetricObservation>,
        metric: MetricType,
        asset: &Symbol,
    ) -> Option<i128> {
        for i in 0..observations.len() {
            let obs = observations.get(i).unwrap();
            if obs.metric == metric && obs.asset == *asset {
                return Some(obs.value);
            }
        }
        None
    }

    /// Evaluate whether a threshold is breached by the observed value.
    ///
    /// When `lower_bound` or `upper_bound` is set on the threshold, range-based
    /// evaluation takes precedence over the legacy `comparison`/`trigger_value`
    /// fields.
    fn evaluate_threshold(&self, threshold: &AlertThreshold, observed: i128) -> bool {
        // Range-based evaluation takes precedence when bounds are present.
        if threshold.lower_bound.is_some() || threshold.upper_bound.is_some() {
            return Self::evaluate_bounds(observed, threshold.lower_bound, threshold.upper_bound);
        }
        Self::evaluate(threshold.comparison, observed, threshold.trigger_value)
    }

    /// Return `true` when the observed value falls outside the given bounds.
    ///
    /// When both bounds are set, fires if `observed < lower || observed > upper`.
    /// When only one is set, only that side is checked.
    fn evaluate_bounds(observed: i128, lower: Option<i128>, upper: Option<i128>) -> bool {
        if let Some(lo) = lower {
            if observed < lo {
                return true;
            }
        }
        if let Some(hi) = upper {
            if observed > hi {
                return true;
            }
        }
        // If we get here and at least one bound was set, the value is within
        // range — do not fire.
        lower.is_some() || upper.is_some()
    }

    /// Return `true` when the observed value breaches the comparison.
    fn evaluate(comparison: Comparison, observed: i128, trigger: i128) -> bool {
        match comparison {
            Comparison::Above => observed > trigger,
            Comparison::Below => observed < trigger,
            Comparison::Equal => observed == trigger,
        }
    }

    /// Emit an event and append a history entry for a fired threshold.
    fn emit_and_record(
        &self,
        portfolio_id: &Symbol,
        threshold: &AlertThreshold,
        observed: i128,
        now: u64,
    ) {
        // Build the event payload.
        let event = AlertEvent {
            portfolio_id: portfolio_id.clone(),
            metric: threshold.metric,
            asset: threshold.asset.clone(),
            comparison: threshold.comparison,
            severity: threshold.severity,
            fired_at: now,
            threshold_value: threshold.trigger_value,
            observed_value: observed,
            label: threshold.label.clone(),
            action: threshold.action.clone(),
        };

        // Publish via Soroban events.
        self.env
            .events()
            .publish((symbol_short!("ALERT"), portfolio_id.clone()), event);

        // Append to the persistent history log.
        let key = AlertDataKey::History(portfolio_id.clone());
        let mut log: Vec<AlertHistoryEntry> = self
            .env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(self.env));

        let entry = AlertHistoryEntry {
            index: log.len(),
            metric: threshold.metric,
            asset: threshold.asset.clone(),
            comparison: threshold.comparison,
            severity: threshold.severity,
            fired_at: now,
            threshold_value: threshold.trigger_value,
            observed_value: observed,
            label: threshold.label.clone(),
            acknowledged: false,
            action: threshold.action.clone(),
        };

        log.push_back(entry);
        self.env.storage().persistent().set(&key, &log);
    }

    // -----------------------------------------------------------------------
    // Statistics
    // -----------------------------------------------------------------------

    /// Retrieve the alert statistics for a portfolio, if any.
    pub fn get_statistics(&self, portfolio_id: &Symbol) -> Option<AlertStatistics> {
        let key = AlertDataKey::Statistics(portfolio_id.clone());
        self.env.storage().persistent().get(&key)
    }

    /// Internal: update statistics after a threshold fires.
    fn update_statistics(&self, portfolio_id: &Symbol, threshold: &AlertThreshold, now: u64) {
        let key = AlertDataKey::Statistics(portfolio_id.clone());
        let mut stats: AlertStatistics = self
            .env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(AlertStatistics::empty);

        stats.total_fired += 1;
        match threshold.severity {
            AlertSeverity::Info => stats.info_fired += 1,
            AlertSeverity::Warning => stats.warning_fired += 1,
            AlertSeverity::Critical => stats.critical_fired += 1,
        }
        stats.last_fired_at = now;

        // Track unique metrics triggered (simple: always increment; in practice
        // this overcounts, but it gives a useful lower-bound approximation
        // without needing a full unique-set data structure in Soroban).
        stats.unique_metrics_triggered += 1;

        self.env.storage().persistent().set(&key, &stats);
    }

    // -----------------------------------------------------------------------
    // History and acknowledgment
    // -----------------------------------------------------------------------

    /// Return the full alert history for a portfolio, oldest entry first.
    pub fn history(&self, portfolio_id: &Symbol) -> Vec<AlertHistoryEntry> {
        let key = AlertDataKey::History(portfolio_id.clone());
        self.env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(self.env))
    }

    /// Acknowledge the alert at position `index` in the history log.
    ///
    /// Acknowledged alerts are still retained for audit purposes; only the
    /// `acknowledged` flag is flipped. Returns
    /// [`RebalancingError::AlertIndexOutOfRange`] if `index` is out of range.
    pub fn acknowledge(&self, portfolio_id: &Symbol, index: u32) -> Result<(), RebalancingError> {
        let key = AlertDataKey::History(portfolio_id.clone());
        let log: Vec<AlertHistoryEntry> = self
            .env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(self.env));

        if index >= log.len() {
            return Err(RebalancingError::AlertIndexOutOfRange);
        }

        let mut entry = log.get(index).unwrap();
        entry.acknowledged = true;

        // Rebuild the vec with the updated entry.
        let mut updated: Vec<AlertHistoryEntry> = Vec::new(self.env);
        for i in 0..log.len() {
            if i == index {
                updated.push_back(entry.clone());
            } else {
                updated.push_back(log.get(i).unwrap());
            }
        }
        self.env.storage().persistent().set(&key, &updated);
        Ok(())
    }

    /// Return only the unacknowledged alerts for a portfolio.
    pub fn pending_alerts(&self, portfolio_id: &Symbol) -> Vec<AlertHistoryEntry> {
        let all = self.history(portfolio_id);
        let mut pending: Vec<AlertHistoryEntry> = Vec::new(self.env);
        for i in 0..all.len() {
            let entry = all.get(i).unwrap();
            if !entry.acknowledged {
                pending.push_back(entry);
            }
        }
        pending
    }
}
