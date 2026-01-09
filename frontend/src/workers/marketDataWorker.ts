/**
 * Market Data Web Worker
 *
 * Handles all data processing off the main thread:
 * - WebSocket message parsing
 * - Orderbook delta application
 * - Price history management
 * - Best bid/ask calculation
 *
 * This eliminates main thread blocking for HFT scenarios.
 */

// Sorted price map (same logic as main thread but self-contained for worker)
class SortedPriceMap {
  private levels: { price: number; quantity: number }[] = [];
  private priceToIndex: Map<number, number> = new Map();
  private _bestPrice: number | null = null;
  private isBidSide: boolean;

  constructor(isBidSide: boolean) {
    this.isBidSide = isBidSide;
  }

  get bestPrice(): number | null {
    return this._bestPrice;
  }

  private findInsertIndex(price: number): number {
    let low = 0;
    let high = this.levels.length;

    while (low < high) {
      const mid = (low + high) >>> 1;
      const cmp = this.isBidSide
        ? this.levels[mid].price > price
        : this.levels[mid].price < price;

      if (cmp) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    return low;
  }

  set(price: number, quantity: number): void {
    const existingIndex = this.priceToIndex.get(price);

    if (quantity <= 0) {
      if (existingIndex !== undefined) {
        this.levels.splice(existingIndex, 1);
        this.priceToIndex.delete(price);

        for (let i = existingIndex; i < this.levels.length; i++) {
          this.priceToIndex.set(this.levels[i].price, i);
        }

        this._bestPrice = this.levels.length > 0 ? this.levels[0].price : null;
      }
      return;
    }

    if (existingIndex !== undefined) {
      this.levels[existingIndex].quantity = quantity;
    } else {
      const insertIdx = this.findInsertIndex(price);
      this.levels.splice(insertIdx, 0, { price, quantity });

      for (let i = insertIdx; i < this.levels.length; i++) {
        this.priceToIndex.set(this.levels[i].price, i);
      }

      this._bestPrice = this.levels[0].price;
    }
  }

  clear(): void {
    this.levels = [];
    this.priceToIndex.clear();
    this._bestPrice = null;
  }

  getSortedLevels(): readonly { price: number; quantity: number }[] {
    return this.levels;
  }
}

// Fast number formatting
function formatPrice(price: number): string {
  if (!Number.isFinite(price)) return '0.00';
  const rounded = Math.round(price * 100) / 100;
  const intPart = Math.floor(rounded);
  const decPart = Math.round((rounded - intPart) * 100);
  const intStr = intPart.toLocaleString('en-US');
  const decStr = decPart.toString().padStart(2, '0');
  return `${intStr}.${decStr}`;
}

function formatQuantity(qty: number): string {
  if (!Number.isFinite(qty)) return '0.000000';
  const rounded = Math.round(qty * 1000000) / 1000000;
  const intPart = Math.floor(rounded);
  const decPart = Math.round((rounded - intPart) * 1000000);
  return `${intPart}.${decPart.toString().padStart(6, '0')}`;
}

function formatQuantityShort(qty: number): string {
  if (!Number.isFinite(qty)) return '0.00';
  const rounded = Math.round(qty * 100) / 100;
  const intPart = Math.floor(rounded);
  const decPart = Math.round((rounded - intPart) * 100);
  return `${intPart}.${decPart.toString().padStart(2, '0')}`;
}

function formatTime(timestamp: number): string {
  const date = new Date(timestamp);
  const h = date.getHours();
  const m = date.getMinutes();
  const s = date.getSeconds();
  return `${h.toString().padStart(2, '0')}:${m.toString().padStart(2, '0')}:${s.toString().padStart(2, '0')}`;
}

// Binary insert for price history
function binaryInsertPricePoint(
  arr: { price: number; time: number }[],
  point: { price: number; time: number }
): void {
  const time = point.time;
  const len = arr.length;

  if (len === 0 || time >= arr[len - 1].time) {
    arr.push(point);
    return;
  }

  if (time <= arr[0].time) {
    arr.unshift(point);
    return;
  }

  let low = 0;
  let high = len;

  while (low < high) {
    const mid = (low + high) >>> 1;
    if (arr[mid].time < time) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }

  arr.splice(low, 0, point);
}

// Worker state
const bids = new SortedPriceMap(true);
const asks = new SortedPriceMap(false);
let trades: Array<{
  id: number;
  price: number;
  quantity: number;
  side: 'Bid' | 'Ask';
  timestamp: number;
  priceStr: string;
  quantityStr: string;
  timeStr: string;
}> = [];
let priceHistory: { price: number; time: number }[] = [];
let lastPrice = 0;
let hasReceivedSnapshot = false;

// 24h stats from gateway
let high24h = 0;
let low24h = 0;
let volume24h = 0;
let open24h = 0;

const TRADES_LIMIT = 50;
const PRICE_HISTORY_LIMIT = 500;

// Output interfaces
interface OrderBookLevel {
  price: number;
  quantity: number;
  total: number;
  priceStr: string;
  quantityStr: string;
  totalStr: string;
}

function buildOrderBookLevels(priceMap: SortedPriceMap): OrderBookLevel[] {
  const levels = priceMap.getSortedLevels();
  const result: OrderBookLevel[] = new Array(levels.length);
  let total = 0;

  for (let i = 0; i < levels.length; i++) {
    const level = levels[i];
    total += level.quantity;
    result[i] = {
      price: level.price,
      quantity: level.quantity,
      total,
      priceStr: formatPrice(level.price),
      quantityStr: formatQuantity(level.quantity),
      totalStr: formatQuantity(total),
    };
  }

  return result;
}

// Message types
interface Stats24h {
  high_24h: number;
  low_24h: number;
  volume_24h: number;
  open_24h: number;
  last_price: number;
}

// Trade tuple format: [price, quantity, side, timestamp, is_liquidation]
type TradeTuple = [number, number, string, number, boolean];

// Helper to extract data from trade tuple
function extractTrade(t: TradeTuple) {
  return { price: t[0], quantity: t[1], side: t[2], timestamp: t[3], isLiquidation: t[4] };
}

interface ChannelNotification {
  channel_name: string;
  notification: {
    trades: TradeTuple[];
    bid_changes: Array<[number, number, number]>;
    ask_changes: Array<[number, number, number]>;
    total_bid_amount: number;
    total_ask_amount: number;
    time: number;
    stats_24h?: Stats24h;
    snapshot?: boolean;
  };
}

type WorkerMessage =
  | { type: 'PROCESS_MESSAGE'; data: ChannelNotification }
  | { type: 'RESET' }
  | { type: 'LOAD_OHLCV'; data: Array<{ open_time: string; close: string }> };

interface MarketStateUpdate {
  type: 'UPDATE';
  orderBook: {
    bids: OrderBookLevel[];
    asks: OrderBookLevel[];
  };
  trades: typeof trades;
  priceHistory: typeof priceHistory;
  stats: {
    lastPrice: number;
    bestBid: number | null;
    bestAsk: number | null;
    spread: number | null;
    high24h: number;
    low24h: number;
    volume24h: number;
    priceChange24h: number;
    priceChangePercent24h: number;
  };
}

// Process incoming WebSocket messages
function processChannelNotification(data: ChannelNotification): void {
  const { notification } = data;

  // On snapshot, clear existing orderbook data first
  if (notification.snapshot) {
    bids.clear();
    asks.clear();
    hasReceivedSnapshot = true;
  }

  // Apply bid changes
  for (let i = 0; i < notification.bid_changes.length; i++) {
    const [price, , newQty] = notification.bid_changes[i];
    bids.set(price, newQty);
  }

  // Apply ask changes
  for (let i = 0; i < notification.ask_changes.length; i++) {
    const [price, , newQty] = notification.ask_changes[i];
    asks.set(price, newQty);
  }

  // Update 24h stats if present
  if (notification.stats_24h) {
    high24h = notification.stats_24h.high_24h;
    low24h = notification.stats_24h.low_24h;
    volume24h = notification.stats_24h.volume_24h;
    open24h = notification.stats_24h.open_24h;
    lastPrice = notification.stats_24h.last_price;
  }

  // Best bid/ask O(1)
  const bestBid = bids.bestPrice;
  const bestAsk = asks.bestPrice;

  // Only update lastPrice from spread if we don't have stats
  if (!notification.stats_24h && bestBid !== null && bestAsk !== null) {
    lastPrice = (bestBid + bestAsk) / 2;
  }

  // Process trades
  if (notification.trades && notification.trades.length > 0) {
    console.log('[Worker] Processing trades:', notification.trades);
    const baseId = Date.now();

    for (let i = 0; i < notification.trades.length; i++) {
      const trade = extractTrade(notification.trades[i]);
      // timestamp is in seconds with decimals, convert to ms
      const timestampMs = Math.floor(trade.timestamp * 1000);
      const timeInSeconds = Math.floor(trade.timestamp);

      binaryInsertPricePoint(priceHistory, {
        price: trade.price,
        time: timeInSeconds,
      });

      const newTrade = {
        id: baseId + i,
        price: trade.price,
        quantity: trade.quantity,
        side: trade.side === 'buy' ? 'Bid' : 'Ask' as 'Bid' | 'Ask',
        timestamp: timestampMs,
        priceStr: formatPrice(trade.price),
        quantityStr: formatQuantityShort(trade.quantity),
        timeStr: formatTime(timestampMs),
      };
      console.log('[Worker] Adding trade:', newTrade);
      trades.unshift(newTrade);
    }

    console.log('[Worker] Total trades after processing:', trades.length);

    if (trades.length > TRADES_LIMIT) {
      trades.length = TRADES_LIMIT;
    }

    if (priceHistory.length > PRICE_HISTORY_LIMIT) {
      priceHistory = priceHistory.slice(-PRICE_HISTORY_LIMIT);
    }
  }
}

function loadOHLCV(data: Array<{ open_time: string; close: string }>): void {
  for (const bar of data) {
    const timeInSeconds = Math.floor(new Date(bar.open_time).getTime() / 1000);
    binaryInsertPricePoint(priceHistory, {
      price: parseFloat(bar.close),
      time: timeInSeconds,
    });
  }

  if (data.length > 0) {
    const lastBar = data[data.length - 1];
    lastPrice = parseFloat(lastBar.close);
  }
}

function reset(): void {
  bids.clear();
  asks.clear();
  trades = [];
  hasReceivedSnapshot = false;
}

function getMarketStateUpdate(): MarketStateUpdate {
  const bestBid = bids.bestPrice;
  const bestAsk = asks.bestPrice;
  const spread = bestBid !== null && bestAsk !== null ? bestAsk - bestBid : null;

  // Calculate price change
  const priceChange24h = open24h > 0 ? lastPrice - open24h : 0;
  const priceChangePercent24h = open24h > 0 ? (priceChange24h / open24h) * 100 : 0;

  return {
    type: 'UPDATE',
    orderBook: {
      bids: buildOrderBookLevels(bids),
      asks: buildOrderBookLevels(asks),
    },
    trades: trades.slice(0, TRADES_LIMIT),
    priceHistory: priceHistory.slice(-PRICE_HISTORY_LIMIT),
    stats: {
      lastPrice,
      bestBid,
      bestAsk,
      spread,
      high24h,
      low24h,
      volume24h,
      priceChange24h,
      priceChangePercent24h,
    },
  };
}

// Worker message handler
self.onmessage = (event: MessageEvent<WorkerMessage>) => {
  const message = event.data;
  console.log('[Worker] Received message type:', message.type);

  switch (message.type) {
    case 'PROCESS_MESSAGE':
      processChannelNotification(message.data);
      const update = getMarketStateUpdate();
      console.log('[Worker] Sending update with trades:', update.trades.length);
      self.postMessage(update);
      break;

    case 'LOAD_OHLCV':
      loadOHLCV(message.data);
      self.postMessage(getMarketStateUpdate());
      break;

    case 'RESET':
      reset();
      break;
  }
};

// Export type for main thread
export type { WorkerMessage, MarketStateUpdate };
