use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::types::{GatewayMessage, MarketState, OpenOrder, OrderRequest, ChannelNotification, TaggedMessage, Side, TickerNotification, LwtNotification};

/// Request ID counter for JSON-RPC style requests
static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn get_next_request_id() -> String {
    REQUEST_ID_COUNTER.fetch_add(1, Ordering::SeqCst).to_string()
}

/// JSON-RPC style WebSocket request
#[derive(Debug, serde::Serialize)]
struct WsRequest<P: serde::Serialize> {
    id: String,
    method: String,
    params: P,
}

/// Auth parameters
#[derive(Debug, serde::Serialize)]
struct AuthParams {
    token: String,
}

/// Subscribe parameters
#[derive(Debug, serde::Serialize)]
struct SubscribeParams {
    channels: Vec<String>,
}

/// Place order parameters
#[derive(Debug, serde::Serialize)]
struct PlaceOrderParams {
    symbol: String,
    side: String,
    order_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    price: Option<rust_decimal::Decimal>,
    quantity: rust_decimal::Decimal,
}

/// Cancel order parameters
#[derive(Debug, serde::Serialize)]
struct CancelOrderParams {
    order_id: String,
}

/// Auth response from accounts service
#[derive(Debug, serde::Deserialize)]
struct AuthResponse {
    access_token: String,
}

pub struct GatewayClient {
    accounts_url: String,
    ws_url: String,
    market_state: Arc<RwLock<MarketState>>,
    ws_sender: Arc<RwLock<Option<mpsc::Sender<String>>>>,
    access_token: Arc<RwLock<Option<String>>>,
    /// Track open orders by ID
    open_orders: Arc<RwLock<HashMap<Uuid, OpenOrder>>>,
    /// Track inventory (positive = long, negative = short)
    inventory: Arc<RwLock<rust_decimal::Decimal>>,
}

impl GatewayClient {
    pub fn new(gateway_host: &str, gateway_port: u16) -> Self {
        let ws_url = format!("ws://{}:{}/ws", gateway_host, gateway_port);
        // Use gateway as proxy for accounts API (gateway proxies /auth/* to accounts)
        let accounts_url = format!("http://{}:{}", gateway_host, gateway_port);

        Self {
            accounts_url,
            ws_url,
            market_state: Arc::new(RwLock::new(MarketState::new())),
            ws_sender: Arc::new(RwLock::new(None)),
            access_token: Arc::new(RwLock::new(None)),
            open_orders: Arc::new(RwLock::new(HashMap::new())),
            inventory: Arc::new(RwLock::new(rust_decimal::Decimal::ZERO)),
        }
    }

    pub fn open_orders(&self) -> Arc<RwLock<HashMap<Uuid, OpenOrder>>> {
        Arc::clone(&self.open_orders)
    }

    pub fn inventory(&self) -> Arc<RwLock<rust_decimal::Decimal>> {
        Arc::clone(&self.inventory)
    }

