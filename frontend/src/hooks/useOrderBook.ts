import { useEffect, useCallback, useRef, useMemo, useReducer } from 'react';
import type { OrderBook, OrderBookLevel, Trade, MarketStats, Side, OrderType } from '../types';
import { useWebSocket } from './useWebSocket';
import {
  SortedPriceMap,
  binaryInsertPricePoint,
  formatPrice,
  formatQuantity,
  formatQuantityShort,
  formatTime,
  type PricePoint,
} from '../utils/hftOptimizations';

const GATEWAY_URL = (import.meta.env.VITE_GATEWAY_URL || 'ws://localhost:3000/ws').trim();
const API_URL = (import.meta.env.VITE_API_URL || 'http://localhost:3000').trim();

// Throttle UI updates to 60fps max (16ms) - prevents excessive re-renders
const UI_UPDATE_INTERVAL = 16;

// Pre-allocated limits
const PRICE_HISTORY_LIMIT = 500;
const TRADES_LIMIT = 50;

// ============================================================================
// Consolidated Market State Reducer (eliminates multiple setState calls)
// ============================================================================

interface MarketState {
  orderBook: OrderBook;
  trades: Trade[];
  priceHistory: PricePoint[];
  stats: MarketStats;
}

type MarketAction =
  | { type: 'UPDATE_ALL'; payload: MarketState }
  | { type: 'RESET' };

function marketReducer(state: MarketState, action: MarketAction): MarketState {
  switch (action.type) {
    case 'UPDATE_ALL':
      return action.payload;
    case 'RESET':
      return initialMarketState;
    default:
      return state;
  }
}

const initialMarketState: MarketState = {
  orderBook: { bids: [], asks: [] },
  trades: [],
  priceHistory: [],
  stats: {
    symbol: 'KCN/EUR',
    lastPrice: 42.00,
    priceChange24h: 0.35,
    priceChangePercent24h: 0.84,
    high24h: 43.50,
    low24h: 41.20,
    volume24h: 12345.67,
    bestBid: null,
    bestAsk: null,
    spread: null,
  },
};

// ============================================================================
// Internal State (mutable, not triggering re-renders)
// ============================================================================

interface InternalState {
  // Sorted price maps with O(1) best price lookup
  bids: SortedPriceMap;
  asks: SortedPriceMap;
  // Pre-formatted trades
  trades: Trade[];
  // Price history with binary insert
  priceHistory: PricePoint[];
  lastPrice: number;
}

function createInternalState(): InternalState {
  return {
    bids: new SortedPriceMap(true),  // true = bid side (descending)
    asks: new SortedPriceMap(false), // false = ask side (ascending)
    trades: [],
    priceHistory: [],
    lastPrice: 42.00,
  };
}

// ============================================================================
// Pre-allocated arrays for output (reduces GC pressure)
// ============================================================================

// Reusable output arrays - mutated in place
let bidsOutputArray: OrderBookLevel[] = [];
let asksOutputArray: OrderBookLevel[] = [];

/**
 * Build output array from sorted price map
 * Mutates the output array in place and returns it
 */
function buildOrderBookLevels(
  priceMap: SortedPriceMap,
  outputArray: OrderBookLevel[]
): OrderBookLevel[] {
  const levels = priceMap.getSortedLevels();
  const len = levels.length;

  // Resize output array if needed
  if (outputArray.length !== len) {
    outputArray.length = len;
  }

  let total = 0;
  for (let i = 0; i < len; i++) {
    const level = levels[i];
    total += level.quantity;

    // Reuse or create level object
    let outLevel = outputArray[i];
    if (!outLevel) {
      outLevel = {
        price: 0,
        quantity: 0,
        total: 0,
        priceStr: '',
        quantityStr: '',
        totalStr: '',
      };
      outputArray[i] = outLevel;
    }

    // Update values
    outLevel.price = level.price;
    outLevel.quantity = level.quantity;
    outLevel.total = total;

    // Pre-format strings (done once here, not in render)
    outLevel.priceStr = formatPrice(level.price);
    outLevel.quantityStr = formatQuantity(level.quantity);
    outLevel.totalStr = formatQuantity(total);
  }

  return outputArray;
}

// ============================================================================
// Main Hook
// ============================================================================

