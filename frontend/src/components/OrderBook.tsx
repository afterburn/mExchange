import { memo, useMemo, useRef, useEffect, useCallback, useState } from 'react';
import type { OrderBookLevel } from '../types';

interface OrderBookProps {
  bids: OrderBookLevel[];
  asks: OrderBookLevel[];
  maxLevels?: number;
}

const ROW_HEIGHT = 24; // Fixed row height for virtualization
const OVERSCAN = 3;

// Memoized row components using pre-formatted strings and CSS custom properties
const AskRow = memo(function AskRow({
  ask,
  widthPercent,
}: {
  ask: OrderBookLevel;
  widthPercent: number;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (ref.current) {
      ref.current.style.setProperty('--depth-width', `${widthPercent}%`);
    }
  }, [widthPercent]);

  return (
    <div ref={ref} className="orderbook-row orderbook-row--ask">
      <span className="orderbook-price orderbook-price--ask">{ask.priceStr}</span>
      <span className="orderbook-qty">{ask.quantityStr}</span>
      <span className="orderbook-total">{ask.totalStr}</span>
    </div>
  );
});

const BidRow = memo(function BidRow({
  bid,
  widthPercent,
}: {
  bid: OrderBookLevel;
  widthPercent: number;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (ref.current) {
      ref.current.style.setProperty('--depth-width', `${widthPercent}%`);
    }
  }, [widthPercent]);

  return (
    <div ref={ref} className="orderbook-row orderbook-row--bid">
      <span className="orderbook-price orderbook-price--bid">{bid.priceStr}</span>
      <span className="orderbook-qty">{bid.quantityStr}</span>
      <span className="orderbook-total">{bid.totalStr}</span>
    </div>
  );
});

const SpreadRow = memo(function SpreadRow({
  midPriceStr,
  spreadStr,
  spreadPercentStr,
}: {
  midPriceStr: string;
  spreadStr: string;
  spreadPercentStr: string;
}) {
  return (
    <div className="border-t border-b border-white/10 py-2 px-3 bg-white/5 flex justify-between items-center">
      <span className="text-sm font-semibold text-white">{midPriceStr}</span>
      <span className="text-xs text-white/40">
        Spread: {spreadStr} ({spreadPercentStr} bps)
      </span>
    </div>
  );
});

// Virtualized row data
interface VirtualRow {
  type: 'ask' | 'bid' | 'spread';
  level?: OrderBookLevel;
  widthPercent?: number;
  spreadInfo?: { midPriceStr: string; spreadStr: string; spreadPercentStr: string };
}

