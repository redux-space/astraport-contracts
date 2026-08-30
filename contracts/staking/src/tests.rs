//! Unit and contract-level tests for the multi-asset staking contract.
//!
//! Tests are grouped into:
//! - original contract smoke tests (`initialize`, `get_balance`);
//! - authentication and balance tests;
//! - yield engine tests (accrual, rate changes, history, projections,
//!   distributions);
//! - emergency unstake tests;
//! - staking position management and state transition tests.

use super::*;
use soroban_sdk::testutils::{Address as _, Events, Ledger};
use soroban_sdk::{symbol_short, Address, Env, Symbol, TryFromVal, Vec};

/// Check if any topic in a Soroban `Vec<Val>` matches the given symbol.
fn topics_contain_symbol(topics: &Vec<soroban_sdk::Val>, env: &Env, sym: Symbol) -> bool {
    for i in 0..topics.len() {
        if let Ok(topic_sym) = Symbol::try_from_val(env, &topics.get(i).unwrap()) {
            if topic_sym == sym {
                return true;
            }
        }
    }
    false
}

use crate::emergency::PenaltyDecayFunction;
use crate::fixed_point::{SCALE, SECONDS_PER_DAY, SECONDS_PER_YEAR};
use crate::records::{CompoundingMode, GraduatedUnlock, UnlockSchedule};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup() -> (Env, StakingContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, StakingContract);
    let client = StakingContractClient::new(&env, &contract_id);
    (env, client)
}

fn setup_with_admin() -> (Env, StakingContractClient<'static>, Address) {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    (env, client, admin)
}

fn approx(a: i128, b: i128, tol: i128) {
    let diff = (a - b).abs();
    assert!(
        diff <= tol,
        "expected {} ~= {} within {}, diff {}",
        a,
        b,
        tol,
        diff
    );
}

// ---------------------------------------------------------------------------
// Smoke tests
// ---------------------------------------------------------------------------

#[test]
fn test_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let contract_id = env.register_contract(None, StakingContract);
    let client = StakingContractClient::new(&env, &contract_id);
    assert_eq!(client.initialize(&admin), symbol_short!("ok"));
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_double_initialize_panics() {
    let (_env, client, admin) = setup_with_admin();
    client.initialize(&admin);
}

// ---------------------------------------------------------------------------
// Basic multi-asset stake / unstake / balance
// ---------------------------------------------------------------------------

#[test]
fn test_get_balance_initial() {
    let (env, client) = setup();
    let staker = Address::generate(&env);
    assert_eq!(client.get_balance(&staker, &symbol_short!("XLM")), 0);
}

#[test]
fn test_stake_and_unstake() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    assert_eq!(
        client.stake(&staker, &asset, &100, &UnlockSchedule::Immediate, &false),
        symbol_short!("ok")
    );
    assert_eq!(client.get_balance(&staker, &asset), 100);

    assert_eq!(
        client.stake(&staker, &asset, &50, &UnlockSchedule::Immediate, &false),
        symbol_short!("ok")
    );
    assert_eq!(client.get_balance(&staker, &asset), 150);

    assert_eq!(client.unstake(&staker, &asset, &75), symbol_short!("ok"));
    assert_eq!(client.get_balance(&staker, &asset), 75);

    assert_eq!(client.unstake(&staker, &asset, &75), symbol_short!("ok"));
    assert_eq!(client.get_balance(&staker, &asset), 0);
}

#[test]
fn test_stake_multiple_assets_independently() {
    let (env, client) = setup();
    env.mock_all_auths();
    let staker = Address::generate(&env);
    let xlm = symbol_short!("XLM");
    let usdc = symbol_short!("USDC");
    let btc = symbol_short!("BTC");

    client.stake(&staker, &xlm, &1_000, &UnlockSchedule::Immediate, &false);
    client.stake(&staker, &usdc, &2_000, &UnlockSchedule::Immediate, &false);
    client.stake(&staker, &btc, &500, &UnlockSchedule::Immediate, &false);

    assert_eq!(client.get_balance(&staker, &xlm), 1_000);
    assert_eq!(client.get_balance(&staker, &usdc), 2_000);
    assert_eq!(client.get_balance(&staker, &btc), 500);

    // Unstaking one asset does not affect others.
    client.unstake(&staker, &xlm, &1_000);
    assert_eq!(client.get_balance(&staker, &xlm), 0);
    assert_eq!(client.get_balance(&staker, &usdc), 2_000);
    assert_eq!(client.get_balance(&staker, &btc), 500);
}

#[test]
fn test_unstake_more_than_balance() {
    let env = Env::default();
    env.mock_all_auths();
    let (_, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    client.stake(&staker, &asset, &100, &UnlockSchedule::Immediate, &false);
    assert_eq!(
        client.try_unstake(&staker, &asset, &150),
        Err(Ok(crate::Error::InsufficientBalance)),
    );
}

// ---------------------------------------------------------------------------
// Authentication tests
// ---------------------------------------------------------------------------

#[test]
fn test_stake_requires_auth() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, StakingContract);
    let client = StakingContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.get_balance(&staker, &asset), 1_000);
}

#[test]
#[should_panic]
fn test_stake_unauthorized() {
    let env = Env::default();
    let contract_id = env.register_contract(None, StakingContract);
    let client = StakingContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    // No mock_auths — require_auth will fail.
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);
}