export function useOrderBook() {
  // Single consolidated state update via reducer
  const [marketState, dispatch] = useReducer(marketReducer, initialMarketState);

  // Mutable internal state - doesn't trigger re-renders
  const internalStateRef = useRef<InternalState>(createInternalState());

  // Update scheduling refs
  const updateScheduledRef = useRef(false);
  const lastUpdateTimeRef = useRef(0);

  // Track if we've received the initial snapshot
  const hasReceivedSnapshotRef = useRef(false);

  // Flush internal state to React state (throttled)
  const flushToReactState = useCallback(() => {
    const state = internalStateRef.current;

    // Build output arrays (mutates in place, pre-formats strings)
    const bidsArray = buildOrderBookLevels(state.bids, bidsOutputArray);
    const asksArray = buildOrderBookLevels(state.asks, asksOutputArray);

    // Store references for next cycle
    bidsOutputArray = bidsArray;
    asksOutputArray = asksArray;

    // Get best prices from sorted maps - O(1)
    const bestBid = state.bids.bestPrice;
    const bestAsk = state.asks.bestPrice;
    const spread = bestBid !== null && bestAsk !== null ? bestAsk - bestBid : null;

    // Create shallow copies for React (required for change detection)
    // But the OrderBookLevel objects inside are reused
    const newOrderBook: OrderBook = {
      bids: [...bidsArray],
      asks: [...asksArray],
    };

    // Single dispatch updates all state atomically
    dispatch({
      type: 'UPDATE_ALL',
      payload: {
        orderBook: newOrderBook,
        trades: state.trades.slice(0, TRADES_LIMIT),
        priceHistory: state.priceHistory.slice(-PRICE_HISTORY_LIMIT),
        stats: {
          ...marketState.stats,
          lastPrice: state.lastPrice,
          bestBid,
          bestAsk,
          spread,
        },
      },
    });

    updateScheduledRef.current = false;
    lastUpdateTimeRef.current = performance.now();
  }, [marketState.stats]);

  // Schedule a throttled update
  const scheduleUpdate = useCallback(() => {
    if (updateScheduledRef.current) return;

    const now = performance.now();
    const timeSinceLastUpdate = now - lastUpdateTimeRef.current;

    if (timeSinceLastUpdate >= UI_UPDATE_INTERVAL) {
      // Immediate update using requestAnimationFrame for smooth rendering
      updateScheduledRef.current = true;
      requestAnimationFrame(flushToReactState);
    } else {
      // Schedule for next available slot
      updateScheduledRef.current = true;
      setTimeout(() => {
        requestAnimationFrame(flushToReactState);
      }, UI_UPDATE_INTERVAL - timeSinceLastUpdate);
    }
  }, [flushToReactState]);

  const handleChannelNotification = useCallback((data: {
    channel_name: string;
    notification: {
      trades: Array<{ price: number; quantity: number; side: string; timestamp: number }>;
      bid_changes: Array<[number, number, number]>;
      ask_changes: Array<[number, number, number]>;
      total_bid_amount: number;
      total_ask_amount: number;
      time: number;
    };
  }) => {
    const { notification } = data;
    const state = internalStateRef.current;
    const isSnapshot = !hasReceivedSnapshotRef.current;

    // Apply bid changes to sorted map
    for (let i = 0; i < notification.bid_changes.length; i++) {
      const [price, , newQty] = notification.bid_changes[i];
      state.bids.set(price, newQty);
    }

    // Apply ask changes to sorted map
    for (let i = 0; i < notification.ask_changes.length; i++) {
      const [price, , newQty] = notification.ask_changes[i];
      state.asks.set(price, newQty);
    }

    // Best bid/ask now available in O(1) via sorted maps
    const bestBid = state.bids.bestPrice;
    const bestAsk = state.asks.bestPrice;

    if (bestBid !== null && bestAsk !== null) {
      const midPrice = (bestBid + bestAsk) / 2;
      state.lastPrice = midPrice;
    }

    // Process trades with pre-formatted strings
    if (notification.trades && notification.trades.length > 0) {
      const baseId = Date.now();

      for (let i = 0; i < notification.trades.length; i++) {
        const trade = notification.trades[i];
        const timeInSeconds = Math.floor(trade.timestamp / 1000);

        // Binary insert into price history (maintains sorted order)
        binaryInsertPricePoint(state.priceHistory, {
          price: trade.price,
          time: timeInSeconds,
        });

        // Create pre-formatted trade
        const formattedTrade: Trade = {
          id: baseId + i,
          price: trade.price,
          quantity: trade.quantity,
          side: trade.side === 'buy' ? 'Bid' : 'Ask',
          timestamp: trade.timestamp,
          // Pre-format strings here, not in render
          priceStr: formatPrice(trade.price),
          quantityStr: formatQuantityShort(trade.quantity),
          timeStr: formatTime(trade.timestamp),
        };

        state.trades.unshift(formattedTrade);
      }

      // Trim trades
      if (state.trades.length > TRADES_LIMIT) {
        state.trades.length = TRADES_LIMIT;
      }

      // Trim price history
      if (state.priceHistory.length > PRICE_HISTORY_LIMIT) {
        state.priceHistory = state.priceHistory.slice(-PRICE_HISTORY_LIMIT);
      }
    }

    // Mark that we've received the snapshot
    if (isSnapshot) {
      hasReceivedSnapshotRef.current = true;
    }

    scheduleUpdate();
  }, [scheduleUpdate]);

  const { isConnected, subscribe, send } = useWebSocket(GATEWAY_URL, handleChannelNotification);
  const channelName = 'book.KCN/EUR.none.10.100ms';
  const wasConnectedRef = useRef(false);
  const hasFetchedOHLCVRef = useRef(false);

  // Fetch historical OHLCV data on mount
  useEffect(() => {
    if (hasFetchedOHLCVRef.current) return;
    hasFetchedOHLCVRef.current = true;

    const fetchOHLCV = async () => {
      try {
        const res = await fetch(`${API_URL}/api/ohlcv?symbol=KCN/EUR&interval=1m&limit=500`);
        if (!res.ok) return;

        const { data } = await res.json();
        if (!data || data.length === 0) return;

        const state = internalStateRef.current;

        // Convert OHLCV to price history using binary insert
        for (const bar of data) {
          const timeInSeconds = Math.floor(new Date(bar.open_time).getTime() / 1000);
          binaryInsertPricePoint(state.priceHistory, {
            price: parseFloat(bar.close),
            time: timeInSeconds,
          });
        }

        // Update last price from most recent bar
        const lastBar = data[data.length - 1];
        if (lastBar) {
          state.lastPrice = parseFloat(lastBar.close);
        }

        scheduleUpdate();
      } catch (err) {
        console.error('[OHLCV] Failed to fetch historical data:', err);
      }
    };

    fetchOHLCV();
  }, [scheduleUpdate]);

  useEffect(() => {
    if (isConnected && !wasConnectedRef.current) {
      // Fresh connection - reset snapshot flag and clear stale orderbook/trades
      hasReceivedSnapshotRef.current = false;
      const state = internalStateRef.current;
      state.bids.clear();
      state.asks.clear();
      state.trades = [];

      subscribe(channelName);
      wasConnectedRef.current = true;
    } else if (!isConnected && wasConnectedRef.current) {
      // Disconnected - mark for resubscribe on reconnect
      wasConnectedRef.current = false;
    }
  }, [isConnected, subscribe, channelName]);

  const placeOrder = useCallback(async (side: Side, orderType: OrderType, price: number | null, quantity: number) => {
    const response = await fetch(`${API_URL}/api/order`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        side: side.toLowerCase(),
        order_type: orderType.toLowerCase(),
        price,
        quantity,
      }),
    });

    if (!response.ok) {
      throw new Error(`Order failed: ${response.statusText}`);
    }
  }, []);

  const cancelOrder = useCallback(async (orderId: number) => {
    const response = await fetch(`${API_URL}/api/order/cancel`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ order_id: orderId }),
    });

    if (!response.ok) {
      throw new Error(`Cancel failed: ${response.statusText}`);
    }
  }, []);

  return useMemo(() => ({
    orderBook: marketState.orderBook,
    trades: marketState.trades,
    priceHistory: marketState.priceHistory,
    stats: marketState.stats,
    placeOrder,
    cancelOrder,
    isConnected,
    send,
  }), [marketState, placeOrder, cancelOrder, isConnected, send]);
}
