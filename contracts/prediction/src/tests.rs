//! Comprehensive tests for the Prediction Markets contract.

use super::*;
use soroban_sdk::{
    symbol_short,
    testutils::Address as _,
    Address, Env, Symbol, Vec,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn setup() -> (Env, PredictionMarketClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, PredictionMarket);
    let client = PredictionMarketClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    (env, client, admin)
}

fn binary_outcomes() -> (Vec<Outcome>, Vec<Outcome>) {
    let env = Env::default();
    let yes_no = vec![
        &env,
        Outcome {
            name: symbol_short!("Yes"),
            index: 0,
        },
        Outcome {
            name: symbol_short!("No"),
            index: 1,
        },
    ];
    let three_way = vec![
        &env,
        Outcome {
            name: symbol_short!("TeamA"),
            index: 0,
        },
        Outcome {
            name: symbol_short!("Draw"),
            index: 1,
        },
        Outcome {
            name: symbol_short!("TeamB"),
            index: 2,
        },
    ];
    (yes_no, three_way)
}

fn create_binary_market(
    env: &Env,
    client: &PredictionMarketClient,
    admin: &Address,
) -> u64 {
    let (outcomes, _) = binary_outcomes();
    let now = env.ledger().timestamp();
    client.create_market(
        admin,
        &symbol_short!("BTC>100k"),
        &MarketCategory::Crypto,
        &outcomes,
        &(now + 86400),    // trading_end_time: 1 day
        &(now + 172800),   // resolution_time: 2 days
        &symbol_short!("Chainlink"),
        &1_000_000_000,     // max supply
        &30,                // 0.3% fee
        &true,              // allow early close
    )
}

fn create_multi_outcome_market(
    env: &Env,
    client: &PredictionMarketClient,
    admin: &Address,
) -> u64 {
    let (_, outcomes) = binary_outcomes();
    let now = env.ledger().timestamp();
    client.create_market(
        admin,
        &symbol_short!("WC_Final"),
        &MarketCategory::Sports,
        &outcomes,
        &(now + 86400),
        &(now + 172800),
        &symbol_short!("SportsOracle"),
        &5_000_000_000,
        &50, // 0.5% fee
        &false,
    )
}

// ---------------------------------------------------------------------------
// Initialization tests
// ---------------------------------------------------------------------------

#[test]
fn test_initialize() {
    let (_env, client, _admin) = setup();
    let count = client.get_market_count();
    assert_eq!(count, 0);
}

#[test]
fn test_double_initialize_fails() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PredictionMarket);
    let client = PredictionMarketClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&admin);
    assert_eq!(
        client.try_initialize(&admin),
        Err(Ok(PredictionError::AlreadyInitialized)),
    );
}

// ---------------------------------------------------------------------------
// Market creation tests
// ---------------------------------------------------------------------------

#[test]
fn test_create_binary_market() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    let market = client.get_market(market_id).unwrap();
    assert_eq!(market.description, symbol_short!("BTC>100k"));
    assert_eq!(market.category, MarketCategory::Crypto);
    assert_eq!(market.outcomes.len(), 2);
    assert!(market.is_binary());
    assert_eq!(market.status, MarketStatus::Active);
    assert_eq!(market.fee_bps, 30);
    assert!(market.allow_early_close);
}

#[test]
fn test_create_multi_outcome_market() {
    let (env, client, admin) = setup();
    let market_id = create_multi_outcome_market(&env, &client, &admin);

    let market = client.get_market(market_id).unwrap();
    assert_eq!(market.outcomes.len(), 3);
    assert!(!market.is_binary());
    assert_eq!(market.category, MarketCategory::Sports);
}

#[test]
fn test_market_list_tracking() {
    let (env, client, admin) = setup();
    let id1 = create_binary_market(&env, &client, &admin);
    let id2 = create_multi_outcome_market(&env, &client, &admin);

    let ids = client.get_all_market_ids();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids.get(0).unwrap(), id1);
    assert_eq!(ids.get(1).unwrap(), id2);
    assert_eq!(client.get_market_count(), 2);
}

