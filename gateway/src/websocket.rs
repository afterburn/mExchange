use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, warn, error};
use uuid::Uuid;

use crate::channel_updates::OrderBookState;
use crate::events::{MarketEvent, OrderCommand, Side};
use crate::proxy::ProxyState;
use crate::udp_transport::UdpOrderSender;


/// All possible client message types
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Authenticate with JWT token
    #[serde(rename = "auth")]
    Auth { token: String },

    /// Subscribe to a channel
    #[serde(rename = "subscribe")]
    Subscribe { channel: String },

    /// Unsubscribe from a channel
    #[serde(rename = "unsubscribe")]
    Unsubscribe { channel: String },

    /// Place an order (requires auth)
    #[serde(rename = "place_order")]
    PlaceOrder {
        symbol: String,
        side: String,
        order_type: String,
        #[serde(default)]
        price: Option<Decimal>,
        #[serde(default)]
        quantity: Option<Decimal>,
        #[serde(default)]
        quote_amount: Option<Decimal>,
        #[serde(default)]
        max_slippage_price: Option<Decimal>,
    },

    /// Cancel an order (requires auth)
    #[serde(rename = "cancel_order")]
    CancelOrder { order_id: String },

    /// Bot registration
    #[serde(rename = "register_bot")]
    RegisterBot,

    /// Bot status update
    #[serde(rename = "bot_status")]
    BotStatus { strategies: Vec<StrategyStatus> },
}

