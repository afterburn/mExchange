const API_URL = (import.meta.env.VITE_API_URL || 'http://localhost:3000').trim();

export interface User {
  id: string;
  email: string;
}

export interface Balance {
  asset: string;
  available: string;
  locked: string;
}

export interface AuthResponse {
  access_token: string;
  user: User;
}

export interface OHLCVBar {
  open_time: string;
  open: string;
  high: string;
  low: string;
  close: string;
  volume: string;
  trade_count: number;
}

export interface Order {
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

export interface Trade {
  id: string;
  symbol: string;
  side: string;
  price: string;
  quantity: string;
  total: string;
  fee: string;
  settled_at: string;
}

export interface FaucetStatus {
  available: boolean;
  next_claim_at: string | null;
  last_claim_at: string | null;
  amount_per_claim: string;
  cooldown_hours: number;
}

export interface FaucetClaimResponse {
  success: boolean;
  asset: string;
  amount: string;
  new_balance: string;
  next_claim_at: string;
}

class AccountsAPI {
  private accessToken: string | null = null;

  setAccessToken(token: string | null) {
    this.accessToken = token;
  }

  private async fetch<T>(path: string, options: RequestInit = {}): Promise<T> {
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      ...options.headers as Record<string, string>,
    };

    if (this.accessToken) {
      headers['Authorization'] = `Bearer ${this.accessToken}`;
    }

    // Add timeout using AbortController
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 30000); // 30s timeout

    try {
      const res = await fetch(`${API_URL}${path}`, {
        ...options,
        headers,
        credentials: 'include', // Send cookies for refresh token
        signal: controller.signal,
      });

      clearTimeout(timeoutId);

      if (!res.ok) {
        const error = await res.json().catch(() => ({ error: 'Request failed' }));
        // Attach status code to error for special handling
        const err = new Error(error.error || 'Request failed') as Error & { status?: number };
        err.status = res.status;
        throw err;
      }

      return res.json();
    } catch (e) {
      clearTimeout(timeoutId);
      if (e instanceof Error && e.name === 'AbortError') {
        throw new Error('Request timed out');
      }
      throw e;
    }
  }

  async requestOtp(email: string): Promise<{ message: string }> {
    return this.fetch('/auth/request-otp', {
      method: 'POST',
      body: JSON.stringify({ email }),
    });
  }

  async verifyOtp(email: string, code: string): Promise<AuthResponse> {
    return this.fetch('/auth/verify-otp', {
      method: 'POST',
      body: JSON.stringify({ email, code }),
    });
  }

  async signup(email: string, code: string): Promise<AuthResponse> {
    return this.fetch('/auth/signup', {
      method: 'POST',
      body: JSON.stringify({ email, code }),
    });
  }

  async refresh(): Promise<{ access_token: string }> {
    return this.fetch('/auth/refresh', { method: 'POST' });
  }

  async logout(): Promise<void> {
    await this.fetch('/auth/logout', { method: 'POST' });
  }

  async getMe(): Promise<User> {
    return this.fetch('/api/me');
  }

  async getBalances(): Promise<{ balances: Balance[] }> {
    return this.fetch('/api/balances');
  }

  async getOHLCV(symbol: string, interval: string, limit: number = 500): Promise<{ data: OHLCVBar[] }> {
    return this.fetch(`/api/ohlcv?symbol=${encodeURIComponent(symbol)}&interval=${interval}&limit=${limit}`);
  }

  async deposit(asset: string, amount: string): Promise<{ success: boolean; balance: Balance }> {
    return this.fetch('/api/balances/deposit', {
      method: 'POST',
      body: JSON.stringify({ asset, amount }),
    });
  }

  async withdraw(asset: string, amount: string): Promise<{ success: boolean; balance: Balance }> {
    return this.fetch('/api/balances/withdraw', {
      method: 'POST',
      body: JSON.stringify({ asset, amount }),
    });
  }

  // Orders API (order placement is via WebSocket only)
  async getOrders(limit: number = 20, offset: number = 0): Promise<{ orders: Order[]; total: number; limit: number; offset: number }> {
    return this.fetch(`/api/orders?limit=${limit}&offset=${offset}`);
  }

  async getOrder(orderId: string): Promise<Order> {
    return this.fetch(`/api/orders/${orderId}`);
  }

  async getOrderFills(orderId: string): Promise<{ fills: Trade[] }> {
    return this.fetch(`/api/orders/${orderId}/fills`);
  }

  async getTrades(limit: number = 20, offset: number = 0): Promise<{ trades: Trade[]; total: number; limit: number; offset: number }> {
    return this.fetch(`/api/orders/trades?limit=${limit}&offset=${offset}`);
  }

  // Faucet API
  async getFaucetStatus(): Promise<FaucetStatus> {
    return this.fetch('/api/faucet/status');
  }

  async claimFaucet(): Promise<FaucetClaimResponse> {
    return this.fetch('/api/faucet/claim', {
      method: 'POST',
      body: JSON.stringify({ asset: 'KCN' }),
    });
  }
}

export const accountsAPI = new AccountsAPI();