#[test]
fn test_create_market_invalid_outcomes() {
    let (env, client, admin) = setup();
    let now = env.ledger().timestamp();

    // Single outcome should fail
    let one_outcome = vec![
        &env,
        Outcome {
            name: symbol_short!("Only"),
            index: 0,
        },
    ];
    assert_eq!(
        client.try_create_market(
            &admin,
            &symbol_short!("Bad"),
            &MarketCategory::Other,
            &one_outcome,
            &(now + 86400),
            &(now + 172800),
            &symbol_short!("Oracle"),
            &1_000_000,
            &30,
            &false,
        ),
        Err(Ok(PredictionError::TooManyOutcomes)),
    );
}

#[test]
fn test_create_market_invalid_times() {
    let (env, client, admin) = setup();
    let now = env.ledger().timestamp();
    let (outcomes, _) = binary_outcomes();

    // trading_end_time must be after now
    assert_eq!(
        client.try_create_market(
            &admin,
            &symbol_short!("Bad"),
            &MarketCategory::Other,
            &outcomes,
            &now, // Not in the future
            &(now + 172800),
            &symbol_short!("Oracle"),
            &1_000_000,
            &30,
            &false,
        ),
        Err(Ok(PredictionError::InvalidMarketParams)),
    );

    // resolution_time must be after trading_end_time
    assert_eq!(
        client.try_create_market(
            &admin,
            &symbol_short!("Bad"),
            &MarketCategory::Other,
            &outcomes,
            &(now + 172800),
            &(now + 86400), // Before trading end
            &symbol_short!("Oracle"),
            &1_000_000,
            &30,
            &false,
        ),
        Err(Ok(PredictionError::InvalidMarketParams)),
    );
}

#[test]
fn test_markets_by_category() {
    let (env, client, admin) = setup();
    create_binary_market(&env, &client, &admin);
    create_multi_outcome_market(&env, &client, &admin);

    let crypto_markets = client.get_markets_by_category(&MarketCategory::Crypto);
    assert_eq!(crypto_markets.len(), 1);

    let sports_markets = client.get_markets_by_category(&MarketCategory::Sports);
    assert_eq!(sports_markets.len(), 1);

    let events_markets = client.get_markets_by_category(&MarketCategory::Events);
    assert_eq!(events_markets.len(), 0);
}

// ---------------------------------------------------------------------------
// Liquidity pool tests
// ---------------------------------------------------------------------------

#[test]
fn test_create_liquidity_pool() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    let result = client.create_liquidity_pool(&admin, &market_id, &100_000);
    assert!(result.is_ok());
    let lp_result = result.unwrap();
    assert_eq!(lp_result.lp_tokens_minted, 100_000);
    assert_eq!(lp_result.collateral_deposited, 100_000);

    let lp_balance = client.get_lp_balance(market_id, admin.clone());
    assert_eq!(lp_balance, 100_000);
}

#[test]
fn test_create_pool_insufficient_liquidity() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    assert_eq!(
        client.try_create_liquidity_pool(&admin, &market_id, &100),
        Err(Ok(PredictionError::InsufficientLiquidity)),
    );
}

#[test]
fn test_pool_invariant_after_creation() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    client.create_liquidity_pool(&admin, &market_id, &100_000);
    assert!(client.check_invariant(market_id));
}

#[test]
fn test_cpmm_prices_after_creation() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let prices = client.get_all_prices(market_id).unwrap();
    assert_eq!(prices.len(), 2);

    // Both outcomes should be priced at 1.0 (collateral / reserve = 1)
    // 100_000 * 10^18 / 100_000 = 10^18
    let expected_price = 1_000_000_000_000_000_000; // 1.0 in 18 decimal
    assert_eq!(prices.get(0).unwrap(), expected_price);
    assert_eq!(prices.get(1).unwrap(), expected_price);
}

