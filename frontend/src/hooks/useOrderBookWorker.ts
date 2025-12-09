import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import type { OrderBook, OrderBookLevel, Trade, MarketStats, Side, OrderType } from '../types';
import { useWebSocket, type OrderEvent } from './useWebSocket';

// Re-export OrderEvent for consumers
export type { OrderEvent } from './useWebSocket';

const GATEWAY_URL = (import.meta.env.VITE_GATEWAY_URL || 'ws://localhost:3000/ws').trim();
const API_URL = (import.meta.env.VITE_API_URL || 'http://localhost:3000').trim();

// Throttle UI updates to 60fps max (16ms)
const UI_UPDATE_INTERVAL = 16;

interface PricePoint {
  price: number;
  time: number;
}

interface MarketState {
  orderBook: OrderBook;
  trades: Trade[];
  priceHistory: PricePoint[];
  stats: MarketStats;
}

interface WorkerMessage {
  type: 'PROCESS_MESSAGE' | 'RESET' | 'LOAD_OHLCV';
  data?: unknown;
}

interface MarketStateUpdate {
  type: 'UPDATE';
  orderBook: {
    bids: OrderBookLevel[];
    asks: OrderBookLevel[];
  };
  trades: Trade[];
  priceHistory: PricePoint[];
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

const initialMarketState: MarketState = {
  orderBook: { bids: [], asks: [] },
  trades: [],
  priceHistory: [],
  stats: {
    symbol: 'KCN/EUR',
    lastPrice: 0,
    priceChange24h: 0,
    priceChangePercent24h: 0,
    high24h: 0,
    low24h: 0,
    volume24h: 0,
    bestBid: null,
    bestAsk: null,
    spread: null,
  },
};

export interface TradeWithOrderIds {
  price: number;
  quantity: number;
  buy_order_id?: string;
  sell_order_id?: string;
}

/**
 * useOrderBookWorker - Web Worker based orderbook processing
 *
 * Moves all data processing to a dedicated Web Worker:
 * - WebSocket message parsing
 * - Orderbook delta application
 * - Price history management
 *
 * This keeps the main thread free for rendering.
 */
export function useOrderBookWorker(
  onTradeWithOrderId?: (trade: TradeWithOrderIds) => void,
  onOrderEvent?: (event: OrderEvent) => void
) {
  const [marketState, setMarketState] = useState<MarketState>(initialMarketState);
  const workerRef = useRef<Worker | null>(null);
  const updateScheduledRef = useRef(false);
  const lastUpdateTimeRef = useRef(0);
  const pendingUpdateRef = useRef<MarketStateUpdate | null>(null);

  // Throttled state update
  const flushUpdate = useCallback(() => {
    const update = pendingUpdateRef.current;
    if (!update) return;

    setMarketState(prev => ({
      orderBook: update.orderBook,
      trades: update.trades,
      priceHistory: update.priceHistory,
      stats: {
        ...prev.stats,
        lastPrice: update.stats.lastPrice || prev.stats.lastPrice,
        bestBid: update.stats.bestBid,
        bestAsk: update.stats.bestAsk,
        spread: update.stats.spread,
        high24h: update.stats.high24h || prev.stats.high24h,
        low24h: update.stats.low24h || prev.stats.low24h,
        volume24h: update.stats.volume24h || prev.stats.volume24h,
        priceChange24h: update.stats.priceChange24h,
        priceChangePercent24h: update.stats.priceChangePercent24h,
      },
    }));

    pendingUpdateRef.current = null;
    updateScheduledRef.current = false;
    lastUpdateTimeRef.current = performance.now();
  }, []);

  const scheduleUpdate = useCallback(() => {
    if (updateScheduledRef.current) return;

    const now = performance.now();
    const timeSinceLastUpdate = now - lastUpdateTimeRef.current;

    if (timeSinceLastUpdate >= UI_UPDATE_INTERVAL) {
      updateScheduledRef.current = true;
      requestAnimationFrame(flushUpdate);
    } else {
      updateScheduledRef.current = true;
      setTimeout(() => {
        requestAnimationFrame(flushUpdate);
      }, UI_UPDATE_INTERVAL - timeSinceLastUpdate);
    }
  }, [flushUpdate]);

  // Initialize Web Worker
  useEffect(() => {
    // Create worker using Vite's worker import syntax
    const worker = new Worker(
      new URL('../workers/marketDataWorker.ts', import.meta.url),
      { type: 'module' }
    );

    worker.onmessage = (event: MessageEvent<MarketStateUpdate>) => {
      if (event.data.type === 'UPDATE') {
        pendingUpdateRef.current = event.data;
        scheduleUpdate();
      }
    };

    worker.onerror = (error) => {
      console.error('[Worker] Error:', error);
    };

    workerRef.current = worker;

    return () => {
      worker.terminate();
      workerRef.current = null;
    };
  }, [scheduleUpdate]);

  // Handle WebSocket messages - forward to worker
  const handleChannelNotification = useCallback((data: {
    channel_name: string;
    notification: {
      trades: Array<{ price: number; quantity: number; side: string; timestamp: number; buy_order_id?: string; sell_order_id?: string }>;
      bid_changes: Array<[number, number, number]>;
      ask_changes: Array<[number, number, number]>;
      total_bid_amount: number;
      total_ask_amount: number;
      time: number;
    };
  }) => {
    // Check for trades with order IDs and notify callback
    if (onTradeWithOrderId && data.notification.trades) {
      for (const trade of data.notification.trades) {
        if (trade.buy_order_id || trade.sell_order_id) {
          onTradeWithOrderId({
            price: trade.price,
            quantity: trade.quantity,
            buy_order_id: trade.buy_order_id,
            sell_order_id: trade.sell_order_id,
          });
        }
      }
    }

    if (workerRef.current) {
      workerRef.current.postMessage({
        type: 'PROCESS_MESSAGE',
        data,
      } as WorkerMessage);
    }
  }, [onTradeWithOrderId]);

  const { isConnected, subscribe, send } = useWebSocket(GATEWAY_URL, handleChannelNotification, onOrderEvent);
  const channelName = 'book.KCN/EUR.none.10.100ms';
  const wasConnectedRef = useRef(false);
  const hasFetchedOHLCVRef = useRef(false);

  // Fetch historical OHLCV data
  useEffect(() => {
    if (hasFetchedOHLCVRef.current) return;
    hasFetchedOHLCVRef.current = true;

    const fetchOHLCV = async () => {
      try {
        const res = await fetch(`${API_URL}/api/ohlcv?symbol=KCN/EUR&interval=1m&limit=500`);
        if (!res.ok) return;

        const { data } = await res.json();
        if (!data || data.length === 0) return;

        // Forward to worker for processing
        if (workerRef.current) {
          workerRef.current.postMessage({
            type: 'LOAD_OHLCV',
            data,
          } as WorkerMessage);
        }
      } catch (err) {
        console.error('[OHLCV] Failed to fetch historical data:', err);
      }
    };

    fetchOHLCV();
  }, []);

  // Handle connection changes
  useEffect(() => {
    if (isConnected && !wasConnectedRef.current) {
      // Reset worker state on reconnect
      if (workerRef.current) {
        workerRef.current.postMessage({ type: 'RESET' } as WorkerMessage);
      }

      subscribe(channelName);
      wasConnectedRef.current = true;
    } else if (!isConnected && wasConnectedRef.current) {
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
