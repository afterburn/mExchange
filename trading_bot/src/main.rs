use clap::Parser;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

use trading_bot::strategies::StrategyContext;
use trading_bot::{GatewayClient, MarketState, StrategyType};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Parser, Debug)]
#[command(name = "trading_bot")]
#[command(about = "Automated trading bot for mexchange")]
struct Args {
    /// Gateway host
    #[arg(long, default_value = "localhost")]
    gateway_host: String,

    /// Gateway port
    #[arg(long, default_value_t = 3000)]
    gateway_port: u16,

    /// Trading symbol
    #[arg(long, default_value = "KCN/EUR")]
    symbol: String,

    /// Strategies to run (can specify multiple)
    #[arg(long, value_enum, num_args = 1..)]
    strategies: Vec<StrategyType>,

    /// Run all strategies
    #[arg(long, default_value_t = false)]
    all: bool,
}

/// Bot account configuration per strategy
struct BotAccount {
    email: String,
    strategy_type: StrategyType,
}

impl BotAccount {
    fn new(strategy_type: StrategyType) -> Self {
        // Each strategy gets its own email/account
        let email = format!("bot-{}@mexchange.local", strategy_type.name().to_lowercase().replace(" ", "-"));
        Self { email, strategy_type }
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let strategies: Vec<StrategyType> = if args.all {
        vec![
            StrategyType::MarketMaker,
            StrategyType::Aggressive,
            StrategyType::Random,
            StrategyType::MeanReversion,
            StrategyType::RandomWalk,
        ]
    } else if args.strategies.is_empty() {
        warn!("No strategies specified, defaulting to RandomWalk");
        vec![StrategyType::RandomWalk]
    } else {
        args.strategies
    };

    info!(
        "Starting trading bot with strategies: {:?}",
        strategies.iter().map(|s| format!("{:?}", s)).collect::<Vec<_>>()
    );
    info!("Gateway: {}:{}", args.gateway_host, args.gateway_port);
    info!("Symbol: {}", args.symbol);

    // Create shared market state (all strategies see the same market)
    let shared_market_state = Arc::new(RwLock::new(MarketState::new()));

    // First, create a seeder client to set up the initial orderbook
    let seeder_client = GatewayClient::new(&args.gateway_host, args.gateway_port);
    let seeder_email = "tradingbot@mexchange.local";
    info!("Setting up market with seeder account: {}", seeder_email);

    if let Err(e) = seeder_client.login(seeder_email).await {
        error!("Failed to login seeder: {}", e);
        return Err(e);
    }

    // Fund the seeder (effectively unlimited)
    let _ = seeder_client.deposit("EUR", "10000000000").await;
    let _ = seeder_client.mint("KCN", "10000000000").await;

    // Connect seeder to WebSocket for market data
    if let Err(e) = seeder_client.connect_websocket_with_state(Arc::clone(&shared_market_state)).await {
        error!("Failed to connect seeder WebSocket: {}", e);
        return Err(e);
    }

    // Seed the orderbook
    info!("Seeding orderbook with initial orders...");
    seed_orderbook(&seeder_client, &args.symbol).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Give time for market data to arrive
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Now spawn each strategy with its OWN client/account
    let mut handles = Vec::new();

    for strategy_type in strategies {
        let bot = BotAccount::new(strategy_type);
        let gateway_host = args.gateway_host.clone();
        let gateway_port = args.gateway_port;
        let symbol = args.symbol.clone();
        let market_state = Arc::clone(&shared_market_state);

        let handle = tokio::spawn(async move {
            if let Err(e) = run_strategy_with_own_account(
                bot,
                &gateway_host,
                gateway_port,
                market_state,
                symbol,
            ).await {
                error!("Strategy failed: {}", e);
            }
        });

        handles.push(handle);
    }

    // Wait for all strategies (they run forever unless error)
    for handle in handles {
        if let Err(e) = handle.await {
            error!("Strategy task failed: {}", e);
        }
    }

    Ok(())
}

/// Each strategy runs with its own account - proper isolation
async fn run_strategy_with_own_account(
    bot: BotAccount,
    gateway_host: &str,
    gateway_port: u16,
    shared_market_state: Arc<RwLock<MarketState>>,
    symbol: String,
) -> Result<(), BoxError> {
    let strategy_name = bot.strategy_type.name();

    // Create dedicated client for this strategy
    let client = GatewayClient::new(gateway_host, gateway_port);

    info!("[{}] Logging in as {}", strategy_name, bot.email);
    client.login(&bot.email).await?;

    // Fund this bot's account (effectively unlimited)
    if let Err(e) = client.deposit("EUR", "10000000000").await {
        warn!("[{}] EUR deposit: {}", strategy_name, e);
    }
    if let Err(e) = client.mint("KCN", "10000000000").await {
        warn!("[{}] KCN mint: {}", strategy_name, e);
    }

    // Connect to WebSocket with shared market state
    client.connect_websocket_with_state(Arc::clone(&shared_market_state)).await?;
    info!("[{}] Connected to gateway", strategy_name);

    // Now run the strategy loop
    let mut strategy = bot.strategy_type.create();
    let interval = Duration::from_millis(strategy.interval_ms());

    info!(
        "[{}] Starting strategy (interval: {}ms)",
        strategy_name,
        strategy.interval_ms()
    );

    loop {
        // Get current market state (shared across all strategies)
        let state = shared_market_state.read().await.clone();

        // Use locally tracked open orders (updated via WebSocket) - no REST call needed
        let open_orders = client.open_orders().read().await.clone();

        let inventory = client.get_inventory().await;

        // Build strategy context
        let ctx = StrategyContext {
            market: &state,
            symbol: &symbol,
            open_orders: &open_orders,
            inventory,
        };

        // Generate actions (cancels and new orders)
        let actions = strategy.generate_actions(&ctx);

        let has_actions = !actions.orders_to_cancel.is_empty() || !actions.orders_to_place.is_empty();

        // Cancel orders via WebSocket (fast, no REST)
        for order_id in &actions.orders_to_cancel {
            if let Err(e) = client.cancel_order(*order_id).await {
                debug!("[{}] Failed to cancel order {}: {}", strategy_name, order_id, e);
            }
        }

        // Place new orders via WebSocket
        for order in &actions.orders_to_place {
            if let Err(e) = client.submit_order(order).await {
                warn!("[{}] Failed to submit order: {}", strategy_name, e);
            }
        }

        // Log summary
        if has_actions && !actions.orders_to_place.is_empty() {
            info!(
                "[{}] Placed {} orders (cancelled {}, mid={:?}, inv={}, open={})",
                strategy_name,
                actions.orders_to_place.len(),
                actions.orders_to_cancel.len(),
                state.mid_price(),
                inventory,
                open_orders.len()
            );
        }

        tokio::time::sleep(interval).await;
    }
}

/// Seeds the orderbook with initial limit orders to establish a price
async fn seed_orderbook(client: &GatewayClient, symbol: &str) {
    use rust_decimal_macros::dec;
    use trading_bot::types::{OrderRequest, OrderType, Side};

    // Initial price around 10,000
    let base_price = dec!(10000);

    // Place 5 bid orders below the base price
    for i in 1..=5 {
        let price = base_price - rust_decimal::Decimal::from(i * 10);
        let order = OrderRequest {
            symbol: symbol.to_string(),
            side: Side::Bid,
            order_type: OrderType::Limit,
            price: Some(price),
            quantity: dec!(100),
        };
        if let Err(e) = client.submit_order(&order).await {
            warn!("Failed to seed bid order: {}", e);
        }
    }

    // Place 5 ask orders above the base price
    for i in 1..=5 {
        let price = base_price + rust_decimal::Decimal::from(i * 10);
        let order = OrderRequest {
            symbol: symbol.to_string(),
            side: Side::Ask,
            order_type: OrderType::Limit,
            price: Some(price),
            quantity: dec!(100),
        };
        if let Err(e) = client.submit_order(&order).await {
            warn!("Failed to seed ask order: {}", e);
        }
    }

    info!("Seeded orderbook with 10 initial orders around {}", base_price);
}