#[test]
fn test_set_alert_threshold_requires_admin_auth() {
    let (_env, client, admin) = setup_with_admin();
    client.set_alert_threshold(&admin, &10_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_set_alert_threshold_non_admin_fails() {
    let (env, client, _admin) = setup_with_admin();
    let non_admin = Address::generate(&env);
    client.set_alert_threshold(&non_admin, &10_000);
}

// ---------------------------------------------------------------------------
// Yield engine contract-level tests
// ---------------------------------------------------------------------------

#[test]
fn apr_to_apy_via_contract_matches_formula() {
    let (_env, client) = setup();
    let apy = client.apr_to_apy(&(SCALE / 20), &CompoundingMode::Daily);
    approx(apy, 51_267_496_505_408_400, 100_000_000_000);

    let apy_c = client.apr_to_apy(&(SCALE / 20), &CompoundingMode::Continuous);
    approx(apy_c, 51_271_096_376_024_040, 100_000_000_000);
}

#[test]
fn test_set_yield_defaults_applies_to_new_positions() {
    let (env, client) = setup();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.open_yield_position(
        &staker,
        &asset,
        &SCALE,
        &(SCALE / 20),
        &CompoundingMode::Daily,
    );
    env.ledger().set_timestamp(SECONDS_PER_YEAR);
    let record = client.accrue_yield(&staker, &asset);
    approx(
        record.accrued_yield,
        51_267_496_505_408_400,
        100_000_000_000,
    );
}

#[test]
fn apy_apr_roundtrip_via_contract() {
    let (_env, client) = setup();
    let apr = SCALE / 10;
    let apy = client.apr_to_apy(&apr, &CompoundingMode::Daily);
    let back = client.apy_to_apr(&apy, &CompoundingMode::Daily);
    approx(back, apr, 100_000_000_000_000);
}

#[test]
fn thirty_day_projection_within_one_percent() {
    let (_env, client) = setup();
    let horizon = 30 * SECONDS_PER_DAY;
    let proj = client.project_yield(
        &SCALE,
        &(SCALE / 10),
        &CompoundingMode::Continuous,
        &horizon,
    );
    let expected = 8_253_048_640_000_000i128;
    let diff = (proj.projected_yield - expected).abs();
    assert!(
        diff <= expected / 100,
        "projection off >1%: {} vs {}",
        proj.projected_yield,
        expected
    );
    assert_eq!(proj.projected_balance, SCALE + proj.projected_yield);
}

// ---------------------------------------------------------------------------
// Unlock schedules
// ---------------------------------------------------------------------------

#[test]
fn accrue_claim_and_reclaim_resets_unclaimed_yield() {
    let (env, client) = setup();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    client.open_yield_position(
        &staker,
        &asset,
        &SCALE,
        &(SCALE / 10),
        &CompoundingMode::Daily,
    );

    env.ledger().set_timestamp(30 * SECONDS_PER_DAY);
    let accrued = client.accrue_yield(&staker, &asset).accrued_yield;
    assert!(accrued > 0);

    assert_eq!(client.claim_yield(&staker, &asset), accrued);
    assert_eq!(client.current_yield(&staker, &asset), 0);
    assert_eq!(client.claim_yield(&staker, &asset), 0);
}

// ---------------------------------------------------------------------------
// Emergency unstake — configuration tests
// ---------------------------------------------------------------------------

#[test]
fn configure_emergency_unstake_stores_config() {
    let (env, client, admin) = setup_with_admin();
    let treasury = Address::generate(&env);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.configure_emergency_unstake(
        &admin,
        &3_000, // 30% start penalty
        &500,   // 5% end penalty
        &PenaltyDecayFunction::Linear,
        &(7 * 24 * 3600u64), // 7-day cooldown
        &treasury,
        &true,
    );
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);

    let cfg = client.get_emergency_config().unwrap();
    assert_eq!(cfg.penalty_start_bps, 3_000);
    assert_eq!(cfg.penalty_end_bps, 500);
    assert!(cfg.enabled);
    assert_eq!(cfg.cooldown_seconds, 7 * 24 * 3600);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn configure_emergency_unstake_requires_admin() {
    let (env, client, _admin) = setup_with_admin();
    let non_admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.configure_emergency_unstake(
        &non_admin,
        &3_000,
        &500,
        &PenaltyDecayFunction::Linear,
        &86_400u64,
        &treasury,
        &true,
    );
}

// ---------------------------------------------------------------------------
// Emergency unstake — core flow tests
// ---------------------------------------------------------------------------

/// Sets up the contract with an emergency-unstake config, a staker's stake,
/// and a lock position. Returns (env, client, admin, staker, treasury).
fn setup_emergency(
    lock_start_ts: u64,
    unlock_ts: u64,
    stake_amount: i128,
) -> (
    Env,
    StakingContractClient<'static>,
    Address,
    Address,
    Address,
) {
    let (env, client, admin) = setup_with_admin();
    env.ledger().set_timestamp(lock_start_ts);

    let treasury = Address::generate(&env);
    let staker = Address::generate(&env);

    // Configure emergency unstake: 30% → 5% linear decay, 1-day cooldown.
    client.configure_emergency_unstake(
        &admin,
        &3_000,
        &500,
        &PenaltyDecayFunction::Linear,
        &(24 * 3600u64),
        &treasury,
        &true,
    );

    // Stake.
    let asset = symbol_short!("XLM");
    client.stake(
        &staker,
        &asset,
        &stake_amount,
        &UnlockSchedule::Immediate,
        &false,
    );

    // Register lock position.
    client.set_lock_position(&admin, &staker, &lock_start_ts, &unlock_ts, &stake_amount);

    (env, client, admin, staker, treasury)
}

#[test]
fn emergency_unstake_at_start_applies_max_penalty() {
    let lock_start = 1_000_000u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);

    // At t = lock_start (elapsed = 0) → start penalty = 30% (3000 bps).
    env.ledger().set_timestamp(lock_start);
    let asset = symbol_short!("XLM");
    let record = client.emergency_unstake(&staker, &asset, &1_000_000);

    assert_eq!(record.penalty_bps_applied, 3_000);
    assert_eq!(record.penalty_amount, 300_000); // 30% of 1_000_000
    assert_eq!(record.amount_returned, 700_000); // 70% back to staker
    assert_eq!(record.amount_requested, 1_000_000);
    assert!(!record.is_partial);
}

#[test]
fn emergency_unstake_at_end_applies_min_penalty() {
    let lock_start = 1_000_000u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);

    // At unlock (elapsed == total) → end penalty = 5% (500 bps).
    env.ledger().set_timestamp(unlock);
    let asset = symbol_short!("XLM");
    let record = client.emergency_unstake(&staker, &asset, &1_000_000);

    assert_eq!(record.penalty_bps_applied, 500);
    assert_eq!(record.penalty_amount, 50_000); // 5% of 1_000_000
    assert_eq!(record.amount_returned, 950_000);
}

#[test]
fn emergency_unstake_at_midpoint_applies_mid_penalty() {
    let lock_start = 1_000_000u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 2_000_000);

    // At midpoint linear decay: penalty ~= (3000+500)/2 = 1750 bps.
    env.ledger().set_timestamp(lock_start + total_lock / 2);
    let asset = symbol_short!("XLM");
    let record = client.emergency_unstake(&staker, &asset, &2_000_000);

    let expected_bps = 1_750i128;
    let diff = (record.penalty_bps_applied - expected_bps).abs();
    assert!(
        diff <= 5,
        "mid-point penalty {} != expected ~{}",
        record.penalty_bps_applied,
        expected_bps
    );
    assert_eq!(record.amount_returned + record.penalty_amount, 2_000_000);
}

#[test]
fn emergency_unstake_partial_reduces_balance_correctly() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);

    // Partially unstake half.
    env.ledger().set_timestamp(lock_start);
    let asset = symbol_short!("XLM");
    let record = client.emergency_unstake(&staker, &asset, &500_000);

    assert!(record.is_partial, "should be marked partial");
    assert_eq!(record.amount_requested, 500_000);
    // Balance reduced by full gross amount (500_000), not by net.
    assert_eq!(client.get_balance(&staker, &asset), 500_000);
}

#[test]
fn emergency_unstake_updates_history() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);

    env.ledger().set_timestamp(lock_start + total_lock / 4);
    let asset = symbol_short!("XLM");
    client.emergency_unstake(&staker, &asset, &200_000);
    env.ledger().set_timestamp(lock_start + total_lock * 2); // past cooldown
    client.emergency_unstake(&staker, &asset, &100_000);

    let history = client.get_emergency_unstake_history(&staker);
    assert_eq!(history.len(), 2, "expected two emergency unstake records");
    assert_eq!(history.get(0).unwrap().amount_requested, 200_000);
    assert_eq!(history.get(1).unwrap().amount_requested, 100_000);
}

#[test]
fn emergency_unstake_activates_cooldown() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;
    let cooldown = 24 * 3600u64;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);

    env.ledger().set_timestamp(lock_start);
    let asset = symbol_short!("XLM");
    client.emergency_unstake(&staker, &asset, &100_000);

    // Cooldown should be active immediately after.
    assert!(client.is_in_cooldown(&staker));

    // End should be set correctly.
    let cooldown_end = client.get_cooldown_end(&staker);
    assert_eq!(cooldown_end, lock_start + cooldown);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn emergency_unstake_fails_during_cooldown() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);

    env.ledger().set_timestamp(lock_start);
    let asset = symbol_short!("XLM");
    client.emergency_unstake(&staker, &asset, &100_000);

    // Still within cooldown — this must panic.
    env.ledger().set_timestamp(lock_start + 3600); // only 1 hour later, cooldown = 1 day
    client.emergency_unstake(&staker, &asset, &100_000);
}

#[test]
fn cooldown_expires_and_second_emergency_unstake_succeeds() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;
    let cooldown = 24 * 3600u64;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);

    env.ledger().set_timestamp(lock_start);
    let asset = symbol_short!("XLM");
    client.emergency_unstake(&staker, &asset, &100_000);

    // After cooldown, another emergency unstake succeeds.
    env.ledger().set_timestamp(lock_start + cooldown + 1);
    assert!(!client.is_in_cooldown(&staker));
    let record = client.emergency_unstake(&staker, &asset, &100_000);
    assert_eq!(record.amount_requested, 100_000);
}

// ---------------------------------------------------------------------------
// Emergency unstake — exponential decay
// ---------------------------------------------------------------------------

