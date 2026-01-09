use rand::Rng;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::{Strategy, StrategyContext};
use crate::types::{OrderRequest, OrderType, Side, StrategyActions};

/// Enhanced random walk strategy with realistic market dynamics:
/// - Momentum: trends tend to continue
/// - Volatility clustering: volatile periods cluster together
/// - Mean reversion: price gravitates toward a slowly-drifting fair value
pub struct RandomWalk {
    /// Current walk price (moves with momentum and mean reversion)
    walk_price: Option<Decimal>,
    /// Fair value that drifts slowly (price mean-reverts toward this)
    fair_value: Option<Decimal>,
    /// Base spread between bid and ask (in absolute terms)
    spread: Decimal,
    /// Last direction: 1 = up, -1 = down
    last_direction: i32,
    /// Recent volatility (exponential moving average of step sizes)
    volatility: Decimal,
}

impl RandomWalk {
    pub fn new() -> Self {
        Self {
            walk_price: None,
            fair_value: None,
            spread: dec!(10),
            last_direction: 1,
            volatility: dec!(3), // Start with moderate volatility
        }
    }

    fn round_price(price: Decimal) -> Decimal {
        price.round_dp(2)
    }

    fn round_quantity(qty: Decimal) -> Decimal {
        qty.round_dp(2).max(dec!(0.01))
    }

    /// Momentum factor: probability of continuing in same direction (0.5 = no momentum, 0.7 = strong momentum)
    const MOMENTUM: f64 = 0.65;
    /// Mean reversion strength: how strongly price pulls toward fair value (0 = none, 0.1 = moderate)
    const MEAN_REVERSION: f64 = 0.05;
    /// Volatility decay: how quickly volatility returns to baseline (lower = slower decay)
    const VOL_DECAY: f64 = 0.9;
    /// Fair value drift: how much fair value can move per tick
    const FAIR_VALUE_DRIFT: f64 = 0.5;
}

impl Strategy for RandomWalk {
    fn name(&self) -> &'static str {
        "RandomWalk"
    }

    fn interval_ms(&self) -> u64 {
        200 // 5x per second - moderate pace
    }

    fn generate_actions(&mut self, ctx: &StrategyContext) -> StrategyActions {
        let mut actions = StrategyActions::default();
        let mut rng = rand::thread_rng();

        // Initialize prices from market or use defaults
        let initial_price = ctx.market.mid_price().unwrap_or(dec!(10000));
        if self.walk_price.is_none() {
            self.walk_price = Some(initial_price);
        }
        if self.fair_value.is_none() {
            self.fair_value = Some(initial_price);
        }

        let current_price = self.walk_price.unwrap();
        let fair_value = self.fair_value.unwrap();

        // --- Momentum: bias toward continuing in same direction ---
        let continue_direction = rng.gen_bool(Self::MOMENTUM);
        let base_direction = if continue_direction {
            self.last_direction
        } else {
            -self.last_direction
        };

        // --- Mean reversion: pull toward fair value ---
        let price_deviation = current_price - fair_value;
        let reversion_bias = if price_deviation > dec!(50) {
            -1 // Price too high, bias down
        } else if price_deviation < dec!(-50) {
            1 // Price too low, bias up
        } else if rng.gen_bool(Self::MEAN_REVERSION) && price_deviation.abs() > dec!(20) {
            // Weak pull toward fair value
            if price_deviation > Decimal::ZERO { -1 } else { 1 }
        } else {
            0 // No reversion this tick
        };

        // Final direction combines momentum and mean reversion
        let direction = if reversion_bias != 0 && rng.gen_bool(0.3) {
            reversion_bias
        } else {
            base_direction
        };
        self.last_direction = direction;

        // --- Volatility clustering: scale step by recent volatility ---
        let base_step = rng.gen_range(1.0..=4.0);
        let vol_multiplier = (self.volatility / dec!(3)).min(dec!(3)).max(dec!(0.5));
        let vol_f64: f64 = vol_multiplier.try_into().unwrap_or(1.0);
        let step_size = Decimal::try_from(base_step * vol_f64).unwrap_or(dec!(2));

        // Update volatility (exponential moving average)
        let new_vol = dec!(0.1) * step_size + dec!(0.9) * self.volatility;
        // Add random volatility shocks (5% chance of vol spike)
        self.volatility = if rng.gen_range(0..100) < 5 {
            (new_vol * dec!(1.5)).min(dec!(10))
        } else {
            // Decay toward baseline
            let decay = Decimal::try_from(Self::VOL_DECAY).unwrap_or(dec!(0.9));
            new_vol * decay + dec!(3) * (Decimal::ONE - decay)
        };

        // --- Update fair value (slow drift) ---
        let fv_drift = Decimal::try_from(rng.gen_range(-Self::FAIR_VALUE_DRIFT..=Self::FAIR_VALUE_DRIFT))
            .unwrap_or(Decimal::ZERO);
        self.fair_value = Some((fair_value + fv_drift).max(dec!(1000)));

        // --- Calculate new price ---
        let step = step_size * Decimal::from(direction);
        let new_price = Self::round_price(current_price + step).max(dec!(100));
        self.walk_price = Some(new_price);

        // Cancel all existing orders first
        for order_id in ctx.open_orders.keys() {
            actions.orders_to_cancel.push(*order_id);
        }

        // --- Dynamic spread: widen in volatile conditions ---
        let vol_spread_factor = (self.volatility / dec!(3)).max(dec!(1));
        let dynamic_spread = Self::round_price(self.spread * vol_spread_factor);
        let half_spread = dynamic_spread / dec!(2);

        let bid_price = Self::round_price(new_price - half_spread);
        let ask_price = Self::round_price(new_price + half_spread);

        // Random quantity
        let quantity = Self::round_quantity(Decimal::from(rng.gen_range(5u32..20u32)));

        // Place bid order
        if bid_price > Decimal::ZERO {
            actions.orders_to_place.push(OrderRequest {
                symbol: ctx.symbol.to_string(),
                side: Side::Bid,
                order_type: OrderType::Limit,
                price: Some(bid_price),
                quantity,
            });
        }

        // Place ask order
        actions.orders_to_place.push(OrderRequest {
            symbol: ctx.symbol.to_string(),
            side: Side::Ask,
            order_type: OrderType::Limit,
            price: Some(ask_price),
            quantity,
        });

        // Market orders follow momentum (more likely to buy in uptrend)
        if rng.gen_range(0..100) < 20 {
            let market_side = if direction > 0 {
                if rng.gen_bool(0.6) { Side::Bid } else { Side::Ask }
            } else {
                if rng.gen_bool(0.6) { Side::Ask } else { Side::Bid }
            };
            let market_qty = Self::round_quantity(Decimal::from(rng.gen_range(1u32..5u32)));

            actions.orders_to_place.push(OrderRequest {
                symbol: ctx.symbol.to_string(),
                side: market_side,
                order_type: OrderType::Market,
                price: None,
                quantity: market_qty,
            });
        }

        actions
    }
}

impl Default for RandomWalk {
    fn default() -> Self {
        Self::new()
    }
}
