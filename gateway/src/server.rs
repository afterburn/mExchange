use axum::{
    extract::{
        ws::{WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::Response,
    routing::{any, get, post},
    Json, Router,
};
use axum::http::{header::{AUTHORIZATION, CONTENT_TYPE, ACCEPT}, HeaderValue, Method};
use tower_http::cors::CorsLayer;
use serde::Serialize;
use tracing::{error, info};

use crate::channel_updates::OrderBookState;
use crate::events::MarketEvent;
use crate::udp_transport::{UdpOrderSender, UdpEventReceiver};
use crate::proxy::{proxy_accounts, proxy_market_data, ProxyState};
use crate::state::GatewayState;
use crate::websocket::{BotCommand, BotCommandInner, ChannelManager};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

pub struct GatewayServer {
    state: GatewayState,
    order_sender: Arc<UdpOrderSender>,
    channel_manager: Arc<tokio::sync::RwLock<ChannelManager>>,
    orderbook_states: Arc<tokio::sync::RwLock<HashMap<String, OrderBookState>>>,
    proxy_state: ProxyState,
}

type AppState = (
    GatewayState,
    Arc<UdpOrderSender>,
    Arc<tokio::sync::RwLock<ChannelManager>>,
    Arc<tokio::sync::RwLock<HashMap<String, OrderBookState>>>,
    ProxyState,
);

impl GatewayServer {
    pub fn new(order_sender: Arc<UdpOrderSender>, accounts_url: String, market_data_url: String) -> Self {
        Self {
            state: GatewayState::new(),
            order_sender,
            channel_manager: Arc::new(tokio::sync::RwLock::new(ChannelManager::new())),
            orderbook_states: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            proxy_state: ProxyState::new(accounts_url, market_data_url),
        }
    }

    pub fn state(&self) -> GatewayState {
        self.state.clone()
    }

    pub fn router(&self) -> Router {
        // CORS configuration - credentials mode requires explicit origins, methods, and headers
        let allowed_headers = [AUTHORIZATION, CONTENT_TYPE, ACCEPT];
        let allowed_methods = [Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS];

        let cors = if let Ok(origins) = std::env::var("CORS_ALLOWED_ORIGINS") {
            let allowed: Vec<HeaderValue> = origins
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            if allowed.is_empty() {
                // Fall back to dev defaults for invalid config
                tracing::warn!("CORS_ALLOWED_ORIGINS set but no valid origins, using dev defaults");
                let dev_origins: Vec<HeaderValue> = [
                    "http://localhost:5173",
                    "http://localhost:5174",
                    "http://localhost:3000",
                ]
                .iter()
                .filter_map(|o| o.parse().ok())
                .collect();
                CorsLayer::new()
                    .allow_origin(dev_origins)
                    .allow_methods(allowed_methods.clone())
                    .allow_headers(allowed_headers.clone())
                    .allow_credentials(true)
            } else {
                tracing::info!("CORS restricted to: {:?}", allowed);
                CorsLayer::new()
                    .allow_origin(allowed)
                    .allow_methods(allowed_methods.clone())
                    .allow_headers(allowed_headers.clone())
                    .allow_credentials(true)
            }
        } else {
            // Development: allow common localhost origins with credentials
            let dev_origins = [
                "http://localhost:5173",  // Vite default
                "http://localhost:5174",
                "http://localhost:5175",
                "http://localhost:5176",
                "http://localhost:5177",
                "http://localhost:5178",
                "http://localhost:5179",
                "http://localhost:3000",
                "http://127.0.0.1:5173",
                "http://127.0.0.1:5174",
                "http://127.0.0.1:5175",
                "http://127.0.0.1:5176",
                "http://127.0.0.1:5177",
                "http://127.0.0.1:5178",
                "http://127.0.0.1:5179",
                "http://127.0.0.1:3000",
            ];
            let origins: Vec<HeaderValue> = dev_origins
                .iter()
                .filter_map(|o| o.parse().ok())
                .collect();
            tracing::info!("Development CORS enabled for: {:?}", dev_origins);
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods(allowed_methods)
                .allow_headers(allowed_headers)
                .allow_credentials(true)
        };

        // Proxy router for accounts service endpoints
        let accounts_proxy_router = Router::new()
            .route("/auth/*path", any(proxy_accounts))
            .route("/api/me", any(proxy_accounts))
            .route("/api/me/*path", any(proxy_accounts))
            .route("/api/balances", any(proxy_accounts))
            .route("/api/balances/*path", any(proxy_accounts))
            .route("/api/orders", any(proxy_accounts))
            .route("/api/orders/*path", any(proxy_accounts))
            .route("/api/faucet/*path", any(proxy_accounts))
            .with_state(self.proxy_state.clone());

        // Proxy router for market_data service endpoints
        let market_data_proxy_router = Router::new()
            .route("/api/ohlcv", any(proxy_market_data))
            .route("/api/ohlcv/*path", any(proxy_market_data))
            .with_state(self.proxy_state.clone());

        Router::new()
            .route("/health", get(health))
            .route("/ws", get(websocket_handler))
            .route("/api/bot/start", post(start_bot))
            .route("/api/bot/stop", post(stop_bot))
            .route("/api/bot/status", get(bot_status))
            .merge(accounts_proxy_router)
            .merge(market_data_proxy_router)
            .layer(cors)
            .with_state((
                self.state.clone(),
                self.order_sender.clone(),
                self.channel_manager.clone(),
                self.orderbook_states.clone(),
                self.proxy_state.clone(),
            ))
    }

    pub async fn start_udp_event_receiver(
        &self,
        bind_addr: SocketAddr,
    ) -> anyhow::Result<()> {
        let state = self.state.clone();

        let (_receiver, mut event_rx) = UdpEventReceiver::new(bind_addr)?;

        tokio::spawn(async move {
            info!("UDP event receiver started, waiting for market events...");
            while let Some(event) = event_rx.recv().await {
                info!("Received market event via UDP");
                state.publish_event(event);
            }
            error!("UDP event receiver channel closed");
        });

        Ok(())
    }

    pub async fn start_event_broadcaster(&self) {
        let mut event_rx = self.state.subscribe_events();
        let channel_manager = self.channel_manager.clone();
        let orderbook_states = self.orderbook_states.clone();

        tokio::spawn(async move {
            while let Ok(event) = event_rx.recv().await {
                // Note: OrderFilled/OrderCancelled events are now handled per-client in websocket.rs
                // Here we only handle orderbook updates for channel subscribers

                let mut ob_states = orderbook_states.write().await;

                // Get symbol from event
                let (symbol, event_type) = match &event {
                    MarketEvent::OrderBookSnapshot { symbol, bids, asks, .. } => {
                        info!("Event broadcaster: OrderBookSnapshot for {} with {} bids, {} asks", symbol, bids.len(), asks.len());
                        (Some(symbol.clone()), "snapshot")
                    },
                    MarketEvent::OrderBookDelta { symbol, deltas, .. } => {
                        info!("Event broadcaster: OrderBookDelta for {} with {} deltas", symbol, deltas.len());
                        (Some(symbol.clone()), "delta")
                    },
                    MarketEvent::Fill { symbol, price, quantity, .. } => {
                        info!("Event broadcaster: Fill for {} price={} qty={}", symbol, price, quantity);
                        (Some(symbol.clone()), "fill")
                    },
                    _ => (None, "other"),
                };

                if let Some(symbol) = symbol {
                    let ob_state = ob_states
                        .entry(symbol.clone())
                        .or_insert_with(crate::channel_updates::OrderBookState::new);

                    if let Some(notification) = ob_state.apply_orderbook_update(&event) {
                        let channel_name = notification.channel_name.clone();
                        let bid_changes_count = notification.notification.bid_changes.len();
                        let ask_changes_count = notification.notification.ask_changes.len();
                        let trades_count = notification.notification.trades.len();

                        info!("Broadcasting {} to channel {}: {} bid_changes, {} ask_changes, {} trades",
                            event_type, channel_name, bid_changes_count, ask_changes_count, trades_count);

                        let json = match serde_json::to_string(&notification) {
                            Ok(json) => json,
                            Err(e) => {
                                error!("Failed to serialize notification: {}", e);
                                continue;
                            }
                        };

                        let cm = channel_manager.read().await;
                        let subscribers = cm.get_subscribers(&channel_name);
                        info!("Channel {} has {} subscribers", channel_name, subscribers.len());
                        for client_id in subscribers {
                            if let Some(sender) = cm.get_sender(client_id) {
                                let _ = sender.send(json.clone());
                            }
                        }
                    } else {
                        info!("Event broadcaster: apply_orderbook_update returned None for {} event", event_type);
                    }
                }
            }
        });
    }
}

async fn health() -> &'static str {
    "ok"
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    State((state, order_sender, channel_manager, orderbook_states, proxy_state)): State<AppState>,
) -> Response {
    let (client_id, event_rx) = state.add_client().await;
    ws.on_upgrade(move |socket| {
        crate::websocket::handle_websocket_connection(
            socket,
            client_id,
            event_rx,
            channel_manager,
            orderbook_states,
            order_sender,
            proxy_state,
        )
    })
}

#[derive(Serialize)]
struct StrategyStatusResponse {
    strategy: String,
    running: bool,
    uptime_secs: Option<u64>,
}

#[derive(Serialize)]
struct BotStatusResponse {
    connected: bool,
    strategies: Vec<StrategyStatusResponse>,
}

#[derive(serde::Deserialize)]
struct BotControlRequest {
    strategy: String,
}

async fn start_bot(
    State((_, _, channel_manager, _, _)): State<AppState>,
    Json(request): Json<BotControlRequest>,
) -> Result<Json<BotStatusResponse>, StatusCode> {
    let cm = channel_manager.read().await;

    if !cm.is_bot_connected() {
        return Ok(Json(BotStatusResponse {
            connected: false,
            strategies: vec![],
        }));
    }

    // Send start command to bot
    if let Some(sender) = cm.get_bot_sender() {
        let command = BotCommand {
            command: BotCommandInner::Start {
                strategy: request.strategy,
            },
        };
        if let Ok(json) = serde_json::to_string(&command) {
            let _ = sender.send(json);
            info!("Sent start command to bot");
        }
    }

    // Return current status (will be updated async)
    let strategies = cm
        .get_bot_status()
        .map(|s| {
            s.iter()
                .map(|ss| StrategyStatusResponse {
                    strategy: ss.strategy.clone(),
                    running: ss.running,
                    uptime_secs: ss.uptime_secs,
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(BotStatusResponse {
        connected: true,
        strategies,
    }))
}

async fn stop_bot(
    State((_, _, channel_manager, _, _)): State<AppState>,
    Json(request): Json<BotControlRequest>,
) -> Result<Json<BotStatusResponse>, StatusCode> {
    let cm = channel_manager.read().await;

    if !cm.is_bot_connected() {
        return Ok(Json(BotStatusResponse {
            connected: false,
            strategies: vec![],
        }));
    }

    // Send stop command to bot
    if let Some(sender) = cm.get_bot_sender() {
        let command = BotCommand {
            command: BotCommandInner::Stop {
                strategy: request.strategy,
            },
        };
        if let Ok(json) = serde_json::to_string(&command) {
            let _ = sender.send(json);
            info!("Sent stop command to bot");
        }
    }

    // Return current status
    let strategies = cm
        .get_bot_status()
        .map(|s| {
            s.iter()
                .map(|ss| StrategyStatusResponse {
                    strategy: ss.strategy.clone(),
                    running: ss.running,
                    uptime_secs: ss.uptime_secs,
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(BotStatusResponse {
        connected: true,
        strategies,
    }))
}

async fn bot_status(
    State((_, _, channel_manager, _, _)): State<AppState>,
) -> Result<Json<BotStatusResponse>, StatusCode> {
    let cm = channel_manager.read().await;

    if !cm.is_bot_connected() {
        return Ok(Json(BotStatusResponse {
            connected: false,
            strategies: vec![],
        }));
    }

    // Request fresh status from bot
    if let Some(sender) = cm.get_bot_sender() {
        let command = BotCommand {
            command: BotCommandInner::Status,
        };
        if let Ok(json) = serde_json::to_string(&command) {
            let _ = sender.send(json);
        }
    }

    // Return cached status
    let strategies = cm
        .get_bot_status()
        .map(|s| {
            s.iter()
                .map(|ss| StrategyStatusResponse {
                    strategy: ss.strategy.clone(),
                    running: ss.running,
                    uptime_secs: ss.uptime_secs,
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(BotStatusResponse {
        connected: cm.is_bot_connected(),
        strategies,
    }))
}
