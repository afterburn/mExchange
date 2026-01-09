use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, warn, error};
use uuid::Uuid;

use crate::channel_updates::OrderBookState;
use crate::events::{MarketEvent, OrderCommand, Side};
use crate::proxy::ProxyState;
use crate::udp_transport::UdpOrderSender;

/// JSON-RPC style client request
#[derive(Debug, Clone, Deserialize)]
pub struct ClientRequest {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Subscription parameters
#[derive(Debug, Clone, Deserialize)]
pub struct SubscribeParams {
    pub channels: Vec<String>,
}

/// Auth parameters
#[derive(Debug, Clone, Deserialize)]
pub struct AuthParams {
    pub token: String,
}

/// Place order parameters
#[derive(Debug, Clone, Deserialize)]
pub struct PlaceOrderParams {
    pub symbol: String,
    pub side: String,
    pub order_type: String,
    #[serde(default)]
    pub price: Option<Decimal>,
    #[serde(default)]
    pub quantity: Option<Decimal>,
    #[serde(default)]
    pub quote_amount: Option<Decimal>,
    #[serde(default)]
    pub max_slippage_price: Option<Decimal>,
}

/// Cancel order parameters
#[derive(Debug, Clone, Deserialize)]
pub struct CancelOrderParams {
    pub order_id: String,
}

/// Bot status parameters
#[derive(Debug, Clone, Deserialize)]
pub struct BotStatusParams {
    pub strategies: Vec<StrategyStatus>,
}

/// JSON-RPC style success response
#[derive(Debug, Clone, Serialize)]
pub struct SuccessResponse<T: Serialize> {
    pub id: String,
    pub result: T,
}

/// JSON-RPC style error response
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    pub id: String,
    pub error: RpcError,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

/// Error codes
pub const ERR_INVALID_PARAMS: i32 = -32602;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERR_UNAUTHORIZED: i32 = 10000;
pub const ERR_ORDER_REJECTED: i32 = 10001;
pub const ERR_CANCEL_FAILED: i32 = 10002;

/// Auth result for private methods
#[derive(Debug, Clone, Serialize)]
pub struct AuthResult {
    pub access_token: String,
    pub token_type: String,
    pub user_id: String,
}

/// Order placement result
#[derive(Debug, Clone, Serialize)]
pub struct OrderResult {
    pub order_id: String,
}

/// Order cancellation result
#[derive(Debug, Clone, Serialize)]
pub struct CancelResult {
    pub order_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyStatus {
    pub strategy: String,
    pub running: bool,
    pub uptime_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotCommand {
    pub command: BotCommandInner,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "action")]
pub enum BotCommandInner {
    #[serde(rename = "start")]
    Start { strategy: String },
    #[serde(rename = "stop")]
    Stop { strategy: String },
    #[serde(rename = "status")]
    Status,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelNotification {
    pub channel_name: String,
    pub notification: NotificationData,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationData {
    pub trades: Vec<TradeData>,
    pub bid_changes: Vec<PriceLevelChange>,
    pub ask_changes: Vec<PriceLevelChange>,
    pub total_bid_amount: f64,
    pub total_ask_amount: f64,
    pub time: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats_24h: Option<Stats24h>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats24h {
    pub high_24h: f64,
    pub low_24h: f64,
    pub volume_24h: f64,
    pub open_24h: f64,
    pub last_price: f64,
}

/// Ticker channel notification (ticker.SYMBOL.INTERVAL)
#[derive(Debug, Clone, Serialize)]
pub struct TickerNotification {
    pub channel_name: String,
    pub notification: TickerData,
}

#[derive(Debug, Clone, Serialize)]
pub struct TickerData {
    pub mark_price: f64,
    pub mark_timestamp: f64,
    pub best_bid_price: f64,
    pub best_bid_amount: f64,
    pub best_ask_price: f64,
    pub best_ask_amount: f64,
    pub last_price: f64,
    pub delta: f64,
    pub volume_24h: f64,
    pub value_24h: f64,
    pub low_price_24h: f64,
    pub high_price_24h: f64,
    pub change_24h: f64,
    pub index: f64,
    pub forward: f64,
    pub funding_mark: f64,
    pub funding_rate: f64,
    pub collar_low: f64,
    pub collar_high: f64,
    pub realised_funding_24h: f64,
    pub average_funding_rate_24h: f64,
    pub open_interest: f64,
}

/// LWT (Last, Best Bid/Ask, Mark) channel notification (lwt.SYMBOL.INTERVAL)
#[derive(Debug, Clone, Serialize)]
pub struct LwtNotification {
    pub channel_name: String,
    pub notification: LwtData,
}

#[derive(Debug, Clone, Serialize)]
pub struct LwtData {
    pub b: (f64, f64, f64), // best bid: [price, amount, total]
    pub a: (f64, f64, f64), // best ask: [price, amount, total]
    pub l: f64,             // last price
    pub m: f64,             // mark price
}

/// Index components channel notification (index_components.INDEX)
#[derive(Debug, Clone, Serialize)]
pub struct IndexComponentsNotification {
    pub channel_name: String,
    pub notification: IndexComponentsData,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexComponentsData {
    pub index_price: f64,
    pub components: std::collections::HashMap<String, f64>,
}

#[derive(Debug, Clone)]
pub struct TradeData {
    pub price: f64,
    pub quantity: f64,
    pub side: String,
    pub timestamp: f64,
    pub is_liquidation: bool,
    pub buy_order_id: Option<String>,
    pub sell_order_id: Option<String>,
}

// Serialize TradeData as tuple: [price, quantity, side, timestamp, is_liquidation]
impl serde::Serialize for TradeData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeTuple;
        let mut tuple = serializer.serialize_tuple(5)?;
        tuple.serialize_element(&self.price)?;
        tuple.serialize_element(&self.quantity)?;
        tuple.serialize_element(&self.side)?;
        tuple.serialize_element(&self.timestamp)?;
        tuple.serialize_element(&self.is_liquidation)?;
        tuple.end()
    }
}

#[derive(Debug, Clone)]
pub struct PriceLevelChange {
    pub price: f64,
    pub old_quantity: f64,
    pub new_quantity: f64,
}

impl serde::Serialize for PriceLevelChange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeTuple;
        let mut tuple = serializer.serialize_tuple(3)?;
        tuple.serialize_element(&self.price)?;
        tuple.serialize_element(&self.old_quantity)?;
        tuple.serialize_element(&self.new_quantity)?;
        tuple.end()
    }
}

/// Response from accounts service /api/me endpoint
#[derive(Debug, Deserialize)]
struct MeResponse {
    id: String,
}

/// Response from accounts service for order creation
#[derive(Debug, Deserialize)]
struct AccountsOrderResponse {
    order: AccountsOrder,
}

#[derive(Debug, Deserialize)]
struct AccountsOrder {
    id: String,
    quantity: String,
}

pub struct ChannelManager {
    /// Channel subscriptions: channel_name -> set of client_ids
    subscribers: HashMap<String, HashSet<u64>>,
    /// Client send channels: client_id -> broadcast sender
    client_senders: HashMap<u64, broadcast::Sender<String>>,
    /// Authenticated users: client_id -> user_id
    authenticated_users: HashMap<u64, Uuid>,
    /// Reverse lookup: user_id -> client_id (for targeted events)
    user_to_client: HashMap<Uuid, u64>,
    /// Order ownership: order_id -> user_id (for targeted OrderFilled/OrderCancelled events)
    order_owners: HashMap<Uuid, Uuid>,
    /// Bot state
    bot_client_id: Option<u64>,
    last_bot_status: Option<Vec<StrategyStatus>>,
}

impl ChannelManager {
    pub fn new() -> Self {
        Self {
            subscribers: HashMap::new(),
            client_senders: HashMap::new(),
            authenticated_users: HashMap::new(),
            user_to_client: HashMap::new(),
            order_owners: HashMap::new(),
            bot_client_id: None,
            last_bot_status: None,
        }
    }

