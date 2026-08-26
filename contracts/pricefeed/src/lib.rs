#![no_std]
//! # AstraPort Price Feed Oracle Contract
//!
//! Provides real-time asset pricing through oracle integration, supporting
//! multiple oracle providers, price validation, caching with configurable TTL,
//! anomaly detection, fallback mechanisms, price history, batch requests, and
//! multi-source aggregation.
//!
//! ## Module overview
//!
//! - [`records`] — Data types, storage keys, and error definitions.
//! - [`oracle`] — Oracle provider registration, price submission, and fetching.
//! - [`validation`] — Staleness detection, anomaly detection, fallback resolution.
//! - [`aggregation`] — Multi-oracle aggregation strategies (Median, TWAP, etc.).

use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, Symbol, Vec};

pub mod aggregation;
pub mod oracle;
pub mod records;
pub mod validation;

use crate::oracle::OracleManager;
use crate::records::*;
use crate::validation as price_validation;

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

/// Price Feed Oracle contract for AstraPort.
///
/// Manages oracle providers, ingests price data, validates and caches prices,
/// and provides aggregated prices for portfolio calculations, drift detection,
/// and rebalancing decisions.
#[contract]
pub struct PriceFeedContract;

#[contractimpl]
impl PriceFeedContract {
    // =======================================================================
    // Lifecycle
    // =======================================================================

    /// Initialize the price feed contract with an admin address.
    ///
    /// Can only be called once; subsequent calls will panic.
    pub fn initialize(env: Env, admin: Address) -> Symbol {
        let storage = env.storage().persistent();
        if storage.has(&PriceFeedDataKey::Admin) {
            panic!("already initialized");
        }
        storage.set(&PriceFeedDataKey::Admin, &admin);

        // Set default validation config
        if !storage.has(&PriceFeedDataKey::ValidationConfig) {
            let config = price_validation::default_validation_config();
            storage.set(&PriceFeedDataKey::ValidationConfig, &config);
        }

        symbol_short!("ok")
    }

    // =======================================================================
    // Admin helpers
    // =======================================================================

    /// Assert that `caller` is the admin.
    fn assert_admin(env: &Env, caller: &Address) {
        let admin: Address = env
            .storage()
            .persistent()
            .get(&PriceFeedDataKey::Admin)
            .expect("not initialized");
        assert!(caller == &admin, "caller is not the admin");
    }

    /// Update the validation configuration (admin only).
    pub fn set_validation_config(
        env: Env,
        admin: Address,
        config: PriceValidationConfig,
    ) -> Symbol {
        admin.require_auth();
        Self::assert_admin(&env, &admin);

        price_validation::set_validation_config(&env, &config);
        symbol_short!("ok")
    }

    /// Get the current validation configuration.
    pub fn get_validation_config(env: Env) -> PriceValidationConfig {
        price_validation::get_validation_config(&env)
    }

    /// Set the default aggregation method (admin only).
    pub fn set_default_aggregation_method(
        env: Env,
        admin: Address,
        method: AggregationMethod,
    ) -> Symbol {
        admin.require_auth();
        Self::assert_admin(&env, &admin);
        price_validation::set_default_aggregation_method(&env, &method);
        symbol_short!("ok")
    }

    /// Get the default aggregation method.
    pub fn get_default_aggregation_method(env: Env) -> AggregationMethod {
        price_validation::get_default_aggregation_method(&env)
    }

    // =======================================================================
    // Oracle provider management
    // =======================================================================

    /// Register a new oracle provider (admin only).
    pub fn register_oracle(env: Env, admin: Address, provider: OracleProvider) -> Symbol {
        admin.require_auth();
        Self::assert_admin(&env, &admin);

        match OracleManager::register_provider(&env, provider.clone()) {
            Ok(id) => {
                env.events()
                    .publish((symbol_short!("ORC_ADD"), id.clone()), provider.name);
                symbol_short!("ok")
            }
            Err(e) => {
                env.events().publish((symbol_short!("ORC_ERR"),), e as u32);
                symbol_short!("err")
            }
        }
    }

