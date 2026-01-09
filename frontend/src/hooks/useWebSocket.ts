import { useEffect, useRef, useState, useCallback } from 'react';

// Trade tuple format: [price, quantity, side, timestamp, is_liquidation]
export type TradeTuple = [number, number, string, number, boolean];

interface ChannelNotification {
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
}

// Ticker channel notification
export interface TickerNotification {
  channel_name: string;
  notification: {
    mark_price: number;
    mark_timestamp: number;
    best_bid_price: number;
    best_bid_amount: number;
    best_ask_price: number;
    best_ask_amount: number;
    last_price: number;
    delta: number;
    volume_24h: number;
    value_24h: number;
    low_price_24h: number;
    high_price_24h: number;
    change_24h: number;
    index: number;
    forward: number;
    funding_mark: number;
    funding_rate: number;
    collar_low: number;
    collar_high: number;
    realised_funding_24h: number;
    average_funding_rate_24h: number;
    open_interest: number;
  };
}

// LWT (Last, Best Bid/Ask, Mark) channel notification
export interface LwtNotification {
  channel_name: string;
  notification: {
    b: [number, number, number]; // best bid: [price, amount, total]
    a: [number, number, number]; // best ask: [price, amount, total]
    l: number; // last price
    m: number; // mark price
  };
}

// Index components channel notification
export interface IndexComponentsNotification {
  channel_name: string;
  notification: {
    index_price: number;
    components: Record<string, number>;
  };
}

// Direct market events from the gateway
export interface OrderFilledEvent {
  type: 'order_filled';
  order_id: string;
}

export interface OrderCancelledEvent {
  type: 'order_cancelled';
  order_id: string;
  filled_quantity: string;
}

export type OrderEvent = OrderFilledEvent | OrderCancelledEvent;

// JSON-RPC style response types
interface RpcError {
  code: number;
  message: string;
}

interface RpcSuccessResponse<T = unknown> {
  id: string;
  result: T;
}

interface RpcErrorResponse {
  id: string;
  error: RpcError;
}

// Auth result from server
interface AuthResult {
  access_token: string;
  token_type: string;
  user_id: string;
}

// Order result from server
interface OrderResultData {
  order_id: string;
}

// Cancel result from server
interface CancelResultData {
  order_id: string;
}

// Callback-friendly result types
export interface AuthResultMessage {
  success: boolean;
  user_id?: string;
  error?: string;
}

export interface OrderResultMessage {
  success: boolean;
  order_id?: string;
  error?: string;
}

export interface CancelResultMessage {
  success: boolean;
  order_id?: string;
  error?: string;
}

type MessageHandler = (data: ChannelNotification) => void;
type OrderEventHandler = (event: OrderEvent) => void;
type AuthResultHandler = (result: AuthResultMessage) => void;
type OrderResultHandler = (result: OrderResultMessage) => void;
type CancelResultHandler = (result: CancelResultMessage) => void;

interface UseWebSocketOptions {
  onMessage: MessageHandler;
  onOrderEvent?: OrderEventHandler;
  onAuthResult?: AuthResultHandler;
  onOrderResult?: OrderResultHandler;
  onCancelResult?: CancelResultHandler;
}

// Request ID counter for JSON-RPC style requests
let requestIdCounter = 0;
function getNextRequestId(): string {
  return String(++requestIdCounter);
}

