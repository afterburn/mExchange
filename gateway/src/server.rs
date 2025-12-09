use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{any, get, post},
    Json, Router,
};
use axum::http::{header::{AUTHORIZATION, CONTENT_TYPE, ACCEPT}, HeaderValue, Method};
use tower_http::cors::CorsLayer;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::channel_updates::OrderBookState;
use crate::events::{CancelOrderRequest, MarketEvent, OrderCommand, OrderRequest, Side};
use crate::udp_transport::{UdpOrderSender, UdpEventReceiver};
use crate::proxy::{proxy_accounts, proxy_market_data, ProxyState};
use crate::state::GatewayState;
use crate::websocket::{BotCommand, BotCommandInner, ChannelManager};
use std::collections::HashMap;
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
            .route("/api/order", post(place_order))
            .route("/api/order/cancel", post(cancel_order))
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
        let state = self.state.clone();
        let channel_manager = self.channel_manager.clone();
        let orderbook_states = self.orderbook_states.clone();

        tokio::spawn(async move {
            while let Ok(event) = event_rx.recv().await {
                state.broadcast_event(event.clone()).await;

                // Note: Settlement is now handled synchronously in matching_engine_service
                // before events are published. The gateway is a pure event relay.

                let mut ob_states = orderbook_states.write().await;

                // Get symbol from event
                let symbol = match &event {
                    MarketEvent::OrderBookSnapshot { symbol, .. } => Some(symbol.clone()),
                    MarketEvent::OrderBookDelta { symbol, .. } => Some(symbol.clone()),
                    MarketEvent::Fill { symbol, .. } => Some(symbol.clone()),
                    _ => None,
                };

                if let Some(symbol) = symbol {
                    let ob_state = ob_states
                        .entry(symbol.clone())
                        .or_insert_with(crate::channel_updates::OrderBookState::new);

                    if let Some(notification) = ob_state.apply_orderbook_update(&event) {
                        let channel_name = notification.channel_name.clone();
                        let json = match serde_json::to_string(&notification) {
                            Ok(json) => json,
                            Err(e) => {
                                error!("Failed to serialize notification: {}", e);
                                continue;
                            }
                        };

                        let cm = channel_manager.read().await;
                        let subscribers = cm.get_subscribers(&channel_name);
                        for client_id in subscribers {
                            if let Some(sender) = cm.get_sender(client_id) {
                                let _ = sender.send(json.clone());
                            }
                        }
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
    State((state, order_sender, channel_manager, orderbook_states, _)): State<AppState>,
) -> Response {
    let (client_id, event_rx) = state.add_client().await;
    ws.on_upgrade(move |socket| {
        crate::websocket::handle_websocket_connection(socket, client_id, event_rx, channel_manager, orderbook_states, order_sender)
    })
}

/// Request for placing an authenticated order
#[derive(Debug, Deserialize)]
struct AuthenticatedOrderRequest {
    symbol: String,
    side: Side,
    order_type: String,
    #[serde(default)]
    price: Option<Decimal>,
    /// Quantity of base asset. For quote currency orders, this can be omitted.
    #[serde(default)]
    quantity: Option<Decimal>,
    /// For quote currency orders: amount of quote asset to spend (e.g., 1000 EUR).
    /// Backend calculates the quantity based on this and max_slippage_price.
    /// Only valid for market buy orders.
    #[serde(default)]
    quote_amount: Option<Decimal>,
    /// For market buy orders: max slippage price for fund locking
    #[serde(default)]
    max_slippage_price: Option<Decimal>,
}

/// Response from accounts service for order creation
#[derive(Debug, Deserialize)]
struct AccountsOrderResponse {
    order: AccountsOrder,
    locked_asset: String,
    locked_amount: String,
}

#[derive(Debug, Deserialize)]
struct AccountsOrder {
    id: String,
    quantity: String,
    // Other fields we don't need
}

/// Response for authenticated order placement
#[derive(Debug, Serialize)]
struct PlaceOrderResponse {
    order_id: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

/// Place an order - handles both authenticated and anonymous orders
///
/// For authenticated users (with Authorization header):
/// 1. Create order in accounts service (locks funds)
/// 2. Forward to matching engine with order_id
///
/// For anonymous users (demo mode):
/// Just forward to matching engine
async fn place_order(
    State((_, order_sender, _, _, proxy_state)): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedOrderRequest>,
) -> Result<Json<PlaceOrderResponse>, (StatusCode, Json<ErrorResponse>)> {
    info!("Received order request: {:?}", request);

    // Check if authenticated
    let auth_header = headers.get("authorization").and_then(|v| v.to_str().ok());

    if let Some(auth) = auth_header {
        // Authenticated flow: create order in accounts first, then forward to matching engine
        let accounts_url = &proxy_state.accounts_url;

        // 1. Create order in accounts service (locks funds)
        let accounts_req = serde_json::json!({
            "symbol": request.symbol,
            "side": request.side,
            "order_type": request.order_type,
            "price": request.price,
            "quantity": request.quantity,
            "quote_amount": request.quote_amount,
            "max_slippage_price": request.max_slippage_price,
        });

        let response = proxy_state.client
            .post(format!("{}/api/orders", accounts_url))
            .header("Authorization", auth)
            .header("Content-Type", "application/json")
            .json(&accounts_req)
            .send()
            .await
            .map_err(|e| {
                error!("Failed to create order in accounts: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to create order".into() }))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            warn!("Accounts service rejected order: {} - {}", status, error_text);

            // Try to parse error response
            if let Ok(err) = serde_json::from_str::<serde_json::Value>(&error_text) {
                let msg = err.get("error").and_then(|e| e.as_str()).unwrap_or("Order rejected");
                return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: msg.into() })));
            }
            return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: "Order rejected".into() })));
        }

        let accounts_response: AccountsOrderResponse = response.json().await.map_err(|e| {
            error!("Failed to parse accounts response: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to parse response".into() }))
        })?;

        let order_id: Uuid = accounts_response.order.id.parse().map_err(|e| {
            error!("Failed to parse order ID: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Invalid order ID".into() }))
        })?;

        // Parse quantity from accounts response (may be calculated from quote_amount)
        let quantity: Decimal = accounts_response.order.quantity.parse().map_err(|e| {
            error!("Failed to parse quantity: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Invalid quantity".into() }))
        })?;

        // 2. Forward to matching engine with the order_id from accounts
        let command = OrderCommand::PlaceOrder {
            order_id,
            side: request.side,
            order_type: request.order_type.clone(),
            price: request.price,
            quantity,
            user_id: None, // Could extract from JWT if needed
        };

        if let Err(e) = order_sender.send_order_command(&command).await {
            error!("Failed to send order via UDP: {}", e);

            // Compensating transaction: cancel the order in accounts service
            // Retry up to 3 times with exponential backoff
            let mut cancel_success = false;
            for attempt in 1..=3u32 {
                let cancel_result = proxy_state.client
                    .delete(format!("{}/api/orders/{}", accounts_url, order_id))
                    .header("Authorization", auth.clone())
                    .send()
                    .await;

                match cancel_result {
                    Ok(resp) if resp.status().is_success() => {
                        info!("Compensating transaction: cancelled order {} after UDP failure", order_id);
                        cancel_success = true;
                        break;
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        if status.is_server_error() && attempt < 3 {
                            let delay = std::time::Duration::from_millis(100 * 2u64.pow(attempt - 1));
                            warn!("Compensating transaction failed (attempt {}), retrying in {:?}: status {}", attempt, delay, status);
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        error!("Failed to cancel order {} after UDP failure: status {}", order_id, status);
                        break;
                    }
                    Err(cancel_err) => {
                        if attempt < 3 {
                            let delay = std::time::Duration::from_millis(100 * 2u64.pow(attempt - 1));
                            warn!("Compensating transaction failed (attempt {}), retrying in {:?}: {}", attempt, delay, cancel_err);
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        error!("Failed to cancel order {} after UDP failure: {}", order_id, cancel_err);
                        break;
                    }
                }
            }

            if !cancel_success {
                // Critical: funds may be locked - include order_id in response for manual cleanup
                error!("CRITICAL: Order {} may have locked funds - compensating transaction failed", order_id);
            }

            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: format!("Failed to submit to matching engine. Order ID: {}", order_id) })));
        }

        Ok(Json(PlaceOrderResponse {
            order_id: order_id.to_string(),
            status: "pending".into(),
        }))
    } else {
        // Anonymous flow: generate a UUID and forward to matching engine (demo mode)
        // The settlement service will return ORDER_NOT_FOUND for this UUID since
        // it doesn't exist in the accounts database

        // For anonymous mode, quantity is required (no accounts service to calculate from quote_amount)
        let quantity = request.quantity.ok_or_else(|| {
            (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: "Quantity is required for anonymous orders".into() }))
        })?;

        let anon_order_id = Uuid::new_v4();
        let command = OrderCommand::PlaceOrder {
            order_id: anon_order_id,
            side: request.side,
            order_type: request.order_type.clone(),
            price: request.price,
            quantity,
            user_id: None,
        };

        if let Err(e) = order_sender.send_order_command(&command).await {
            error!("Failed to send order via UDP: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to submit order".into() })));
        }

        Ok(Json(PlaceOrderResponse {
            order_id: anon_order_id.to_string(),
            status: "accepted".into(),
        }))
    }
}

async fn cancel_order(
    State((_, order_sender, _, _, _)): State<AppState>,
    Json(request): Json<CancelOrderRequest>,
) -> Result<StatusCode, StatusCode> {
    info!("Received cancel request: {:?}", request);

    let command = OrderCommand::CancelOrder {
        order_id: request.order_id,
        user_id: None,
    };

    if let Err(e) = order_sender.send_order_command(&command).await {
        error!("Failed to send cancel via UDP: {}", e);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok(StatusCode::ACCEPTED)
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

#[derive(Deserialize)]
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