    /// Login to accounts service and get access token
    pub async fn login(&self, email: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = reqwest::Client::new();

        // Use dev-login endpoint for bot
        let response = client
            .post(format!("{}/auth/dev-login", self.accounts_url))
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("Login failed: {}", error_text).into());
        }

        let auth: AuthResponse = response.json().await?;

        let mut token = self.access_token.write().await;
        *token = Some(auth.access_token);

        info!("Bot logged in successfully");
        Ok(())
    }

    /// Deposit funds for the bot
    pub async fn deposit(&self, asset: &str, amount: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let token = self.access_token.read().await;
        let token = token.as_ref().ok_or("Not logged in")?;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/api/balances/deposit", self.accounts_url))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({ "asset": asset, "amount": amount }))
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("Deposit failed: {}", error_text).into());
        }

        info!("Deposited {} {}", amount, asset);
        Ok(())
    }

    /// Claim faucet for KCN
    pub async fn claim_faucet(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let token = self.access_token.read().await;
        let token = token.as_ref().ok_or("Not logged in")?;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/api/faucet/claim", self.accounts_url))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({ "asset": "KCN" }))
            .send()
            .await?;

        if !response.status().is_success() {
            // Faucet might be on cooldown, that's okay
            let error_text = response.text().await.unwrap_or_default();
            warn!("Faucet claim: {}", error_text);
            return Ok(());
        }

        info!("Claimed faucet KCN");
        Ok(())
    }

    /// Mint tokens for the bot (internal API)
    pub async fn mint(&self, asset: &str, amount: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = reqwest::Client::new();

        // We need to call the internal API which requires X-User-Id header
        // First get user ID from /api/me
        let token = self.access_token.read().await;
        let token = token.as_ref().ok_or("Not logged in")?;

        let me_response = client
            .get(format!("{}/api/me", self.accounts_url))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;

        if !me_response.status().is_success() {
            return Err("Failed to get user ID".into());
        }

        let me: serde_json::Value = me_response.json().await?;
        let user_id = me.get("id").and_then(|v| v.as_str()).ok_or("No user ID")?;

        // Now call internal mint endpoint
        let response = client
            .post(format!("{}/internal/mint", self.accounts_url))
            .header("X-User-Id", user_id)
            .json(&serde_json::json!({ "asset": asset, "amount": amount }))
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("Mint failed: {}", error_text).into());
        }

        info!("Minted {} {}", amount, asset);
        Ok(())
    }

    pub fn market_state(&self) -> Arc<RwLock<MarketState>> {
        Arc::clone(&self.market_state)
    }

    pub async fn connect_websocket(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.connect_websocket_with_state(Arc::clone(&self.market_state)).await
    }

    /// Connect to WebSocket using a shared market state (for multi-strategy setups)
    pub async fn connect_websocket_with_state(&self, market_state: Arc<RwLock<MarketState>>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Connecting to WebSocket: {}", self.ws_url);

        let (ws_stream, _) = connect_async(&self.ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        // Create channel for sending messages
        let (tx, mut rx) = mpsc::channel::<String>(100);

        // Store sender for order submission
        {
            let mut sender = self.ws_sender.write().await;
            *sender = Some(tx);
        }

        let market_state = market_state;
        let open_orders = Arc::clone(&self.open_orders);
        let inventory = Arc::clone(&self.inventory);

        // Spawn task to handle outgoing messages
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = write.send(Message::Text(msg)).await {
                    error!("Failed to send WebSocket message: {}", e);
                    break;
                }
            }
        });

        // Handle incoming messages
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Err(e) = Self::handle_message(&text, &market_state, &open_orders, &inventory).await {
                            warn!("Failed to handle message: {}", e);
                        }
                    }
                    Ok(Message::Close(_)) => {
                        info!("WebSocket closed");
                        break;
                    }
                    Ok(Message::Ping(_)) => {
                        debug!("Received ping");
                    }
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        // Authenticate on WebSocket if we have a token
        self.authenticate_websocket().await?;

        // Subscribe to orderbook channel
        self.subscribe("book.KCN/EUR.none.10.100ms").await?;

        Ok(())
    }

    /// Send auth message on WebSocket
    async fn authenticate_websocket(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let token = self.access_token.read().await;
        if let Some(token) = token.as_ref() {
            let request = WsRequest {
                id: get_next_request_id(),
                method: "public/auth".to_string(),
                params: AuthParams { token: token.clone() },
            };
            let json = serde_json::to_string(&request)?;

            let sender = self.ws_sender.read().await;
            if let Some(tx) = sender.as_ref() {
                tx.send(json).await?;
                info!("Sent WebSocket auth");
                // Give server time to process auth
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        } else {
            warn!("No access token - WebSocket not authenticated");
        }
        Ok(())
    }

    async fn subscribe(&self, channel: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let request = WsRequest {
            id: get_next_request_id(),
            method: "public/subscribe".to_string(),
            params: SubscribeParams {
                channels: vec![channel.to_string()],
            },
        };
        let json = serde_json::to_string(&request)?;

        let sender = self.ws_sender.read().await;
        if let Some(tx) = sender.as_ref() {
            tx.send(json).await?;
            info!("Subscribed to channel: {}", channel);
        }
        Ok(())
    }

    async fn handle_message(
        text: &str,
        market_state: &Arc<RwLock<MarketState>>,
        open_orders: &Arc<RwLock<HashMap<Uuid, OpenOrder>>>,
        inventory: &Arc<RwLock<rust_decimal::Decimal>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let msg: GatewayMessage = serde_json::from_str(text)?;

        match msg {
            GatewayMessage::RpcResponse(resp) => {
                debug!("RPC response for request {}: {:?}", resp.id, resp.result);
            }
            GatewayMessage::RpcError(err) => {
                warn!("RPC error for request {}: {} (code {})", err.id, err.error.message, err.error.code);
            }
            GatewayMessage::ChannelNotification(notif) => {
                Self::handle_channel_notification(notif, market_state).await;
            }
            GatewayMessage::TickerNotification(notif) => {
                Self::handle_ticker_notification(notif, market_state).await;
            }
            GatewayMessage::LwtNotification(notif) => {
                Self::handle_lwt_notification(notif, market_state).await;
            }
            GatewayMessage::Tagged(tagged) => {
                Self::handle_tagged_message(tagged, market_state, open_orders, inventory).await;
            }
        }

        Ok(())
    }

    async fn handle_channel_notification(
        notif: ChannelNotification,
        market_state: &Arc<RwLock<MarketState>>,
    ) {
        let data = notif.notification;
        let mut state = market_state.write().await;

        // Update best bid from bid_changes (find highest bid with qty > 0)
        let mut best_bid_price: Option<f64> = None;
        for (price, _old_qty, new_qty) in &data.bid_changes {
            if *new_qty > 0.0 {
                if best_bid_price.is_none() || *price > best_bid_price.unwrap() {
                    best_bid_price = Some(*price);
                }
            }
        }
        if let Some(price) = best_bid_price {
            state.best_bid = Some(rust_decimal::Decimal::from_f64_retain(price).unwrap_or_default());
        }

        // Update best ask from ask_changes (find lowest ask with qty > 0)
        let mut best_ask_price: Option<f64> = None;
        for (price, _old_qty, new_qty) in &data.ask_changes {
            if *new_qty > 0.0 {
                if best_ask_price.is_none() || *price < best_ask_price.unwrap() {
                    best_ask_price = Some(*price);
                }
            }
        }
        if let Some(price) = best_ask_price {
            state.best_ask = Some(rust_decimal::Decimal::from_f64_retain(price).unwrap_or_default());
        }

        // Update from trades
        for trade in &data.trades {
            let price = rust_decimal::Decimal::from_f64_retain(trade.price).unwrap_or_default();
            state.update_price(price);
        }

        debug!(
            "Channel notification: bid={:?}, ask={:?}",
            state.best_bid, state.best_ask
        );
    }

    async fn handle_ticker_notification(
        notif: TickerNotification,
        market_state: &Arc<RwLock<MarketState>>,
    ) {
        let data = notif.notification;
        let mut state = market_state.write().await;

        // Update state from ticker data
        if data.best_bid_price > 0.0 {
            state.best_bid = Some(rust_decimal::Decimal::from_f64_retain(data.best_bid_price).unwrap_or_default());
        }
        if data.best_ask_price > 0.0 {
            state.best_ask = Some(rust_decimal::Decimal::from_f64_retain(data.best_ask_price).unwrap_or_default());
        }
        if data.last_price > 0.0 {
            let price = rust_decimal::Decimal::from_f64_retain(data.last_price).unwrap_or_default();
            state.update_price(price);
        }

        debug!(
            "Ticker notification: bid={:?}, ask={:?}, last={:?}",
            state.best_bid, state.best_ask, data.last_price
        );
    }

    async fn handle_lwt_notification(
        notif: LwtNotification,
        market_state: &Arc<RwLock<MarketState>>,
    ) {
        let data = notif.notification;
        let mut state = market_state.write().await;

        // LWT format: b = (bid_price, bid_amount, bid_total), a = (ask_price, ask_amount, ask_total)
        if data.b.0 > 0.0 {
            state.best_bid = Some(rust_decimal::Decimal::from_f64_retain(data.b.0).unwrap_or_default());
        }
        if data.a.0 > 0.0 {
            state.best_ask = Some(rust_decimal::Decimal::from_f64_retain(data.a.0).unwrap_or_default());
        }
        if data.l > 0.0 {
            let price = rust_decimal::Decimal::from_f64_retain(data.l).unwrap_or_default();
            state.update_price(price);
        }

        debug!(
            "LWT notification: bid={:?}, ask={:?}, last={}",
            state.best_bid, state.best_ask, data.l
        );
    }

    async fn handle_tagged_message(
        msg: TaggedMessage,
        market_state: &Arc<RwLock<MarketState>>,
        open_orders: &Arc<RwLock<HashMap<Uuid, OpenOrder>>>,
        inventory: &Arc<RwLock<rust_decimal::Decimal>>,
    ) {
        match msg {
            TaggedMessage::OrderbookSnapshot { bids, asks, .. } => {
                let mut state = market_state.write().await;
                if let Some(best_bid) = bids.first() {
                    state.best_bid = Some(best_bid.price);
                }
                if let Some(best_ask) = asks.first() {
                    state.best_ask = Some(best_ask.price);
                }
                debug!(
                    "Orderbook snapshot: bid={:?}, ask={:?}",
                    state.best_bid, state.best_ask
                );
            }
            TaggedMessage::OrderbookUpdate { bids, asks, .. } => {
                let mut state = market_state.write().await;
                if let Some(best_bid) = bids.first() {
                    state.best_bid = Some(best_bid.price);
                }
                if let Some(best_ask) = asks.first() {
                    state.best_ask = Some(best_ask.price);
                }
            }
            TaggedMessage::Trade { price, .. } => {
                let mut state = market_state.write().await;
                state.update_price(price);
                debug!("Trade: price={}", price);
            }
            TaggedMessage::OrderFilled { order_id } => {
                // Parse order_id and update inventory based on the filled order
                if let Ok(uuid) = Uuid::parse_str(&order_id) {
                    let mut orders = open_orders.write().await;
                    if let Some(order) = orders.remove(&uuid) {
                        // Update inventory based on the order side
                        let mut inv = inventory.write().await;
                        match order.side {
                            Side::Bid => *inv += order.quantity,  // Bought = increase inventory
                            Side::Ask => *inv -= order.quantity,  // Sold = decrease inventory
                        }
                        info!("Order filled: {} ({:?} {} @ {:?}), inventory now: {}",
                            order_id, order.side, order.quantity, order.price, *inv);
                    } else {
                        debug!("Order filled but not tracked: {}", order_id);
                    }
                }
            }
            TaggedMessage::OrderCancelled { order_id, .. } => {
                // Remove from tracked orders
                if let Ok(uuid) = Uuid::parse_str(&order_id) {
                    let mut orders = open_orders.write().await;
                    if orders.remove(&uuid).is_some() {
                        debug!("Order cancelled and removed: {}", order_id);
                    }
                }
            }
            TaggedMessage::Unknown => {
                debug!("Unknown tagged message type");
            }
        }
    }

    pub async fn submit_order(
        &self,
        order: &OrderRequest,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let request = WsRequest {
            id: get_next_request_id(),
            method: "private/place_order".to_string(),
            params: PlaceOrderParams {
                symbol: order.symbol.clone(),
                side: match order.side {
                    crate::types::Side::Bid => "bid".to_string(),
                    crate::types::Side::Ask => "ask".to_string(),
                },
                order_type: match order.order_type {
                    crate::types::OrderType::Limit => "limit".to_string(),
                    crate::types::OrderType::Market => "market".to_string(),
                },
                price: order.price,
                quantity: order.quantity,
            },
        };

        let json = serde_json::to_string(&request)?;

        let sender = self.ws_sender.read().await;
        if let Some(tx) = sender.as_ref() {
            tx.send(json).await?;
            debug!("Order submitted via WebSocket: {:?}", order);
            Ok(())
        } else {
            Err("WebSocket not connected".into())
        }
    }

    pub async fn submit_orders(
        &self,
        orders: &[OrderRequest],
    ) -> Vec<Result<(), Box<dyn std::error::Error + Send + Sync>>> {
        let mut results = Vec::with_capacity(orders.len());

        for order in orders {
            results.push(self.submit_order(order).await);
        }

        results
    }

    /// Cancel an order by ID
    pub async fn cancel_order(
        &self,
        order_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let request = WsRequest {
            id: get_next_request_id(),
            method: "private/cancel_order".to_string(),
            params: CancelOrderParams {
                order_id: order_id.to_string(),
            },
        };

        let json = serde_json::to_string(&request)?;

        let sender = self.ws_sender.read().await;
        if let Some(tx) = sender.as_ref() {
            tx.send(json).await?;
            // Remove from tracked orders
            let mut orders = self.open_orders.write().await;
            orders.remove(&order_id);
            debug!("Cancel order sent: {}", order_id);
            Ok(())
        } else {
            Err("WebSocket not connected".into())
        }
    }

    /// Cancel all open orders for a strategy
    pub async fn cancel_all_orders(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let order_ids: Vec<Uuid> = {
            let orders = self.open_orders.read().await;
            orders.keys().cloned().collect()
        };

        let count = order_ids.len();
        for order_id in order_ids {
            if let Err(e) = self.cancel_order(order_id).await {
                warn!("Failed to cancel order {}: {}", order_id, e);
            }
        }

        Ok(count)
    }

    /// Track an order that was successfully placed
    pub async fn track_order(&self, order_id: Uuid, order: &OrderRequest) {
        let open_order = OpenOrder {
            id: order_id,
            symbol: order.symbol.clone(),
            side: order.side,
            price: order.price,
            quantity: order.quantity,
        };
        let mut orders = self.open_orders.write().await;
        orders.insert(order_id, open_order);
    }

    /// Remove an order from tracking (e.g., when filled or cancelled)
    pub async fn untrack_order(&self, order_id: Uuid) {
        let mut orders = self.open_orders.write().await;
        orders.remove(&order_id);
    }

    /// Get count of open orders
    pub async fn open_order_count(&self) -> usize {
        let orders = self.open_orders.read().await;
        orders.len()
    }

    /// Update inventory when a fill occurs
    pub async fn update_inventory(&self, side: Side, quantity: rust_decimal::Decimal) {
        let mut inv = self.inventory.write().await;
        match side {
            Side::Bid => *inv += quantity,  // Bought = increase inventory
            Side::Ask => *inv -= quantity,  // Sold = decrease inventory
        }
    }

    /// Get current inventory
    pub async fn get_inventory(&self) -> rust_decimal::Decimal {
        *self.inventory.read().await
    }

    /// Fetch open orders from REST API and sync with local tracking
    pub async fn fetch_open_orders(&self) -> Result<Vec<OpenOrder>, Box<dyn std::error::Error + Send + Sync>> {
        let token = self.access_token.read().await;
        let token = token.as_ref().ok_or("Not logged in")?;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/api/orders?limit=100", self.accounts_url))
            .bearer_auth(token)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(format!("Failed to fetch orders: {}", resp.status()).into());
        }

        let data: serde_json::Value = resp.json().await?;
        let orders_json = data["orders"].as_array().ok_or("Invalid response format")?;

        let mut open_orders = Vec::new();
        for order_json in orders_json {
            let status = order_json["status"].as_str().unwrap_or("");
            // Only include open/pending orders (not filled/cancelled)
            if status != "open" && status != "pending" && status != "partially_filled" {
                continue;
            }

            let id_str = order_json["id"].as_str().unwrap_or("");
            let id = Uuid::parse_str(id_str).ok();
            let Some(id) = id else { continue };

            let side_str = order_json["side"].as_str().unwrap_or("bid");
            let side = if side_str == "ask" { Side::Ask } else { Side::Bid };

            let price_str = order_json["price"].as_str().unwrap_or("0");
            let price = rust_decimal::Decimal::from_str_exact(price_str).ok();

            let qty_str = order_json["quantity"].as_str().unwrap_or("0");
            let quantity = rust_decimal::Decimal::from_str_exact(qty_str).unwrap_or_default();

            let symbol = order_json["symbol"].as_str().unwrap_or("").to_string();

            open_orders.push(OpenOrder {
                id,
                symbol,
                side,
                price,
                quantity,
            });
        }

        // Sync local tracking
        {
            let mut tracked = self.open_orders.write().await;
            tracked.clear();
            for order in &open_orders {
                tracked.insert(order.id, order.clone());
            }
        }

        debug!("Fetched {} open orders from REST API", open_orders.len());
        Ok(open_orders)
    }

    /// Cancel an order via REST API
    pub async fn cancel_order_rest(
        &self,
        order_id: Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let token = self.access_token.read().await;
        let token = token.as_ref().ok_or("Not logged in")?;

        let client = reqwest::Client::new();
        let resp = client
            .delete(format!("{}/api/orders/{}", self.accounts_url, order_id))
            .bearer_auth(token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            debug!("Failed to cancel order {}: {} - {}", order_id, status, body);
            return Err(format!("Failed to cancel order: {}", status).into());
        }

        // Remove from tracked orders
        {
            let mut orders = self.open_orders.write().await;
            orders.remove(&order_id);
        }

        debug!("Cancelled order {} via REST API", order_id);
        Ok(())
    }

    /// Cancel all open orders via REST API
    pub async fn cancel_all_orders_rest(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        // First fetch current open orders
        let open_orders = self.fetch_open_orders().await?;
        let count = open_orders.len();

        // Cancel each one
        for order in open_orders {
            if let Err(e) = self.cancel_order_rest(order.id).await {
                debug!("Failed to cancel order {}: {}", order.id, e);
            }
        }

        info!("Cancelled {} orders via REST API", count);
        Ok(count)
    }
}
