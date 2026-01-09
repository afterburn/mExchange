use axum::{
    routing::get,
    Router,
};
use matching_engine::{OrderBook, OrderId, OrderResult, Side as MatchingSide};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

mod events;
mod settlement;
mod udp_transport;

use events::{DeltaAction, LevelDelta, MarketEvent, OrderCommand, PriceLevel, Side};
use settlement::{SettlementClient, SettlementResult};
use udp_transport::{UdpEventSender, UdpOrderReceiver, UdpTransportConfig};

#[derive(Clone)]
struct AppState {
    orderbook: Arc<RwLock<OrderBook>>,
    event_sender: Arc<UdpEventSender>,
    symbol: String,
}

#[derive(Debug, Deserialize)]
struct OrderRequest {
    side: Side,
    order_type: String,
    price: Option<Decimal>,
    quantity: Decimal,
}

#[derive(Debug, Deserialize)]
struct CancelOrderRequest {
    order_id: OrderId,
}

#[derive(Debug, Serialize)]
struct OrderResponse {
    order_id: OrderId,
    fills: Vec<FillResponse>,
}

#[derive(Debug, Serialize)]
struct FillResponse {
    buy_order_id: OrderId,
    sell_order_id: OrderId,
    price: Decimal,
    quantity: Decimal,
}

fn main() {
    use std::io::Write;

    // Set up panic hook with backtrace for better debugging
    std::panic::set_hook(Box::new(|panic_info| {
        let backtrace = std::backtrace::Backtrace::capture();
        let _ = std::io::stderr().write_all(
            format!(
                "PANIC: {}\nBacktrace:\n{}\n",
                panic_info,
                backtrace
            ).as_bytes()
        );
        let _ = std::io::stderr().flush();
        std::process::exit(1);
    }));

    let _ = std::io::stderr().write_all(b"Starting matching engine service...\n");
    let _ = std::io::stderr().flush();

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = std::io::stderr().write_all(format!("Failed to create runtime: {}\n", e).as_bytes());
            let _ = std::io::stderr().flush();
            std::process::exit(1);
        }
    };

    let _ = std::io::stderr().write_all(b"Runtime created, calling tokio_main...\n");
    let _ = std::io::stderr().flush();

    match rt.block_on(tokio_main()) {
        Ok(_) => {
            let _ = std::io::stderr().write_all(b"tokio_main returned Ok\n");
            let _ = std::io::stderr().flush();
        }
        Err(e) => {
            let _ = std::io::stderr().write_all(format!("Error: {}\n", e).as_bytes());
            let _ = std::io::stderr().flush();
            std::process::exit(1);
        }
    }
}