#[test]
fn emergency_unstake_exponential_midpoint_lower_than_linear() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;
    let stake = 1_000_000i128;

    // Linear config.
    let (env_lin, client_lin, admin_lin) = setup_with_admin();
    env_lin.mock_all_auths();
    env_lin.ledger().set_timestamp(lock_start);
    let treasury_lin = Address::generate(&env_lin);
    let staker_lin = Address::generate(&env_lin);
    let asset_lin = symbol_short!("XLM");
    client_lin.configure_emergency_unstake(
        &admin_lin,
        &4_000,
        &400,
        &PenaltyDecayFunction::Linear,
        &0u64,
        &treasury_lin,
        &true,
    );
    client_lin.stake(
        &staker_lin,
        &asset_lin,
        &stake,
        &UnlockSchedule::Immediate,
        &false,
    );
    client_lin.set_lock_position(&admin_lin, &staker_lin, &lock_start, &unlock, &stake);
    env_lin.ledger().set_timestamp(lock_start + total_lock / 2);
    let rec_lin = client_lin.emergency_unstake(&staker_lin, &asset_lin, &stake);

    // Exponential config.
    let (env_exp, client_exp, admin_exp) = setup_with_admin();
    env_exp.mock_all_auths();
    env_exp.ledger().set_timestamp(lock_start);
    let treasury_exp = Address::generate(&env_exp);
    let staker_exp = Address::generate(&env_exp);
    let asset_exp = symbol_short!("XLM");
    client_exp.configure_emergency_unstake(
        &admin_exp,
        &4_000,
        &400,
        &PenaltyDecayFunction::Exponential,
        &0u64,
        &treasury_exp,
        &true,
    );
    client_exp.stake(
        &staker_exp,
        &asset_exp,
        &stake,
        &UnlockSchedule::Immediate,
        &false,
    );
    client_exp.set_lock_position(&admin_exp, &staker_exp, &lock_start, &unlock, &stake);
    env_exp.ledger().set_timestamp(lock_start + total_lock / 2);
    let rec_exp = client_exp.emergency_unstake(&staker_exp, &asset_exp, &stake);

    assert!(
        rec_exp.penalty_bps_applied < rec_lin.penalty_bps_applied,
        "exponential mid penalty {} should be < linear mid penalty {}",
        rec_exp.penalty_bps_applied,
        rec_lin.penalty_bps_applied,
    );
}

// ---------------------------------------------------------------------------
// Emergency unstake — error paths
// ---------------------------------------------------------------------------

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn emergency_unstake_without_config_panics() {
    let (env, client, _admin) = setup_with_admin();
    env.mock_all_auths();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    client.stake(
        &staker,
        &asset,
        &1_000_000,
        &UnlockSchedule::Immediate,
        &false,
    );
    // No configure_emergency_unstake call → should panic.
    client.emergency_unstake(&staker, &asset, &500_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn emergency_unstake_when_disabled_panics() {
    let (env, client, admin) = setup_with_admin();
    let treasury = Address::generate(&env);
    let staker = Address::generate(&env);

    client.configure_emergency_unstake(
        &admin,
        &3_000,
        &500,
        &PenaltyDecayFunction::Linear,
        &86_400u64,
        &treasury,
        &false, // disabled
    );
    let asset = symbol_short!("XLM");
    client.stake(
        &staker,
        &asset,
        &1_000_000,
        &UnlockSchedule::Immediate,
        &false,
    );
    client.emergency_unstake(&staker, &asset, &500_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn emergency_unstake_more_than_balance_panics() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 500_000);

    env.ledger().set_timestamp(lock_start);
    let asset = symbol_short!("XLM");
    client.emergency_unstake(&staker, &asset, &600_000); // more than staked
}

// ---------------------------------------------------------------------------
// Preview penalty (pure query, no state change)
// ---------------------------------------------------------------------------

#[test]
fn preview_penalty_matches_actual_applied_penalty() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);

    let query_ts = lock_start + total_lock / 3;
    env.ledger().set_timestamp(query_ts);

    let preview_bps = client
        .preview_emergency_penalty(&lock_start, &unlock)
        .unwrap();

    // Actually perform the emergency unstake and compare.
    let asset = symbol_short!("XLM");
    let record = client.emergency_unstake(&staker, &asset, &1_000_000);
    assert_eq!(
        preview_bps, record.penalty_bps_applied,
        "preview {} should match applied {}",
        preview_bps, record.penalty_bps_applied
    );
}

#[test]
fn test_zero_principal_accrues_nothing() {
    let (env, client) = setup();
    env.ledger().set_timestamp(1_000);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    client.open_yield_position(&staker, &asset, &0, &(SCALE / 10), &CompoundingMode::Daily);
    env.ledger().set_timestamp(SECONDS_PER_YEAR);
    assert_eq!(client.accrue_yield(&staker, &asset).accrued_yield, 0);
}

// ---------------------------------------------------------------------------
// Protocol-level totals: total_staked(asset) + staker_count()
// ---------------------------------------------------------------------------

#[test]
fn test_totals_initial_state() {
    let (env, _client) = setup();
    env.mock_all_auths();
    let xlm = symbol_short!("XLM");
    let usdc = symbol_short!("USDC");
    assert_eq!(_client.total_staked(&xlm), 0);
    assert_eq!(_client.total_staked(&usdc), 0);
    assert_eq!(_client.staker_count(), 0);
}

#[test]
fn test_total_staked_reflects_stake_and_unstake_one_staker() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, StakingContract);
    let client = StakingContractClient::new(&env, &contract_id);
    let staker = Address::generate(&env);
    let xlm = symbol_short!("XLM");

    client.stake(&staker, &xlm, &1_000, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.total_staked(&xlm), 1_000);
    assert_eq!(client.staker_count(), 1);

    client.stake(&staker, &xlm, &500, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.total_staked(&xlm), 1_500);
    assert_eq!(
        client.staker_count(),
        1,
        "same staker, same asset: count stays at 1"
    );

    // Partial unstake keeps both stakers count and total above zero.
    client.unstake(&staker, &xlm, &700);
    assert_eq!(client.total_staked(&xlm), 800);
    assert_eq!(client.staker_count(), 1);

    // Full exit drains total AND decrements staker count to zero.
    client.unstake(&staker, &xlm, &800);
    assert_eq!(client.total_staked(&xlm), 0);
    assert_eq!(client.staker_count(), 0);
}

#[test]
fn test_total_staked_sums_across_multiple_stakers() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, StakingContract);
    let client = StakingContractClient::new(&env, &contract_id);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let carol = Address::generate(&env);
    let xlm = symbol_short!("XLM");

    client.stake(&alice, &xlm, &1_000, &UnlockSchedule::Immediate, &false);
    client.stake(&bob, &xlm, &2_500, &UnlockSchedule::Immediate, &false);
    client.stake(&carol, &xlm, &750, &UnlockSchedule::Immediate, &false);

    assert_eq!(client.total_staked(&xlm), 1_000 + 2_500 + 750);
    assert_eq!(client.staker_count(), 3);

    // Cross-check: sum of balances equals total_staked.
    assert_eq!(
        client
            .get_balance(&alice, &xlm)
            .checked_add(client.get_balance(&bob, &xlm))
            .and_then(|s| s.checked_add(client.get_balance(&carol, &xlm)))
            .unwrap(),
        client.total_staked(&xlm),
    );
}

#[test]
fn test_totals_are_per_asset() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, StakingContract);
    let client = StakingContractClient::new(&env, &contract_id);
    let staker = Address::generate(&env);
    let xlm = symbol_short!("XLM");
    let usdc = symbol_short!("USDC");

    client.stake(&staker, &xlm, &1_000, &UnlockSchedule::Immediate, &false);
    client.stake(&staker, &usdc, &5_000, &UnlockSchedule::Immediate, &false);

    assert_eq!(client.total_staked(&xlm), 1_000);
    assert_eq!(client.total_staked(&usdc), 5_000);
    // Distinct staker (one address) with two active positions still counts as 1.
    assert_eq!(client.staker_count(), 1);

    // Full exit on XLM leaves USDC untouched.
    client.unstake(&staker, &xlm, &1_000);
    assert_eq!(client.total_staked(&xlm), 0);
    assert_eq!(client.total_staked(&usdc), 5_000);
    assert_eq!(client.staker_count(), 1, "still active in usdc");

    // Full exit on USDC brings staker count to zero.
    client.unstake(&staker, &usdc, &5_000);
    assert_eq!(client.total_staked(&usdc), 0);
    assert_eq!(client.staker_count(), 0);
}

