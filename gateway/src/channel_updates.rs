use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::{BTreeMap, VecDeque};

use crate::events::{DeltaAction, MarketEvent, Side};
use crate::websocket::{ChannelNotification, NotificationData, PriceLevelChange, Stats24h, TradeData, TickerNotification, TickerData, LwtNotification, LwtData};

const TWENTY_FOUR_HOURS_MS: u64 = 24 * 60 * 60 * 1000;

/// A trade record for 24h stats calculation
#[derive(Clone)]
struct StatsTradeRecord {
    price: f64,
    quantity: f64,
    timestamp: u64,
}

pub struct OrderBookState {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
    last_trades: Vec<TradeData>,
    last_sequence: u64,
    /// Rolling window of trades for 24h stats (ordered by timestamp)
    stats_trades: VecDeque<StatsTradeRecord>,
    last_price: f64,
}

impl OrderBookState {
    pub fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_trades: Vec::new(),
            last_sequence: 0,
            stats_trades: VecDeque::new(),
            last_price: 0.0,
        }
    }

    /// Remove trades older than 24 hours from the stats window
    fn expire_old_trades(&mut self, current_timestamp: u64) {
        let cutoff = current_timestamp.saturating_sub(TWENTY_FOUR_HOURS_MS);
        while let Some(front) = self.stats_trades.front() {
            if front.timestamp < cutoff {
                self.stats_trades.pop_front();
            } else {
                break;
            }
        }
    }

    fn update_stats(&mut self, price: f64, quantity: f64, timestamp: u64) {
        // Add new trade to the rolling window
        self.stats_trades.push_back(StatsTradeRecord {
            price,
            quantity,
            timestamp,
        });
        self.last_price = price;

        // Expire old trades
        self.expire_old_trades(timestamp);
    }

    fn get_stats_24h(&self) -> Option<Stats24h> {
        if self.stats_trades.is_empty() {
            return None;
        }

        // Calculate stats from the rolling window
        let mut high_24h = f64::MIN;
        let mut low_24h = f64::MAX;
        let mut volume_24h = 0.0;

        for trade in &self.stats_trades {
            if trade.price > high_24h {
                high_24h = trade.price;
            }
            if trade.price < low_24h {
                low_24h = trade.price;
            }
            // Volume is price × quantity (total value traded in quote currency)
            volume_24h += trade.price * trade.quantity;
        }

        // Open price is the first trade in the 24h window
        let open_24h = self.stats_trades.front().map(|t| t.price).unwrap_or(0.0);

        Some(Stats24h {
            high_24h,
            low_24h,
            volume_24h,
            open_24h,
            last_price: self.last_price,
        })
    }

    pub fn apply_orderbook_update(&mut self, event: &MarketEvent) -> Option<ChannelNotification> {
        match event {
            MarketEvent::OrderBookSnapshot { symbol, sequence, bids, asks } => {
                // Build new state from incoming snapshot
                let mut new_bids = BTreeMap::new();
                let mut new_asks = BTreeMap::new();

                for level in bids {
                    if !level.quantity.is_zero() {
                        new_bids.insert(level.price, level.quantity);
                    }
                }

                for level in asks {
                    if !level.quantity.is_zero() {
                        new_asks.insert(level.price, level.quantity);
                    }
                }

                // Check if anything changed
                let changed = new_bids != self.bids || new_asks != self.asks;

                // Replace state with new snapshot
                self.bids = new_bids;
                self.asks = new_asks;
                self.last_sequence = *sequence;

                // Only send notification if something changed
                if !changed {
                    return None;
                }

                // Always send full orderbook as "changes" so frontend gets complete state
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

                self.build_notification(symbol, bid_changes, ask_changes, Vec::new(), true)
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

                self.build_notification(symbol, bid_changes, ask_changes, Vec::new(), false)
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

                // Update 24h stats with timestamp for rolling window
                self.update_stats(price_f64, quantity_f64, *timestamp);

                let trade = TradeData {
                    price: price_f64,
                    quantity: quantity_f64,
                    side: "buy".to_string(),
                    timestamp: (*timestamp as f64) / 1000.0, // Convert ms to seconds with decimals
                    is_liquidation: false,
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
                        snapshot: None,
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

        self.build_notification(symbol, bid_changes, ask_changes, self.last_trades.clone(), true)
            .unwrap()
    }

    fn build_notification(
        &self,
        symbol: &str,
        bid_changes: Vec<PriceLevelChange>,
        ask_changes: Vec<PriceLevelChange>,
        trades: Vec<TradeData>,
        is_snapshot: bool,
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
                snapshot: if is_snapshot { Some(true) } else { None },
            },
        })
    }

    /// Generate ticker notification (ticker.SYMBOL.INTERVAL)
    pub fn get_ticker_notification(&self, symbol: &str, interval: &str) -> TickerNotification {
        let stats = self.get_stats_24h();
        let (best_bid, best_bid_amount) = self.bids.iter().rev().next()
            .map(|(p, q)| (p.to_f64().unwrap_or(0.0), q.to_f64().unwrap_or(0.0)))
            .unwrap_or((0.0, 0.0));
        let (best_ask, best_ask_amount) = self.asks.iter().next()
            .map(|(p, q)| (p.to_f64().unwrap_or(0.0), q.to_f64().unwrap_or(0.0)))
            .unwrap_or((0.0, 0.0));

        let mark_price = if best_bid > 0.0 && best_ask > 0.0 {
            (best_bid + best_ask) / 2.0
        } else {
            self.last_price
        };

        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        let (high_24h, low_24h, volume_24h, open_24h) = if let Some(s) = &stats {
            (s.high_24h, s.low_24h, s.volume_24h, s.open_24h)
        } else {
            (self.last_price, self.last_price, 0.0, self.last_price)
        };

        let change_24h = self.last_price - open_24h;

        TickerNotification {
            channel_name: format!("ticker.{}.{}", symbol, interval),
            notification: TickerData {
                mark_price,
                mark_timestamp: time,
                best_bid_price: best_bid,
                best_bid_amount,
                best_ask_price: best_ask,
                best_ask_amount,
                last_price: self.last_price,
                delta: 1.0, // Placeholder - would need position tracking
                volume_24h,
                value_24h: volume_24h, // Already in quote currency
                low_price_24h: low_24h,
                high_price_24h: high_24h,
                change_24h,
                index: mark_price, // Use mark as index for now
                forward: mark_price,
                funding_mark: 0.0,
                funding_rate: 0.0,
                collar_low: mark_price * 0.99,
                collar_high: mark_price * 1.01,
                realised_funding_24h: 0.0,
                average_funding_rate_24h: 0.0,
                open_interest: 0.0,
            },
        }
    }

    /// Generate LWT notification (lwt.SYMBOL.INTERVAL)
    pub fn get_lwt_notification(&self, symbol: &str, interval: &str) -> LwtNotification {
        let total_bid_amount: f64 = self.bids.values().map(|q| q.to_f64().unwrap_or(0.0)).sum();
        let total_ask_amount: f64 = self.asks.values().map(|q| q.to_f64().unwrap_or(0.0)).sum();

        let (best_bid, best_bid_amount) = self.bids.iter().rev().next()
            .map(|(p, q)| (p.to_f64().unwrap_or(0.0), q.to_f64().unwrap_or(0.0)))
            .unwrap_or((0.0, 0.0));
        let (best_ask, best_ask_amount) = self.asks.iter().next()
            .map(|(p, q)| (p.to_f64().unwrap_or(0.0), q.to_f64().unwrap_or(0.0)))
            .unwrap_or((0.0, 0.0));

        let mark_price = if best_bid > 0.0 && best_ask > 0.0 {
            (best_bid + best_ask) / 2.0
        } else {
            self.last_price
        };

        LwtNotification {
            channel_name: format!("lwt.{}.{}", symbol, interval),
            notification: LwtData {
                b: (best_bid, best_bid_amount, total_bid_amount),
                a: (best_ask, best_ask_amount, total_ask_amount),
                l: self.last_price,
                m: mark_price,
            },
        }
    }
}