#[test]
fn test_add_liquidity() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let lp2 = Address::generate(&env);
    let result = client.add_liquidity(&lp2, &market_id, &50_000);
    assert!(result.is_ok());

    let lp_balance = client.get_lp_balance(market_id, lp2);
    assert!(lp_balance > 0);

    // Pool should still maintain invariant
    assert!(client.check_invariant(market_id));
}

// ---------------------------------------------------------------------------
// CPMM trading tests
// ---------------------------------------------------------------------------

#[test]
fn test_buy_outcome_tokens() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let buyer = Address::generate(&env);
    let result = client.buy_outcome(&buyer, &market_id, &0, &10_000, &0);
    assert!(result.is_ok());

    let swap = result.unwrap();
    assert!(swap.outcome_amount > 0);
    assert_eq!(swap.collateral_amount, 10_000);

    // Buyer should have outcome tokens
    let balance = client.get_outcome_balance(market_id, buyer, 0);
    assert_eq!(balance, swap.outcome_amount);
}

#[test]
fn test_buy_price_impact() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let buyer = Address::generate(&env);

    // First buy at ~1.0
    let result1 = client.buy_outcome(&buyer, &market_id, &0, &10_000, &0).unwrap();
    let price1 = result1.collateral_amount / result1.outcome_amount;

    // Second buy should be at a higher price due to price impact
    let result2 = client.buy_outcome(&buyer, &market_id, &0, &10_000, &0).unwrap();
    let price2 = result2.collateral_amount / result2.outcome_amount;

    // Price should increase after buying (slippage)
    assert!(price2 >= price1);
}

#[test]
fn test_sell_outcome_tokens() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let trader = Address::generate(&env);

    // Buy first
    let buy_result = client
        .buy_outcome(&trader, &market_id, &0, &10_000, &0)
        .unwrap();
    let bought = buy_result.outcome_amount;

    // Now sell half
    let sell_result = client
        .sell_outcome(&trader, &market_id, &0, &bought / 2, &0)
        .unwrap();
    assert!(sell_result.collateral_amount > 0);
}

#[test]
fn test_sell_insufficient_balance() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let trader = Address::generate(&env);
    assert_eq!(
        client.try_sell_outcome(&trader, &market_id, &0, &1000, &0),
        Err(Ok(PredictionError::InsufficientBalance)),
    );
}

#[test]
fn test_pool_invariant_after_trades() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let buyer = Address::generate(&env);

    // Execute several buys
    for i in 0..5 {
        let amount = 1_000 * (i + 1) as i128;
        client.buy_outcome(&buyer, &market_id, &0, &amount, &0);
    }

    // Pool should maintain invariant
    assert!(client.check_invariant(market_id));
    assert!(client.verify_pool_invariant(market_id));
}

#[test]
fn test_buy_market_not_tradable() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    // Close the market
    client.early_close(&admin, &market_id);

    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_buy_outcome(&buyer, &market_id, &0, &1000, &0),
        Err(Ok(PredictionError::MarketNotTradable)),
    );
}

#[test]
fn test_buy_invalid_outcome_index() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_buy_outcome(&buyer, &market_id, &5, &1000, &0), // Index 5 doesn't exist
        Err(Ok(PredictionError::InvalidOutcomeIndex)),
    );
}

// ---------------------------------------------------------------------------
// Order book tests
// ---------------------------------------------------------------------------

#[test]
fn test_place_order() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    let trader = Address::generate(&env);
    let order_id = client.place_order(
        &trader,
        &market_id,
        &0,
        &OrderSide::Buy,
        &500_000_000_000_000_000, // 0.5 in 18 decimal
        &1_000,
    );
    assert!(order_id.is_ok());
    assert_eq!(order_id.unwrap(), 1);

    let order = client.get_order(market_id, 1).unwrap();
    assert_eq!(order.side, OrderSide::Buy);
    assert_eq!(order.amount, 1_000);
    assert_eq!(order.status, OrderStatus::Active);
}

