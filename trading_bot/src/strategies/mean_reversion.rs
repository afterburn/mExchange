use rand::Rng;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::{Strategy, StrategyContext};
use crate::indicators::z_score;
use crate::types::{OrderRequest, OrderType, Side, StrategyActions};

/// Maximum position (smaller = less inventory depletion)
const MAX_POSITION: Decimal = dec!(25);

/// Mean reversion - only trades on extreme moves, provides counter-trend pressure
/// Slower and less aggressive to allow trends to develop
pub struct MeanReversion;

impl MeanReversion {
    pub fn new() -> Self {
        Self
    }

    fn round_quantity(qty: Decimal) -> Decimal {
        qty.round_dp(2).max(dec!(0.01))
    }
}

impl Strategy for MeanReversion {
    fn name(&self) -> &'static str {
        "MeanReversion"
    }

    fn interval_ms(&self) -> u64 {
        500 // Slower - 2x per second (patient strategy)
    }

    fn generate_actions(&mut self, ctx: &StrategyContext) -> StrategyActions {
        let mut actions = StrategyActions::default();
        let mut rng = rand::thread_rng();

        if ctx.market.price_history.len() < 10 {
            return actions;
        }

        let _mid = match ctx.market.mid_price() {
            Some(p) => p,
            None => return actions,
        };

        let z = z_score(&ctx.market.price_history).unwrap_or(Decimal::ZERO);
        let z_abs = z.abs();

        // Only trade on EXTREME moves (z > 2.5 std devs)
        // This allows trends to develop before mean reversion kicks in
        if z_abs < dec!(2.5) {
            return actions;
        }

        // 50% chance to act even on extreme moves
        if rng.gen_range(0..100) > 50 {
            return actions;
        }

        let can_buy = ctx.inventory < MAX_POSITION;
        let can_sell = ctx.inventory > -MAX_POSITION;

        // Fade the extreme move
        let is_overbought = z > dec!(2.5);
        let side = if is_overbought && can_sell {
            Side::Ask
        } else if !is_overbought && can_buy {
            Side::Bid
        } else {
            return actions;
        };

        // Smaller size to preserve inventory
        let quantity = Self::round_quantity(Decimal::from(rng.gen_range(1u32..5u32)));

        // Use market orders to fade extremes
        actions.orders_to_place.push(OrderRequest {
            symbol: ctx.symbol.to_string(),
            side,
            order_type: OrderType::Market,
            price: None,
            quantity,
        });

        actions
    }
}

impl Default for MeanReversion {
    fn default() -> Self {
        Self::new()
    }
}
