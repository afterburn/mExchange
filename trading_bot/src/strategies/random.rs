use rand::Rng;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::{Strategy, StrategyContext};
use crate::types::{OrderRequest, OrderType, Side, StrategyActions};

/// Maximum position (smaller = less inventory depletion)
const MAX_POSITION: Decimal = dec!(25);
/// Maximum open orders for this strategy
const MAX_OPEN_ORDERS: usize = 3;

/// Noise trader - random activity that adds volume and unpredictability
/// Mixes market orders (immediate execution) with limit orders (resting)
pub struct Random;

impl Random {
    pub fn new() -> Self {
        Self
    }

    fn round_price(price: Decimal) -> Decimal {
        price.round_dp(2)
    }

    fn round_quantity(qty: Decimal) -> Decimal {
        qty.round_dp(2).max(dec!(0.01))
    }
}

impl Strategy for Random {
    fn name(&self) -> &'static str {
        "Random"
    }

    fn interval_ms(&self) -> u64 {
        75 // Fast - ~13x per second
    }

    fn generate_actions(&mut self, ctx: &StrategyContext) -> StrategyActions {
        let mut actions = StrategyActions::default();
        let mut rng = rand::thread_rng();

        let mid = match ctx.market.mid_price() {
            Some(p) => p,
            None => return actions,
        };

        // Cancel old orders if we have too many
        if ctx.open_orders.len() > MAX_OPEN_ORDERS {
            for order_id in ctx.open_orders.keys() {
                actions.orders_to_cancel.push(*order_id);
            }
        }

        // 60% chance to trade each tick
        if rng.gen_range(0..100) > 60 {
            return actions;
        }

        // Check position limits
        let can_buy = ctx.inventory < MAX_POSITION;
        let can_sell = ctx.inventory > -MAX_POSITION;

        // Completely random direction
        let side = if rng.gen_bool(0.5) {
            if can_buy { Side::Bid } else if can_sell { Side::Ask } else { return actions; }
        } else {
            if can_sell { Side::Ask } else if can_buy { Side::Bid } else { return actions; }
        };

        // Small retail-sized orders (reduced for inventory preservation)
        let quantity = Self::round_quantity(Decimal::from(rng.gen_range(1u32..3u32)));

        // 50% market orders, 50% limit orders
        let is_market = rng.gen_range(0..100) < 50;

        if is_market {
            actions.orders_to_place.push(OrderRequest {
                symbol: ctx.symbol.to_string(),
                side,
                order_type: OrderType::Market,
                price: None,
                quantity,
            });
        } else {
            // Limit orders close to market
            let offset_pct = Decimal::from(rng.gen_range(1i32..30i32)) / dec!(10000);
            let price = match side {
                Side::Bid => Self::round_price(mid * (dec!(1) - offset_pct)),
                Side::Ask => Self::round_price(mid * (dec!(1) + offset_pct)),
            };

            if price > Decimal::ZERO {
                actions.orders_to_place.push(OrderRequest {
                    symbol: ctx.symbol.to_string(),
                    side,
                    order_type: OrderType::Limit,
                    price: Some(price),
                    quantity,
                });
            }
        }

        actions
    }
}

impl Default for Random {
    fn default() -> Self {
        Self::new()
    }
}