#[test]
fn test_cancel_order() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    let trader = Address::generate(&env);
    let order_id = client.place_order(
        &trader,
        &market_id,
        &0,
        &OrderSide::Buy,
        &500_000_000_000_000_000,
        &1_000,
    ).unwrap();

    let result = client.cancel_order(&trader, &market_id, &order_id);
    assert_eq!(result, symbol_short!("ok"));

    let order = client.get_order(market_id, order_id).unwrap();
    assert_eq!(order.status, OrderStatus::Cancelled);
    assert_eq!(order.remaining, 0);
}

#[test]
fn test_order_book_snapshot() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);

    client.place_order(&buyer, &market_id, &0, &OrderSide::Buy, &500, &100);
    client.place_order(&seller, &market_id, &0, &OrderSide::Sell, &600, &50);

    let snap = client.get_order_book(market_id, 0);
    assert_eq!(snap.bid_count, 1);
    assert_eq!(snap.ask_count, 1);
    assert_eq!(snap.best_bid, 500);
    assert_eq!(snap.best_ask, 600);
    assert_eq!(snap.spread, 100);
}

#[test]
fn test_order_not_found() {
    let (env, _client, _admin) = setup();
    let env2 = Env::default();
    let contract_id = env2.register_contract(None, PredictionMarket);
    let client2 = PredictionMarketClient::new(&env2, &contract_id);
    let admin2 = Address::generate(&env2);
    env2.mock_all_auths();
    client2.initialize(&admin2);

    assert!(client2.get_order(1, 999).is_none());
}

#[test]
fn test_order_book_market_not_tradable() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.early_close(&admin, &market_id);

    let trader = Address::generate(&env);
    assert_eq!(
        client.try_place_order(
            &trader,
            &market_id,
            &0,
            &OrderSide::Buy,
            &500,
            &100,
        ),
        Err(Ok(PredictionError::MarketNotTradable)),
    );
}

// ---------------------------------------------------------------------------
// Oracle resolution tests
// ---------------------------------------------------------------------------

#[test]
fn test_submit_resolution() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    // Create pool and do some trading
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    // Fast forward to resolution window
    let mut ledger_info = env.ledger().get();
    ledger_info.timestamp += 172800; // Past resolution_time
    env.ledger().set(ledger_info);

    // Finalize trading
    client.finalize_trading(&market_id);

    let market = client.get_market(market_id).unwrap();
    assert_eq!(market.status, MarketStatus::PendingResolution);

    // Submit resolution
    let result = client.submit_resolution(
        &market_id,
        &symbol_short!("Chainlink"),
        &0, // "Yes" wins
    );
    assert!(result.is_ok());

    let market = client.get_market(market_id).unwrap();
    assert_eq!(market.status, MarketStatus::Resolved);
    assert_eq!(market.resolved_outcome, Some(0));
}

#[test]
fn test_resolution_before_window_closed() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    // Try to finalize trading before trading_end_time
    assert_eq!(
        client.try_finalize_trading(&market_id),
        Err(Ok(PredictionError::ResolutionWindowNotClosed)),
    );
}

#[test]
fn test_resolution_wrong_oracle() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    // Fast forward past resolution time
    let mut ledger_info = env.ledger().get();
    ledger_info.timestamp += 172800;
    env.ledger().set(ledger_info);

    client.finalize_trading(&market_id);

    // Wrong oracle
    assert_eq!(
        client.try_submit_resolution(
            &market_id,
            &symbol_short!("WrongOracle"),
            &0,
        ),
        Err(Ok(PredictionError::Unauthorized)),
    );
}

#[test]
fn test_get_resolution_data() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let mut ledger_info = env.ledger().get();
    ledger_info.timestamp += 172800;
    env.ledger().set(ledger_info);

    client.finalize_trading(&market_id);
    client.submit_resolution(&market_id, &symbol_short!("Chainlink"), &0);

    let resolution = client.get_resolution(market_id).unwrap();
    assert_eq!(resolution.resolved_outcome, 0);
    assert!(resolution.confirmed);
}

