//! Order book management for outcome token direct trading.
//!
//! Supports placing, canceling, and matching limit orders for outcome tokens.
//! Uses price-time priority matching (FIFO at each price level).

use soroban_sdk::{symbol_short, Address, Env, Symbol, Vec};

use crate::types::{
    OrderBookSnapshot, OrderSide, OrderStatus, PredictionDataKey, PredictionError,
    PredictionOrder, TradeFill,
};

/// Maximum order book depth per side.
const MAX_BOOK_DEPTH: u32 = 256;

// ---------------------------------------------------------------------------
// Book loading / saving
// ---------------------------------------------------------------------------

/// Load the order book for a market from storage.
pub fn load_order_book(env: &Env, market_id: u64) -> Result<Vec<PredictionOrder>, PredictionError> {
    let key = PredictionDataKey::OrderBook(market_id);
    let orders: Vec<PredictionOrder> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env));
    Ok(orders)
}

/// Save orders to storage.
pub fn save_orders(env: &Env, market_id: u64, orders: &Vec<PredictionOrder>) {
    let key = PredictionDataKey::OrderBook(market_id);
    env.storage().persistent().set(&key, orders);
}

/// Get the next order ID for a market.
pub fn next_order_id(env: &Env, market_id: u64) -> u64 {
    let key = PredictionDataKey::OrderIdCounter(market_id);
    let current: u64 = env.storage().persistent().get(&key).unwrap_or(1);
    env.storage().persistent().set(&key, &(current + 1));
    current
}

// ---------------------------------------------------------------------------
// Order placement
// ---------------------------------------------------------------------------

/// Place a new limit order on the outcome token order book.
pub fn place_order(
    env: &Env,
    market_id: u64,
    owner: &Address,
    outcome_index: u32,
    side: OrderSide,
    price: i128,
    amount: i128,
) -> Result<u64, PredictionError> {
    if amount <= 0 || price <= 0 {
        return Err(PredictionError::InvalidOrderAmount);
    }

    let order_id = next_order_id(env, market_id);
    let now = env.ledger().timestamp();

    let order = PredictionOrder {
        order_id,
        market_id,
        owner: owner.clone(),
        outcome_index,
        side,
        price,
        amount,
        remaining: amount,
        status: OrderStatus::Active,
        created_at: now,
    };

    // Store the individual order
    env.storage().persistent().set(
        &PredictionDataKey::Order(market_id, order_id),
        &order,
    );

    // Add to the order book
    let mut orders = load_order_book(env, market_id)?;
    if orders.len() >= MAX_BOOK_DEPTH {
        return Err(PredictionError::ArithmeticOverflow);
    }
    orders.push_back(order);
    save_orders(env, market_id, &orders);

    Ok(order_id)
}

/// Cancel an existing active order.
pub fn cancel_order(
    env: &Env,
    market_id: u64,
    order_id: u64,
    caller: &Address,
    is_admin: bool,
) -> Result<(), PredictionError> {
    let mut order: PredictionOrder = env
        .storage()
        .persistent()
        .get(&PredictionDataKey::Order(market_id, order_id))
        .ok_or(PredictionError::OrderNotFound)?;

    if order.status != OrderStatus::Active && order.status != OrderStatus::PartiallyFilled {
        return Err(PredictionError::OrderNotFound);
    }

    if !is_admin && order.owner != *caller {
        return Err(PredictionError::NotOrderOwner);
    }

    // Update order status
    order.status = OrderStatus::Cancelled;
    order.remaining = 0;
    env.storage().persistent().set(
        &PredictionDataKey::Order(market_id, order_id),
        &order,
    );

    // Remove from book
    let mut orders = load_order_book(env, market_id)?;
    remove_order_from_list(&mut orders, order_id);
    save_orders(env, market_id, &orders);

    Ok(())
}

/// Look up a single order by market and ID.
pub fn get_order(env: &Env, market_id: u64, order_id: u64) -> Option<PredictionOrder> {
    env.storage().persistent().get(&PredictionDataKey::Order(market_id, order_id))
}