/// Server response messages
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Authentication result
    #[serde(rename = "auth_result")]
    AuthResult { success: bool, user_id: Option<String>, error: Option<String> },

    /// Order placement result
    #[serde(rename = "order_result")]
    OrderResult { success: bool, order_id: Option<String>, error: Option<String> },

    /// Order cancellation result
    #[serde(rename = "cancel_result")]
    CancelResult { success: bool, order_id: String, error: Option<String> },

    /// Error message
    #[serde(rename = "error")]
    Error { message: String },
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
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats24h {
    pub high_24h: f64,
    pub low_24h: f64,
    pub volume_24h: f64,
    pub open_24h: f64,
    pub last_price: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TradeData {
    pub price: f64,
    pub quantity: f64,
    pub side: String,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buy_order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sell_order_id: Option<String>,
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
            match serde_json::from_str::<ClientMessage>(&text) {
                Ok(ClientMessage::Auth { token }) => {
                    info!("Client {} auth request", client_id_clone);
                    // Validate token by calling accounts service
                    let result = validate_token(&proxy_state, &token).await;

                    let response = match result {
                        Ok(user_id) => {
                            let mut cm = channel_manager_clone.write().await;
                            cm.authenticate_client(client_id_clone, user_id);
                            ServerMessage::AuthResult {
                                success: true,
                                user_id: Some(user_id.to_string()),
                                error: None,
                            }
                        }
                        Err(e) => {
                            warn!("Auth failed for client {}: {}", client_id_clone, e);
                            ServerMessage::AuthResult {
                                success: false,
                                user_id: None,
                                error: Some(e),
                            }
                        }
                    };

                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = tx_for_recv.send(json);
                    }
                }

                Ok(ClientMessage::Subscribe { channel }) => {
                    let symbol = channel
                        .strip_prefix("book.")
                        .and_then(|s| s.split('.').next())
                        .unwrap_or("KCN/EUR");

                    let ob_states = orderbook_states_clone.read().await;
                    if let Some(ob_state) = ob_states.get(symbol) {
                        let snapshot = ob_state.get_orderbook_snapshot(symbol);
                        if let Ok(json) = serde_json::to_string(&snapshot) {
                            let _ = tx_for_recv.send(json);
                            info!("Sent orderbook snapshot to client {} for {}", client_id_clone, symbol);
                        }
                    }
                    drop(ob_states);

                    let mut cm = channel_manager_clone.write().await;
                    cm.subscribe(client_id_clone, channel, tx_for_recv.clone());
                }

                Ok(ClientMessage::Unsubscribe { channel }) => {
                    let mut cm = channel_manager_clone.write().await;
                    cm.unsubscribe(client_id_clone, &channel);
                }

                Ok(ClientMessage::PlaceOrder { symbol, side, order_type, price, quantity, quote_amount, max_slippage_price }) => {
                    info!("Client {} PlaceOrder: symbol={}, side={}, type={}, price={:?}, qty={:?}",
                        client_id_clone, symbol, side, order_type, price, quantity);
                    let cm = channel_manager_clone.read().await;
                    let user_id = cm.get_user_id(client_id_clone);
                    drop(cm);

                    if user_id.is_none() {
                        let response = ServerMessage::OrderResult {
                            success: false,
                            order_id: None,
                            error: Some("Not authenticated".into()),
                        };
                        if let Ok(json) = serde_json::to_string(&response) {
                            let _ = tx_for_recv.send(json);
                        }
                        continue;
                    }

                    let user_id = user_id.unwrap();

                    // Call accounts service to create order and lock funds
                    info!("Client {} calling create_order_in_accounts...", client_id_clone);
                    let result = create_order_in_accounts(
                        &proxy_state,
                        &symbol,
                        &side,
                        &order_type,
                        price,
                        quantity,
                        quote_amount,
                        max_slippage_price,
                        user_id,
                    ).await;
                    info!("Client {} create_order_in_accounts result: {:?}", client_id_clone, result.is_ok());

                    let response = match result {
                        Ok((order_id, qty)) => {
                            info!("Client {} order created in accounts: order_id={}, qty={}", client_id_clone, order_id, qty);
                            // Register order ownership for targeted events
                            {
                                let mut cm = channel_manager_clone.write().await;
                                cm.register_order(order_id, user_id);
                            }

                            // Parse side
                            let side_enum = match side.as_str() {
                                "bid" => Side::Bid,
                                "ask" => Side::Ask,
                                _ => {
                                    let response = ServerMessage::OrderResult {
                                        success: false,
                                        order_id: None,
                                        error: Some("Invalid side".into()),
                                    };
                                    if let Ok(json) = serde_json::to_string(&response) {
                                        let _ = tx_for_recv.send(json);
                                    }
                                    continue;
                                }
                            };

                            // Send to matching engine
                            let command = OrderCommand::PlaceOrder {
                                order_id,
                                side: side_enum,
                                order_type: order_type.clone(),
                                price,
                                quantity: qty,
                                user_id: Some(user_id),
                            };

                            info!("Sending order to matching engine: {:?}", command);
                            if let Err(e) = order_sender_clone.send_order_command(&command).await {
                                error!("Failed to send order to matching engine: {}", e);
                                // TODO: Compensating transaction to cancel order in accounts
                                ServerMessage::OrderResult {
                                    success: false,
                                    order_id: Some(order_id.to_string()),
                                    error: Some("Failed to submit to matching engine".into()),
                                }
                            } else {
                                ServerMessage::OrderResult {
                                    success: true,
                                    order_id: Some(order_id.to_string()),
                                    error: None,
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Client {} create_order_in_accounts error: {}", client_id_clone, e);
                            ServerMessage::OrderResult {
                                success: false,
                                order_id: None,
                                error: Some(e),
                            }
                        }
                    };

                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = tx_for_recv.send(json);
                    }
                }

                Ok(ClientMessage::CancelOrder { order_id }) => {
                    let cm = channel_manager_clone.read().await;
                    let user_id = cm.get_user_id(client_id_clone);
                    drop(cm);

                    if user_id.is_none() {
                        let response = ServerMessage::CancelResult {
                            success: false,
                            order_id: order_id.clone(),
                            error: Some("Not authenticated".into()),
                        };
                        if let Ok(json) = serde_json::to_string(&response) {
                            let _ = tx_for_recv.send(json);
                        }
                        continue;
                    }

                    let user_id = user_id.unwrap();

                    // Parse order_id
                    let order_uuid = match order_id.parse::<Uuid>() {
                        Ok(id) => id,
                        Err(_) => {
                            let response = ServerMessage::CancelResult {
                                success: false,
                                order_id: order_id.clone(),
                                error: Some("Invalid order ID".into()),
                            };
                            if let Ok(json) = serde_json::to_string(&response) {
                                let _ = tx_for_recv.send(json);
                            }
                            continue;
                        }
                    };

                    // Cancel in accounts service first
                    let result = cancel_order_in_accounts(&proxy_state, &order_id, user_id).await;

                    let response = match result {
                        Ok(()) => {
                            // Send cancel to matching engine
                            let command = OrderCommand::CancelOrder {
                                order_id: order_uuid,
                                user_id: Some(user_id),
                            };

                            if let Err(e) = order_sender_clone.send_order_command(&command).await {
                                error!("Failed to send cancel to matching engine: {}", e);
                                // Order is already cancelled in accounts, this is okay
                            }

                            ServerMessage::CancelResult {
                                success: true,
                                order_id,
                                error: None,
                            }
                        }
                        Err(e) => {
                            ServerMessage::CancelResult {
                                success: false,
                                order_id,
                                error: Some(e),
                            }
                        }
                    };

                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = tx_for_recv.send(json);
                    }
                }

                Ok(ClientMessage::RegisterBot) => {
                    let mut cm = channel_manager_clone.write().await;
                    cm.register_bot(client_id_clone, tx_for_recv.clone());
                }

                Ok(ClientMessage::BotStatus { strategies }) => {
                    let mut cm = channel_manager_clone.write().await;
                    cm.set_bot_status(strategies);
                    info!("Received bot status update");
                }

                Err(e) => {
                    warn!("Invalid client message: {} - {}", text, e);
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