// ---------------------------------------------------------------------------
// Dispute mechanism tests
// ---------------------------------------------------------------------------

#[test]
fn test_file_dispute() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let mut ledger_info = env.ledger().get();
    ledger_info.timestamp += 172800;
    env.ledger().set(ledger_info);

    client.finalize_trading(&market_id);
    client.submit_resolution(&market_id, &symbol_short!("Chainlink"), &0);

    let disputer = Address::generate(&env);
    let result = client.file_dispute(
        &disputer,
        &market_id,
        &1, // Claims "No" should win
        &symbol_short!("evidence"),
        &1_000, // Bond
    );
    assert!(result.is_ok());

    let market = client.get_market(market_id).unwrap();
    assert_eq!(market.status, MarketStatus::Disputed);

    let dispute = client.get_dispute(market_id).unwrap();
    assert_eq!(dispute.claimed_outcome, 1);
    assert_eq!(dispute.status, DisputeStatus::Pending);
}

#[test]
fn test_resolve_dispute_accepted() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let mut ledger_info = env.ledger().get();
    ledger_info.timestamp += 172800;
    env.ledger().set(ledger_info);

    client.finalize_trading(&market_id);
    client.submit_resolution(&market_id, &symbol_short!("Chainlink"), &0);

    let disputer = Address::generate(&env);
    client.file_dispute(
        &disputer,
        &market_id,
        &1,
        &symbol_short!("proof"),
        &1_000,
    );

    // Admin accepts the dispute
    let result = client.resolve_dispute(&admin, &market_id, &true);
    assert!(result.is_ok());

    let market = client.get_market(market_id).unwrap();
    assert_eq!(market.resolved_outcome, Some(1)); // Overturned to "No"
}

#[test]
fn test_resolve_dispute_rejected() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let mut ledger_info = env.ledger().get();
    ledger_info.timestamp += 172800;
    env.ledger().set(ledger_info);

    client.finalize_trading(&market_id);
    client.submit_resolution(&market_id, &symbol_short!("Chainlink"), &0);

    let disputer = Address::generate(&env);
    client.file_dispute(
        &disputer,
        &market_id,
        &1,
        &symbol_short!("weak_evidence"),
        &1_000,
    );

    // Admin rejects the dispute
    let result = client.resolve_dispute(&admin, &market_id, &false);
    assert!(result.is_ok());

    let market = client.get_market(market_id).unwrap();
    assert_eq!(market.resolved_outcome, Some(0)); // Original stands
}

// ---------------------------------------------------------------------------
// Settlement tests
// ---------------------------------------------------------------------------

#[test]
fn test_redeem_winning_tokens() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let buyer = Address::generate(&env);
    let buy_result = client
        .buy_outcome(&buyer, &market_id, &0, &10_000, &0)
        .unwrap();
    let bought = buy_result.outcome_amount;

    // Fast forward to resolution
    let mut ledger_info = env.ledger().get();
    ledger_info.timestamp += 172800;
    env.ledger().set(ledger_info);

    client.finalize_trading(&market_id);
    client.submit_resolution(&market_id, &symbol_short!("Chainlink"), &0); // "Yes" wins

    // Redeem
    let payout = client.redeem_tokens(&buyer, &market_id, &bought);
    assert!(payout.is_ok());
    assert_eq!(payout.unwrap(), bought); // 1:1 redemption
}

#[test]
fn test_redeem_losing_tokens_fails() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let buyer = Address::generate(&env);
    let buy_result = client
        .buy_outcome(&buyer, &market_id, &1, &10_000, &0) // Buy "No"
        .unwrap();
    let bought = buy_result.outcome_amount;

    // Resolve with "Yes" winning
    let mut ledger_info = env.ledger().get();
    ledger_info.timestamp += 172800;
    env.ledger().set(ledger_info);

    client.finalize_trading(&market_id);
    client.submit_resolution(&market_id, &symbol_short!("Chainlink"), &0);

    // Trying to redeem "No" tokens should give 0 or fail
    let payout = client.redeem_tokens(&buyer, &market_id, &bought);
    // Should get 0 payout since "No" lost
    assert_eq!(payout.unwrap(), 0);
}

