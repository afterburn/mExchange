import { useEffect, useRef, useState, useCallback } from 'react';

interface ChannelNotification {
  channel_name: string;
  notification: {
    trades: Array<{
      price: number;
      quantity: number;
      side: string;
      timestamp: number;
    }>;
    bid_changes: Array<[number, number, number]>;
    ask_changes: Array<[number, number, number]>;
    total_bid_amount: number;
    total_ask_amount: number;
    time: number;
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

// Server response messages
export interface AuthResultMessage {
  type: 'auth_result';
  success: boolean;
  user_id?: string;
  error?: string;
}

export interface OrderResultMessage {
  type: 'order_result';
  success: boolean;
  order_id?: string;
  error?: string;
}

export interface CancelResultMessage {
  type: 'cancel_result';
  success: boolean;
  order_id: string;
  error?: string;
}

export type ServerMessage = AuthResultMessage | OrderResultMessage | CancelResultMessage;

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
            ws.send(JSON.stringify({ type: 'auth', token: pendingAuthRef.current }));
          }

          // Process pending subscriptions
          pendingSubscriptionsRef.current.forEach(channel => {
            ws.send(JSON.stringify({ type: 'subscribe', channel }));
            subscribedChannelsRef.current.add(channel);
          });
          pendingSubscriptionsRef.current.clear();
        };

        ws.onmessage = (event) => {
          if (!mountedRef.current) return;
          try {
            const data = JSON.parse(event.data);

            // Check message type
            if (data.type === 'order_filled' || data.type === 'order_cancelled') {
              if (onOrderEventRef.current) {
                onOrderEventRef.current(data as OrderEvent);
              }
              return;
            }

            if (data.type === 'auth_result') {
              setIsAuthenticated(data.success);
              if (onAuthResultRef.current) {
                onAuthResultRef.current(data as AuthResultMessage);
              }
              return;
            }

            if (data.type === 'order_result') {
              if (onOrderResultRef.current) {
                onOrderResultRef.current(data as OrderResultMessage);
              }
              return;
            }

            if (data.type === 'cancel_result') {
              if (onCancelResultRef.current) {
                onCancelResultRef.current(data as CancelResultMessage);
              }
              return;
            }

            // Channel notification
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
      wsRef.current.send(JSON.stringify({ type: 'auth', token }));
    }
  }, []);

  const subscribe = useCallback((channel: string) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      if (!subscribedChannelsRef.current.has(channel)) {
        wsRef.current.send(JSON.stringify({ type: 'subscribe', channel }));
        subscribedChannelsRef.current.add(channel);
        pendingSubscriptionsRef.current.delete(channel);
      }
    } else {
      pendingSubscriptionsRef.current.add(channel);
    }
  }, []);

  const unsubscribe = useCallback((channel: string) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify({ type: 'unsubscribe', channel }));
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
      const message = {
        type: 'place_order',
        ...params,
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
      const message = {
        type: 'cancel_order',
        order_id: orderId,
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
