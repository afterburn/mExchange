import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import type { OrderBook, OrderBookLevel, Trade, MarketStats, Side, OrderType } from '../types';
import { useWebSocket, type OrderEvent, type OrderResultMessage, type CancelResultMessage, type AuthResultMessage, type TradeTuple } from './useWebSocket';
import { useAuthStore } from '../stores/authStore';

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
  _onTradeWithOrderId?: (trade: TradeWithOrderIds) => void,
  onOrderEvent?: (event: OrderEvent) => void
) {
  const [marketState, setMarketState] = useState<MarketState>(initialMarketState);
  const [isWsAuthenticated, setIsWsAuthenticated] = useState(false);
  const workerRef = useRef<Worker | null>(null);
  const updateScheduledRef = useRef(false);
  const lastUpdateTimeRef = useRef(0);
  const pendingUpdateRef = useRef<MarketStateUpdate | null>(null);

  // Auth store for token
  const accessToken = useAuthStore(state => state.accessToken);
  const user = useAuthStore(state => state.user);

  // Callbacks for order results
  const orderResultCallbackRef = useRef<((result: OrderResultMessage) => void) | null>(null);
  const cancelResultCallbackRef = useRef<((result: CancelResultMessage) => void) | null>(null);

  // Throttled state update
  const flushUpdate = useCallback(() => {
    const update = pendingUpdateRef.current;
    if (!update) return;

    // Debug: log when trades are in the update
    if (update.trades && update.trades.length > 0) {
      console.log('[flushUpdate] Received update with trades:', update.trades.length);
    }

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
      trades: TradeTuple[];
      bid_changes: Array<[number, number, number]>;
      ask_changes: Array<[number, number, number]>;
      total_bid_amount: number;
      total_ask_amount: number;
      time: number;
      stats_24h?: {
        high_24h: number;
        low_24h: number;
        volume_24h: number;
        open_24h: number;
        last_price: number;
      };
      snapshot?: boolean;
    };
  }) => {
    // Debug: log when trades arrive
    if (data.notification.trades && data.notification.trades.length > 0) {
      console.log('[useOrderBookWorker] Received trades:', data.notification.trades);
    }

    if (workerRef.current) {
      workerRef.current.postMessage({
        type: 'PROCESS_MESSAGE',
        data,
      } as WorkerMessage);
    }
  }, []);

  // Handle auth result
  const handleAuthResult = useCallback((result: AuthResultMessage) => {
    console.log('[WS Auth] Result:', result);
    setIsWsAuthenticated(result.success);
  }, []);

  // Handle order result
  const handleOrderResult = useCallback((result: OrderResultMessage) => {
    console.log('[WS Order] Result:', result);
    if (orderResultCallbackRef.current) {
      orderResultCallbackRef.current(result);
      orderResultCallbackRef.current = null;
    }
  }, []);

  // Handle cancel result
  const handleCancelResult = useCallback((result: CancelResultMessage) => {
    console.log('[WS Cancel] Result:', result);
    if (cancelResultCallbackRef.current) {
      cancelResultCallbackRef.current(result);
      cancelResultCallbackRef.current = null;
    }
  }, []);

  const {
    isConnected,
    isAuthenticated,
    authenticate,
    subscribe,
    placeOrder: wsPlaceOrder,
    cancelOrder: wsCancelOrder,
    send
  } = useWebSocket(GATEWAY_URL, {
    onMessage: handleChannelNotification,
    onOrderEvent,
    onAuthResult: handleAuthResult,
    onOrderResult: handleOrderResult,
    onCancelResult: handleCancelResult,
  });

  const channelName = 'book.KCN/EUR.none.10.100ms';
  const wasConnectedRef = useRef(false);
  const hasFetchedOHLCVRef = useRef(false);

  // Authenticate when connected and we have a token
  useEffect(() => {
    if (isConnected && accessToken && user) {
      console.log('[WS Auth] Sending auth token');
      authenticate(accessToken);
    }
  }, [isConnected, accessToken, user, authenticate]);

  // isWsAuthenticated is automatically reset when:
  // 1. The WebSocket reconnects (handled in useWebSocket onclose)
  // 2. Auth fails (handled in handleAuthResult)
  // When user logs out, the WebSocket will be re-established and we won't re-auth
  // because accessToken will be null

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

  // Place order via WebSocket (for authenticated users) or HTTP (for demo)
  const placeOrder = useCallback(async (side: Side, orderType: OrderType, price: number | null, quantity: number): Promise<{ orderId?: string }> => {
    // Demo mode - use HTTP endpoint
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

    return {};
  }, []);

  // Place order via WebSocket for authenticated users
  const placeOrderWs = useCallback((params: {
    symbol: string;
    side: 'bid' | 'ask';
    order_type: 'limit' | 'market';
    price?: number;
    quantity?: number;
    quote_amount?: number;
    max_slippage_price?: number;
  }): Promise<OrderResultMessage> => {
    return new Promise((resolve, reject) => {
      if (!isAuthenticated && !isWsAuthenticated) {
        reject(new Error('Not authenticated on WebSocket'));
        return;
      }

      orderResultCallbackRef.current = (result) => {
        if (result.success) {
          resolve(result);
        } else {
          reject(new Error(result.error || 'Order failed'));
        }
      };

      // Set timeout for response
      const timeout = setTimeout(() => {
        orderResultCallbackRef.current = null;
        reject(new Error('Order timeout'));
      }, 10000);

      orderResultCallbackRef.current = (result) => {
        clearTimeout(timeout);
        if (result.success) {
          resolve(result);
        } else {
          reject(new Error(result.error || 'Order failed'));
        }
      };

      const sent = wsPlaceOrder(params);
      if (!sent) {
        clearTimeout(timeout);
        orderResultCallbackRef.current = null;
        reject(new Error('WebSocket not connected'));
      }
    });
  }, [isAuthenticated, isWsAuthenticated, wsPlaceOrder]);

  // Cancel order via WebSocket
  const cancelOrderWs = useCallback((orderId: string): Promise<CancelResultMessage> => {
    return new Promise((resolve, reject) => {
      if (!isAuthenticated && !isWsAuthenticated) {
        reject(new Error('Not authenticated on WebSocket'));
        return;
      }

      const timeout = setTimeout(() => {
        cancelResultCallbackRef.current = null;
        reject(new Error('Cancel timeout'));
      }, 10000);

      cancelResultCallbackRef.current = (result) => {
        clearTimeout(timeout);
        if (result.success) {
          resolve(result);
        } else {
          reject(new Error(result.error || 'Cancel failed'));
        }
      };

      const sent = wsCancelOrder(orderId);
      if (!sent) {
        clearTimeout(timeout);
        cancelResultCallbackRef.current = null;
        reject(new Error('WebSocket not connected'));
      }
    });
  }, [isAuthenticated, isWsAuthenticated, wsCancelOrder]);

  return useMemo(() => ({
    orderBook: marketState.orderBook,
    trades: marketState.trades,
    priceHistory: marketState.priceHistory,
    stats: marketState.stats,
    placeOrder,
    placeOrderWs,
    cancelOrderWs,
    isConnected,
    isWsAuthenticated: isAuthenticated || isWsAuthenticated,
    send,
  }), [marketState, placeOrder, placeOrderWs, cancelOrderWs, isConnected, isAuthenticated, isWsAuthenticated, send]);
}