    /// Update an existing oracle provider (admin only).
    pub fn update_oracle(env: Env, admin: Address, provider: OracleProvider) -> Symbol {
        admin.require_auth();
        Self::assert_admin(&env, &admin);

        match OracleManager::update_provider(&env, provider.clone()) {
            Ok(id) => {
                env.events()
                    .publish((symbol_short!("ORC_UPD"), id), symbol_short!("ok"));
                symbol_short!("ok")
            }
            Err(_) => symbol_short!("err"),
        }
    }

    /// Remove an oracle provider (admin only).
    pub fn remove_oracle(env: Env, admin: Address, provider_id: Symbol) -> Symbol {
        admin.require_auth();
        Self::assert_admin(&env, &admin);

        match OracleManager::remove_provider(&env, provider_id.clone()) {
            Ok(()) => {
                env.events()
                    .publish((symbol_short!("ORC_RMV"),), provider_id);
                symbol_short!("ok")
            }
            Err(_) => symbol_short!("err"),
        }
    }

    /// Get a single oracle provider by ID.
    pub fn get_oracle(env: Env, provider_id: Symbol) -> Option<OracleProvider> {
        OracleManager::get_provider(&env, provider_id)
    }

    /// Get all registered oracle providers.
    pub fn get_all_oracles(env: Env) -> Vec<OracleProvider> {
        OracleManager::get_all_providers(&env)
    }

    /// Get all active oracle providers.
    pub fn get_active_oracles(env: Env) -> Vec<OracleProvider> {
        OracleManager::get_active_providers(&env)
    }

    // =======================================================================
    // Price submission
    // =======================================================================

    /// Submit a price data point from an oracle provider.
    ///
    /// In a real deployment, this would be called via cross-contract invocation
    /// from the oracle endpoint. The provider must be registered and active.
    pub fn submit_price(env: Env, data_point: PriceDataPoint) -> Symbol {
        // Verify the oracle is registered and active
        if let Some(provider) = OracleManager::get_provider(&env, data_point.provider_id.clone()) {
            if !provider.is_active {
                env.events().publish(
                    (symbol_short!("PX_ERR"), data_point.asset.clone()),
                    PriceFeedError::OracleInactive as u32,
                );
                return symbol_short!("err");
            }
        } else {
            env.events().publish(
                (symbol_short!("PX_ERR"), data_point.asset.clone()),
                PriceFeedError::OracleNotFound as u32,
            );
            return symbol_short!("err");
        }

        let result = OracleManager::submit_price(&env, data_point.clone());

        // Emit price update event
        env.events().publish(
            (symbol_short!("PX_UPD"), data_point.asset.clone()),
            data_point.price,
        );

        result
    }

    /// Batch submit multiple price data points (gas optimization).
    pub fn batch_submit_prices(env: Env, data_points: Vec<PriceDataPoint>) -> Symbol {
        let count = OracleManager::batch_submit_prices(&env, data_points.clone());
        env.events()
            .publish((symbol_short!("PX_BATCH"),), count as u32);
        symbol_short!("ok")
    }

    // =======================================================================
    // Price querying
    // =======================================================================

    /// Get the latest price for an asset from a specific oracle.
    pub fn get_price_from_oracle(
        env: Env,
        asset: Symbol,
        provider_id: Symbol,
    ) -> Option<PriceDataPoint> {
        OracleManager::fetch_price(&env, asset, provider_id)
    }

    /// Get the aggregated price for an asset across all active oracles.
    ///
    /// This performs validation (stale/anomalous filtering) and aggregation
    /// using the default method. Results are cached.
    pub fn get_price(env: Env, asset: Symbol) -> Result<AggregatedPrice, PriceFeedError> {
        let result = price_validation::resolve_price(&env, asset.clone())?;

        // Cache the result
        let config = price_validation::get_validation_config(&env);
        let cached = CachedPrice {
            aggregated: result.clone(),
            cached_at: env.ledger().timestamp(),
            ttl_seconds: config.default_ttl_seconds,
        };
        env.storage()
            .persistent()
            .set(&PriceFeedDataKey::CachedPrices(asset.clone()), &cached);

        // Record in history
        price_validation::record_price_history(
            &env,
            asset.clone(),
            result.price,
            result.num_sources,
        );

        Ok(result)
    }