    pub fn register_bot(&mut self, client_id: u64, sender: broadcast::Sender<String>) {
        self.bot_client_id = Some(client_id);
        self.client_senders.insert(client_id, sender);
        info!("Bot registered with client_id {}", client_id);
    }

    pub fn is_bot_connected(&self) -> bool {
        self.bot_client_id.is_some()
    }

    pub fn get_bot_sender(&self) -> Option<&broadcast::Sender<String>> {
        self.bot_client_id.and_then(|id| self.client_senders.get(&id))
    }

    pub fn set_bot_status(&mut self, status: Vec<StrategyStatus>) {
        self.last_bot_status = Some(status);
    }

    pub fn get_bot_status(&self) -> Option<&Vec<StrategyStatus>> {
        self.last_bot_status.as_ref()
    }

    pub fn authenticate_client(&mut self, client_id: u64, user_id: Uuid) {
        // Remove old mapping if user was connected on another client
        if let Some(old_client) = self.user_to_client.get(&user_id) {
            self.authenticated_users.remove(old_client);
        }
        self.authenticated_users.insert(client_id, user_id);
        self.user_to_client.insert(user_id, client_id);
        info!("Client {} authenticated as user {}", client_id, user_id);
    }

    pub fn get_user_id(&self, client_id: u64) -> Option<Uuid> {
        self.authenticated_users.get(&client_id).copied()
    }