async fn tokio_main() -> anyhow::Result<()> {
    // Load .env file if present (for local development)
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "matching_engine_service=info".into()),
        )
        .init();

    info!("Starting matching engine service with UDP transport...");

    let symbol = std::env::var("SYMBOL").unwrap_or_else(|_| "KCN/EUR".to_string());
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    // UDP transport configuration
    let udp_config = UdpTransportConfig::from_env();
    info!("UDP config: orders from {}, events to {}",
        udp_config.order_receiver_bind,
        udp_config.gateway_event_addr);

    // Create UDP event sender
    let event_sender = match UdpEventSender::new(
        udp_config.gateway_event_addr,
        udp_config.event_sender_bind,
    ) {
        Ok(s) => {
            info!("UDP event sender created");
            Arc::new(s)
        }
        Err(e) => {
            error!("Failed to create UDP event sender: {}", e);
            return Err(e);
        }
    };

    // Create settlement client for synchronous settlement
    let accounts_url = std::env::var("ACCOUNTS_URL")
        .unwrap_or_else(|_| "http://localhost:3001".to_string());
    info!("Settlement client configured for: {}", accounts_url);
    let settlement_client = Arc::new(SettlementClient::new(accounts_url));

    let orderbook = Arc::new(RwLock::new(OrderBook::new()));
    let _state = AppState {
        orderbook: orderbook.clone(),
        event_sender: event_sender.clone(),
        symbol: symbol.clone(),
    };

    // Start UDP order receiver
    if let Err(e) = start_order_receiver(
        udp_config.order_receiver_bind,
        orderbook.clone(),
        event_sender.clone(),
        settlement_client.clone(),
        symbol.clone(),
    ).await {
        error!("Failed to start UDP order receiver: {}", e);
        return Err(e);
    }

    // Spawn orderbook update task with delta publishing
    let ob_clone = orderbook.clone();
    let sender_clone = event_sender.clone();
    let symbol_clone = symbol.clone();
    tokio::spawn(async move {
        let mut publisher = OrderBookPublisher::new();
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
        loop {
            interval.tick().await;
            let ob = ob_clone.read().await;
            if let Err(e) = publisher.publish(&ob, &sender_clone, &symbol_clone).await {
                error!("Failed to publish orderbook update: {}", e);
            }
        }
    });

    let app = Router::new()
        .route("/health", get(health))
        .with_state(());

    info!("Matching engine service listening on {} (orders via UDP)", bind_addr);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    info!("Server started, serving requests...");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn start_order_receiver(
    bind_addr: std::net::SocketAddr,
    orderbook: Arc<RwLock<OrderBook>>,
    event_sender: Arc<UdpEventSender>,
    settlement_client: Arc<SettlementClient>,
    symbol: String,
) -> anyhow::Result<()> {
    let (_receiver, mut order_rx) = UdpOrderReceiver::new(bind_addr)?;

    tokio::spawn(async move {
        info!("UDP order receiver started, waiting for orders...");
        while let Some(command) = order_rx.recv().await {
            info!("Processing order command via UDP: {:?}", command);
            if let Err(e) = process_order_command(
                &orderbook,
                &event_sender,
                &settlement_client,
                command,
                &symbol,
            ).await {
                error!("Failed to process order command: {}", e);
            }
        }
        error!("UDP order receiver channel closed");
    });

    Ok(())
}

