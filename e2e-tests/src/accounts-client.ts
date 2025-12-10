/**
 * Accounts API Client
 * Handles authentication and API calls to the accounts service
 */

import WebSocket from 'ws';

const ACCOUNTS_URL = process.env.ACCOUNTS_URL || 'http://localhost:3001';
const GATEWAY_URL = process.env.GATEWAY_URL || 'http://localhost:3000';
const GATEWAY_WS_URL = process.env.GATEWAY_WS_URL || 'ws://localhost:3000/ws';

// Use dev-login when available (ENVIRONMENT=development on accounts service)
const USE_DEV_LOGIN = process.env.USE_DEV_LOGIN !== 'false';

export interface AuthResponse {
  access_token: string;
  user: {
    id: string;
    email: string;
  };
}

export interface BalanceResponse {
  asset: string;
  available: string;
  locked: string;
}

export interface OrderResponse {
  id: string;
  symbol: string;
  side: string;
  order_type: string;
  price: string | null;
  quantity: string;
  filled_quantity: string;
  status: string;
  created_at: string;
}

export interface PlaceOrderResponse {
  order: OrderResponse;
  locked_asset: string;
  locked_amount: string;
}

export interface TradeResponse {
  id: string;
  symbol: string;
  side: string;
  price: string;
  quantity: string;
  total: string;
  fee: string;
  settled_at: string;
}

// WebSocket message types
interface WsAuthResult {
  type: 'auth_result';
  success: boolean;
  user_id?: string;
  error?: string;
}

interface WsOrderResult {
  type: 'order_result';
  success: boolean;
  order_id?: string;
  error?: string;
}

interface WsCancelResult {
  type: 'cancel_result';
  success: boolean;
  order_id: string;
  error?: string;
}

type WsServerMessage = WsAuthResult | WsOrderResult | WsCancelResult;

export class AccountsClient {
  private accessToken: string | null = null;
  private userId: string | null = null;
  private email: string | null = null;

  // WebSocket for order placement/cancellation
  private ws: WebSocket | null = null;
  private wsConnected = false;
  private wsAuthenticated = false;
  private pendingOrderPromises: Map<string, { resolve: (orderId: string) => void; reject: (err: Error) => void }> = new Map();
  private pendingCancelPromises: Map<string, { resolve: () => void; reject: (err: Error) => void }> = new Map();
  private orderPromiseCounter = 0;

  get isAuthenticated(): boolean {
    return this.accessToken !== null;
  }

  get currentUserId(): string | null {
    return this.userId;
  }

  get currentEmail(): string | null {
    return this.email;
  }

  /**
   * Request OTP for email - returns the OTP code from the database
   * In production this would be sent via email, but for testing we read directly from DB
   */
  async requestOtp(email: string): Promise<{ message: string }> {
    const response = await fetch(`${ACCOUNTS_URL}/auth/request-otp`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Request OTP failed: ${error.error}`);
    }

    return response.json();
  }

  /**
   * Sign up with OTP verification
   */
  async signup(email: string, code: string): Promise<AuthResponse> {
    const response = await fetch(`${ACCOUNTS_URL}/auth/signup`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email, code }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Signup failed: ${error.error}`);
    }