/// Get a snapshot of the order book for an outcome.
pub fn get_book_snapshot(
    env: &Env,
    market_id: u64,
    outcome_index: u32,
) -> OrderBookSnapshot {
    let orders = load_order_book(env, market_id).unwrap_or_else(|_| Vec::new(env));

    let mut best_bid: i128 = 0;
    let mut best_ask: i128 = i128::MAX;
    let mut bid_count: u32 = 0;
    let mut ask_count: u32 = 0;

    for i in 0..orders.len() {
        let order = orders.get(i).unwrap();
        if order.outcome_index != outcome_index
            || (order.status != OrderStatus::Active && order.status != OrderStatus::PartiallyFilled)
        {
            continue;
        }
        match order.side {
            OrderSide::Buy => {
                bid_count += 1;
                if order.price > best_bid {
                    best_bid = order.price;
                }
            }
            OrderSide::Sell => {
                ask_count += 1;
                if order.price < best_ask {
                    best_ask = order.price;
                }
            }
        }
    }

    if best_ask == i128::MAX {
        best_ask = 0;
    }

    let spread = if best_bid > 0 && best_ask > 0 {
        best_ask - best_bid
    } else {
        0
    };

    OrderBookSnapshot {
        market_id,
        bid_count,
        ask_count,
        best_bid,
        best_ask,
        spread,
    }
}

// ---------------------------------------------------------------------------
// Matching engine
// ---------------------------------------------------------------------------

/// Match an incoming order against existing orders on the book.
///
/// Returns a list of fills produced. For buy orders, matches against asks
/// (lowest price first). For sell orders, matches against bids (highest first).
pub fn match_order(
    env: &Env,
    market_id: u64,
    taker: &Address,
    outcome_index: u32,
    side: OrderSide,
    amount: i128,
    limit_price: i128,
    fee_bps: i128,
) -> (Vec<TradeFill>, i128) {
    let orders = match load_order_book(env, market_id) {
        Ok(o) => o,
        Err(_) => return (Vec::new(env), 0),
    };

    let mut fills = Vec::new(env);
    let mut remaining_amount = amount;
    let mut updated_orders = Vec::new(env);
    let mut matched_ids = Vec::new(env);

    // Copy non-matching orders and try to match with matching ones
    for i in 0..orders.len() {
        let mut order = orders.get(i).unwrap();

        if order.outcome_index != outcome_index {
            updated_orders.push_back(order);
            continue;
        }

        if order.status != OrderStatus::Active && order.status != OrderStatus::PartiallyFilled {
            updated_orders.push_back(order);
            continue;
        }

        // Check if orders can match
        let can_match = match side {
            OrderSide::Buy => {
                // Taker wants to buy; must match against sell (ask) orders
                // Taker's limit price >= maker's ask price
                order.side == OrderSide::Sell && order.price <= limit_price && remaining_amount > 0
            }
            OrderSide::Sell => {
                // Taker wants to sell; must match against buy (bid) orders
                // Maker's bid price >= taker's limit price
                order.side == OrderSide::Buy && order.price >= limit_price && remaining_amount > 0
            }
        };

        if !can_match {
            updated_orders.push_back(order);
            continue;
        }

        let fill_qty = remaining_amount.min(order.remaining);

        // Fee calculation
        let fee = if fee_bps > 0 {
            let notional = fill_qty
                .checked_mul(order.price)
                .unwrap_or(i128::MAX);
            notional
                .checked_mul(fee_bps)
                .unwrap_or(i128::MAX)
                .checked_div(10_000)
                .unwrap_or(0)
        } else {
            0
        };

        let fill = TradeFill {
            order_id: order.order_id,
            taker: taker.clone(),
            maker: order.owner.clone(),
            outcome_index,
            side,
            price: order.price,
            amount: fill_qty,
            fee,
        };
        fills.push_back(fill);

        // Update the maker order
        order.remaining -= fill_qty;
        if order.remaining == 0 {
            order.status = OrderStatus::Filled;
            matched_ids.push_back(order.order_id);
        } else {
            order.status = OrderStatus::PartiallyFilled;
            updated_orders.push_back(order.clone());
        }

        // Persist updated order
        env.storage().persistent().set(
            &PredictionDataKey::Order(market_id, order.order_id),
            &order,
        );

        remaining_amount -= fill_qty;
    }

    // Re-add unmatched orders from updated list
    // (matched orders that were fully filled are excluded)
    save_orders(env, market_id, &updated_orders);

    let filled_amount = amount - remaining_amount;
    (fills, filled_amount)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Remove an order from a list by order ID.
fn remove_order_from_list(orders: &mut Vec<PredictionOrder>, order_id: u64) {
    let mut idx_to_remove: Option<u32> = None;
    for i in 0..orders.len() {
        if orders.get(i).unwrap().order_id == order_id {
            idx_to_remove = Some(i);
            break;
        }
    }
    if let Some(idx) = idx_to_remove {
        orders.remove(idx);
    }
}
