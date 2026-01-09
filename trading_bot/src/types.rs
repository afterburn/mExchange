use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Bid,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Limit,
    Market,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    pub symbol: String,
    pub side: Side,
    pub order_type: OrderType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Decimal>,
    pub quantity: Decimal,
}

/// Represents an open order tracked by the bot
#[derive(Debug, Clone)]
pub struct OpenOrder {
    pub id: Uuid,
    pub symbol: String,
    pub side: Side,
    pub price: Option<Decimal>,
    pub quantity: Decimal,
}

/// Actions a strategy can take each iteration
#[derive(Debug, Clone, Default)]
pub struct StrategyActions {
    pub orders_to_cancel: Vec<Uuid>,
    pub orders_to_place: Vec<OrderRequest>,
}

#[derive(Debug, Clone, Default)]
pub struct MarketState {
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub last_price: Option<Decimal>,
    pub price_history: Vec<Decimal>,
}

impl MarketState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mid_price(&self) -> Option<Decimal> {
        match (self.best_bid, self.best_ask) {
            (Some(bid), Some(ask)) => Some((bid + ask) / Decimal::TWO),
            (Some(bid), None) => Some(bid),
            (None, Some(ask)) => Some(ask),
            (None, None) => self.last_price,
        }
    }

    pub fn update_price(&mut self, price: Decimal) {
        self.last_price = Some(price);
        self.price_history.push(price);
        if self.price_history.len() > 50 {
            self.price_history.remove(0);
        }
    }
}

/// Gateway can send multiple formats:
/// 1. Channel notifications with "channel_name" field (book.*, ticker.*, lwt.*)
/// 2. JSON-RPC responses with "id" + "result" or "error"
/// 3. Tagged messages with "type" field (orderbook_snapshot, trade, etc.)
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum GatewayMessage {
    /// JSON-RPC success response (for auth, subscribe, etc.)
    RpcResponse(RpcResponse),
    /// JSON-RPC error response
    RpcError(RpcErrorResponse),
    /// Book channel notification format (channel_name + notification with trades/bid_changes/ask_changes)
    ChannelNotification(ChannelNotification),
    /// Ticker channel notification
    TickerNotification(TickerNotification),
    /// LWT (lightweight ticker) notification
    LwtNotification(LwtNotification),
    /// Tagged message format (type field)
    Tagged(TaggedMessage),
}

/// JSON-RPC style success response
#[derive(Debug, Clone, Deserialize)]
pub struct RpcResponse {
    pub id: String,
    pub result: serde_json::Value,
}

/// JSON-RPC style error response
#[derive(Debug, Clone, Deserialize)]
pub struct RpcErrorResponse {
    pub id: String,
    pub error: RpcError,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

/// Ticker notification from gateway
#[derive(Debug, Clone, Deserialize)]
pub struct TickerNotification {
    pub channel_name: String,
    pub notification: TickerData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TickerData {
    pub mark_price: f64,
    pub best_bid_price: f64,
    pub best_ask_price: f64,
    pub last_price: f64,
    #[serde(flatten)]
    pub extra: serde_json::Value, // Catch any additional fields
}

/// LWT (Last, Best Bid/Ask, Mark) notification
#[derive(Debug, Clone, Deserialize)]
pub struct LwtNotification {
    pub channel_name: String,
    pub notification: LwtData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LwtData {
    pub b: (f64, f64, f64), // best bid: [price, amount, total]
    pub a: (f64, f64, f64), // best ask: [price, amount, total]
    pub l: f64,             // last price
    pub m: f64,             // mark price
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelNotification {
    pub channel_name: String,
    pub notification: NotificationData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotificationData {
    #[serde(default)]
    pub trades: Vec<TradeData>,
    #[serde(default)]
    pub bid_changes: Vec<(f64, f64, f64)>, // (price, old_qty, new_qty)
    #[serde(default)]
    pub ask_changes: Vec<(f64, f64, f64)>,
    #[serde(default)]
    pub total_bid_amount: f64,
    #[serde(default)]
    pub total_ask_amount: f64,
    #[serde(default)]
    pub time: f64,
}

/// Trade data in tuple format: [price, quantity, side, timestamp, is_liquidation]
#[derive(Debug, Clone)]
pub struct TradeData {
    pub price: f64,
    pub quantity: f64,
    pub side: String,
    pub timestamp: f64,
    pub is_liquidation: bool,
}

impl<'de> serde::Deserialize<'de> for TradeData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{SeqAccess, Visitor};

        struct TradeDataVisitor;

        impl<'de> Visitor<'de> for TradeDataVisitor {
            type Value = TradeData;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a tuple of [price, quantity, side, timestamp, is_liquidation]")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let price = seq.next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
                let quantity = seq.next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
                let side = seq.next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(2, &self))?;
                let timestamp = seq.next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(3, &self))?;
                let is_liquidation = seq.next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(4, &self))?;

                Ok(TradeData {
                    price,
                    quantity,
                    side,
                    timestamp,
                    is_liquidation,
                })
            }
        }

        deserializer.deserialize_seq(TradeDataVisitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaggedMessage {
    OrderbookSnapshot {
        symbol: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
    },
    OrderbookUpdate {
        symbol: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
    },
    Trade {
        symbol: String,
        price: Decimal,
        quantity: Decimal,
        side: Side,
    },
    /// Sent when an order is filled (partially or fully)
    OrderFilled {
        order_id: String,
    },
    /// Sent when an order is cancelled
    OrderCancelled {
        order_id: String,
        #[serde(default)]
        filled_quantity: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceLevel {
    pub price: Decimal,
    pub quantity: Decimal,
}