async fn process_order_command(
    orderbook: &Arc<RwLock<OrderBook>>,
    event_sender: &Arc<UdpEventSender>,
    settlement_client: &Arc<SettlementClient>,
    command: OrderCommand,
    symbol: &str,
) -> anyhow::Result<()> {
    match command {
        OrderCommand::PlaceOrder {
            order_id,
            side,
            order_type,
            price,
            quantity,
            ..
        } => {
            let matching_side = match side {
                Side::Bid => MatchingSide::Bid,
                Side::Ask => MatchingSide::Ask,
            };

            // Execute matching
            let mut ob = orderbook.write().await;
            let result: OrderResult = if order_type.to_lowercase() == "market" {
                ob.add_market_order(order_id, matching_side, quantity)
            } else {
                let p = price.ok_or_else(|| anyhow::anyhow!("Limit order requires price"))?;
                ob.add_limit_order(order_id, matching_side, p, quantity)
            };
            drop(ob);

            if !result.fills.is_empty() {
                info!("Order {} produced {} fills, settling...", result.order_id, result.fills.len());
            }

            // Settle each fill synchronously BEFORE publishing events
            let mut settled_fills = Vec::new();
            let mut settlement_failed = false;

            for fill in &result.fills {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);

                // Attempt settlement
                let settlement_result = settlement_client.settle_fill(
                    symbol,
                    fill.buy_order_id,
                    fill.sell_order_id,
                    fill.price,
                    fill.quantity,
                    timestamp,
                ).await;

                match settlement_result {
                    SettlementResult::Success(_) | SettlementResult::Skipped => {
                        // Settlement succeeded or was skipped (anonymous orders)
                        settled_fills.push((fill.clone(), timestamp));
                    }
                    SettlementResult::Failed(reason) => {
                        // Settlement failed - this is a critical issue
                        error!(
                            "CRITICAL: Settlement failed for fill buy={} sell={} qty={} @ {}: {}",
                            fill.buy_order_id, fill.sell_order_id, fill.quantity, fill.price, reason
                        );
                        error!(
                            "INCONSISTENCY: Orderbook shows fill but accounts not updated. Manual intervention required."
                        );
                        settlement_failed = true;
                        // Continue trying to settle other fills
                    }
                }
            }

            // Only publish events for successfully settled fills
            for (fill, timestamp) in &settled_fills {
                let event = MarketEvent::Fill {
                    symbol: symbol.to_string(),
                    buy_order_id: fill.buy_order_id,
                    sell_order_id: fill.sell_order_id,
                    price: fill.price,
                    quantity: fill.quantity,
                    timestamp: *timestamp,
                };
                event_sender.send_event(&event).await?;
            }

            // Calculate total filled quantity from settled fills only
            let filled_quantity: Decimal = settled_fills.iter().map(|(f, _)| f.quantity).sum();

            // For market orders that weren't fully filled, cancel the order in accounts
            // and send a cancel event
            if order_type.to_lowercase() == "market" {
                if filled_quantity < quantity {
                    info!(
                        "Market order {} filled {} of {} - cancelling unfilled portion",
                        result.order_id, filled_quantity, quantity
                    );

                    // Cancel order in accounts service (updates status, unlocks remaining funds)
                    settlement_client.cancel_order(result.order_id, filled_quantity).await;

                    // Send cancel event to clients
                    let cancel_event = MarketEvent::OrderCancelled {
                        order_id: result.order_id,
                        filled_quantity,
                    };
                    event_sender.send_event(&cancel_event).await?;
                }
            }

            // Send OrderAccepted (the order was accepted even if some settlements failed)
            let event = MarketEvent::OrderAccepted {
                order_id: result.order_id,
                side,
                order_type,
                price,
                quantity,
            };
            event_sender.send_event(&event).await?;

            // Send OrderFilled events for all orders that were 100% filled
            if !result.completed_orders.is_empty() {
                info!("Order {} has {} completed orders: {:?}",
                    result.order_id, result.completed_orders.len(), result.completed_orders);
            }
            for completed_order_id in &result.completed_orders {
                info!("Sending OrderFilled for order {}", completed_order_id);
                let filled_event = MarketEvent::OrderFilled {
                    order_id: *completed_order_id,
                };
                event_sender.send_event(&filled_event).await?;
            }

            if settlement_failed {
                warn!(
                    "Order {} completed with settlement failures - check logs for CRITICAL errors",
                    result.order_id
                );
            }
        }
        OrderCommand::CancelOrder { order_id, .. } => {
            let mut ob = orderbook.write().await;
            let cancelled = ob.cancel_order(order_id);
            drop(ob);

            if cancelled {
                let event = MarketEvent::OrderCancelled {
                    order_id,
                    // For manual cancellations, we don't track filled quantity here
                    // The accounts service has this info from partial fill updates
                    filled_quantity: Decimal::ZERO,
                };
                event_sender.send_event(&event).await?;
            }
        }
    }

    Ok(())
}

const MAX_LEVELS: usize = 10;
const SNAPSHOT_INTERVAL: u64 = 10; // Send snapshot every N updates

/// Tracks orderbook state for delta computation
struct OrderBookPublisher {
    prev_bids: BTreeMap<Decimal, Decimal>,
    prev_asks: BTreeMap<Decimal, Decimal>,
    sequence: u64,
    updates_since_snapshot: u64,
}

impl OrderBookPublisher {
    fn new() -> Self {
        Self {
            prev_bids: BTreeMap::new(),
            prev_asks: BTreeMap::new(),
            sequence: 0,
            updates_since_snapshot: SNAPSHOT_INTERVAL, // Force initial snapshot
        }
    }

    fn should_send_snapshot(&self) -> bool {
        self.updates_since_snapshot >= SNAPSHOT_INTERVAL
    }