const OrderBook = memo(function OrderBook({ bids, asks, maxLevels = 20 }: OrderBookProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [containerHeight, setContainerHeight] = useState(300);

  // Compute all row data
  const { allRows } = useMemo(() => {
    const displayAsks = asks.slice(0, maxLevels);
    const displayBids = bids.slice(0, maxLevels);

    // Calculate cumulative volumes
    const asksCumulative: number[] = [];
    let askSum = 0;
    for (let i = 0; i < displayAsks.length; i++) {
      askSum += displayAsks[i].quantity;
      asksCumulative.push(askSum);
    }

    const bidsCumulative: number[] = [];
    let bidSum = 0;
    for (let i = 0; i < displayBids.length; i++) {
      bidSum += displayBids[i].quantity;
      bidsCumulative.push(bidSum);
    }

    const maxCumulativeVolume = Math.max(
      asksCumulative[asksCumulative.length - 1] || 0,
      bidsCumulative[bidsCumulative.length - 1] || 0
    );

    // Build row array: asks (reversed) + spread + bids
    const rows: VirtualRow[] = [];

    // Asks in reverse order (lowest ask at bottom, near spread)
    for (let i = displayAsks.length - 1; i >= 0; i--) {
      const cumulativeVolume = asksCumulative[i] || 0;
      const widthPercent = maxCumulativeVolume > 0 ? (cumulativeVolume / maxCumulativeVolume) * 100 : 0;
      rows.push({ type: 'ask', level: displayAsks[i], widthPercent });
    }

    // Spread row
    if (bids.length > 0 && asks.length > 0) {
      const bestBid = bids[0]?.price || 0;
      const bestAsk = asks[0]?.price || 0;
      const midPrice = (bestBid + bestAsk) / 2;
      const spread = bestAsk - bestBid;
      const spreadBps = bestBid > 0 ? (spread / bestBid) * 10000 : 0;
      rows.push({
        type: 'spread',
        spreadInfo: {
          midPriceStr: midPrice.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 }),
          spreadStr: spread.toFixed(2),
          spreadPercentStr: spreadBps.toFixed(1),
        },
      });
    }

    // Bids
    for (let i = 0; i < displayBids.length; i++) {
      const cumulativeVolume = bidsCumulative[i] || 0;
      const widthPercent = maxCumulativeVolume > 0 ? (cumulativeVolume / maxCumulativeVolume) * 100 : 0;
      rows.push({ type: 'bid', level: displayBids[i], widthPercent });
    }

    return { allRows: rows };
  }, [asks, bids, maxLevels]);

  // Calculate visible range for virtualization
  const { visibleRows, totalHeight, paddingTop } = useMemo(() => {
    const totalHeight = allRows.length * ROW_HEIGHT;
    const startIndex = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
    const visibleCount = Math.ceil(containerHeight / ROW_HEIGHT) + 2 * OVERSCAN;
    const endIndex = Math.min(allRows.length, startIndex + visibleCount);

    const visibleRows: (VirtualRow & { index: number })[] = [];
    for (let i = startIndex; i < endIndex; i++) {
      visibleRows.push({ ...allRows[i], index: i });
    }

    return {
      visibleRows,
      totalHeight,
      paddingTop: startIndex * ROW_HEIGHT,
    };
  }, [allRows, scrollTop, containerHeight]);

  // Handle scroll with RAF throttling
  const rafRef = useRef<number | null>(null);
  const handleScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
    if (rafRef.current) cancelAnimationFrame(rafRef.current);
    rafRef.current = requestAnimationFrame(() => {
      setScrollTop(e.currentTarget.scrollTop);
    });
  }, []);

  // Measure container
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        setContainerHeight(entry.contentRect.height);
      }
    });

    observer.observe(container);
    setContainerHeight(container.clientHeight);

    return () => observer.disconnect();
  }, []);

  // Cleanup
  useEffect(() => {
    return () => {
      if (rafRef.current) cancelAnimationFrame(rafRef.current);
    };
  }, []);

  return (
    <div className="flex flex-col h-full">
      <style>{`
        .orderbook-row {
          position: relative;
          display: flex;
          justify-content: space-between;
          align-items: center;
          padding: 2px 12px;
          font-size: 12px;
          cursor: pointer;
          height: ${ROW_HEIGHT}px;
          box-sizing: border-box;
        }
        .orderbook-row:hover {
          background-color: rgba(255, 255, 255, 0.05);
        }
        .orderbook-row::before {
          content: '';
          position: absolute;
          right: 0;
          top: 0;
          bottom: 0;
          width: var(--depth-width, 0%);
          pointer-events: none;
        }
        .orderbook-row--ask::before {
          background-color: rgba(239, 68, 68, 0.15);
        }
        .orderbook-row--bid::before {
          background-color: rgba(34, 197, 94, 0.15);
        }
        .orderbook-price,
        .orderbook-qty,
        .orderbook-total {
          position: relative;
          z-index: 1;
        }
        .orderbook-price {
          font-weight: 500;
        }
        .orderbook-price--ask {
          color: rgb(248, 113, 113);
        }
        .orderbook-price--bid {
          color: rgb(74, 222, 128);
        }
        .orderbook-qty {
          color: rgba(255, 255, 255, 0.7);
        }
        .orderbook-total {
          color: rgba(255, 255, 255, 0.5);
        }
      `}</style>

      <div className="flex justify-between text-[10px] text-white/40 px-3 py-1 border-b border-white/10">
        <span>Price (EUR)</span>
        <span>Quantity (KCN)</span>
        <span>Total</span>
      </div>

      <div
        ref={containerRef}
        className="flex-1 overflow-y-auto"
        onScroll={handleScroll}
      >
        {allRows.length === 0 ? (
          <div className="flex items-center justify-center h-full text-white/30 text-xs">
            No orders in book
          </div>
        ) : (
          <div style={{ height: totalHeight, position: 'relative' }}>
            <div style={{ transform: `translateY(${paddingTop}px)` }}>
              {visibleRows.map((row) => {
                if (row.type === 'spread' && row.spreadInfo) {
                  return (
                    <SpreadRow
                      key="spread"
                      midPriceStr={row.spreadInfo.midPriceStr}
                      spreadStr={row.spreadInfo.spreadStr}
                      spreadPercentStr={row.spreadInfo.spreadPercentStr}
                    />
                  );
                }
                if (row.type === 'ask' && row.level) {
                  return (
                    <AskRow
                      key={`ask-${row.index}`}
                      ask={row.level}
                      widthPercent={row.widthPercent || 0}
                    />
                  );
                }
                if (row.type === 'bid' && row.level) {
                  return (
                    <BidRow
                      key={`bid-${row.index}`}
                      bid={row.level}
                      widthPercent={row.widthPercent || 0}
                    />
                  );
                }
                return null;
              })}
            </div>
          </div>
        )}
      </div>
    </div>
  );
});

export default OrderBook;