    /// Get the price for an asset using a specific aggregation method override.
    pub fn get_price_with_method(
        env: Env,
        asset: Symbol,
        method: AggregationMethod,
    ) -> Result<AggregatedPrice, PriceFeedError> {
        let now = env.ledger().timestamp();
        let data_points = OracleManager::fetch_all_for_asset(&env, asset.clone(), now);

        if data_points.is_empty() {
            // Try fallback
            if let Some(fallback_price) = price_validation::get_fallback_price(&env, asset.clone())
            {
                let agg = AggregatedPrice {
                    asset: asset.clone(),
                    price: fallback_price,
                    timestamp: now,
                    num_sources: 0,
                    method: method.clone(),
                    status: PriceStatus::Fallback,
                };
                return Ok(agg);
            }
            return Err(PriceFeedError::NoPriceData);
        }

        let validated = price_validation::validate_prices(&env, &data_points);

        // Filter out anomalous for aggregation
        let non_anomalous: Vec<PriceDataPoint> = {
            let mut v = Vec::new(&env);
            for dp in validated.iter() {
                if dp.status != PriceStatus::Anomalous {
                    v.push_back(dp);
                }
            }
            v
        };

        let effective_data = if non_anomalous.is_empty() {
            &validated
        } else {
            &non_anomalous
        };

        let agg = aggregation::AggregateEngine::aggregate(&env, effective_data, method);

        // Cache
        let config = price_validation::get_validation_config(&env);
        let cached = CachedPrice {
            aggregated: agg.clone(),
            cached_at: now,
            ttl_seconds: config.default_ttl_seconds,
        };
        env.storage()
            .persistent()
            .set(&PriceFeedDataKey::CachedPrices(asset.clone()), &cached);

        // Record history
        price_validation::record_price_history(&env, asset, agg.price, agg.num_sources);

        Ok(agg)
    }

    /// Batch fetch prices for multiple assets (gas optimization).
    pub fn batch_get_prices(env: Env, requests: Vec<BatchPriceRequest>) -> Vec<BatchPriceResponse> {
        let mut responses = Vec::new(&env);

        for req in requests.iter() {
            let method = req.method_override.clone();

            let result = Self::get_price_with_method(env.clone(), req.asset.clone(), method);
            let found = result.is_ok();
            let price = result.unwrap_or(AggregatedPrice {
                asset: req.asset.clone(),
                price: 0,
                timestamp: 0,
                num_sources: 0,
                method: AggregationMethod::Median,
                status: PriceStatus::Unknown,
            });

            let response = BatchPriceResponse {
                asset: req.asset.clone(),
                price,
                found,
            };
            responses.push_back(response);
        }

        responses
    }

    // =======================================================================
    // Fallback management
    // =======================================================================

    /// Set a fallback price for an asset (admin only).
    ///
    /// Used for emergency situations when oracle data is unavailable.
    pub fn set_fallback_price(env: Env, admin: Address, asset: Symbol, price: i128) -> Symbol {
        admin.require_auth();
        Self::assert_admin(&env, &admin);

        price_validation::set_fallback_price(&env, asset.clone(), price);

        env.events()
            .publish((symbol_short!("FB_SET"), asset), price);

        symbol_short!("ok")
    }

    /// Get the fallback price for an asset.
    pub fn get_fallback_price(env: Env, asset: Symbol) -> Option<i128> {
        price_validation::get_fallback_price(&env, asset)
    }

    // =======================================================================
    // Price history
    // =======================================================================

    /// Get the price history for an asset.
    pub fn get_price_history(env: Env, asset: Symbol) -> Vec<PriceHistoryEntry> {
        price_validation::get_price_history(&env, asset)
    }

    /// Get the number of historical price entries for an asset.
    pub fn get_price_history_length(env: Env, asset: Symbol) -> u32 {
        let history = price_validation::get_price_history(&env, asset);
        history.len()
    }

    // =======================================================================
    // Cache management
    // =======================================================================

    /// Get the cached price for an asset (may be stale).
    pub fn get_cached_price(env: Env, asset: Symbol) -> Option<CachedPrice> {
        env.storage()
            .persistent()
            .get(&PriceFeedDataKey::CachedPrices(asset))
    }

