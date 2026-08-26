//! Multi-oracle price aggregation strategies.

use soroban_sdk::{symbol_short, Env, Symbol, Vec};

use crate::oracle::OracleManager;
use crate::records::{AggregatedPrice, AggregationMethod, PriceDataPoint, PriceStatus};
use crate::validation;

// ---------------------------------------------------------------------------
// Aggregation Engine
// ---------------------------------------------------------------------------

/// Engine for aggregating prices across multiple oracle sources.
pub struct AggregateEngine;

impl AggregateEngine {
    /// Aggregate a set of validated data points using the given method.
    pub fn aggregate(
        env: &Env,
        data_points: &Vec<PriceDataPoint>,
        method: AggregationMethod,
    ) -> AggregatedPrice {
        if data_points.is_empty() {
            // Return a zero-price placeholder with Unknown status
            return AggregatedPrice {
                asset: symbol_short!(""),
                price: 0,
                timestamp: env.ledger().timestamp(),
                num_sources: 0,
                method,
                status: PriceStatus::Unknown,
            };
        }

        let asset = data_points.get(0).unwrap().asset.clone();
        let _num_sources = data_points.len();
        let now = env.ledger().timestamp();

        match method {
            AggregationMethod::Median => Self::aggregate_median(env, data_points, asset, now),
            AggregationMethod::Latest => Self::aggregate_latest(env, data_points, asset, now),
            AggregationMethod::TWAP => Self::aggregate_twap(env, data_points, asset, now),
            AggregationMethod::WeightedAverage => {
                Self::aggregate_weighted(env, data_points, asset, now)
            }
        }
    }

    /// Median aggregation: sort prices and take the middle value.
    /// Resistant to outlier manipulation.
    fn aggregate_median(
        _env: &Env,
        data_points: &Vec<PriceDataPoint>,
        asset: Symbol,
        now: u64,
    ) -> AggregatedPrice {
        let median = validation::calculate_median(data_points).unwrap_or(0);

        // Determine overall status: use the worst status from non-anomalous sources
        let worst_status = Self::worst_status(data_points);

        AggregatedPrice {
            asset,
            price: median,
            timestamp: now,
            num_sources: data_points.len(),
            method: AggregationMethod::Median,
            status: worst_status,
        }
    }

    /// Latest aggregation: use the most recently updated price.
    fn aggregate_latest(
        _env: &Env,
        data_points: &Vec<PriceDataPoint>,
        asset: Symbol,
        now: u64,
    ) -> AggregatedPrice {
        let mut latest_price: i128 = 0;
        let mut latest_ts: u64 = 0;

        for dp in data_points.iter() {
            if dp.timestamp >= latest_ts {
                latest_ts = dp.timestamp;
                latest_price = dp.price;
            }
        }

        let worst_status = Self::worst_status(data_points);

        AggregatedPrice {
            asset,
            price: latest_price,
            timestamp: now,
            num_sources: data_points.len(),
            method: AggregationMethod::Latest,
            status: worst_status,
        }
    }

    /// TWAP (Time-Weighted Average Price): weights each price by how recent
    /// it is. More recent prices have higher weight.
    fn aggregate_twap(
        _env: &Env,
        data_points: &Vec<PriceDataPoint>,
        asset: Symbol,
        now: u64,
    ) -> AggregatedPrice {
        if data_points.is_empty() {
            return AggregatedPrice {
                asset,
                price: 0,
                timestamp: now,
                num_sources: 0,
                method: AggregationMethod::TWAP,
                status: PriceStatus::Unknown,
            };
        }

        let mut weighted_sum: i128 = 0;
        let mut total_weight: i128 = 0;

        for dp in data_points.iter() {
            // Weight = time since observation (more recent = higher weight)
            // Use (max_age - age) as weight, where max_age is the oldest observation
            let age = now.saturating_sub(dp.timestamp);
            // Invert: more recent → higher weight. Use a large base to avoid negatives.
            let weight = 1_000_000_i128.saturating_sub(age as i128);
            if weight > 0 {
                weighted_sum += dp.price * weight;
                total_weight += weight;
            }
        }

        let price = if total_weight > 0 {
            weighted_sum / total_weight
        } else {
            0
        };

        let worst_status = Self::worst_status(data_points);

        AggregatedPrice {
            asset,
            price,
            timestamp: now,
            num_sources: data_points.len(),
            method: AggregationMethod::TWAP,
            status: worst_status,
        }
    }

    /// Weighted average using oracle provider trust weights.
    fn aggregate_weighted(
        env: &Env,
        data_points: &Vec<PriceDataPoint>,
        asset: Symbol,
        now: u64,
    ) -> AggregatedPrice {
        let oracles = OracleManager::get_all_providers(env);

        // Build a lookup map of provider_id -> trust_weight
        let mut weight_map: soroban_sdk::Map<Symbol, u32> = soroban_sdk::Map::new(env);
        for oracle in oracles.iter() {
            weight_map.set(oracle.provider_id, oracle.trust_weight);
        }

        let mut weighted_sum: i128 = 0;
        let mut total_weight: i128 = 0;

        for dp in data_points.iter() {
            let weight = weight_map.get(dp.provider_id.clone()).unwrap_or(1000) as i128;
            weighted_sum += dp.price * weight;
            total_weight += weight;
        }

        let price = if total_weight > 0 {
            weighted_sum / total_weight
        } else {
            0
        };

        let worst_status = Self::worst_status(data_points);

        AggregatedPrice {
            asset,
            price,
            timestamp: now,
            num_sources: data_points.len(),
            method: AggregationMethod::WeightedAverage,
            status: worst_status,
        }
    }

    /// Determine the worst (highest severity) status among data points.
    /// Priority: Unknown > Anomalous > Stale > Fallback > Fresh
    fn worst_status(data_points: &Vec<PriceDataPoint>) -> PriceStatus {
        let mut worst = PriceStatus::Fresh;
        for dp in data_points.iter() {
            match dp.status {
                PriceStatus::Unknown => return PriceStatus::Unknown,
                PriceStatus::Anomalous => {
                    if worst != PriceStatus::Unknown {
                        worst = PriceStatus::Anomalous;
                    }
                }
                PriceStatus::Stale => {
                    if worst == PriceStatus::Fresh || worst == PriceStatus::Fallback {
                        worst = PriceStatus::Stale;
                    }
                }
                PriceStatus::Fallback => {
                    if worst == PriceStatus::Fresh {
                        worst = PriceStatus::Fallback;
                    }
                }
                PriceStatus::Fresh => {}
            }
        }
        worst
    }
}
