use rand::Rng;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::{Strategy, StrategyContext};
use crate::types::{OrderRequest, OrderType, Side, StrategyActions};

/// Maximum position
const MAX_POSITION: Decimal = dec!(500);

/// Aggressive trend-following trader that creates directional pressure
/// This is the main driver of price movement - uses market orders to push price
pub struct Aggressive {
    /// Current bias direction (changes randomly to create trends)
    bias: Decimal,
    /// How long to maintain current bias (in ticks)
    bias_duration: u32,
}

impl Aggressive {
    pub fn new() -> Self {
        Self {
            bias: Decimal::ZERO,
            bias_duration: 0,
        }
    }

    fn round_quantity(qty: Decimal) -> Decimal {
        qty.round_dp(2).max(dec!(0.01))
    }
}

impl Strategy for Aggressive {
    fn name(&self) -> &'static str {
        "Aggressive"
    }

    fn interval_ms(&self) -> u64 {
        50 // Very fast - 20x per second
    }

    fn generate_actions(&mut self, ctx: &StrategyContext) -> StrategyActions {
        let mut actions = StrategyActions::default();
        let mut rng = rand::thread_rng();

        let _mid = match ctx.market.mid_price() {
            Some(p) => p,
            None => return actions,
        };

        // Update bias - create trends that last 50-200 ticks (2.5-10 seconds)
        if self.bias_duration == 0 {
            // Start a new trend
            let trend_strength = rng.gen_range(30..100) as i32;
            self.bias = if rng.gen_bool(0.5) {
                Decimal::from(trend_strength)
            } else {
                Decimal::from(-trend_strength)
            };
            self.bias_duration = rng.gen_range(50..200);
        } else {
            self.bias_duration -= 1;
        }

        // 70% chance to act each tick during a trend
        if rng.gen_range(0..100) > 70 {
            return actions;
        }

        // Determine direction based on bias
        let is_bullish = self.bias > Decimal::ZERO;

        // Check position limits
        let can_buy = ctx.inventory < MAX_POSITION;
        let can_sell = ctx.inventory > -MAX_POSITION;

        let side = if is_bullish && can_buy {
            Side::Bid
        } else if !is_bullish && can_sell {
            Side::Ask
        } else {
            return actions;
        };

        // Aggressive market orders to move price
        // Size varies to create natural-looking volume
        let base_size = rng.gen_range(1..15) as u32;
        let quantity = Self::round_quantity(Decimal::from(base_size));

        // 100% market orders - this strategy is pure aggression
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

impl Default for Aggressive {
    fn default() -> Self {
        Self::new()
    }
}