export function useWebSocket(url: string, options: UseWebSocketOptions) {
  const { onMessage, onOrderEvent, onAuthResult, onOrderResult, onCancelResult } = options;

  const [isConnected, setIsConnected] = useState(false);
  const [isAuthenticated, setIsAuthenticated] = useState(false);
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimeoutRef = useRef<number | null>(null);
  const onMessageRef = useRef(onMessage);
  const onOrderEventRef = useRef(onOrderEvent);
  const onAuthResultRef = useRef(onAuthResult);
  const onOrderResultRef = useRef(onOrderResult);
  const onCancelResultRef = useRef(onCancelResult);
  const subscribedChannelsRef = useRef<Set<string>>(new Set());
  const pendingSubscriptionsRef = useRef<Set<string>>(new Set());
  const pendingAuthRef = useRef<string | null>(null);
  const mountedRef = useRef(true);
  // Track pending request IDs and their types for response routing
  const pendingRequestsRef = useRef<Map<string, string>>(new Map());

  // Keep handler refs updated
  useEffect(() => {
    onMessageRef.current = onMessage;
  }, [onMessage]);

  useEffect(() => {
    onOrderEventRef.current = onOrderEvent;
  }, [onOrderEvent]);

  useEffect(() => {
    onAuthResultRef.current = onAuthResult;
  }, [onAuthResult]);

  useEffect(() => {
    onOrderResultRef.current = onOrderResult;
  }, [onOrderResult]);

  useEffect(() => {
    onCancelResultRef.current = onCancelResult;
  }, [onCancelResult]);

  useEffect(() => {
    mountedRef.current = true;

    const connect = () => {
      if (wsRef.current?.readyState === WebSocket.OPEN) {
        return;
      }

      try {
        console.log('[WebSocket] Creating new WebSocket to', url);
        const ws = new WebSocket(url);
        wsRef.current = ws;
        console.log('[WebSocket] wsRef.current set to', ws);

        ws.onopen = () => {
          console.log('[WebSocket] onopen fired, mountedRef:', mountedRef.current);
          if (!mountedRef.current) return;

          console.log('[WebSocket] Connection established');
          setIsConnected(true);

          // Re-authenticate if we have a pending token
          if (pendingAuthRef.current) {
            const id = getNextRequestId();
            pendingRequestsRef.current.set(id, 'auth');
            ws.send(JSON.stringify({
              id,
              method: 'public/auth',
              params: { token: pendingAuthRef.current }
            }));
          }

          // Process pending subscriptions
          if (pendingSubscriptionsRef.current.size > 0) {
            const channels = Array.from(pendingSubscriptionsRef.current);
            const id = getNextRequestId();
            pendingRequestsRef.current.set(id, 'subscribe');
            ws.send(JSON.stringify({
              id,
              method: 'public/subscribe',
              params: { channels }
            }));
            channels.forEach(ch => subscribedChannelsRef.current.add(ch));
            pendingSubscriptionsRef.current.clear();
          }
        };

        ws.onmessage = (event) => {
          if (!mountedRef.current) return;
          try {
            const data = JSON.parse(event.data);

            // Check for order events (these are still pushed directly)
            if (data.type === 'order_filled' || data.type === 'order_cancelled') {
              if (onOrderEventRef.current) {
                onOrderEventRef.current(data as OrderEvent);
              }
              return;
            }

            // Handle JSON-RPC style responses (has 'id' field)
            if (data.id !== undefined) {
              const requestType = pendingRequestsRef.current.get(data.id);
              pendingRequestsRef.current.delete(data.id);

              // Check if it's an error response
              if (data.error) {
                const errorResponse = data as RpcErrorResponse;
                if (requestType === 'auth' && onAuthResultRef.current) {
                  onAuthResultRef.current({
                    success: false,
                    error: errorResponse.error.message
                  });
                } else if (requestType === 'place_order' && onOrderResultRef.current) {
                  onOrderResultRef.current({
                    success: false,
                    error: errorResponse.error.message
                  });
                } else if (requestType === 'cancel_order' && onCancelResultRef.current) {
                  onCancelResultRef.current({
                    success: false,
                    error: errorResponse.error.message
                  });
                }
                return;
              }

              // Success response
              if (requestType === 'auth') {
                const result = (data as RpcSuccessResponse<AuthResult>).result;
                setIsAuthenticated(true);
                if (onAuthResultRef.current) {
                  onAuthResultRef.current({
                    success: true,
                    user_id: result.user_id
                  });
                }
              } else if (requestType === 'place_order') {
                const result = (data as RpcSuccessResponse<OrderResultData>).result;
                if (onOrderResultRef.current) {
                  onOrderResultRef.current({
                    success: true,
                    order_id: result.order_id
                  });
                }
              } else if (requestType === 'cancel_order') {
                const result = (data as RpcSuccessResponse<CancelResultData>).result;
                if (onCancelResultRef.current) {
                  onCancelResultRef.current({
                    success: true,
                    order_id: result.order_id
                  });
                }
              }
              // subscribe/unsubscribe responses don't need callbacks
              return;
            }

            // Channel notification (no 'id' field, has 'channel_name')
            if (data.channel_name && data.notification) {
              onMessageRef.current(data as ChannelNotification);
            }
          } catch {
            // Silently ignore parse errors in production
          }
        };

        ws.onerror = () => {
          // Error handling without logging
        };

        ws.onclose = () => {
          if (!mountedRef.current) return;

          setIsConnected(false);
          setIsAuthenticated(false);
          subscribedChannelsRef.current.clear();
          pendingRequestsRef.current.clear();

          if (reconnectTimeoutRef.current) {
            clearTimeout(reconnectTimeoutRef.current);
          }

          reconnectTimeoutRef.current = window.setTimeout(() => {
            if (mountedRef.current) {
              connect();
            }
          }, 3000);
        };
      } catch {
        if (mountedRef.current) {
          setIsConnected(false);
        }
      }
    };

    connect();

    return () => {
      mountedRef.current = false;
      if (reconnectTimeoutRef.current) {
        clearTimeout(reconnectTimeoutRef.current);
      }
      if (wsRef.current) {
        wsRef.current.close();
        wsRef.current = null;
      }
    };
  }, [url]);

  const authenticate = useCallback((token: string) => {
    pendingAuthRef.current = token;
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      const id = getNextRequestId();
      pendingRequestsRef.current.set(id, 'auth');
      wsRef.current.send(JSON.stringify({
        id,
        method: 'public/auth',
        params: { token }
      }));
    }
  }, []);

  const subscribe = useCallback((channel: string) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      if (!subscribedChannelsRef.current.has(channel)) {
        const id = getNextRequestId();
        pendingRequestsRef.current.set(id, 'subscribe');
        wsRef.current.send(JSON.stringify({
          id,
          method: 'public/subscribe',
          params: { channels: [channel] }
        }));
        subscribedChannelsRef.current.add(channel);
        pendingSubscriptionsRef.current.delete(channel);
      }
    } else {
      pendingSubscriptionsRef.current.add(channel);
    }
  }, []);

  const unsubscribe = useCallback((channel: string) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      const id = getNextRequestId();
      pendingRequestsRef.current.set(id, 'unsubscribe');
      wsRef.current.send(JSON.stringify({
        id,
        method: 'unsubscribe',
        params: { channels: [channel] }
      }));
      subscribedChannelsRef.current.delete(channel);
    }
  }, []);

  const placeOrder = useCallback((params: {
    symbol: string;
    side: 'bid' | 'ask';
    order_type: 'limit' | 'market';
    price?: number;
    quantity?: number;
    quote_amount?: number;
    max_slippage_price?: number;
  }) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      const id = getNextRequestId();
      pendingRequestsRef.current.set(id, 'place_order');
      const message = {
        id,
        method: 'private/place_order',
        params,
      };
      console.log('[WebSocket] Sending place_order:', message);
      wsRef.current.send(JSON.stringify(message));
      return true;
    }
    console.log('[WebSocket] Cannot place order - not connected');
    return false;
  }, []);

  const cancelOrder = useCallback((orderId: string) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      const id = getNextRequestId();
      pendingRequestsRef.current.set(id, 'cancel_order');
      const message = {
        id,
        method: 'private/cancel_order',
        params: { order_id: orderId },
      };
      console.log('[WebSocket] Sending cancel_order:', message);
      wsRef.current.send(JSON.stringify(message));
      return true;
    }
    console.log('[WebSocket] Cannot cancel order - not connected');
    return false;
  }, []);

  // Not using useCallback - we want this to always use current wsRef
  const send = (message: unknown) => {
    console.log('[WebSocket] send called, wsRef.current:', wsRef.current, 'readyState:', wsRef.current?.readyState);
    if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
      const json = JSON.stringify(message);
      console.log('[WebSocket] Actually sending to socket:', json.slice(0, 200));
      wsRef.current.send(json);
      return true;
    }
    console.log('[WebSocket] Cannot send - not connected');
    return false;
  };

  return {
    isConnected,
    isAuthenticated,
    authenticate,
    subscribe,
    unsubscribe,
    placeOrder,
    cancelOrder,
    send,
  };
}