#[test]
fn test_redeem_before_resolution() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let buyer = Address::generate(&env);
    let buy_result = client
        .buy_outcome(&buyer, &market_id, &0, &10_000, &0)
        .unwrap();

    // Try to redeem before resolution
    assert_eq!(
        client.try_redeem_tokens(&buyer, &market_id, &buy_result.outcome_amount),
        Err(Ok(PredictionError::MarketNotResolved)),
    );
}

// ---------------------------------------------------------------------------
// LP fee distribution tests
// ---------------------------------------------------------------------------

#[test]
fn test_distribute_fees() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    // Generate some fees through trading
    let buyer = Address::generate(&env);
    client.buy_outcome(&buyer, &market_id, &0, &10_000, &0);
    client.sell_outcome(&buyer, &market_id, &0, &5_000, &0);

    let pool_before = client.get_liquidity_pool(market_id).unwrap();
    let fees = pool_before.fees_accumulated;
    assert!(fees > 0);

    // Distribute fees
    let distributed = client.distribute_fees(&admin, &market_id);
    assert_eq!(distributed.unwrap(), fees);

    let pool_after = client.get_liquidity_pool(market_id).unwrap();
    assert_eq!(pool_after.fees_accumulated, 0);
}

// ---------------------------------------------------------------------------
// Market closing tests
// ---------------------------------------------------------------------------

#[test]
fn test_early_close() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    let result = client.early_close(&admin, &market_id);
    assert_eq!(result, symbol_short!("ok"));

    let market = client.get_market(market_id).unwrap();
    assert_eq!(market.status, MarketStatus::Closed);
}

#[test]
fn test_early_close_unauthorized() {
    let (env, client, _admin) = setup();
    let market_id = create_binary_market(&env, &client, &_admin);

    let random_user = Address::generate(&env);
    assert_eq!(
        client.try_early_close(&random_user, &market_id),
        Err(Ok(PredictionError::Unauthorized)),
    );
}

#[test]
fn test_cancel_market() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    let result = client.cancel_market(&admin, &market_id);
    assert_eq!(result, symbol_short!("ok"));

    let market = client.get_market(market_id).unwrap();
    assert_eq!(market.status, MarketStatus::Cancelled);
}

#[test]
fn test_cancel_market_with_trades_fails() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    // Do a trade
    let buyer = Address::generate(&env);
    client.buy_outcome(&buyer, &market_id, &0, &10_000, &0);

    assert_eq!(
        client.try_cancel_market(&admin, &market_id),
        Err(Ok(PredictionError::ActivePositionsExist)),
    );
}

// ---------------------------------------------------------------------------
// Multi-outcome market tests
// ---------------------------------------------------------------------------

#[test]
fn test_multi_outcome_market() {
    let (env, client, admin) = setup();
    let market_id = create_multi_outcome_market(&env, &client, &admin);

    client.create_liquidity_pool(&admin, &market_id, &100_000);

    // Buy from each outcome
    let buyer = Address::generate(&env);
    let r0 = client.buy_outcome(&buyer, &market_id, &0, &10_000, &0).unwrap();
    let r1 = client.buy_outcome(&buyer, &market_id, &1, &10_000, &0).unwrap();
    let r2 = client.buy_outcome(&buyer, &market_id, &2, &10_000, &0).unwrap();

    // All should return some tokens
    assert!(r0.outcome_amount > 0);
    assert!(r1.outcome_amount > 0);
    assert!(r2.outcome_amount > 0);

    // Pool invariant should hold
    assert!(client.check_invariant(market_id));

    // Prices should shift based on demand
    let prices = client.get_all_prices(market_id).unwrap();
    assert_eq!(prices.len(), 3);

    // Outcome 0 should be more expensive (more demand)
    let price0 = client.get_outcome_price(market_id, 0).unwrap();
    let initial_price = 1_000_000_000_000_000_000; // 1.0
    assert!(price0 > initial_price);
}

