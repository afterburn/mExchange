import WebSocket from 'ws';

export interface PriceLevel {
  price: number;
  quantity: number;
}

export interface OrderBookState {
  bids: Map<number, number>;  // price -> quantity
  asks: Map<number, number>;
  lastUpdateTime: number;
}

export interface Trade {
  price: number;
  quantity: number;
  side: string;
  timestamp: number;
}

export interface ChannelNotification {
  channel_name: string;
  notification: {
    trades: Trade[];
    bid_changes: Array<[number, number, number]>;  // [price, old_qty, new_qty]
    ask_changes: Array<[number, number, number]>;
    total_bid_amount: number;
    total_ask_amount: number;
    time: number;
    snapshot?: boolean;
  };
}

type MessageHandler = (data: unknown) => void;

// Request ID counter for JSON-RPC style requests
let requestIdCounter = 0;
function getNextRequestId(): string {
  return String(++requestIdCounter);
}

export class TestClient {
  private ws: WebSocket | null = null;
  private readonly url: string;
  private readonly symbol: string;
  private connected = false;
  private subscribed = false;

  public orderbook: OrderBookState = {
    bids: new Map(),
    asks: new Map(),
    lastUpdateTime: 0
  };
  public trades: Trade[] = [];
  public errors: string[] = [];
  public updateCount = 0;

  private messageHandlers: MessageHandler[] = [];

  constructor(url: string = 'ws://localhost:3000/ws', symbol: string = 'KCN/EUR') {
    this.url = url;
    this.symbol = symbol;
  }