    fn compute_deltas(
        &self,
        new_bids: &[(Decimal, Decimal)],
        new_asks: &[(Decimal, Decimal)],
    ) -> Vec<LevelDelta> {
        let mut deltas = Vec::new();

        // Build maps for new state
        let new_bid_map: BTreeMap<Decimal, Decimal> = new_bids.iter().cloned().collect();
        let new_ask_map: BTreeMap<Decimal, Decimal> = new_asks.iter().cloned().collect();

        // Check for bid changes
        for (price, qty) in &new_bid_map {
            match self.prev_bids.get(price) {
                Some(old_qty) if old_qty != qty => {
                    deltas.push(LevelDelta {
                        action: DeltaAction::Update,
                        side: Side::Bid,
                        price: *price,
                        quantity: *qty,
                    });
                }
                None => {
                    deltas.push(LevelDelta {
                        action: DeltaAction::Add,
                        side: Side::Bid,
                        price: *price,
                        quantity: *qty,
                    });
                }
                _ => {}
            }
        }

        // Check for removed bids
        for price in self.prev_bids.keys() {
            if !new_bid_map.contains_key(price) {
                deltas.push(LevelDelta {
                    action: DeltaAction::Remove,
                    side: Side::Bid,
                    price: *price,
                    quantity: Decimal::ZERO,
                });
            }
        }

        // Check for ask changes
        for (price, qty) in &new_ask_map {
            match self.prev_asks.get(price) {
                Some(old_qty) if old_qty != qty => {
                    deltas.push(LevelDelta {
                        action: DeltaAction::Update,
                        side: Side::Ask,
                        price: *price,
                        quantity: *qty,
                    });
                }
                None => {
                    deltas.push(LevelDelta {
                        action: DeltaAction::Add,
                        side: Side::Ask,
                        price: *price,
                        quantity: *qty,
                    });
                }
                _ => {}
            }
        }

        // Check for removed asks
        for price in self.prev_asks.keys() {
            if !new_ask_map.contains_key(price) {
                deltas.push(LevelDelta {
                    action: DeltaAction::Remove,
                    side: Side::Ask,
                    price: *price,
                    quantity: Decimal::ZERO,
                });
            }
        }

        deltas
    }

    fn update_state(&mut self, bids: &[(Decimal, Decimal)], asks: &[(Decimal, Decimal)]) {
        self.prev_bids = bids.iter().cloned().collect();
        self.prev_asks = asks.iter().cloned().collect();
        self.sequence += 1;
    }

    async fn publish(
        &mut self,
        orderbook: &OrderBook,
        event_sender: &UdpEventSender,
        symbol: &str,
    ) -> anyhow::Result<()> {
        let bid_levels = orderbook.get_bids(MAX_LEVELS);
        let ask_levels = orderbook.get_asks(MAX_LEVELS);

        if self.should_send_snapshot() {
            // Send full snapshot
            let bids: Vec<PriceLevel> = bid_levels
                .iter()
                .map(|(price, quantity)| PriceLevel { price: *price, quantity: *quantity })
                .collect();
            let asks: Vec<PriceLevel> = ask_levels
                .iter()
                .map(|(price, quantity)| PriceLevel { price: *price, quantity: *quantity })
                .collect();

            let event = MarketEvent::OrderBookSnapshot {
                symbol: symbol.to_string(),
                sequence: self.sequence,
                bids,
                asks,
            };

            event_sender.send_event(&event).await?;
            self.updates_since_snapshot = 0;
        } else {
            // Compute and send deltas
            let deltas = self.compute_deltas(&bid_levels, &ask_levels);

            if !deltas.is_empty() {
                let event = MarketEvent::OrderBookDelta {
                    symbol: symbol.to_string(),
                    sequence: self.sequence,
                    deltas,
                };

                event_sender.send_event(&event).await?;
            }
        }

        self.update_state(&bid_levels, &ask_levels);
        self.updates_since_snapshot += 1;

        Ok(())
    }
}
