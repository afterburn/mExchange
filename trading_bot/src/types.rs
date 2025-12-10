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

/// Gateway can send two formats:
/// 1. Tagged messages with "type" field (orderbook_snapshot, trade, etc.)
/// 2. Channel notifications with "channel_name" field
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum GatewayMessage {
    /// Channel notification format (channel_name + notification)
    ChannelNotification(ChannelNotification),
    /// Tagged message format (type field)
    Tagged(TaggedMessage),
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

#[derive(Debug, Clone, Deserialize)]
pub struct TradeData {
    pub price: f64,
    pub quantity: f64,
    pub side: String,
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
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceLevel {
    pub price: Decimal,
    pub quantity: Decimal,
}