    pub fn register_order(&mut self, order_id: Uuid, user_id: Uuid) {
        self.order_owners.insert(order_id, user_id);
    }

    pub fn get_order_owner(&self, order_id: &Uuid) -> Option<Uuid> {
        self.order_owners.get(order_id).copied()
    }

    pub fn get_client_for_user(&self, user_id: &Uuid) -> Option<u64> {
        self.user_to_client.get(user_id).copied()
    }

    pub fn subscribe(&mut self, client_id: u64, channel: String, sender: broadcast::Sender<String>) {
        self.subscribers
            .entry(channel.clone())
            .or_insert_with(HashSet::new)
            .insert(client_id);
        self.client_senders.insert(client_id, sender);
        info!("Client {} subscribed to channel {}", client_id, channel);
    }

    pub fn unsubscribe(&mut self, client_id: u64, channel: &str) {
        if let Some(subscribers) = self.subscribers.get_mut(channel) {
            subscribers.remove(&client_id);
            if subscribers.is_empty() {
                self.subscribers.remove(channel);
            }
        }
        info!("Client {} unsubscribed from channel {}", client_id, channel);
    }

    pub fn remove_client(&mut self, client_id: u64) {
        let channels: Vec<String> = self
            .subscribers
            .iter()
            .filter(|(_, subs)| subs.contains(&client_id))
            .map(|(channel, _)| channel.clone())
            .collect();

        for channel in channels {
            self.unsubscribe(client_id, &channel);
        }

        // Clean up auth mappings
        if let Some(user_id) = self.authenticated_users.remove(&client_id) {
            self.user_to_client.remove(&user_id);
            info!("Client {} (user {}) disconnected", client_id, user_id);
        }

        self.client_senders.remove(&client_id);

        // If this was the bot, clear bot state
        if self.bot_client_id == Some(client_id) {
            self.bot_client_id = None;
            self.last_bot_status = None;
            info!("Bot disconnected");
        }
    }