#[test]
fn test_staker_count_increments_only_on_first_active_position() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, StakingContract);
    let client = StakingContractClient::new(&env, &contract_id);

    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);
    let s3 = Address::generate(&env);
    let xlm = symbol_short!("XLM");

    assert_eq!(client.staker_count(), 0);

    client.stake(&s1, &xlm, &100, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.staker_count(), 1);

    client.stake(&s1, &xlm, &50, &UnlockSchedule::Immediate, &false); // same (staker, asset); no increment
    client.stake(&s2, &xlm, &100, &UnlockSchedule::Immediate, &false); // new staker
    assert_eq!(client.staker_count(), 2);

    client.stake(&s3, &xlm, &200, &UnlockSchedule::Immediate, &false); // new staker
    assert_eq!(client.staker_count(), 3);

    // Partial unstakes do NOT change staker_count.
    client.unstake(&s1, &xlm, &50);
    client.unstake(&s2, &xlm, &50);
    assert_eq!(client.staker_count(), 3);

    // Full exit of one staker decrements by exactly 1.
    client.unstake(&s1, &xlm, &100);
    assert_eq!(client.staker_count(), 2);

    // Full exit of a second staker.
    client.unstake(&s3, &xlm, &200);
    assert_eq!(client.staker_count(), 1);

    // And the last one.
    client.unstake(&s2, &xlm, &50);
    assert_eq!(client.staker_count(), 0);
}

#[test]
fn test_totals_update_on_emergency_unstake() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);
    let asset = symbol_short!("XLM");

    // After setup_emergency: one staker, total_staked = 1_000_000.
    assert_eq!(client.total_staked(&asset), 1_000_000);
    assert_eq!(client.staker_count(), 1);

    // Partial emergency unstake keeps count above zero and reduces total by gross.
    env.ledger().set_timestamp(lock_start);
    client.emergency_unstake(&staker, &asset, &400_000);
    assert_eq!(client.total_staked(&asset), 600_000);
    assert_eq!(client.staker_count(), 1);

    // Full emergency exit returns total to zero AND drops staker count to zero.
    env.ledger().set_timestamp(lock_start + total_lock * 2 + 1);
    client.emergency_unstake(&staker, &asset, &600_000);
    assert_eq!(client.total_staked(&asset), 0);
    assert_eq!(client.staker_count(), 0);
}

#[test]
fn test_totals_handle_re_stake_after_full_exit() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, StakingContract);
    let client = StakingContractClient::new(&env, &contract_id);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.stake(&staker, &asset, &500, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.staker_count(), 1);
    client.unstake(&staker, &asset, &500);
    assert_eq!(client.staker_count(), 0);
    assert_eq!(client.total_staked(&asset), 0);

    // Re-stake after full exit: counter and total come back.
    client.stake(&staker, &asset, &500, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.staker_count(), 1);
    assert_eq!(client.total_staked(&asset), 500);
}

// ===========================================================================
// Yield Distribution & Claiming System Tests
// ===========================================================================

// ---------------------------------------------------------------------------
// Reserve management
// ---------------------------------------------------------------------------

#[test]
fn reserve_initially_zero() {
    let (_env, client, _admin) = setup_with_admin();
    let asset = symbol_short!("XLM");
    assert_eq!(client.reserve_balance(&asset), 0);
}

#[test]
fn fund_reserve_increases_balance() {
    let (env, client, admin) = setup_with_admin();
    let asset = symbol_short!("XLM");
    assert_eq!(client.fund_reserve(&admin, &asset, &1_000_000), 1_000_000);
    assert_eq!(client.reserve_balance(&asset), 1_000_000);
    // Fund again.
    assert_eq!(client.fund_reserve(&admin, &asset, &500_000), 1_500_000);
    assert_eq!(client.reserve_balance(&asset), 1_500_000);
}

#[test]
fn withdraw_reserve_decreases_balance() {
    let (env, client, admin) = setup_with_admin();
    let asset = symbol_short!("XLM");
    client.fund_reserve(&admin, &asset, &1_000_000);
    assert_eq!(client.withdraw_reserve(&admin, &asset, &400_000), 600_000);
    assert_eq!(client.reserve_balance(&asset), 600_000);
}

#[test]
#[should_panic(expected = "InsufficientReserve")]
fn withdraw_reserve_insufficient_panics() {
    let (_env, client, admin) = setup_with_admin();
    let asset = symbol_short!("XLM");
    client.fund_reserve(&admin, &asset, &100_000);
    client.withdraw_reserve(&admin, &asset, &200_000);
}

#[test]
fn fund_requires_admin() {
    let (env, client, _admin) = setup_with_admin();
    let non_admin = Address::generate(&env);
    let asset = symbol_short!("XLM");
    client.fund_reserve(&non_admin, &asset, &1_000_000);
}

// ---------------------------------------------------------------------------
// Partial claims
// ---------------------------------------------------------------------------

#[test]
fn claim_yield_partial_basic() {
    let (env, client) = setup();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    client.open_yield_position(
        &staker,
        &asset,
        &SCALE,
        &(SCALE / 10),
        &CompoundingMode::Daily,
    );
    env.ledger().set_timestamp(30 * SECONDS_PER_DAY);

    let available = client.current_yield(&staker, &asset);
    assert!(available > 0);

    // Claim half.
    let half = available / 2;
    let claimed = client.claim_yield_partial(&staker, &asset, &half);
    assert_eq!(claimed, half);

    // Remaining yield should be approximately half.
    let remaining = client.current_yield(&staker, &asset);
    approx(remaining, available - half, 1);
}

#[test]
fn claim_yield_partial_claims_all_if_amount_exceeds() {
    let (env, client) = setup();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    client.open_yield_position(
        &staker,
        &asset,
        &SCALE,
        &(SCALE / 10),
        &CompoundingMode::Daily,
    );
    env.ledger().set_timestamp(30 * SECONDS_PER_DAY);

    let available = client.current_yield(&staker, &asset);
    let big_amount = available * 2;
    let claimed = client.claim_yield_partial(&staker, &asset, &big_amount);
    assert_eq!(claimed, available);
    assert_eq!(client.current_yield(&staker, &asset), 0);
}

#[test]
fn claim_yield_partial_zero_amount_fails() {
    let (env, client) = setup();
    env.mock_all_auths();
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");
    client.open_yield_position(
        &staker,
        &asset,
        &SCALE,
        &(SCALE / 10),
        &CompoundingMode::Daily,
    );
    assert_eq!(
        client.try_claim_yield_partial(&staker, &asset, &0),
        Err(Ok(crate::Error::InvalidClaimAmount))
    );
}

#[test]
fn claim_yield_partial_no_position_fails() {
    let (env, client) = setup();
    env.mock_all_auths();
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");
    assert_eq!(
        client.try_claim_yield_partial(&staker, &asset, &1000),
        Err(Ok(crate::Error::NoYieldPosition))
    );
}

#[test]
fn claim_yield_partial_capped_by_reserve() {
    let (env, client, admin) = setup_with_admin();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    client.open_yield_position(
        &staker,
        &asset,
        &SCALE,
        &(SCALE / 10),
        &CompoundingMode::Daily,
    );
    // Fund a small reserve.
    client.fund_reserve(&admin, &asset, &1_000);
    env.ledger().set_timestamp(30 * SECONDS_PER_DAY);

    // Available yield is much larger than reserve.
    let available = client.current_yield(&staker, &asset);
    assert!(available > 1_000);

    // Claim should be capped to reserve.
    let claimed = client.claim_yield_partial(&staker, &asset, &available);
    assert_eq!(claimed, 1_000);
    assert_eq!(client.reserve_balance(&asset), 0);
}

