use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::{Strategy, StrategyContext};
use crate::types::{OrderRequest, OrderType, Side, StrategyActions};

/// Number of price levels on each side
const NUM_LEVELS: usize = 5;
/// Base spread in basis points
const BASE_SPREAD_BPS: Decimal = dec!(10);
/// Order size per level
const ORDER_SIZE: Decimal = dec!(2);

/// Liquidity Provider - a "designated market maker" that ALWAYS maintains both sides.
///
/// Unlike other strategies, this one:
/// - Does NOT check inventory (assumes infinite backing)
/// - Always places symmetric bid/ask orders
/// - Uses wider spreads for stability
/// - Places orders at multiple price levels
///
/// This ensures the orderbook never goes empty on either side.
pub struct LiquidityProvider {
    /// Reference price that slowly follows the market
    reference_price: Option<Decimal>,
}

impl LiquidityProvider {
    pub fn new() -> Self {
        Self {
            reference_price: None,
        }
    }

    fn round_price(price: Decimal) -> Decimal {
        price.round_dp(2)
    }

    fn round_quantity(qty: Decimal) -> Decimal {
        qty.round_dp(2).max(dec!(0.01))
    }
}

impl Strategy for LiquidityProvider {
    fn name(&self) -> &'static str {
        "LiquidityProvider"
    }

    fn interval_ms(&self) -> u64 {
        500 // Slower updates - stable presence
    }

    fn generate_actions(&mut self, ctx: &StrategyContext) -> StrategyActions {
        let mut actions = StrategyActions::default();

        // Cancel all existing orders first
        for order_id in ctx.open_orders.keys() {
            actions.orders_to_cancel.push(*order_id);
        }

        // Get or initialize reference price
        let market_mid = ctx.market.mid_price();
        let mid = match (market_mid, self.reference_price) {
            (Some(m), Some(r)) => {
                // Slowly follow the market (EMA)
                let new_ref = r * dec!(0.95) + m * dec!(0.05);
                self.reference_price = Some(new_ref);
                new_ref
            }
            (Some(m), None) => {
                self.reference_price = Some(m);
                m
            }
            (None, Some(r)) => r,
            (None, None) => {
                // No market data yet, use default
                self.reference_price = Some(dec!(10000));
                dec!(10000)
            }
        };

        // Calculate spread
        let half_spread = BASE_SPREAD_BPS / dec!(10000) * mid;

        // Generate bid orders at multiple levels
        for i in 0..NUM_LEVELS {
            let level_offset = half_spread * Decimal::from(i + 1);
            let price = Self::round_price(mid - level_offset);
            let quantity = Self::round_quantity(ORDER_SIZE);

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

        // Generate ask orders at multiple levels (symmetric)
        for i in 0..NUM_LEVELS {
            let level_offset = half_spread * Decimal::from(i + 1);
            let price = Self::round_price(mid + level_offset);
            let quantity = Self::round_quantity(ORDER_SIZE);

            actions.orders_to_place.push(OrderRequest {
                symbol: ctx.symbol.to_string(),
                side: Side::Ask,
                order_type: OrderType::Limit,
                price: Some(price),
                quantity,
            });
        }

        actions
    }
}

impl Default for LiquidityProvider {
    fn default() -> Self {
        Self::new()
    }
}