    pub fn get_subscribers(&self, channel: &str) -> Vec<u64> {
        self.subscribers
            .get(channel)
            .map(|subs| subs.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn get_sender(&self, client_id: u64) -> Option<&broadcast::Sender<String>> {
        self.client_senders.get(&client_id)
    }
}

pub async fn handle_websocket_connection(
    socket: WebSocket,
    client_id: u64,
    mut event_rx: broadcast::Receiver<MarketEvent>,
    channel_manager: Arc<tokio::sync::RwLock<ChannelManager>>,
    orderbook_states: Arc<tokio::sync::RwLock<HashMap<String, OrderBookState>>>,
    order_sender: Arc<UdpOrderSender>,
    proxy_state: ProxyState,
) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = broadcast::channel(1000);

    {
        let mut cm = channel_manager.write().await;
        cm.client_senders.insert(client_id, tx.clone());
    }

    let channel_manager_clone = channel_manager.clone();
    let channel_manager_for_events = channel_manager.clone();
    let orderbook_states_clone = orderbook_states.clone();
    let order_sender_clone = order_sender.clone();
    let client_id_clone = client_id;
    let tx_for_recv = tx.clone();

    // Forward targeted events (OrderFilled, OrderCancelled) only to the order owner
    let tx_for_events = tx.clone();
    tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            let cm = channel_manager_for_events.read().await;

            match &event {
                MarketEvent::OrderFilled { order_id } | MarketEvent::OrderCancelled { order_id, .. } => {
                    // Only send to the order owner
                    if let Some(user_id) = cm.get_order_owner(order_id) {
                        if let Some(owner_client_id) = cm.get_client_for_user(&user_id) {
                            if owner_client_id == client_id {
                                if let Ok(json) = serde_json::to_string(&event) {
                                    let _ = tx_for_events.send(json);
                                }
                            }
                        }
                    }
                }
                // Other events (Fill, OrderBookSnapshot, OrderBookDelta) are handled by the broadcaster
                _ => {}
            }
        }
    });