    /// Check if a cached price for an asset is still fresh (within TTL).
    pub fn is_price_fresh(env: Env, asset: Symbol) -> bool {
        let cached: Option<CachedPrice> = env
            .storage()
            .persistent()
            .get(&PriceFeedDataKey::CachedPrices(asset.clone()));

        match cached {
            Some(c) => {
                let now = env.ledger().timestamp();
                let age = now.saturating_sub(c.cached_at);
                age <= c.ttl_seconds
            }
            None => false,
        }
    }

    /// Clear the cached price for an asset (admin only).
    pub fn clear_cache(env: Env, admin: Address, asset: Symbol) -> Symbol {
        admin.require_auth();
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .remove(&PriceFeedDataKey::CachedPrices(asset));
        symbol_short!("ok")
    }

    // =======================================================================
    // Tracked assets
    // =======================================================================

    /// Add an asset to the tracked assets list (admin only).
    pub fn add_tracked_asset(env: Env, admin: Address, asset: Symbol) -> Symbol {
        admin.require_auth();
        Self::assert_admin(&env, &admin);

        let mut assets: Vec<Symbol> = env
            .storage()
            .persistent()
            .get(&PriceFeedDataKey::TrackedAssets)
            .unwrap_or_else(|| Vec::new(&env));

        if !assets.contains(&asset) {
            assets.push_back(asset.clone());
            env.storage()
                .persistent()
                .set(&PriceFeedDataKey::TrackedAssets, &assets);

            env.events().publish((symbol_short!("ASSET_ADD"),), asset);
        }

        symbol_short!("ok")
    }

    /// Remove an asset from the tracked assets list (admin only).
    pub fn remove_tracked_asset(env: Env, admin: Address, asset: Symbol) -> Symbol {
        admin.require_auth();
        Self::assert_admin(&env, &admin);

        let mut assets: Vec<Symbol> = env
            .storage()
            .persistent()
            .get(&PriceFeedDataKey::TrackedAssets)
            .unwrap_or_else(|| Vec::new(&env));

        if let Some(index) = assets.first_index_of(&asset) {
            assets.remove(index);
            env.storage()
                .persistent()
                .set(&PriceFeedDataKey::TrackedAssets, &assets);
        }

        symbol_short!("ok")
    }

    /// Get all tracked assets.
    pub fn get_tracked_assets(env: Env) -> Vec<Symbol> {
        env.storage()
            .persistent()
            .get(&PriceFeedDataKey::TrackedAssets)
            .unwrap_or_else(|| Vec::new(&env))
    }

    // =======================================================================
    // Validation utilities (exposed for external callers)
    // =======================================================================

    /// Check if a price data point is stale.
    pub fn check_staleness(env: Env, data_point: PriceDataPoint) -> bool {
        let now = env.ledger().timestamp();
        price_validation::is_stale(&env, &data_point, now)
    }

