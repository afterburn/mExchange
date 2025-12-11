import { useState, useCallback, useEffect, useMemo, useRef } from 'react';
import MarketStats from './components/MarketStats';
import PriceChart from './components/PriceChart';
import DepthChart from './components/DepthChart';
import OrderBook from './components/OrderBook';
import TradeForm from './components/TradeForm';
import RecentTrades from './components/RecentTrades';
import OpenOrders from './components/OpenOrders';
import InlineOrderHistory from './components/InlineOrderHistory';
import ErrorBoundary from './components/ErrorBoundary';
import { useOrderBookWorker, type TradeWithOrderIds, type OrderEvent } from './hooks/useOrderBookWorker';
import { useAuthStore } from './stores/authStore';
import { useToastStore } from './stores/toastStore';
import ToastContainer from './components/ToastContainer';
import EmbedTopBar from './components/EmbedTopBar';
import HeaderBar from './components/HeaderBar';
import { accountsAPI, type Order as APIOrder } from './api/accounts';
import type { Order } from './types';

type ChartView = 'price' | 'depth';
type OrdersTab = 'open' | 'history';

const DEFAULT_BALANCE = { eur: 0, kcn: 0 };

// Refresh token every 10 minutes (access tokens typically expire in 15 min)
const TOKEN_REFRESH_INTERVAL = 10 * 60 * 1000;