    let mut send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = receiver.next().await {
            info!("Client {} received message: {}", client_id_clone, &text[..text.len().min(200)]);

            // Parse as JSON-RPC style request
            let request: ClientRequest = match serde_json::from_str(&text) {
                Ok(req) => req,
                Err(e) => {
                    warn!("Invalid client message: {} - {}", text, e);
                    continue;
                }
            };

            let request_id = request.id.clone();

            match request.method.as_str() {
                "public/auth" => {
                    info!("Client {} auth request", client_id_clone);
                    let params: AuthParams = match serde_json::from_value(request.params) {
                        Ok(p) => p,
                        Err(_) => {
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_INVALID_PARAMS,
                                    message: "Invalid params: expected {token: string}".into(),
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                            continue;
                        }
                    };

                    let result = validate_token(&proxy_state, &params.token).await;

                    match result {
                        Ok(user_id) => {
                            let mut cm = channel_manager_clone.write().await;
                            cm.authenticate_client(client_id_clone, user_id);
                            let response = SuccessResponse {
                                id: request_id,
                                result: AuthResult {
                                    access_token: params.token,
                                    token_type: "bearer".into(),
                                    user_id: user_id.to_string(),
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                        }
                        Err(e) => {
                            warn!("Auth failed for client {}: {}", client_id_clone, e);
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_UNAUTHORIZED,
                                    message: e,
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                        }
                    }
                }

                "public/subscribe" => {
                    let params: SubscribeParams = match serde_json::from_value(request.params) {
                        Ok(p) => p,
                        Err(_) => {
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_INVALID_PARAMS,
                                    message: "Invalid params: expected {channels: string[]}".into(),
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                            continue;
                        }
                    };

                    let subscribed_channels = params.channels.clone();

                    for channel in params.channels {
                        // Parse channel type and symbol from channel name
                        let parts: Vec<&str> = channel.split('.').collect();
                        let channel_type = parts.get(0).copied().unwrap_or("");
                        let symbol = parts.get(1).copied().unwrap_or("KCN/EUR");
                        let interval = parts.last().copied().unwrap_or("1000ms");

                        // Send initial snapshot based on channel type
                        let ob_states = orderbook_states_clone.read().await;
                        if let Some(ob_state) = ob_states.get(symbol) {
                            match channel_type {
                                "book" => {
                                    let mut snapshot = ob_state.get_orderbook_snapshot(symbol);
                                    // Use the subscribed channel name in the snapshot
                                    snapshot.channel_name = channel.clone();
                                    let trades_count = snapshot.notification.trades.len();
                                    if let Ok(json) = serde_json::to_string(&snapshot) {
                                        let _ = tx_for_recv.send(json);
                                        info!("Sent orderbook snapshot to client {} for {} with {} trades", client_id_clone, symbol, trades_count);
                                    }
                                }
                                "ticker" => {
                                    let ticker = ob_state.get_ticker_notification(symbol, interval);
                                    if let Ok(json) = serde_json::to_string(&ticker) {
                                        let _ = tx_for_recv.send(json);
                                        info!("Sent ticker snapshot to client {} for {}", client_id_clone, symbol);
                                    }
                                }
                                "lwt" => {
                                    let lwt = ob_state.get_lwt_notification(symbol, interval);
                                    if let Ok(json) = serde_json::to_string(&lwt) {
                                        let _ = tx_for_recv.send(json);
                                        info!("Sent lwt snapshot to client {} for {}", client_id_clone, symbol);
                                    }
                                }
                                _ => {
                                    info!("Unknown channel type: {}", channel_type);
                                }
                            }
                        }
                        drop(ob_states);

                        let mut cm = channel_manager_clone.write().await;
                        cm.subscribe(client_id_clone, channel, tx_for_recv.clone());
                    }

                    // Send subscription confirmation
                    let response = SuccessResponse {
                        id: request_id,
                        result: subscribed_channels,
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = tx_for_recv.send(json);
                    }
                }

                "private/subscribe" => {
                    // Private subscriptions require authentication
                    let cm = channel_manager_clone.read().await;
                    let user_id = cm.get_user_id(client_id_clone);
                    drop(cm);

                    if user_id.is_none() {
                        let response = ErrorResponse {
                            id: request_id,
                            error: RpcError {
                                code: ERR_UNAUTHORIZED,
                                message: "Not authenticated".into(),
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&response) {
                            let _ = tx_for_recv.send(json);
                        }
                        continue;
                    }

                    let params: SubscribeParams = match serde_json::from_value(request.params) {
                        Ok(p) => p,
                        Err(_) => {
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_INVALID_PARAMS,
                                    message: "Invalid params: expected {channels: string[]}".into(),
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                            continue;
                        }
                    };

                    let subscribed_channels = params.channels.clone();

                    for channel in params.channels {
                        let mut cm = channel_manager_clone.write().await;
                        cm.subscribe(client_id_clone, channel, tx_for_recv.clone());
                    }

                    let response = SuccessResponse {
                        id: request_id,
                        result: subscribed_channels,
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = tx_for_recv.send(json);
                    }
                }

                "unsubscribe" => {
                    let params: SubscribeParams = match serde_json::from_value(request.params) {
                        Ok(p) => p,
                        Err(_) => {
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_INVALID_PARAMS,
                                    message: "Invalid params: expected {channels: string[]}".into(),
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                            continue;
                        }
                    };

                    let unsubscribed_channels = params.channels.clone();

                    for channel in params.channels {
                        let mut cm = channel_manager_clone.write().await;
                        cm.unsubscribe(client_id_clone, &channel);
                    }

                    let response = SuccessResponse {
                        id: request_id,
                        result: unsubscribed_channels,
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = tx_for_recv.send(json);
                    }
                }

                "private/place_order" => {
                    let cm = channel_manager_clone.read().await;
                    let user_id = cm.get_user_id(client_id_clone);
                    drop(cm);

                    if user_id.is_none() {
                        let response = ErrorResponse {
                            id: request_id,
                            error: RpcError {
                                code: ERR_UNAUTHORIZED,
                                message: "Not authenticated".into(),
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&response) {
                            let _ = tx_for_recv.send(json);
                        }
                        continue;
                    }

                    let user_id = user_id.unwrap();

                    let params: PlaceOrderParams = match serde_json::from_value(request.params) {
                        Ok(p) => p,
                        Err(_) => {
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_INVALID_PARAMS,
                                    message: "Invalid params for place_order".into(),
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                            continue;
                        }
                    };

                    info!("Client {} PlaceOrder: symbol={}, side={}, type={}, price={:?}, qty={:?}",
                        client_id_clone, params.symbol, params.side, params.order_type, params.price, params.quantity);

                    // Call accounts service to create order and lock funds
                    info!("Client {} calling create_order_in_accounts...", client_id_clone);
                    let result = create_order_in_accounts(
                        &proxy_state,
                        &params.symbol,
                        &params.side,
                        &params.order_type,
                        params.price,
                        params.quantity,
                        params.quote_amount,
                        params.max_slippage_price,
                        user_id,
                    ).await;
                    info!("Client {} create_order_in_accounts result: {:?}", client_id_clone, result.is_ok());

                    match result {
                        Ok((order_id, qty)) => {
                            info!("Client {} order created in accounts: order_id={}, qty={}", client_id_clone, order_id, qty);
                            {
                                let mut cm = channel_manager_clone.write().await;
                                cm.register_order(order_id, user_id);
                            }

                            let side_enum = match params.side.as_str() {
                                "bid" => Side::Bid,
                                "ask" => Side::Ask,
                                _ => {
                                    let response = ErrorResponse {
                                        id: request_id,
                                        error: RpcError {
                                            code: ERR_INVALID_PARAMS,
                                            message: "Invalid side".into(),
                                        },
                                    };
                                    if let Ok(json) = serde_json::to_string(&response) {
                                        let _ = tx_for_recv.send(json);
                                    }
                                    continue;
                                }
                            };

                            let command = OrderCommand::PlaceOrder {
                                order_id,
                                side: side_enum,
                                order_type: params.order_type.clone(),
                                price: params.price,
                                quantity: qty,
                                user_id: Some(user_id),
                            };

                            info!("Sending order to matching engine: {:?}", command);
                            if let Err(e) = order_sender_clone.send_order_command(&command).await {
                                error!("Failed to send order to matching engine: {}", e);
                                let response = ErrorResponse {
                                    id: request_id,
                                    error: RpcError {
                                        code: ERR_ORDER_REJECTED,
                                        message: "Failed to submit to matching engine".into(),
                                    },
                                };
                                if let Ok(json) = serde_json::to_string(&response) {
                                    let _ = tx_for_recv.send(json);
                                }
                            } else {
                                let response = SuccessResponse {
                                    id: request_id,
                                    result: OrderResult {
                                        order_id: order_id.to_string(),
                                    },
                                };
                                if let Ok(json) = serde_json::to_string(&response) {
                                    let _ = tx_for_recv.send(json);
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Client {} create_order_in_accounts error: {}", client_id_clone, e);
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_ORDER_REJECTED,
                                    message: e,
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                        }
                    }
                }

                "private/cancel_order" => {
                    let cm = channel_manager_clone.read().await;
                    let user_id = cm.get_user_id(client_id_clone);
                    drop(cm);

                    if user_id.is_none() {
                        let response = ErrorResponse {
                            id: request_id,
                            error: RpcError {
                                code: ERR_UNAUTHORIZED,
                                message: "Not authenticated".into(),
                            },
                        };
                        if let Ok(json) = serde_json::to_string(&response) {
                            let _ = tx_for_recv.send(json);
                        }
                        continue;
                    }

                    let user_id = user_id.unwrap();

                    let params: CancelOrderParams = match serde_json::from_value(request.params) {
                        Ok(p) => p,
                        Err(_) => {
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_INVALID_PARAMS,
                                    message: "Invalid params: expected {order_id: string}".into(),
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                            continue;
                        }
                    };

                    let order_uuid = match params.order_id.parse::<Uuid>() {
                        Ok(id) => id,
                        Err(_) => {
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_INVALID_PARAMS,
                                    message: "Invalid order ID".into(),
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                            continue;
                        }
                    };

                    let result = cancel_order_in_accounts(&proxy_state, &params.order_id, user_id).await;

                    match result {
                        Ok(()) => {
                            let command = OrderCommand::CancelOrder {
                                order_id: order_uuid,
                                user_id: Some(user_id),
                            };

                            if let Err(e) = order_sender_clone.send_order_command(&command).await {
                                error!("Failed to send cancel to matching engine: {}", e);
                            }

                            let response = SuccessResponse {
                                id: request_id,
                                result: CancelResult {
                                    order_id: params.order_id,
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                        }
                        Err(e) => {
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_CANCEL_FAILED,
                                    message: e,
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                        }
                    }
                }

                "private/register_bot" => {
                    let mut cm = channel_manager_clone.write().await;
                    cm.register_bot(client_id_clone, tx_for_recv.clone());

                    let response = SuccessResponse {
                        id: request_id,
                        result: "ok",
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = tx_for_recv.send(json);
                    }
                }

                "private/bot_status" => {
                    let params: BotStatusParams = match serde_json::from_value(request.params) {
                        Ok(p) => p,
                        Err(_) => {
                            let response = ErrorResponse {
                                id: request_id,
                                error: RpcError {
                                    code: ERR_INVALID_PARAMS,
                                    message: "Invalid params for bot_status".into(),
                                },
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                            continue;
                        }
                    };

                    let mut cm = channel_manager_clone.write().await;
                    cm.set_bot_status(params.strategies);
                    info!("Received bot status update");

                    let response = SuccessResponse {
                        id: request_id,
                        result: "ok",
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = tx_for_recv.send(json);
                    }
                }

                _ => {
                    warn!("Unknown method: {}", request.method);
                    let response = ErrorResponse {
                        id: request_id,
                        error: RpcError {
                            code: ERR_METHOD_NOT_FOUND,
                            message: format!("Method not found: {}", request.method),
                        },
                    };
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = tx_for_recv.send(json);
                    }
                }
            }
        }

        let mut cm = channel_manager_clone.write().await;
        cm.remove_client(client_id_clone);
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };
}

/// Validate JWT token by calling accounts service /api/me
async fn validate_token(proxy_state: &ProxyState, token: &str) -> Result<Uuid, String> {
    let response = proxy_state.client
        .get(format!("{}/api/me", proxy_state.accounts_url))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("Failed to validate token: {}", e))?;

    if !response.status().is_success() {
        return Err("Invalid or expired token".into());
    }

    let me: MeResponse = response.json().await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    me.id.parse().map_err(|_| "Invalid user ID in response".into())
}

/// Create order in accounts service (locks funds)
async fn create_order_in_accounts(
    proxy_state: &ProxyState,
    symbol: &str,
    side: &str,
    order_type: &str,
    price: Option<Decimal>,
    quantity: Option<Decimal>,
    quote_amount: Option<Decimal>,
    max_slippage_price: Option<Decimal>,
    user_id: Uuid,
) -> Result<(Uuid, Decimal), String> {
    // We need to use internal auth - accounts service needs to trust gateway
    // For now, we'll use a service-to-service token pattern
    // The accounts service should have an internal endpoint or accept a special header

    let body = serde_json::json!({
        "symbol": symbol,
        "side": side,
        "order_type": order_type,
        "price": price,
        "quantity": quantity,
        "quote_amount": quote_amount,
        "max_slippage_price": max_slippage_price,
    });

    let response = proxy_state.client
        .post(format!("{}/internal/orders", proxy_state.accounts_url))
        .header("Content-Type", "application/json")
        .header("X-User-Id", user_id.to_string())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Failed to create order: {}", e))?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        if let Ok(err) = serde_json::from_str::<serde_json::Value>(&error_text) {
            let msg = err.get("error").and_then(|e| e.as_str()).unwrap_or("Order rejected");
            return Err(msg.into());
        }
        return Err("Order rejected".into());
    }

    let order_response: AccountsOrderResponse = response.json().await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    let order_id: Uuid = order_response.order.id.parse()
        .map_err(|_| "Invalid order ID")?;
    let quantity: Decimal = order_response.order.quantity.parse()
        .map_err(|_| "Invalid quantity")?;

    Ok((order_id, quantity))
}

/// Cancel order in accounts service (unlocks funds)
async fn cancel_order_in_accounts(
    proxy_state: &ProxyState,
    order_id: &str,
    user_id: Uuid,
) -> Result<(), String> {
    let response = proxy_state.client
        .delete(format!("{}/internal/orders/{}", proxy_state.accounts_url, order_id))
        .header("X-User-Id", user_id.to_string())
        .send()
        .await
        .map_err(|e| format!("Failed to cancel order: {}", e))?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        if let Ok(err) = serde_json::from_str::<serde_json::Value>(&error_text) {
            let msg = err.get("error").and_then(|e| e.as_str()).unwrap_or("Cancel failed");
            return Err(msg.into());
        }
        return Err("Cancel failed".into());
    }

    Ok(())
}