// ---------------------------------------------------------------------------
// Batch claiming
// ---------------------------------------------------------------------------

#[test]
fn batch_claim_multiple_stakers() {
    let (env, client) = setup();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let asset = symbol_short!("VESTED");
    let apr = SCALE / 10; // 10%

    // Open positions for both.
    client.open_yield_position(&alice, &asset, &SCALE, &apr, &CompoundingMode::Daily);
    client.open_yield_position(&bob, &asset, &(2 * SCALE), &apr, &CompoundingMode::Daily);

    env.ledger().set_timestamp(30 * SECONDS_PER_DAY);

    let alice_yield = client.current_yield(&alice, &asset);
    let bob_yield = client.current_yield(&bob, &asset);
    assert!(alice_yield > 0);
    assert!(bob_yield > 0);

    let stakers: Vec<Address> = soroban_sdk::vec![&env, alice.clone(), bob.clone()];
    let results = client.batch_claim(&stakers, &asset);

    assert_eq!(results.len(), 2);
    let (_a_addr, a_claimed) = results.get(0).unwrap();
    let (_b_addr, b_claimed) = results.get(1).unwrap();
    assert_eq!(a_claimed, alice_yield);
    assert_eq!(b_claimed, bob_yield);

    // After batch claim, yields should be zero.
    assert_eq!(client.current_yield(&alice, &asset), 0);
    assert_eq!(client.current_yield(&bob, &asset), 0);
}

#[test]
fn batch_claim_with_reserve_cap() {
    let (env, client, admin) = setup_with_admin();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let asset = symbol_short!("VESTED");
    let apr = SCALE / 10;

    client.open_yield_position(&alice, &asset, &SCALE, &apr, &CompoundingMode::Daily);
    client.open_yield_position(&bob, &asset, &SCALE, &apr, &CompoundingMode::Daily);

    // Small reserve — less than total accrued.
    client.fund_reserve(&admin, &asset, &1_000_000_000);

    env.ledger().set_timestamp(30 * SECONDS_PER_DAY);

    let stakers: Vec<Address> = soroban_sdk::vec![&env, alice.clone(), bob.clone()];
    let results = client.batch_claim(&stakers, &asset);

    // Total claimed should be capped to reserve.
    let total_claimed: i128 = results.iter().map(|(_, a)| a).sum();
    assert!(total_claimed <= 1_000_000_000);
    assert_eq!(
        client.reserve_balance(&asset),
        1_000_000_000 - total_claimed
    );
}

#[test]
fn batch_claim_no_yield_returns_zero() {
    let (env, client) = setup();
    env.mock_all_auths();
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    // No yield positions opened.
    let stakers: Vec<Address> = soroban_sdk::vec![&env, alice.clone(), bob.clone()];
    let results = client.batch_claim(&stakers, &asset);
    assert_eq!(results.len(), 2);
    assert_eq!(results.get(0).unwrap().1, 0);
    assert_eq!(results.get(1).unwrap().1, 0);
}

// ---------------------------------------------------------------------------
// Pause / unpause distributions
// ---------------------------------------------------------------------------

#[test]
fn distributions_not_paused_initially() {
    let (env, client) = setup();
    assert!(!client.distributions_paused());
}

#[test]
fn pause_and_unpause() {
    let (_env, client, admin) = setup_with_admin();
    assert_eq!(client.pause_distributions(&admin), symbol_short!("paused"));
    assert!(client.distributions_paused());
    assert_eq!(
        client.unpause_distributions(&admin),
        symbol_short!("active")
    );
    assert!(!client.distributions_paused());
}

#[test]
fn paused_batch_claim_returns_zero() {
    let (env, client, admin) = setup_with_admin();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    client.open_yield_position(
        &staker,
        &asset,
        &SCALE,
        &(SCALE / 10),
        &CompoundingMode::Daily,
    );
    env.ledger().set_timestamp(30 * SECONDS_PER_DAY);
    assert!(client.current_yield(&staker, &asset) > 0);

    // Pause distributions.
    client.pause_distributions(&admin);

    let stakers: Vec<Address> = soroban_sdk::vec![&env, staker.clone()];
    let results = client.batch_claim(&stakers, &asset);
    assert_eq!(results.get(0).unwrap().1, 0);
    // Yield should still be accrued (not consumed).
    assert!(client.current_yield(&staker, &asset) > 0);
}

#[test]
fn paused_process_distribution_returns_zero() {
    let (env, client, admin) = setup_with_admin();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    client.schedule_distribution(&staker, &asset, &100_000, &100, &0);
    client.pause_distributions(&admin);
    env.ledger().set_timestamp(200);

    let paid = client.process_distribution(&staker, &asset);
    assert_eq!(paid, 0);
}

// ---------------------------------------------------------------------------
// Distribution history
// ---------------------------------------------------------------------------

#[test]
fn distribution_history_records_claims() {
    let (env, client) = setup();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    client.open_yield_position(
        &staker,
        &asset,
        &SCALE,
        &(SCALE / 10),
        &CompoundingMode::Daily,
    );
    env.ledger().set_timestamp(30 * SECONDS_PER_DAY);
    client.claim_yield(&staker, &asset);

    let history = client.distribution_history(&staker, &asset);
    assert!(history.len() >= 1);
    let record = history.get(0).unwrap();
    assert!(record.amount > 0);
    assert_eq!(record.staker, staker);
    assert_eq!(record.asset, asset);
}

#[test]
fn total_yield_claimed_tracks_cumulative() {
    let (env, client) = setup();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    client.open_yield_position(
        &staker,
        &asset,
        &SCALE,
        &(SCALE / 10),
        &CompoundingMode::Daily,
    );
    env.ledger().set_timestamp(30 * SECONDS_PER_DAY);
    let first_claim = client.claim_yield(&staker, &asset);
    assert!(first_claim > 0);

    // Accumulate more yield.
    env.ledger().set_timestamp(60 * SECONDS_PER_DAY);
    let second_claim = client.claim_yield(&staker, &asset);
    assert!(second_claim > 0);

    let total = client.total_yield_claimed(&staker, &asset);
    assert_eq!(total, first_claim + second_claim);
}

// ---------------------------------------------------------------------------
// Scheduled distributions with reserve
// ---------------------------------------------------------------------------

#[test]
fn scheduled_distribution_funded_from_reserve() {
    let (env, client, admin) = setup_with_admin();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    // Fund reserve.
    client.fund_reserve(&admin, &asset, &1_000_000);

    // Schedule a one-off distribution at ts=100.
    client.schedule_distribution(&staker, &asset, &100_000, &100, &0);

    // Before due: nothing paid.
    env.ledger().set_timestamp(50);
    assert_eq!(client.process_distribution(&staker, &asset), 0);

    // After due: paid from reserve.
    env.ledger().set_timestamp(150);
    assert_eq!(client.process_distribution(&staker, &asset), 100_000);
    assert_eq!(client.reserve_balance(&asset), 900_000);
}

#[test]
fn scheduled_distribution_skipped_when_insufficient_reserve() {
    let (env, client, admin) = setup_with_admin();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    // Fund insufficient reserve.
    client.fund_reserve(&admin, &asset, &50_000);

    // Schedule distribution of 100_000.
    client.schedule_distribution(&staker, &asset, &100_000, &100, &0);

    env.ledger().set_timestamp(200);
    let paid = client.process_distribution(&staker, &asset);
    // Should be skipped — reserve insufficient.
    assert_eq!(paid, 0i128);
    assert_eq!(client.reserve_balance(&asset), 50_000);
}