    const auth: AuthResponse = await response.json();
    this.accessToken = auth.access_token;
    this.userId = auth.user.id;
    this.email = auth.user.email;
    return auth;
  }

  /**
   * Sign in with OTP verification
   */
  async verifyOtp(email: string, code: string): Promise<AuthResponse> {
    const response = await fetch(`${ACCOUNTS_URL}/auth/verify-otp`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email, code }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Verify OTP failed: ${error.error}`);
    }

    const auth: AuthResponse = await response.json();
    this.accessToken = auth.access_token;
    this.userId = auth.user.id;
    this.email = auth.user.email;
    return auth;
  }

  /**
   * Dev-only: Login/signup without OTP (requires ENVIRONMENT=development on server)
   */
  async devLogin(email: string): Promise<AuthResponse> {
    const response = await fetch(`${ACCOUNTS_URL}/auth/dev-login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email }),
    });

    if (!response.ok) {
      const text = await response.text();
      throw new Error(`Dev login failed (${response.status}): ${text}`);
    }

    const auth: AuthResponse = await response.json();
    this.accessToken = auth.access_token;
    this.userId = auth.user.id;
    this.email = auth.user.email;
    return auth;
  }

  /**
   * Login - uses dev-login if available, otherwise OTP flow
   */
  async login(email: string): Promise<AuthResponse> {
    if (USE_DEV_LOGIN) {
      return this.devLogin(email);
    }
    // Fall back to OTP flow - caller needs to handle OTP code
    throw new Error('OTP login not supported in automated tests without dev-login');
  }

  private authHeaders(): Record<string, string> {
    if (!this.accessToken) {
      throw new Error('Not authenticated');
    }
    return {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${this.accessToken}`,
    };
  }

  /**
   * Get user balances
   */
  async getBalances(): Promise<BalanceResponse[]> {
    const response = await fetch(`${ACCOUNTS_URL}/api/balances`, {
      method: 'GET',
      headers: this.authHeaders(),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Get balances failed: ${error.error}`);
    }

    const result = await response.json();
    return result.balances;
  }

  /**
   * Deposit EUR
   */
  async deposit(amount: string): Promise<{ success: boolean; balance: BalanceResponse }> {
    const response = await fetch(`${ACCOUNTS_URL}/api/balances/deposit`, {
      method: 'POST',
      headers: this.authHeaders(),
      body: JSON.stringify({ asset: 'EUR', amount }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Deposit failed: ${error.error}`);
    }

    return response.json();
  }

  /**
   * Claim KCN from faucet
   */
  async claimFaucet(): Promise<{ success: boolean; amount: string; new_balance: string }> {
    const response = await fetch(`${ACCOUNTS_URL}/api/faucet/claim`, {
      method: 'POST',
      headers: this.authHeaders(),
      body: JSON.stringify({ asset: 'KCN' }),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Faucet claim failed: ${error.error}`);
    }

    return response.json();
  }

  /**
   * Place an order (directly via accounts service - no matching engine)
   * Use placeOrderWithMatching() for orders that should be matched
   */
  async placeOrder(params: {
    symbol: string;
    side: 'bid' | 'ask';
    order_type: 'limit' | 'market';
    quantity: string;
    price?: string;
    max_slippage_price?: string;
  }): Promise<PlaceOrderResponse> {
    const response = await fetch(`${ACCOUNTS_URL}/api/orders`, {
      method: 'POST',
      headers: this.authHeaders(),
      body: JSON.stringify(params),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Place order failed: ${error.error}`);
    }

    return response.json();
  }

  /**
   * Connect to gateway WebSocket and authenticate
   */
  async connectWebSocket(timeoutMs: number = 5000): Promise<void> {
    if (this.wsConnected && this.wsAuthenticated) return;

    if (!this.accessToken) {
      throw new Error('Must be authenticated before connecting WebSocket');
    }

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error(`WebSocket connection timeout after ${timeoutMs}ms`));
      }, timeoutMs);

      this.ws = new WebSocket(GATEWAY_WS_URL);

      this.ws.on('open', () => {
        this.wsConnected = true;
        // Immediately authenticate
        this.ws!.send(JSON.stringify({ type: 'auth', token: this.accessToken }));
      });

      this.ws.on('message', (data) => {
        try {
          const msg = JSON.parse(data.toString()) as WsServerMessage;

          if (msg.type === 'auth_result') {
            clearTimeout(timeout);
            if (msg.success) {
              this.wsAuthenticated = true;
              resolve();
            } else {
              reject(new Error(`WebSocket auth failed: ${msg.error}`));
            }
          } else if (msg.type === 'order_result') {
            // Handle order result - resolve the oldest pending promise
            const [key, promise] = this.pendingOrderPromises.entries().next().value || [];
            if (key !== undefined && promise) {
              this.pendingOrderPromises.delete(key);
              if (msg.success && msg.order_id) {
                promise.resolve(msg.order_id);
              } else {
                promise.reject(new Error(msg.error || 'Order placement failed'));
              }
            }
          } else if (msg.type === 'cancel_result') {
            // Handle cancel result
            const promise = this.pendingCancelPromises.get(msg.order_id);
            if (promise) {
              this.pendingCancelPromises.delete(msg.order_id);
              if (msg.success) {
                promise.resolve();
              } else {
                promise.reject(new Error(msg.error || 'Cancel failed'));
              }
            }
          }
        } catch {
          // Ignore non-JSON messages (channel notifications)
        }
      });

      this.ws.on('error', (err) => {
        clearTimeout(timeout);
        reject(err);
      });

      this.ws.on('close', () => {
        this.wsConnected = false;
        this.wsAuthenticated = false;
        this.ws = null;
      });
    });
  }

  /**
   * Disconnect WebSocket
   */
  disconnectWebSocket(): void {
    if (this.ws) {
      this.ws.close();
      this.ws = null;
      this.wsConnected = false;
      this.wsAuthenticated = false;
    }
  }

  /**
   * Place an order through the gateway WebSocket (routes to matching engine for execution)
   * This is how the frontend places orders - they go through gateway which:
   * 1. Creates order in accounts service (locks funds)
   * 2. Forwards to matching engine for execution
   * 3. Settlement happens when fills are received
   */
  async placeOrderWithMatching(params: {
    symbol: string;
    side: 'bid' | 'ask';
    order_type: 'limit' | 'market';
    quantity: string;
    price?: string;
    max_slippage_price?: string;
  }): Promise<PlaceOrderResponse> {
    // Ensure WebSocket is connected and authenticated
    await this.connectWebSocket();

    if (!this.ws || !this.wsAuthenticated) {
      throw new Error('WebSocket not authenticated');
    }

    // Send order via WebSocket
    const orderId = await new Promise<string>((resolve, reject) => {
      const key = `order_${++this.orderPromiseCounter}`;
      this.pendingOrderPromises.set(key, { resolve, reject });

      this.ws!.send(JSON.stringify({
        type: 'place_order',
        symbol: params.symbol,
        side: params.side,
        order_type: params.order_type,
        quantity: parseFloat(params.quantity),
        price: params.price ? parseFloat(params.price) : null,
        max_slippage_price: params.max_slippage_price ? parseFloat(params.max_slippage_price) : null,
      }));

      // Timeout for order response
      setTimeout(() => {
        if (this.pendingOrderPromises.has(key)) {
          this.pendingOrderPromises.delete(key);
          reject(new Error('Order placement timeout'));
        }
      }, 10000);
    });

    // Fetch full order details from accounts service
    const order = await this.getOrder(orderId);

    // Calculate locked amount (approximation - actual is in the ledger)
    let lockedAsset: string;
    let lockedAmount: string;

    if (params.side === 'ask') {
      // Selling: lock base asset (e.g., KCN)
      lockedAsset = params.symbol.split('/')[0];
      lockedAmount = params.quantity;
    } else {
      // Buying: lock quote asset (e.g., EUR)
      lockedAsset = params.symbol.split('/')[1];
      const price = params.price || params.max_slippage_price || '1000';
      lockedAmount = (parseFloat(params.quantity) * parseFloat(price)).toString();
    }

    return {
      order,
      locked_asset: lockedAsset,
      locked_amount: lockedAmount,
    };
  }

  /**
   * Cancel an order via WebSocket (sends to matching engine)
   */
  async cancelOrderWithMatching(orderId: string): Promise<void> {
    // Ensure WebSocket is connected and authenticated
    await this.connectWebSocket();

    if (!this.ws || !this.wsAuthenticated) {
      throw new Error('WebSocket not authenticated');
    }

    return new Promise((resolve, reject) => {
      this.pendingCancelPromises.set(orderId, { resolve, reject });

      this.ws!.send(JSON.stringify({
        type: 'cancel_order',
        order_id: orderId,
      }));

      // Timeout for cancel response
      setTimeout(() => {
        if (this.pendingCancelPromises.has(orderId)) {
          this.pendingCancelPromises.delete(orderId);
          reject(new Error('Order cancellation timeout'));
        }
      }, 10000);
    });
  }

  /**
   * Get user orders
   */
  async getOrders(): Promise<OrderResponse[]> {
    const response = await fetch(`${ACCOUNTS_URL}/api/orders`, {
      method: 'GET',
      headers: this.authHeaders(),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Get orders failed: ${error.error}`);
    }

    const result = await response.json();
    return result.orders;
  }

  /**
   * Get specific order
   */
  async getOrder(orderId: string): Promise<OrderResponse> {
    const response = await fetch(`${ACCOUNTS_URL}/api/orders/${orderId}`, {
      method: 'GET',
      headers: this.authHeaders(),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Get order failed: ${error.error}`);
    }

    return response.json();
  }

  /**
   * Cancel an order
   */
  async cancelOrder(orderId: string): Promise<{ order: OrderResponse }> {
    const response = await fetch(`${ACCOUNTS_URL}/api/orders/${orderId}`, {
      method: 'DELETE',
      headers: this.authHeaders(),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Cancel order failed: ${error.error}`);
    }

    return response.json();
  }

  /**
   * Get user trades
   */
  async getTrades(): Promise<TradeResponse[]> {
    const response = await fetch(`${ACCOUNTS_URL}/api/orders/trades`, {
      method: 'GET',
      headers: this.authHeaders(),
    });

    if (!response.ok) {
      const error = await response.json();
      throw new Error(`Get trades failed: ${error.error}`);
    }

    const result = await response.json();
    return result.trades;
  }

  /**
   * Clear auth state and disconnect WebSocket
   */
  logout(): void {
    this.disconnectWebSocket();
    this.accessToken = null;
    this.userId = null;
    this.email = null;
  }
}