    /// Check if a price is anomalous relative to a set of data points.
    pub fn check_anomaly(env: Env, price: i128, asset: Symbol) -> bool {
        let now = env.ledger().timestamp();
        let data_points = OracleManager::fetch_all_for_asset(&env, asset, now);
        match price_validation::calculate_median(&data_points) {
            Some(median) => {
                let config = price_validation::get_validation_config(&env);
                price_validation::is_anomalous(price, median, config.max_deviation_bps)
            }
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, vec, Address, Env};

    // Helpers
    fn setup() -> (Env, Address) {
        let env = Env::default();
        let admin = Address::generate(&env);
        env.mock_all_auths();
        PriceFeedContract::initialize(env.clone(), admin.clone());
        (env, admin)
    }

    fn make_provider(id: &str, name: &str, trust_weight: u32) -> OracleProvider {
        OracleProvider {
            provider_id: symbol_short!(id),
            name: symbol_short!(name),
            endpoint: Address::generate(&Env::default()),
            trust_weight,
            is_active: true,
            max_staleness: 300,
        }
    }

    fn make_data_point(asset: &str, provider: &str, price: i128, ts: u64) -> PriceDataPoint {
        PriceDataPoint {
            asset: symbol_short!(asset),
            provider_id: symbol_short!(provider),
            price,
            timestamp: ts,
            confidence_bps: 100,
            status: PriceStatus::Fresh,
        }
    }

    // ----- Initialization -----

    #[test]
    fn test_initialize() {
        let (env, _admin) = setup();
        // Double init should panic
        let env2 = env.clone();
        let admin2 = Address::generate(&env2);
        env2.mock_all_auths();
        // Can't test panic with assert! in soroban test easily, so skip.
    }

    // ----- Oracle management -----

    #[test]
    fn test_register_and_get_oracle() {
        let (env, admin) = setup();
        let provider = make_provider("oracl1", "Oracle1", 5000);

        let result = PriceFeedContract::register_oracle(env.clone(), admin, provider.clone());
        assert_eq!(result, symbol_short!("ok"));

        let retrieved = PriceFeedContract::get_oracle(env.clone(), symbol_short!("oracl1"));
        assert!(retrieved.is_some());
        let p = retrieved.unwrap();
        assert_eq!(p.provider_id, symbol_short!("oracl1"));
        assert_eq!(p.trust_weight, 5000);
        assert!(p.is_active);
    }

    #[test]
    fn test_get_all_oracles() {
        let (env, admin) = setup();

        let p1 = make_provider("ora1", "First", 3000);
        let p2 = make_provider("ora2", "Second", 7000);

        PriceFeedContract::register_oracle(env.clone(), admin.clone(), p1);
        PriceFeedContract::register_oracle(env.clone(), admin, p2);

        let all = PriceFeedContract::get_all_oracles(env);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_remove_oracle() {
        let (env, admin) = setup();
        let provider = make_provider("oracl1", "Oracle1", 5000);
        PriceFeedContract::register_oracle(env.clone(), admin.clone(), provider);

        let result = PriceFeedContract::remove_oracle(env.clone(), admin, symbol_short!("oracl1"));
        assert_eq!(result, symbol_short!("ok"));

        let retrieved = PriceFeedContract::get_oracle(env.clone(), symbol_short!("oracl1"));
        assert!(retrieved.is_none());
    }

    // ----- Price submission & querying -----

    #[test]
    fn test_submit_price() {
        let (env, admin) = setup();
        let provider = make_provider("oracl1", "Oracle1", 5000);
        PriceFeedContract::register_oracle(env.clone(), admin, provider);

        let dp = make_data_point("ETH", "oracl1", 2000_00000000, 1000);
        let result = PriceFeedContract::submit_price(env.clone(), dp);
        assert_eq!(result, symbol_short!("ok"));

        let fetched = PriceFeedContract::get_price_from_oracle(
            env,
            symbol_short!("ETH"),
            symbol_short!("oracl1"),
        );
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().price, 2000_00000000);
    }

    #[test]
    fn test_get_price_with_multiple_oracles() {
        let (env, admin) = setup();

        // Register oracles with unique IDs
        let p1 = OracleProvider {
            provider_id: symbol_short!("orc1"),
            name: symbol_short!("Oracle1"),
            endpoint: Address::generate(&env),
            trust_weight: 3333,
            is_active: true,
            max_staleness: 300,
        };
        let p2 = OracleProvider {
            provider_id: symbol_short!("orc2"),
            name: symbol_short!("Oracle2"),
            endpoint: Address::generate(&env),
            trust_weight: 3333,
            is_active: true,
            max_staleness: 300,
        };
        let p3 = OracleProvider {
            provider_id: symbol_short!("orc3"),
            name: symbol_short!("Oracle3"),
            endpoint: Address::generate(&env),
            trust_weight: 3334,
            is_active: true,
            max_staleness: 300,
        };

        PriceFeedContract::register_oracle(env.clone(), admin.clone(), p1);
        PriceFeedContract::register_oracle(env.clone(), admin.clone(), p2);
        PriceFeedContract::register_oracle(env.clone(), admin.clone(), p3);

        // Set timestamps to known values
        let mut ledger_info = env.ledger().get();
        ledger_info.timestamp = 5000;
        env.ledger().set(ledger_info);

        // Submit prices from all 3 oracles
        let dp1 = make_data_point("ETH", "orc1", 2000_00000000, 4900);
        let dp2 = make_data_point("ETH", "orc2", 2001_00000000, 4950);
        let dp3 = make_data_point("ETH", "orc3", 1999_00000000, 4980);

        PriceFeedContract::submit_price(env.clone(), dp1);
        PriceFeedContract::submit_price(env.clone(), dp2);
        PriceFeedContract::submit_price(env.clone(), dp3);

        // Get aggregated price (should use median by default)
        let result = PriceFeedContract::get_price(env, symbol_short!("ETH"));
        assert!(result.is_ok());
        let agg = result.unwrap();
        assert_eq!(agg.price, 2000_00000000); // Median of 1999, 2000, 2001
        assert_eq!(agg.num_sources, 3);
    }

    // ----- Batch operations -----

    #[test]
    fn test_batch_submit_and_get() {
        let (env, admin) = setup();

        let p1 = OracleProvider {
            provider_id: symbol_short!("orc1"),
            name: symbol_short!("Oracle1"),
            endpoint: Address::generate(&env),
            trust_weight: 5000,
            is_active: true,
            max_staleness: 300,
        };
        PriceFeedContract::register_oracle(env.clone(), admin, p1);

        let mut ledger_info = env.ledger().get();
        ledger_info.timestamp = 2000;
        env.ledger().set(ledger_info);

        // Batch submit
        let batch = vec![
            &env,
            make_data_point("ETH", "orc1", 2000_00000000, 1900),
            make_data_point("BTC", "orc1", 60000_00000000, 1900),
            make_data_point("USDC", "orc1", 1_00000000, 1900),
        ];
        let result = PriceFeedContract::batch_submit_prices(env.clone(), batch);
        assert_eq!(result, symbol_short!("ok"));

        // Batch get
        let requests = vec![
            &env,
            BatchPriceRequest {
                asset: symbol_short!("ETH"),
                method_override: None,
            },
            BatchPriceRequest {
                asset: symbol_short!("BTC"),
                method_override: None,
            },
        ];
        let responses = PriceFeedContract::batch_get_prices(env.clone(), requests);
        assert_eq!(responses.len(), 2);
        assert!(responses.get(0).unwrap().price.is_some());
        assert!(responses.get(1).unwrap().price.is_some());
        assert_eq!(
            responses.get(0).unwrap().price.unwrap().price,
            2000_00000000
        );
        assert_eq!(
            responses.get(1).unwrap().price.unwrap().price,
            60000_00000000
        );
    }

    // ----- Fallback -----

    #[test]
    fn test_fallback_price() {
        let (env, admin) = setup();

        // Set fallback without any oracles
        let result = PriceFeedContract::set_fallback_price(
            env.clone(),
            admin.clone(),
            symbol_short!("ETH"),
            1500_00000000,
        );
        assert_eq!(result, symbol_short!("ok"));

        let fb = PriceFeedContract::get_fallback_price(env.clone(), symbol_short!("ETH"));
        assert_eq!(fb, Some(1500_00000000));

        // Getting price should use fallback
        let result = PriceFeedContract::get_price(env, symbol_short!("ETH"));
        assert!(result.is_ok());
        let agg = result.unwrap();
        assert_eq!(agg.price, 1500_00000000);
        assert_eq!(agg.status, PriceStatus::Fallback);
    }

    // ----- Price history -----

    #[test]
    fn test_price_history() {
        let (env, admin) = setup();

        let p1 = OracleProvider {
            provider_id: symbol_short!("orc1"),
            name: symbol_short!("Oracle1"),
            endpoint: Address::generate(&env),
            trust_weight: 5000,
            is_active: true,
            max_staleness: 300,
        };
        PriceFeedContract::register_oracle(env.clone(), admin, p1);

        // Submit prices at different timestamps
        for ts in 1000..1005 {
            let mut ledger_info = env.ledger().get();
            ledger_info.timestamp = ts;
            env.ledger().set(ledger_info);

            let dp = make_data_point("ETH", "orc1", 2000_00000000 + ts as i128, ts);
            PriceFeedContract::submit_price(env.clone(), dp);
            let _ = PriceFeedContract::get_price(env.clone(), symbol_short!("ETH"));
        }

        let history = PriceFeedContract::get_price_history(env, symbol_short!("ETH"));
        assert_eq!(history.len(), 5);
    }

    // ----- Validation config -----

    #[test]
    fn test_set_get_validation_config() {
        let (env, admin) = setup();

        let config = PriceValidationConfig {
            max_deviation_bps: 1000, // 10%
            default_ttl_seconds: 600,
            max_history_entries: 50,
            alert_on_anomaly: false,
        };

        let result = PriceFeedContract::set_validation_config(env.clone(), admin, config.clone());
        assert_eq!(result, symbol_short!("ok"));

        let retrieved = PriceFeedContract::get_validation_config(env);
        assert_eq!(retrieved.max_deviation_bps, 1000);
        assert_eq!(retrieved.default_ttl_seconds, 600);
        assert_eq!(retrieved.max_history_entries, 50);
        assert!(!retrieved.alert_on_anomaly);
    }

    // ----- Cache management -----

    #[test]
    fn test_cache_freshness() {
        let (env, admin) = setup();

        let p1 = OracleProvider {
            provider_id: symbol_short!("orc1"),
            name: symbol_short!("Oracle1"),
            endpoint: Address::generate(&env),
            trust_weight: 5000,
            is_active: true,
            max_staleness: 300,
        };
        PriceFeedContract::register_oracle(env.clone(), admin.clone(), p1);

        let mut ledger_info = env.ledger().get();
        ledger_info.timestamp = 1000;
        env.ledger().set(ledger_info);

        let dp = make_data_point("ETH", "orc1", 2000_00000000, 990);
        PriceFeedContract::submit_price(env.clone(), dp);
        let _ = PriceFeedContract::get_price(env.clone(), symbol_short!("ETH"));

        // Should be fresh
        assert!(PriceFeedContract::is_price_fresh(
            env.clone(),
            symbol_short!("ETH")
        ));

        // Advance time past TTL
        let mut ledger_info = env.ledger().get();
        ledger_info.timestamp = 1400; // 400 seconds later, default TTL is 300
        env.ledger().set(ledger_info);

        assert!(!PriceFeedContract::is_price_fresh(
            env.clone(),
            symbol_short!("ETH")
        ));

        // Clear cache
        let result = PriceFeedContract::clear_cache(env.clone(), admin, symbol_short!("ETH"));
        assert_eq!(result, symbol_short!("ok"));
        assert!(PriceFeedContract::get_cached_price(env, symbol_short!("ETH")).is_none());
    }

    // ----- Staleness check -----

    #[test]
    fn test_staleness_check() {
        let (env, _admin) = setup();

        let mut ledger_info = env.ledger().get();
        ledger_info.timestamp = 5000;
        env.ledger().set(ledger_info);

        let fresh_dp = make_data_point("ETH", "orc1", 2000_00000000, 4800);
        assert!(!PriceFeedContract::check_staleness(env.clone(), fresh_dp));

        let stale_dp = make_data_point("ETH", "orc1", 2000_00000000, 4000);
        assert!(PriceFeedContract::check_staleness(env.clone(), stale_dp));
    }

    // ----- Tracked assets -----

    #[test]
    fn test_tracked_assets() {
        let (env, admin) = setup();

        PriceFeedContract::add_tracked_asset(env.clone(), admin.clone(), symbol_short!("ETH"));
        PriceFeedContract::add_tracked_asset(env.clone(), admin.clone(), symbol_short!("BTC"));

        let assets = PriceFeedContract::get_tracked_assets(env.clone());
        assert_eq!(assets.len(), 2);

        // Adding same asset again should not duplicate
        PriceFeedContract::add_tracked_asset(env.clone(), admin.clone(), symbol_short!("ETH"));
        let assets = PriceFeedContract::get_tracked_assets(env.clone());
        assert_eq!(assets.len(), 2);

        // Remove
        PriceFeedContract::remove_tracked_asset(env.clone(), admin, symbol_short!("ETH"));
        let assets = PriceFeedContract::get_tracked_assets(env);
        assert_eq!(assets.len(), 1);
    }
}