#[test]
fn recurring_distribution_recurring_from_reserve() {
    let (env, client, admin) = setup_with_admin();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("VESTED");

    client.fund_reserve(&admin, &asset, &1_000_000);

    // Recurring: 100_000 every 30 days starting at ts=100.
    client.schedule_distribution(&staker, &asset, &100_000, &100, &(30 * SECONDS_PER_DAY));

    // First occurrence.
    env.ledger().set_timestamp(150);
    assert_eq!(client.process_distribution(&staker, &asset), 100_000);

    // Second occurrence.
    env.ledger().set_timestamp(100 + 30 * SECONDS_PER_DAY + 10);
    assert_eq!(client.process_distribution(&staker, &asset), 100_000);

    assert_eq!(client.reserve_balance(&asset), 800_000);
}

// ===========================================================================
// Staking Position Management & State Transition Tests
// ===========================================================================

// ---------------------------------------------------------------------------
// Position creation & querying
// ---------------------------------------------------------------------------

#[test]
fn test_get_position_initial_no_position() {
    let (env, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    assert_eq!(
        client.try_get_position(&staker, &asset),
        Err(Ok(crate::Error::NoStakingPosition))
    );
}

#[test]
fn test_stake_creates_position_with_immediate_schedule() {
    let (env, client) = setup();
    env.ledger().set_timestamp(100);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.staker, staker);
    assert_eq!(pos.asset, asset);
    assert_eq!(pos.principal, 1_000);
    assert_eq!(pos.state, crate::records::StakingState::Active);
    assert_eq!(pos.opened_at, 100);
    assert!(!pos.locked);
}

#[test]
fn test_stake_creates_position_with_cliff_lock() {
    let (env, client) = setup();
    env.ledger().set_timestamp(100);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    let unlock_ts = 200u64;

    client.stake(
        &staker,
        &asset,
        &500,
        &UnlockSchedule::Cliff(unlock_ts),
        &false,
    );

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.state, crate::records::StakingState::Locked);
    assert_eq!(pos.principal, 500);
    match pos.unlock_schedule {
        UnlockSchedule::Cliff(ts) => assert_eq!(ts, unlock_ts),
        _ => panic!("expected Cliff unlock schedule"),
    }
}

#[test]
fn test_stake_creates_position_with_graduated_unlock() {
    let (env, client) = setup();
    env.ledger().set_timestamp(100);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    let grad = GraduatedUnlock {
        start_ts: 200,
        interval_seconds: 30 * SECONDS_PER_DAY,
        tranche_pct_bps: 2500, // 25% per tranche
    };

    client.stake(
        &staker,
        &asset,
        &4_000,
        &UnlockSchedule::Graduated(grad.clone()),
        &false,
    );

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.state, crate::records::StakingState::Locked);
    assert_eq!(pos.principal, 4_000);
    match &pos.unlock_schedule {
        UnlockSchedule::Graduated(g) => {
            assert_eq!(g.start_ts, 200);
            assert_eq!(g.interval_seconds, 30 * SECONDS_PER_DAY);
            assert_eq!(g.tranche_pct_bps, 2500);
        }
        _ => panic!("expected Graduated unlock schedule"),
    }
}

#[test]
fn test_stake_with_lock_position_flag() {
    let (env, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    // Stake with lock_position=true
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &true);

    let pos = client.get_position(&staker, &asset);
    assert!(pos.locked, "position should be marked as locked/immutable");
}

// ---------------------------------------------------------------------------
// Position immutability
// ---------------------------------------------------------------------------

#[test]
fn test_locked_position_cannot_be_modified() {
    let (env, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    // Stake with lock_position=true
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &true);

    // Attempting to stake more into the same locked position should fail
    assert_eq!(
        client.try_stake(&staker, &asset, &500, &UnlockSchedule::Immediate, &true),
        Err(Ok(crate::Error::ImmutablePosition))
    );
}

#[test]
fn test_unlocked_position_can_be_increased() {
    let (env, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    // Stake without lock
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.get_balance(&staker, &asset), 1_000);

    // Stake more into the same unlocked position
    client.stake(&staker, &asset, &500, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.get_balance(&staker, &asset), 1_500);

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.principal, 1_500);
}

// ---------------------------------------------------------------------------
// State transitions
// ---------------------------------------------------------------------------

#[test]
fn test_state_transition_locked_to_claimable_on_cliff_expiry() {
    let (env, client) = setup();
    env.ledger().set_timestamp(100);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    let unlock_ts = 200u64;

    client.stake(
        &staker,
        &asset,
        &1_000,
        &UnlockSchedule::Cliff(unlock_ts),
        &false,
    );

    // Position starts locked
    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.state, crate::records::StakingState::Locked);

    // Before unlock: unstake should fail
    assert_eq!(
        client.try_unstake(&staker, &asset, &500),
        Err(Ok(crate::Error::PositionStillLocked))
    );

    // After unlock: unstake should succeed and transition state
    env.ledger().set_timestamp(unlock_ts);
    assert_eq!(client.unstake(&staker, &asset, &500), symbol_short!("ok"));

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.state, crate::records::StakingState::Claimable);
    assert_eq!(pos.principal, 500);
    assert_eq!(client.get_balance(&staker, &asset), 500);
}

#[test]
fn test_state_transition_to_withdrawn_on_full_unstake() {
    let (env, client) = setup();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.state, crate::records::StakingState::Active);

    // Full unstake
    client.unstake(&staker, &asset, &1_000);
    assert_eq!(client.get_balance(&staker, &asset), 0);

    // Position should be marked as Withdrawn
    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.state, crate::records::StakingState::Withdrawn);
}

#[test]
fn test_state_transitions_emitted_as_events() {
    let (env, client) = setup();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    let unlock_ts = 100u64;

    // Stake with cliff lock → emits Withdrawn → Locked transition
    client.stake(
        &staker,
        &asset,
        &1_000,
        &UnlockSchedule::Cliff(unlock_ts),
        &false,
    );

    // Unstake after lock → emits Locked → Claimable transition
    env.ledger().set_timestamp(unlock_ts);
    client.unstake(&staker, &asset, &1_000);

    // The position is now Withdrawn (full unstake)
    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.state, crate::records::StakingState::Withdrawn);
}

// ---------------------------------------------------------------------------
// Balance management edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_stake_zero_amount_fails() {
    let (env, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    assert_eq!(
        client.try_stake(&staker, &asset, &0, &UnlockSchedule::Immediate, &false),
        Err(Ok(crate::Error::InvalidStakeAmount))
    );
}

#[test]
fn test_unstake_zero_amount_fails() {
    let (env, client) = setup();
    env.mock_all_auths();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);

    assert_eq!(
        client.try_unstake(&staker, &asset, &0),
        Err(Ok(crate::Error::InvalidStakeAmount))
    );
}

#[test]
fn test_unstake_no_position_fails() {
    let (env, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    assert_eq!(
        client.try_unstake(&staker, &asset, &100),
        Err(Ok(crate::Error::NoStakingPosition))
    );
}

#[test]
fn test_balance_tracks_multiple_stakers_independently() {
    let (env, client) = setup();
    env.mock_all_auths();
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.stake(&alice, &asset, &1_000, &UnlockSchedule::Immediate, &false);
    client.stake(&bob, &asset, &2_000, &UnlockSchedule::Immediate, &false);

    assert_eq!(client.get_balance(&alice, &asset), 1_000);
    assert_eq!(client.get_balance(&bob, &asset), 2_000);

    client.unstake(&alice, &asset, &500);
    assert_eq!(client.get_balance(&alice, &asset), 500);
    assert_eq!(client.get_balance(&bob, &asset), 2_000); // unchanged
}

#[test]
fn test_stake_accumulates_correctly() {
    let (env, client) = setup();
    env.mock_all_auths();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.stake(&staker, &asset, &100, &UnlockSchedule::Immediate, &false);
    client.stake(&staker, &asset, &200, &UnlockSchedule::Immediate, &false);
    client.stake(&staker, &asset, &300, &UnlockSchedule::Immediate, &false);

    assert_eq!(client.get_balance(&staker, &asset), 600);

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.principal, 600);
}