// ---------------------------------------------------------------------------
// Trading volume tests
// ---------------------------------------------------------------------------

#[test]
fn test_trading_volume_tracking() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let buyer = Address::generate(&env);
    client.buy_outcome(&buyer, &market_id, &0, &5_000, &0);
    client.buy_outcome(&buyer, &market_id, &0, &3_000, &0);

    let volume = client.get_trading_volume(market_id);
    assert_eq!(volume, 8_000);
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn test_get_nonexistent_market() {
    let (env, _client, _admin) = setup();
    let env2 = Env::default();
    let contract_id = env2.register_contract(None, PredictionMarket);
    let client2 = PredictionMarketClient::new(&env2, &contract_id);
    let admin2 = Address::generate(&env2);
    env2.mock_all_auths();
    client2.initialize(&admin2);

    assert!(client2.get_market(999).is_none());
}

#[test]
fn test_pool_for_nonexistent_market() {
    let (env, client, _admin) = setup();
    assert_eq!(
        client.try_get_liquidity_pool(&999),
        Err(Ok(PredictionError::MarketNotFound)),
    );
}

#[test]
fn test_zero_amount_trade() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &100_000);

    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_buy_outcome(&buyer, &market_id, &0, &0, &0),
        Err(Ok(PredictionError::InvalidOrderAmount)),
    );
}

#[test]
fn test_admin_only_functions() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);

    let random_user = Address::generate(&env);

    // distribute_fees requires admin
    assert_eq!(
        client.try_distribute_fees(&random_user, &market_id),
        Err(Ok(PredictionError::Unauthorized)),
    );

    // resolve_dispute requires admin
    assert_eq!(
        client.try_resolve_dispute(&random_user, &market_id, &true),
        Err(Ok(PredictionError::Unauthorized)),
    );
}

// ---------------------------------------------------------------------------
// Fuzzing-style tests for CPMM invariant
// ---------------------------------------------------------------------------

#[test]
fn test_cpmm_invariant_maintained_across_many_trades() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &1_000_000);

    // Simulate many trades with varying sizes
    let amounts = [
        100, 500, 1000, 2000, 500, 3000, 100, 2500, 750, 4000,
        200, 1500, 800, 3500, 100, 2000, 600, 1200, 900, 300,
    ];

    for (i, &amount) in amounts.iter().enumerate() {
        let buyer = Address::generate(&env);
        let outcome = if i % 2 == 0 { 0u32 } else { 1u32 };
        let result = client.buy_outcome(&buyer, &market_id, &outcome, &amount, &0);
        if result.is_ok() {
            // Verify invariant after each successful trade
            assert!(
                client.check_invariant(market_id),
                "Invariant failed after trade {} (amount={}, outcome={})",
                i,
                amount,
                outcome
            );
        }
    }
}

#[test]
fn test_cpmm_invariant_maintained_across_buys_and_sells() {
    let (env, client, admin) = setup();
    let market_id = create_binary_market(&env, &client, &admin);
    client.create_liquidity_pool(&admin, &market_id, &1_000_000);

    let buyer = Address::generate(&env);

    // Buy some tokens first
    let buy_result = client
        .buy_outcome(&buyer, &market_id, &0, &100_000, &0)
        .unwrap();
    let bought = buy_result.outcome_amount;

    // Sell various amounts
    let sell_amounts = [bought / 10, bought / 5, bought / 3, bought / 2];
    for &amount in &sell_amounts {
        if amount > 0 {
            let _ = client.sell_outcome(&buyer, &market_id, &0, &amount, &0);
            assert!(
                client.check_invariant(market_id),
                "Invariant failed after sell of {}",
                amount
            );
        }
    }
}