function App() {
  const { user, balances, fetchBalances, refreshToken } = useAuthStore();

  const [chartView, setChartView] = useState<ChartView>('price');
  const [ordersTab, setOrdersTab] = useState<OrdersTab>('open');
  const [openOrders, setOpenOrders] = useState<Order[]>([]);
  const [apiOrders, setApiOrders] = useState<APIOrder[]>([]);
  const [isLoadingOrders, setIsLoadingOrders] = useState(false);
  const [nextOrderId, setNextOrderId] = useState(1);

  // Use ref to track open order IDs for matching against incoming trades
  const openOrderIdsRef = useRef<Set<string>>(new Set());

  // Update ref when apiOrders changes
  useEffect(() => {
    const openIds = new Set(
      apiOrders
        .filter(o => o.status === 'open' || o.status === 'pending' || o.status === 'partially_filled')
        .map(o => o.id)
    );
    openOrderIdsRef.current = openIds;
  }, [apiOrders]);

  const fetchOrders = useCallback(async () => {
    if (!user) return;
    setIsLoadingOrders(true);
    try {
      const { orders } = await accountsAPI.getOrders();
      setApiOrders(orders);
    } catch (e) {
      console.error('Failed to fetch orders:', e);
    } finally {
      setIsLoadingOrders(false);
    }
  }, [user]);

  const addToast = useToastStore((state) => state.addToast);

  // Track fills per order for detailed toast messages when order completes
  const orderFillsRef = useRef<Map<string, { side: 'Buy' | 'Sell'; totalQty: number; totalValue: number; count: number }>>(new Map());

  // Callback for when trades with order IDs come in via WebSocket
  const handleTradeWithOrderId = useCallback((trade: TradeWithOrderIds) => {
    // Check if this trade involves one of the user's open orders
    const matchedOrderId = trade.buy_order_id && openOrderIdsRef.current.has(trade.buy_order_id)
      ? trade.buy_order_id
      : trade.sell_order_id && openOrderIdsRef.current.has(trade.sell_order_id)
        ? trade.sell_order_id
        : null;

    if (matchedOrderId) {
      const side = trade.buy_order_id === matchedOrderId ? 'Buy' : 'Sell';
      const tradeValue = trade.quantity * trade.price;
      const existing = orderFillsRef.current.get(matchedOrderId);

      if (existing) {
        existing.totalQty += trade.quantity;
        existing.totalValue += tradeValue;
        existing.count += 1;
      } else {
        orderFillsRef.current.set(matchedOrderId, {
          side,
          totalQty: trade.quantity,
          totalValue: tradeValue,
          count: 1,
        });
      }

      // Update open orders state in real-time for partial fills
      setApiOrders(prev => prev.map(order => {
        if (order.id === matchedOrderId) {
          const newFilledQty = parseFloat(order.filled_quantity) + trade.quantity;
          const totalQty = parseFloat(order.quantity);
          return {
            ...order,
            filled_quantity: newFilledQty.toString(),
            status: newFilledQty >= totalQty ? 'filled' : 'partially_filled',
          };
        }
        return order;
      }));
    }
  }, []);

  // Handle order lifecycle events (order_filled, order_cancelled)
  const handleOrderEvent = useCallback((event: OrderEvent) => {
    if (event.type === 'order_filled') {
      // Check if this is one of our orders
      if (openOrderIdsRef.current.has(event.order_id)) {
        // Get accumulated fill data for this order
        const fillData = orderFillsRef.current.get(event.order_id);

        // Refresh balances and orders
        fetchOrders();
        fetchBalances();

        // Show toast with fill details
        if (fillData) {
          const avgPrice = fillData.totalValue / fillData.totalQty;
          addToast({
            type: 'success',
            title: `${fillData.side} Order Filled`,
            message: `${fillData.totalQty.toFixed(2)} KCN @ ${avgPrice.toFixed(2)} EUR avg${fillData.count > 1 ? ` (${fillData.count} fills)` : ''}`,
          });
          orderFillsRef.current.delete(event.order_id);
        } else {
          addToast({
            type: 'success',
            title: 'Order Filled',
            message: 'Your order has been completely filled',
          });
        }
      }
    } else if (event.type === 'order_cancelled') {
      // Check if this is one of our orders
      if (openOrderIdsRef.current.has(event.order_id)) {
        fetchOrders();
        fetchBalances();

        const fillData = orderFillsRef.current.get(event.order_id);
        const filledQty = parseFloat(event.filled_quantity) || 0;

        if (filledQty > 0 && fillData && fillData.totalQty > 0) {
          // Partial fill - show as success (common for market orders hitting liquidity limits)
          const avgPrice = fillData.totalValue / fillData.totalQty;
          const totalValue = fillData.totalValue;
          addToast({
            type: 'success',
            title: `${fillData.side} Order Executed`,
            message: `${fillData.side === 'Buy' ? 'Bought' : 'Sold'} ${fillData.totalQty.toFixed(2)} KCN @ ${avgPrice.toFixed(2)} EUR avg (${totalValue.toFixed(2)} EUR total). Remaining returned - insufficient liquidity.`,
          });
        } else if (filledQty > 0) {
          // Partial fill but no fill tracking data
          addToast({
            type: 'success',
            title: 'Order Executed (Partial)',
            message: `Filled ${filledQty.toFixed(2)} KCN. Remaining returned - insufficient liquidity.`,
          });
        } else {
          // No fills - actual cancellation
          addToast({
            type: 'info',
            title: 'Order Cancelled',
            message: 'Your order has been cancelled',
          });
        }
        orderFillsRef.current.delete(event.order_id);
      }
    }
  }, [fetchOrders, fetchBalances, addToast]);

  const { orderBook, trades, stats, placeOrder, placeOrderWs, cancelOrderWs, isConnected, isWsAuthenticated } = useOrderBookWorker(handleTradeWithOrderId, handleOrderEvent);

  // Update document title with current price
  useEffect(() => {
    if (stats.lastPrice) {
      document.title = `€${stats.lastPrice.toFixed(2)} - KCN/EUR`;
    } else {
      document.title = 'KCN/EUR';
    }
  }, [stats.lastPrice]);

  // Fetch balances and orders on mount if logged in
  useEffect(() => {
    if (user) {
      fetchBalances();
      fetchOrders();
    }
  }, [user, fetchBalances, fetchOrders]);

  // Periodic token refresh to keep session alive
  useEffect(() => {
    if (!user) return;

    const intervalId = setInterval(() => {
      refreshToken();
    }, TOKEN_REFRESH_INTERVAL);

    return () => clearInterval(intervalId);
  }, [user, refreshToken]);

  // Convert balances array to object for TradeForm
  const balance = useMemo(() => {
    if (!balances.length) return DEFAULT_BALANCE;
    const eurBalance = balances.find(b => b.asset === 'EUR');
    const kcnBalance = balances.find(b => b.asset === 'KCN');
    return {
      eur: eurBalance ? parseFloat(eurBalance.available) : 0,
      kcn: kcnBalance ? parseFloat(kcnBalance.available) : 0,
    };
  }, [balances]);

  const handlePlaceOrder = useCallback(async (side: 'Bid' | 'Ask', orderType: 'Limit' | 'Market', price: number | null, quantity: number | null, quoteAmount?: number) => {
    if (user) {
      // Logged in: place order through WebSocket
      if (!isWsAuthenticated) {
        addToast({
          type: 'error',
          title: 'Not Ready',
          message: 'Connecting to trading server... please try again in a moment',
        });
        return;
      }

      try {
        // For market buy orders, calculate max slippage price (best ask + 5% buffer)
        let maxSlippagePrice: number | undefined;
        if (orderType === 'Market' && side === 'Bid' && stats.bestAsk) {
          const slippageBuffer = 1.05; // 5% slippage tolerance
          maxSlippagePrice = stats.bestAsk * slippageBuffer;
        }

        await placeOrderWs({
          symbol: 'KCN/EUR',
          side: side.toLowerCase() as 'bid' | 'ask',
          order_type: orderType.toLowerCase() as 'limit' | 'market',
          quantity: quantity !== null ? quantity : undefined,
          price: price !== null ? price : undefined,
          max_slippage_price: maxSlippagePrice,
          quote_amount: quoteAmount,
        });

        // Refresh balances and orders after placing
        fetchBalances();
        fetchOrders();
      } catch (e) {
        console.error('Failed to place order:', e);
        addToast({
          type: 'error',
          title: 'Order Failed',
          message: (e as Error).message,
        });
      }
    } else {
      // Not logged in: just forward to matching engine (demo mode via WebSocket)
      // Demo mode requires quantity, so use calculated quantity
      if (quantity !== null) {
        placeOrder(side, orderType, price, quantity);
        if (orderType === 'Limit' && price !== null) {
          setOpenOrders(prev => [...prev, {
            id: nextOrderId,
            side,
            orderType,
            price,
            quantity,
            remainingQuantity: quantity,
          }]);
          setNextOrderId(prev => prev + 1);
        }
      }
    }
  }, [user, isWsAuthenticated, placeOrderWs, placeOrder, nextOrderId, fetchBalances, fetchOrders, stats.bestAsk, addToast]);

  const handleCancelOrder = useCallback(async (orderId: number | string) => {
    // Check if it's an API order (UUID string) or local order (number)
    if (typeof orderId === 'string') {
      if (!isWsAuthenticated) {
        addToast({
          type: 'error',
          title: 'Not Ready',
          message: 'Connecting to trading server... please try again in a moment',
        });
        return;
      }

      // Use WebSocket for cancellation
      try {
        await cancelOrderWs(orderId);
        fetchBalances();
        fetchOrders();
      } catch (e) {
        console.error('Failed to cancel order:', e);
        addToast({
          type: 'error',
          title: 'Cancel Failed',
          message: (e as Error).message,
        });
      }
    } else {
      // Demo mode: just remove from local state (no persistent backend)
      setOpenOrders(prev => prev.filter(o => o.id !== orderId));
    }
  }, [isWsAuthenticated, cancelOrderWs, fetchBalances, fetchOrders, addToast]);

  // Convert API orders to display format for OpenOrders component
  const displayOrders = useMemo((): Order[] => {
    if (user && apiOrders.length > 0) {
      return apiOrders
        .filter(o => ['pending', 'open', 'partially_filled'].includes(o.status))
        .map(o => ({
          id: o.id as unknown as number, // Use string ID but cast for type compatibility
          side: o.side === 'bid' ? 'Bid' : 'Ask',
          orderType: o.order_type === 'limit' ? 'Limit' : 'Market',
          price: o.price ? parseFloat(o.price) : null,
          quantity: parseFloat(o.quantity),
          remainingQuantity: parseFloat(o.quantity) - parseFloat(o.filled_quantity),
        }));
    }
    return openOrders;
  }, [user, apiOrders, openOrders]);

  const handleSetPriceChart = useCallback(() => setChartView('price'), []);
  const handleSetDepthChart = useCallback(() => setChartView('depth'), []);

  return (
    <ErrorBoundary>
      <ToastContainer />
      <div className="flex flex-col h-screen bg-black text-white overflow-hidden">
        <EmbedTopBar />

        <HeaderBar />

        <MarketStats stats={stats} />

        <div className="flex-1 trading-grid">
          {/* Chart Section */}
          <div className="grid-chart flex flex-col min-w-0 border-b md:border-b-0 md:border-r border-white/10">
            <div className="flex gap-1 px-3 py-1.5 border-b border-white/10 text-xs shrink-0">
              <button
                onClick={handleSetPriceChart}
                className={`px-2 py-1 rounded transition-colors ${
                  chartView === 'price'
                    ? 'text-white bg-white/10 border border-white/20'
                    : 'text-white/60 hover:text-white hover:bg-white/10'
                }`}
              >
                Price
              </button>
              <button
                onClick={handleSetDepthChart}
                className={`px-2 py-1 rounded transition-colors ${
                  chartView === 'depth'
                    ? 'text-white bg-white/10 border border-white/20'
                    : 'text-white/60 hover:text-white hover:bg-white/10'
                }`}
              >
                Depth
              </button>
            </div>
            <div className="flex-1 min-h-0">
              {chartView === 'price' ? (
                <PriceChart trades={trades} />
              ) : (
                <DepthChart bids={orderBook.bids} asks={orderBook.asks} />
              )}
            </div>
          </div>

          {/* OrderBook Section */}
          <div className="grid-orderbook flex flex-col border-b md:border-b-0 md:border-r border-white/10">
            <div className="px-3 py-2 border-b border-white/10 text-xs font-medium text-white/60 shrink-0">Order Book</div>
            <div className="flex-1 overflow-hidden">
              <OrderBook bids={orderBook.bids} asks={orderBook.asks} maxLevels={10} />
            </div>
          </div>

          {/* TradeForm Section */}
          <div className="grid-tradeform border-b md:border-b-0 md:border-l border-white/10 flex flex-col md:overflow-hidden">
            <TradeForm
              onPlaceOrder={handlePlaceOrder}
              bestBid={stats.bestBid}
              bestAsk={stats.bestAsk}
              balance={balance}
              isLoggedIn={!!user}
            />
          </div>

          {/* Open Orders Section */}
          <div className="grid-openorders border-t border-white/10 flex flex-col min-h-0 overflow-hidden">
            <div className="flex items-center gap-4 px-3 py-1.5 border-b border-white/10 shrink-0">
              <button
                onClick={() => setOrdersTab('open')}
                className={`text-xs font-medium transition-colors ${
                  ordersTab === 'open' ? 'text-white' : 'text-white/40 hover:text-white/60'
                }`}
              >
                Open Orders
              </button>
              <button
                onClick={() => setOrdersTab('history')}
                className={`text-xs font-medium transition-colors ${
                  ordersTab === 'history' ? 'text-white' : 'text-white/40 hover:text-white/60'
                }`}
              >
                Order History
              </button>
            </div>
            <div className="flex-1 overflow-hidden">
              {ordersTab === 'open' ? (
                <OpenOrders orders={displayOrders} onCancel={handleCancelOrder} />
              ) : (
                <InlineOrderHistory orders={apiOrders} isLoading={isLoadingOrders} />
              )}
            </div>
          </div>

          {/* Recent Trades Section */}
          <div className="grid-recenttrades border-t md:border-l border-white/10 flex flex-col min-h-0 overflow-hidden">
            <div className="px-3 py-1.5 border-b border-white/10 text-xs font-medium text-white/60 shrink-0">Recent Trades</div>
            <div className="flex-1 overflow-y-auto">
              <RecentTrades trades={trades} />
            </div>
          </div>
        </div>

        {/* Bottom Status Bar */}
        <div className="h-6 border-t border-white/10 bg-black flex items-center px-4 text-xs">
          <div className="flex items-center gap-2">
            <span className={`w-2 h-2 rounded-full ${isConnected ? 'bg-green-500' : 'bg-yellow-500 animate-pulse'}`} />
            <span className={isConnected ? 'text-white/60' : 'text-yellow-500'}>
              {isConnected ? 'Connected' : 'Connecting...'}
            </span>
          </div>
          <div className="ml-auto text-white/40">
            KCN/EUR
          </div>
        </div>
      </div>
    </ErrorBoundary>
  );
}

export default App;