  async connect(timeoutMs: number = 5000): Promise<void> {
    if (this.connected) return;

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error(`Connection timeout after ${timeoutMs}ms`));
      }, timeoutMs);

      this.ws = new WebSocket(this.url);

      this.ws.on('open', () => {
        clearTimeout(timeout);
        this.connected = true;
        resolve();
      });

      this.ws.on('error', (err) => {
        clearTimeout(timeout);
        this.errors.push(`WebSocket error: ${err.message}`);
        reject(err);
      });

      this.ws.on('close', () => {
        this.connected = false;
        this.subscribed = false;
      });

      this.ws.on('message', (data) => {
        this.handleMessage(data.toString());
      });
    });
  }

  private handleMessage(raw: string): void {
    try {
      const msg = JSON.parse(raw) as ChannelNotification;

      // Notify all handlers
      for (const handler of this.messageHandlers) {
        handler(msg);
      }

      // Handle channel notification (orderbook updates)
      if (msg.channel_name && msg.notification) {
        this.applyNotification(msg);
        this.updateCount++;

        if (!this.subscribed) {
          this.subscribed = true;
        }
      }

    } catch (e) {
      this.errors.push(`Failed to parse message: ${raw.slice(0, 100)}`);
    }
  }

  private applyNotification(msg: ChannelNotification): void {
    const { notification } = msg;

    // Apply bid changes
    for (const [price, , newQty] of notification.bid_changes) {
      if (newQty === 0) {
        this.orderbook.bids.delete(price);
      } else {
        this.orderbook.bids.set(price, newQty);
      }
    }

    // Apply ask changes
    for (const [price, , newQty] of notification.ask_changes) {
      if (newQty === 0) {
        this.orderbook.asks.delete(price);
      } else {
        this.orderbook.asks.set(price, newQty);
      }
    }

    // Record trades
    if (notification.trades && notification.trades.length > 0) {
      this.trades.push(...notification.trades);
    }

    this.orderbook.lastUpdateTime = notification.time;
  }

  async subscribe(timeoutMs: number = 5000): Promise<void> {
    if (!this.connected || !this.ws) {
      throw new Error('Not connected');
    }

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error(`Subscribe timeout after ${timeoutMs}ms`));
      }, timeoutMs);

      const handler = (msg: unknown) => {
        const m = msg as ChannelNotification;
        if (m.channel_name && m.notification) {
          clearTimeout(timeout);
          this.removeMessageHandler(handler);
          resolve();
        }
      };

      this.addMessageHandler(handler);

      this.ws.send(JSON.stringify({
        id: getNextRequestId(),
        method: 'public/subscribe',
        params: { channels: [`book.${this.symbol}.none.10.100ms`] },
      }));
    });
  }

  async placeOrder(
    side: 'bid' | 'ask',
    orderType: 'limit' | 'market',
    quantity: number,
    price?: number
  ): Promise<void> {
    if (!this.connected || !this.ws) {
      throw new Error('Not connected');
    }

    const order = {
      side,
      order_type: orderType,
      quantity,
      price: price ?? null,
    };

    this.ws.send(JSON.stringify({
      type: 'orders',
      orders: [order],
    }));
  }

  async placeBatchOrders(
    orders: Array<{ side: 'bid' | 'ask'; order_type: 'limit' | 'market'; quantity: number; price?: number }>
  ): Promise<void> {
    if (!this.connected || !this.ws) {
      throw new Error('Not connected');
    }

    this.ws.send(JSON.stringify({
      type: 'orders',
      orders: orders.map(o => ({
        side: o.side,
        order_type: o.order_type,
        quantity: o.quantity,
        price: o.price ?? null,
      })),
    }));
  }

  async waitForOrderbookUpdate(timeoutMs: number = 5000): Promise<void> {
    const startCount = this.updateCount;

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error(`Orderbook update timeout after ${timeoutMs}ms`));
      }, timeoutMs);

      const handler = () => {
        if (this.updateCount > startCount) {
          clearTimeout(timeout);
          this.removeMessageHandler(handler);
          resolve();
        }
      };

      this.addMessageHandler(handler);
    });
  }

  async waitForTrade(timeoutMs: number = 5000): Promise<Trade> {
    const startCount = this.trades.length;

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error(`Trade timeout after ${timeoutMs}ms`));
      }, timeoutMs);

      const handler = () => {
        if (this.trades.length > startCount) {
          clearTimeout(timeout);
          this.removeMessageHandler(handler);
          resolve(this.trades[this.trades.length - 1]);
        }
      };

      this.addMessageHandler(handler);
    });
  }

  addMessageHandler(handler: MessageHandler): void {
    this.messageHandlers.push(handler);
  }

  removeMessageHandler(handler: MessageHandler): void {
    const idx = this.messageHandlers.indexOf(handler);
    if (idx >= 0) {
      this.messageHandlers.splice(idx, 1);
    }
  }

  async disconnect(): Promise<void> {
    if (this.ws) {
      this.ws.close();
      this.ws = null;
      this.connected = false;
      this.subscribed = false;
      // Reset orderbook state for clean reconnection
      this.orderbook = {
        bids: new Map(),
        asks: new Map(),
        lastUpdateTime: 0
      };
      this.updateCount = 0;
      this.messageHandlers = [];
    }
  }

  isConnected(): boolean {
    return this.connected;
  }

  isSubscribed(): boolean {
    return this.subscribed;
  }

  getBestBid(): PriceLevel | null {
    if (this.orderbook.bids.size === 0) return null;
    const prices = Array.from(this.orderbook.bids.keys()).sort((a, b) => b - a);
    const price = prices[0];
    return { price, quantity: this.orderbook.bids.get(price)! };
  }

  getBestAsk(): PriceLevel | null {
    if (this.orderbook.asks.size === 0) return null;
    const prices = Array.from(this.orderbook.asks.keys()).sort((a, b) => a - b);
    const price = prices[0];
    return { price, quantity: this.orderbook.asks.get(price)! };
  }

  getSpread(): number | null {
    const bid = this.getBestBid();
    const ask = this.getBestAsk();
    if (!bid || !ask) return null;
    return ask.price - bid.price;
  }

  getBidLevels(limit: number = 10): PriceLevel[] {
    const prices = Array.from(this.orderbook.bids.keys()).sort((a, b) => b - a);
    return prices.slice(0, limit).map(price => ({
      price,
      quantity: this.orderbook.bids.get(price)!,
    }));
  }

  getAskLevels(limit: number = 10): PriceLevel[] {
    const prices = Array.from(this.orderbook.asks.keys()).sort((a, b) => a - b);
    return prices.slice(0, limit).map(price => ({
      price,
      quantity: this.orderbook.asks.get(price)!,
    }));
  }

  clearState(): void {
    this.trades = [];
    this.errors = [];
  }
}