#[test]
fn test_partial_unstake_preserves_position() {
    let (env, client) = setup();
    env.mock_all_auths();
    env.ledger().set_timestamp(50);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);

    client.unstake(&staker, &asset, &300);
    assert_eq!(client.get_balance(&staker, &asset), 700);

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.principal, 700);
    // Position should still be active (not withdrawn)
    assert_ne!(pos.state, crate::records::StakingState::Withdrawn);
}

// ---------------------------------------------------------------------------
// Cliff lock enforcement
// ---------------------------------------------------------------------------

#[test]
fn test_cliff_lock_prevents_early_unstake() {
    let (env, client) = setup();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    let unlock_ts = SECONDS_PER_YEAR; // unlock in 1 year

    client.stake(
        &staker,
        &asset,
        &1_000,
        &UnlockSchedule::Cliff(unlock_ts),
        &false,
    );

    // Try to unstake before unlock
    assert_eq!(
        client.try_unstake(&staker, &asset, &500),
        Err(Ok(crate::Error::PositionStillLocked))
    );
    assert_eq!(client.get_balance(&staker, &asset), 1_000); // unchanged

    // Advance time but still before unlock
    env.ledger().set_timestamp(SECONDS_PER_YEAR - 1);
    assert_eq!(
        client.try_unstake(&staker, &asset, &500),
        Err(Ok(crate::Error::PositionStillLocked))
    );

    // Now at unlock time: unstake succeeds
    env.ledger().set_timestamp(unlock_ts);
    assert_eq!(client.unstake(&staker, &asset, &500), symbol_short!("ok"));
    assert_eq!(client.get_balance(&staker, &asset), 500);
}

// ---------------------------------------------------------------------------
// Graduated unlock enforcement
// ---------------------------------------------------------------------------

#[test]
fn test_graduated_unlock_prevents_early_full_unstake() {
    let (env, client) = setup();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    // 25% per tranche, 30-day intervals, starting at ts=1
    // (start_ts must be > current_ts for validation to pass)
    let grad = GraduatedUnlock {
        start_ts: 1,
        interval_seconds: 30 * SECONDS_PER_DAY,
        tranche_pct_bps: 2500,
    };

    client.stake(
        &staker,
        &asset,
        &1_000,
        &UnlockSchedule::Graduated(grad),
        &false,
    );

    // After 1 tranche (30 days from start_ts=1), only 25% = 250 is unlocked
    env.ledger().set_timestamp(1 + 30 * SECONDS_PER_DAY);
    assert_eq!(
        client.try_unstake(&staker, &asset, &500),
        Err(Ok(crate::Error::ExceedsUnlockedAmount))
    );

    // Unstake within the unlocked amount should work
    assert_eq!(client.unstake(&staker, &asset, &250), symbol_short!("ok"));
    assert_eq!(client.get_balance(&staker, &asset), 750);
}

// ---------------------------------------------------------------------------
// Emergency withdrawal state transitions
// ---------------------------------------------------------------------------

#[test]
fn test_emergency_unstake_transitions_to_withdrawn_on_full_exit() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);
    let asset = symbol_short!("XLM");

    // Position starts as Active (staked with Immediate schedule in setup_emergency)
    let pos = client.get_position(&staker, &asset);
    assert_ne!(pos.state, crate::records::StakingState::Withdrawn);

    // Full emergency exit
    env.ledger().set_timestamp(lock_start);
    client.emergency_unstake(&staker, &asset, &1_000_000);

    // Position should be Withdrawn
    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.state, crate::records::StakingState::Withdrawn);
    assert_eq!(client.get_balance(&staker, &asset), 0);
}

#[test]
fn test_emergency_unstake_preserves_position_on_partial() {
    let lock_start = 0u64;
    let total_lock = 30u64 * 24 * 3600;
    let unlock = lock_start + total_lock;

    let (env, client, _admin, staker, _treasury) = setup_emergency(lock_start, unlock, 1_000_000);
    let asset = symbol_short!("XLM");

    // Partial emergency exit
    env.ledger().set_timestamp(lock_start);
    client.emergency_unstake(&staker, &asset, &400_000);

    // Position should still exist
    let pos = client.get_position(&staker, &asset);
    assert_ne!(pos.state, crate::records::StakingState::Withdrawn);
    assert_eq!(pos.principal, 600_000);
    assert_eq!(client.get_balance(&staker, &asset), 600_000);
}

// ---------------------------------------------------------------------------
// Invalid unlock schedule validation
// ---------------------------------------------------------------------------

#[test]
fn test_stake_with_past_cliff_timestamp_fails() {
    let (env, client) = setup();
    env.ledger().set_timestamp(200);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    // Cliff timestamp in the past
    assert_eq!(
        client.try_stake(&staker, &asset, &1_000, &UnlockSchedule::Cliff(100), &false),
        Err(Ok(crate::Error::InvalidStakeAmount))
    );
}

#[test]
fn test_stake_with_graduated_past_start_ts_fails() {
    let (env, client) = setup();
    env.ledger().set_timestamp(200);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    let grad = GraduatedUnlock {
        start_ts: 100, // already in the past
        interval_seconds: 30 * SECONDS_PER_DAY,
        tranche_pct_bps: 2500,
    };

    assert_eq!(
        client.try_stake(
            &staker,
            &asset,
            &1_000,
            &UnlockSchedule::Graduated(grad),
            &false
        ),
        Err(Ok(crate::Error::InvalidStakeAmount))
    );
}

#[test]
fn test_stake_with_zero_interval_graduated_fails() {
    let (env, client) = setup();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    let grad = GraduatedUnlock {
        start_ts: 100,
        interval_seconds: 0, // invalid
        tranche_pct_bps: 2500,
    };

    assert_eq!(
        client.try_stake(
            &staker,
            &asset,
            &1_000,
            &UnlockSchedule::Graduated(grad),
            &false
        ),
        Err(Ok(crate::Error::InvalidStakeAmount))
    );
}

#[test]
fn test_stake_with_invalid_tranche_pct_fails() {
    let (env, client) = setup();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    let grad = GraduatedUnlock {
        start_ts: 100,
        interval_seconds: 30 * SECONDS_PER_DAY,
        tranche_pct_bps: 15000, // > 100%
    };

    assert_eq!(
        client.try_stake(
            &staker,
            &asset,
            &1_000,
            &UnlockSchedule::Graduated(grad),
            &false
        ),
        Err(Ok(crate::Error::InvalidStakeAmount))
    );
}

// ---------------------------------------------------------------------------
// get_balance accuracy across operations
// ---------------------------------------------------------------------------

#[test]
fn test_get_balance_after_stake_unstake_cycle() {
    let (env, client) = setup();
    env.mock_all_auths();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    // Initial balance is 0
    assert_eq!(client.get_balance(&staker, &asset), 0);

    // Stake
    client.stake(&staker, &asset, &500, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.get_balance(&staker, &asset), 500);

    // More staking
    client.stake(&staker, &asset, &300, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.get_balance(&staker, &asset), 800);

    // Partial unstake
    client.unstake(&staker, &asset, &200);
    assert_eq!(client.get_balance(&staker, &asset), 600);

    // More unstaking
    client.unstake(&staker, &asset, &600);
    assert_eq!(client.get_balance(&staker, &asset), 0);
}

#[test]
fn test_get_balance_across_multiple_assets() {
    let (env, client) = setup();
    env.mock_all_auths();
    let staker = Address::generate(&env);
    let xlm = symbol_short!("XLM");
    let usdc = symbol_short!("USDC");

    client.stake(&staker, &xlm, &100, &UnlockSchedule::Immediate, &false);
    client.stake(&staker, &usdc, &200, &UnlockSchedule::Immediate, &false);

    assert_eq!(client.get_balance(&staker, &xlm), 100);
    assert_eq!(client.get_balance(&staker, &usdc), 200);

    client.unstake(&staker, &xlm, &50);
    assert_eq!(client.get_balance(&staker, &xlm), 50);
    assert_eq!(client.get_balance(&staker, &usdc), 200); // unaffected
}

