mod market_maker;
mod aggressive;
mod random;
mod mean_reversion;
mod random_walk;
mod liquidity_provider;

pub use market_maker::MarketMaker;
pub use aggressive::Aggressive;
pub use random::Random;
pub use mean_reversion::MeanReversion;
pub use random_walk::RandomWalk;
pub use liquidity_provider::LiquidityProvider;

use crate::types::{MarketState, OrderRequest, StrategyActions};
use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

/// Context passed to strategies with current state
pub struct StrategyContext<'a> {
    pub market: &'a MarketState,
    pub symbol: &'a str,
    pub open_orders: &'a HashMap<Uuid, crate::types::OpenOrder>,
    pub inventory: Decimal,
}

pub trait Strategy: Send {
    fn name(&self) -> &'static str;

    /// Generate strategy actions (orders to cancel and place)
    /// This is the new preferred method that supports cancel-and-replace
    fn generate_actions(&mut self, ctx: &StrategyContext) -> StrategyActions;

    /// Legacy method - default implementation wraps generate_actions
    fn generate_orders(&mut self, market: &MarketState, symbol: &str) -> Vec<OrderRequest> {
        let ctx = StrategyContext {
            market,
            symbol,
            open_orders: &HashMap::new(),
            inventory: Decimal::ZERO,
        };
        self.generate_actions(&ctx).orders_to_place
    }

    fn interval_ms(&self) -> u64;
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum StrategyType {
    MarketMaker,
    Aggressive,
    Random,
    MeanReversion,
    RandomWalk,
    LiquidityProvider,
}

impl StrategyType {
    pub fn create(&self) -> Box<dyn Strategy> {
        match self {
            StrategyType::MarketMaker => Box::new(MarketMaker::new()),
            StrategyType::Aggressive => Box::new(Aggressive::new()),
            StrategyType::Random => Box::new(Random::new()),
            StrategyType::MeanReversion => Box::new(MeanReversion::new()),
            StrategyType::RandomWalk => Box::new(RandomWalk::new()),
            StrategyType::LiquidityProvider => Box::new(LiquidityProvider::new()),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            StrategyType::MarketMaker => "MarketMaker",
            StrategyType::Aggressive => "Aggressive",
            StrategyType::Random => "Random",
            StrategyType::MeanReversion => "MeanReversion",
            StrategyType::RandomWalk => "RandomWalk",
            StrategyType::LiquidityProvider => "LiquidityProvider",
        }
    }
}
