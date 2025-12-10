use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::{Strategy, StrategyContext};
use crate::types::{OrderRequest, OrderType, Side, StrategyActions};

/// Number of price levels on each side (bid/ask)
const NUM_LEVELS: usize = 3;
/// Base spread in basis points (tighter = more fills)
const BASE_SPREAD_BPS: Decimal = dec!(5);
/// Maximum inventory before stopping quotes on one side
const MAX_INVENTORY: Decimal = dec!(200);
/// Base order size (smaller for more activity)
const BASE_ORDER_SIZE: Decimal = dec!(5);

/// Market maker - provides liquidity but follows the market
pub struct MarketMaker;

impl MarketMaker {
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

impl Strategy for MarketMaker {
    fn name(&self) -> &'static str {
        "MarketMaker"
    }

    fn interval_ms(&self) -> u64 {
        100 // Very fast - 10x per second
    }

    fn generate_actions(&mut self, ctx: &StrategyContext) -> StrategyActions {
        let mut actions = StrategyActions::default();

        // Cancel all existing orders
        for order_id in ctx.open_orders.keys() {
            actions.orders_to_cancel.push(*order_id);
        }

        let mid = match ctx.market.mid_price() {
            Some(p) => p,
            None => return actions,
        };

        // Tighter spread for more activity
        let half_spread = BASE_SPREAD_BPS / dec!(10000) * mid;

        // Strong inventory skew - aggressively reduce position
        let inventory_skew = (ctx.inventory / MAX_INVENTORY) * half_spread * dec!(2);
        let adjusted_mid = mid - inventory_skew;

        let can_quote_bid = ctx.inventory < MAX_INVENTORY;
        let can_quote_ask = ctx.inventory > -MAX_INVENTORY;

        // Generate bid orders
        if can_quote_bid {
            for i in 0..NUM_LEVELS {
                let level_offset = half_spread * Decimal::from(i + 1);
                let price = Self::round_price(adjusted_mid - level_offset);
                let quantity = Self::round_quantity(BASE_ORDER_SIZE);

                if price > Decimal::ZERO {
                    actions.orders_to_place.push(OrderRequest {
                        symbol: ctx.symbol.to_string(),
                        side: Side::Bid,
                        order_type: OrderType::Limit,
                        price: Some(price),
                        quantity,
                    });
                }
            }
        }

        // Generate ask orders
        if can_quote_ask {
            for i in 0..NUM_LEVELS {
                let level_offset = half_spread * Decimal::from(i + 1);
                let price = Self::round_price(adjusted_mid + level_offset);
                let quantity = Self::round_quantity(BASE_ORDER_SIZE);

                actions.orders_to_place.push(OrderRequest {
                    symbol: ctx.symbol.to_string(),
                    side: Side::Ask,
                    order_type: OrderType::Limit,
                    price: Some(price),
                    quantity,
                });
            }
        }

        actions
    }
}

impl Default for MarketMaker {
    fn default() -> Self {
        Self::new()
    }
}