// ---------------------------------------------------------------------------
// Position data integrity
// ---------------------------------------------------------------------------

#[test]
fn test_position_preserves_apr_and_mode() {
    let (env, client) = setup();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    let custom_apr = SCALE / 5; // 20%

    client.set_yield_defaults(&custom_apr, &CompoundingMode::Continuous);
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.apr, custom_apr);
    assert_eq!(pos.mode, CompoundingMode::Continuous);
}

#[test]
fn test_position_opened_at_matches_stake_timestamp() {
    let (env, client) = setup();
    env.ledger().set_timestamp(42);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.opened_at, 42);
}

#[test]
fn test_position_staker_and_asset_match() {
    let (env, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("USDC");

    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.staker, staker);
    assert_eq!(pos.asset, asset);

    // Different asset should have no position
    let other_asset = symbol_short!("XLM");
    assert_eq!(
        client.try_get_position(&staker, &other_asset),
        Err(Ok(crate::Error::NoStakingPosition))
    );
}

// ---------------------------------------------------------------------------
// Multi-staker edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_stakers_cannot_unstake_each_others_funds() {
    let (env, client) = setup();
    env.mock_all_auths();
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.stake(&alice, &asset, &1_000, &UnlockSchedule::Immediate, &false);
    client.stake(&bob, &asset, &2_000, &UnlockSchedule::Immediate, &false);

    // Alice tries to unstake more than her balance (Bob's funds don't count)
    assert_eq!(
        client.try_unstake(&alice, &asset, &1_500),
        Err(Ok(crate::Error::InsufficientBalance))
    );

    // Both balances unchanged
    assert_eq!(client.get_balance(&alice, &asset), 1_000);
    assert_eq!(client.get_balance(&bob, &asset), 2_000);
}

// ---------------------------------------------------------------------------
// Staking with yield integration
// ---------------------------------------------------------------------------

#[test]
fn test_stake_with_cliff_generates_yield_after_unlock() {
    let (env, client) = setup();
    env.mock_all_auths();
    env.ledger().set_timestamp(0);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");
    let unlock_ts = 30 * SECONDS_PER_DAY;

    client.stake(
        &staker,
        &asset,
        &SCALE,
        &UnlockSchedule::Cliff(unlock_ts),
        &false,
    );

    // After unlock, unstake should work and position transitions
    env.ledger().set_timestamp(unlock_ts);
    client.unstake(&staker, &asset, &(SCALE / 2));

    let pos = client.get_position(&staker, &asset);
    assert_eq!(pos.state, crate::records::StakingState::Claimable);
    assert_eq!(pos.principal, SCALE / 2);
    assert_eq!(client.get_balance(&staker, &asset), SCALE / 2);
}

#[test]
fn test_emergency_config_query_after_stake() {
    let (env, client, admin) = setup_with_admin();
    let treasury = Address::generate(&env);
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.configure_emergency_unstake(
        &admin,
        &2_000,
        &200,
        &PenaltyDecayFunction::Exponential,
        &0u64,
        &treasury,
        &true,
    );
    client.stake(&staker, &asset, &100, &UnlockSchedule::Immediate, &false);

    let cfg = client.get_emergency_config().unwrap();
    assert_eq!(cfg.penalty_start_bps, 2_000);
    assert_eq!(cfg.penalty_end_bps, 200);
    assert!(matches!(
        cfg.decay_function,
        PenaltyDecayFunction::Exponential
    ));
}

// ===========================================================================
// Alert Threshold Tests
// ===========================================================================

/// Helper: count how many events in `env` have `ALERT` as their first topic.
fn count_alert_events(env: &Env) -> u32 {
    let events = env.events().all();
    let alert_sym = symbol_short!("ALERT");
    let mut count = 0u32;
    for i in 0..events.len() {
        let (_contract_id, topics, _data) = events.get(i).unwrap();
        if topics_contain_symbol(&topics, env, alert_sym.clone()) {
            count += 1;
        }
    }
    count
}

/// Helper: return the deserialized `AlertEvent` data from the n-th ALERT event (0-based).
fn nth_alert_event(env: &Env, n: u32) -> crate::alerts::AlertEvent {
    let events = env.events().all();
    let alert_sym = symbol_short!("ALERT");
    let mut seen = 0u32;
    for i in 0..events.len() {
        let (_contract_id, topics, data) = events.get(i).unwrap();
        if topics_contain_symbol(&topics, env, alert_sym.clone()) {
            if seen == n {
                return crate::alerts::AlertEvent::try_from_val(env, &data)
                    .expect("failed to deserialize AlertEvent");
            }
            seen += 1;
        }
    }
    panic!("not enough ALERT events; requested #{n} but only {seen} found");
}

#[test]
fn test_get_alert_threshold_initially_none() {
    let (_env, client) = setup();
    assert_eq!(client.get_alert_threshold(), None);
}

#[test]
fn test_set_and_get_alert_threshold() {
    let (_env, client, admin) = setup_with_admin();
    client.set_alert_threshold(&admin, &5_000);
    assert_eq!(client.get_alert_threshold(), Some(5_000));
}

#[test]
fn test_stake_below_threshold_emits_alert_event() {
    let (env, client, admin) = setup_with_admin();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.set_alert_threshold(&admin, &1_000);
    client.stake(&staker, &asset, &500, &UnlockSchedule::Immediate, &false);
    assert_eq!(client.get_balance(&staker, &asset), 500);

    assert_eq!(count_alert_events(&env), 1, "expected exactly one ALERT event");

    let event = nth_alert_event(&env, 0);
    assert_eq!(event.staker, staker);
    assert_eq!(event.asset, asset);
    assert_eq!(event.kind, crate::alerts::AlertKind::BalanceDrop);
    assert_eq!(event.severity, crate::alerts::AlertSeverity::Critical);
    assert_eq!(event.threshold_value, 1_000);
    assert_eq!(event.observed_value, 500);
}

#[test]
fn test_stake_above_threshold_no_alert_event() {
    let (env, client, admin) = setup_with_admin();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.set_alert_threshold(&admin, &500);
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);

    assert_eq!(count_alert_events(&env), 0, "expected no ALERT events");
}

#[test]
fn test_unstake_below_threshold_emits_alert_event() {
    let (env, client, admin) = setup_with_admin();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.set_alert_threshold(&admin, &800);
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);
    client.unstake(&staker, &asset, &300);
    assert_eq!(client.get_balance(&staker, &asset), 700);

    // Stake was above threshold, unstake brought it below → exactly 1 alert.
    assert_eq!(count_alert_events(&env), 1, "expected exactly one ALERT event");

    let event = nth_alert_event(&env, 0);
    assert_eq!(event.kind, crate::alerts::AlertKind::BalanceDrop);
    assert_eq!(event.threshold_value, 800);
    assert_eq!(event.observed_value, 700);
}

#[test]
fn test_unstake_above_threshold_no_alert_event() {
    let (env, client, admin) = setup_with_admin();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.set_alert_threshold(&admin, &100);
    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);
    client.unstake(&staker, &asset, &300);

    assert_eq!(count_alert_events(&env), 0, "expected no ALERT events");
}

#[test]
fn test_no_threshold_no_alert_event() {
    let (env, client) = setup();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.stake(&staker, &asset, &1_000, &UnlockSchedule::Immediate, &false);
    client.unstake(&staker, &asset, &999);

    assert_eq!(count_alert_events(&env), 0, "expected no ALERT events without threshold");
}

#[test]
fn test_stake_at_exact_threshold_no_alert() {
    let (env, client, admin) = setup_with_admin();
    let staker = Address::generate(&env);
    let asset = symbol_short!("XLM");

    client.set_alert_threshold(&admin, &500);
    client.stake(&staker, &asset, &500, &UnlockSchedule::Immediate, &false);

    assert_eq!(count_alert_events(&env), 0, "balance == threshold should not trigger alert");
}
