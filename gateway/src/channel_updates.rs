use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::BTreeMap;

use crate::events::{DeltaAction, MarketEvent, Side};
use crate::websocket::{ChannelNotification, NotificationData, PriceLevelChange, Stats24h, TradeData};

pub struct OrderBookState {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
    last_trades: Vec<TradeData>,
    last_sequence: u64,
    // 24h stats tracking
    high_24h: f64,
    low_24h: f64,
    volume_24h: f64,
    open_24h: f64,
    last_price: f64,
    first_trade_seen: bool,
}

impl OrderBookState {
    pub fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_trades: Vec::new(),
            last_sequence: 0,
            high_24h: 0.0,
            low_24h: 0.0,
            volume_24h: 0.0,
            open_24h: 0.0,
            last_price: 0.0,
            first_trade_seen: false,
        }
    }

    fn update_stats(&mut self, price: f64, quantity: f64) {
        if !self.first_trade_seen {
            self.open_24h = price;
            self.high_24h = price;
            self.low_24h = price;
            self.first_trade_seen = true;
        } else {
            if price > self.high_24h {
                self.high_24h = price;
            }
            if price < self.low_24h {
                self.low_24h = price;
            }
        }
        self.last_price = price;
        self.volume_24h += quantity;
    }

    fn get_stats_24h(&self) -> Option<Stats24h> {
        if !self.first_trade_seen {
            return None;
        }
        Some(Stats24h {
            high_24h: self.high_24h,
            low_24h: self.low_24h,
            volume_24h: self.volume_24h,
            open_24h: self.open_24h,
            last_price: self.last_price,
        })
    }

    pub fn apply_orderbook_update(&mut self, event: &MarketEvent) -> Option<ChannelNotification> {
        match event {
            MarketEvent::OrderBookSnapshot { symbol, sequence, bids, asks } => {
                let mut bid_changes = Vec::new();
                let mut ask_changes = Vec::new();

                // Build new state from incoming snapshot
                let mut new_bids = BTreeMap::new();
                let mut new_asks = BTreeMap::new();

                for level in bids {
                    tracing::info!("Snapshot bid: price={}, quantity={}", level.price, level.quantity);
                    let old_qty = self
                        .bids
                        .get(&level.price)
                        .copied()
                        .unwrap_or(Decimal::ZERO);
                    tracing::info!("  old_qty={}, comparison: {} != {} = {}",
                        old_qty, old_qty, level.quantity, old_qty != level.quantity);
                    if old_qty != level.quantity {
                        bid_changes.push(PriceLevelChange {
                            price: level.price.to_f64().unwrap_or(0.0),
                            old_quantity: old_qty.to_f64().unwrap_or(0.0),
                            new_quantity: level.quantity.to_f64().unwrap_or(0.0),
                        });
                    }
                    if !level.quantity.is_zero() {
                        new_bids.insert(level.price, level.quantity);
                    }
                }

                for level in asks {
                    let old_qty = self
                        .asks
                        .get(&level.price)
                        .copied()
                        .unwrap_or(Decimal::ZERO);
                    if old_qty != level.quantity {
                        ask_changes.push(PriceLevelChange {
                            price: level.price.to_f64().unwrap_or(0.0),
                            old_quantity: old_qty.to_f64().unwrap_or(0.0),
                            new_quantity: level.quantity.to_f64().unwrap_or(0.0),
                        });
                    }
                    if !level.quantity.is_zero() {
                        new_asks.insert(level.price, level.quantity);
                    }
                }

                // Emit removals for levels no longer present
                for (price, qty) in &self.bids {
                    if !new_bids.contains_key(price) {
                        bid_changes.push(PriceLevelChange {
                            price: price.to_f64().unwrap_or(0.0),
                            old_quantity: qty.to_f64().unwrap_or(0.0),
                            new_quantity: 0.0,
                        });
                    }
                }

                for (price, qty) in &self.asks {
                    if !new_asks.contains_key(price) {
                        ask_changes.push(PriceLevelChange {
                            price: price.to_f64().unwrap_or(0.0),
                            old_quantity: qty.to_f64().unwrap_or(0.0),
                            new_quantity: 0.0,
                        });
                    }
                }

                // Replace state with new snapshot
                self.bids = new_bids;
                self.asks = new_asks;
                self.last_sequence = *sequence;

                self.build_notification(symbol, bid_changes, ask_changes, Vec::new())
            }
            MarketEvent::OrderBookDelta { symbol, sequence, deltas } => {
                let mut bid_changes = Vec::new();
                let mut ask_changes = Vec::new();

                // Apply deltas to current state
                for delta in deltas {
                    let (book, changes) = match delta.side {
                        Side::Bid => (&mut self.bids, &mut bid_changes),
                        Side::Ask => (&mut self.asks, &mut ask_changes),
                    };

                    let old_qty = book.get(&delta.price).copied().unwrap_or(Decimal::ZERO);

                    match delta.action {
                        DeltaAction::Add | DeltaAction::Update => {
                            book.insert(delta.price, delta.quantity);
                            changes.push(PriceLevelChange {
                                price: delta.price.to_f64().unwrap_or(0.0),
                                old_quantity: old_qty.to_f64().unwrap_or(0.0),
                                new_quantity: delta.quantity.to_f64().unwrap_or(0.0),
                            });
                        }
                        DeltaAction::Remove => {
                            book.remove(&delta.price);
                            changes.push(PriceLevelChange {
                                price: delta.price.to_f64().unwrap_or(0.0),
                                old_quantity: old_qty.to_f64().unwrap_or(0.0),
                                new_quantity: 0.0,
                            });
                        }
                    }
                }

                self.last_sequence = *sequence;

                // Only send notification if there were actual changes
                if bid_changes.is_empty() && ask_changes.is_empty() {
                    return None;
                }

                self.build_notification(symbol, bid_changes, ask_changes, Vec::new())
            }
            MarketEvent::Fill {
                buy_order_id,
                sell_order_id,
                price,
                quantity,
                timestamp,
                symbol,
            } => {
                let price_f64 = price.to_f64().unwrap_or(0.0);
                let quantity_f64 = quantity.to_f64().unwrap_or(0.0);

                // Update 24h stats
                self.update_stats(price_f64, quantity_f64);

                let trade = TradeData {
                    price: price_f64,
                    quantity: quantity_f64,
                    side: "buy".to_string(),
                    timestamp: *timestamp,
                    buy_order_id: Some(buy_order_id.to_string()),
                    sell_order_id: Some(sell_order_id.to_string()),
                };

                self.last_trades.push(trade.clone());
                if self.last_trades.len() > 100 {
                    self.last_trades.remove(0);
                }

                // Broadcast the trade with stats
                let channel_name = format!("book.{}.none.10.100ms", symbol);
                Some(ChannelNotification {
                    channel_name,
                    notification: NotificationData {
                        trades: vec![trade],
                        bid_changes: Vec::new(),
                        ask_changes: Vec::new(),
                        total_bid_amount: self.bids.values().map(|q| q.to_f64().unwrap_or(0.0)).sum(),
                        total_ask_amount: self.asks.values().map(|q| q.to_f64().unwrap_or(0.0)).sum(),
                        time: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs_f64(),
                        stats_24h: self.get_stats_24h(),
                    },
                })
            }
            _ => None,
        }
    }

    pub fn get_trades_snapshot(&self) -> Vec<TradeData> {
        self.last_trades.clone()
    }

    pub fn get_orderbook_snapshot(&self, symbol: &str) -> ChannelNotification {
        let bid_changes: Vec<PriceLevelChange> = self
            .bids
            .iter()
            .map(|(price, qty)| PriceLevelChange {
                price: price.to_f64().unwrap_or(0.0),
                old_quantity: 0.0,
                new_quantity: qty.to_f64().unwrap_or(0.0),
            })
            .collect();

        let ask_changes: Vec<PriceLevelChange> = self
            .asks
            .iter()
            .map(|(price, qty)| PriceLevelChange {
                price: price.to_f64().unwrap_or(0.0),
                old_quantity: 0.0,
                new_quantity: qty.to_f64().unwrap_or(0.0),
            })
            .collect();

        self.build_notification(symbol, bid_changes, ask_changes, self.last_trades.clone())
            .unwrap()
    }

    fn build_notification(
        &self,
        symbol: &str,
        bid_changes: Vec<PriceLevelChange>,
        ask_changes: Vec<PriceLevelChange>,
        trades: Vec<TradeData>,
    ) -> Option<ChannelNotification> {
        let total_bid_amount: f64 = self
            .bids
            .values()
            .map(|q| q.to_f64().unwrap_or(0.0))
            .sum();
        let total_ask_amount: f64 = self
            .asks
            .values()
            .map(|q| q.to_f64().unwrap_or(0.0))
            .sum();

        let channel_name = format!("book.{}.none.10.100ms", symbol);
        Some(ChannelNotification {
            channel_name,
            notification: NotificationData {
                trades,
                bid_changes,
                ask_changes,
                total_bid_amount,
                total_ask_amount,
                time: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64(),
                stats_24h: self.get_stats_24h(),
            },
        })
    }
}

