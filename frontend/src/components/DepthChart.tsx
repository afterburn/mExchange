import { useMemo, memo, useState, useCallback } from 'react';
import type { OrderBookLevel } from '../types';

function formatVolume(vol: number): string {
  if (vol >= 1_000_000) return `${(vol / 1_000_000).toFixed(1)}M`;
  if (vol >= 1_000) return `${(vol / 1_000).toFixed(1)}K`;
  if (vol >= 1) return vol.toFixed(0);
  return vol.toFixed(2);
}

function formatPrice(price: number): string {
  return price.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

interface DepthChartProps {
  bids: OrderBookLevel[];
  asks: OrderBookLevel[];
}

interface TooltipData {
  x: number;
  y: number;
  price: number;
  cumulative: number;
  side: 'bid' | 'ask';
}

const DepthChart = memo(function DepthChart({ bids, asks }: DepthChartProps) {
  const [tooltip, setTooltip] = useState<TooltipData | null>(null);

  const chartData = useMemo(() => {
    if (bids.length === 0 && asks.length === 0) return null;

    // Sort bids high to low (best bid first), asks low to high (best ask first)
    const sortedBids = [...bids].sort((a, b) => b.price - a.price);
    const sortedAsks = [...asks].sort((a, b) => a.price - b.price);

    // Bids: cumulative from best bid going down to lower prices
    // Standard depth chart: cumulative INCREASES as you move AWAY from midpoint
    const bidPoints = sortedBids.reduce<Array<{ price: number; cumulative: number }>>((acc, bid) => {
      const prevCumulative = acc.length > 0 ? acc[acc.length - 1].cumulative : 0;
      acc.push({ price: bid.price, cumulative: prevCumulative + bid.quantity });
      return acc;
    }, []);

    // Asks: cumulative from best ask going up to higher prices
    const askPoints = sortedAsks.reduce<Array<{ price: number; cumulative: number }>>((acc, ask) => {
      const prevCumulative = acc.length > 0 ? acc[acc.length - 1].cumulative : 0;
      acc.push({ price: ask.price, cumulative: prevCumulative + ask.quantity });
      return acc;
    }, []);

    const bestBid = sortedBids[0]?.price ?? 0;
    const bestAsk = sortedAsks[0]?.price ?? 0;
    const midPrice = bestBid && bestAsk ? (bestBid + bestAsk) / 2 : bestBid || bestAsk;
    const spread = bestBid && bestAsk ? bestAsk - bestBid : 0;

    // Calculate price range - use symmetric range around mid price
    const lowestBid = bidPoints[bidPoints.length - 1]?.price ?? midPrice;
    const highestAsk = askPoints[askPoints.length - 1]?.price ?? midPrice;

    const bidRange = midPrice - lowestBid;
    const askRange = highestAsk - midPrice;
    const maxRange = Math.max(bidRange, askRange) || midPrice * 0.1;

    // Add 10% padding and ensure symmetric range
    const chartMinPrice = midPrice - maxRange * 1.1;
    const chartMaxPrice = midPrice + maxRange * 1.1;
    const chartPriceRange = chartMaxPrice - chartMinPrice;

    // Get max cumulative volume (use same scale for both sides for fair comparison)
    const maxVolume = Math.max(
      bidPoints[bidPoints.length - 1]?.cumulative || 0,
      askPoints[askPoints.length - 1]?.cumulative || 0
    ) || 1;

    return {
      bidPoints,
      askPoints,
      chartMinPrice,
      chartMaxPrice,
      chartPriceRange,
      maxVolume,
      midPrice,
      bestBid,
      bestAsk,
      spread,
    };
  }, [bids, asks]);

  // Handle mouse move for tooltip
  const handleMouseMove = useCallback((e: React.MouseEvent<SVGSVGElement>) => {
    if (!chartData) return;

    const svg = e.currentTarget;
    const rect = svg.getBoundingClientRect();
    const x = ((e.clientX - rect.left) / rect.width) * 100;

    // Convert x to price
    const { chartMinPrice, chartPriceRange, bidPoints, askPoints, midPrice } = chartData;
    const padding = { left: 5, right: 5 };
    const chartWidth = 100 - padding.left - padding.right;

    const price = chartMinPrice + ((x - padding.left) / chartWidth) * chartPriceRange;

    // Find the cumulative at this price point
    let cumulative = 0;
    let side: 'bid' | 'ask' = 'bid';

    if (price < midPrice) {
      // On bid side - find cumulative at or below this price
      side = 'bid';
      for (let i = 0; i < bidPoints.length; i++) {
        if (bidPoints[i].price >= price) {
          cumulative = bidPoints[i].cumulative;
        }
      }
    } else {
      // On ask side - find cumulative at or above this price
      side = 'ask';
      for (let i = 0; i < askPoints.length; i++) {
        if (askPoints[i].price <= price) {
          cumulative = askPoints[i].cumulative;
        }
      }
    }

    setTooltip({ x: e.clientX, y: e.clientY, price, cumulative, side });
  }, [chartData]);

  const handleMouseLeave = useCallback(() => {
    setTooltip(null);
  }, []);

  if (!chartData) {
    return (
      <div className="flex items-center justify-center h-full bg-black text-white/40">
        <div>Waiting for order book data...</div>
      </div>
    );
  }

  const { bidPoints, askPoints, chartMinPrice, chartMaxPrice, chartPriceRange, maxVolume, midPrice, bestBid, bestAsk } = chartData;

  // SVG dimensions
  const width = 100;
  const height = 100;
  const padding = { top: 5, right: 5, bottom: 5, left: 5 };
  const chartWidth = width - padding.left - padding.right;
  const chartHeight = height - padding.top - padding.bottom;

  // Scale functions
  const scaleX = (price: number) => {
    return padding.left + ((price - chartMinPrice) / chartPriceRange) * chartWidth;
  };

  const scaleY = (volume: number) => {
    return padding.top + chartHeight - (volume / maxVolume) * chartHeight;
  };

  // Build stepped path for bids (green)
  // Depth charts show: horizontal line at cumulative level, then step DOWN to next level
  // For bids: start from LEFT edge at max cumulative, step towards midpoint decreasing cumulative
  const buildBidPath = () => {
    if (bidPoints.length === 0) return '';

    const points: string[] = [];

    // Reverse bid points so we draw from lowest price (highest cumulative) to best bid (lowest cumulative)
    const reversedBids = [...bidPoints].reverse();

    // Start at midpoint at baseline (curves meet in the middle)
    points.push(`M ${scaleX(midPrice)} ${scaleY(0)}`);

    // Go to best bid at baseline, then up to its cumulative
    const bestBidPoint = reversedBids[reversedBids.length - 1];
    points.push(`L ${scaleX(bestBidPoint.price)} ${scaleY(0)}`);

    // Draw stepped line from best bid to lowest price (right to left)
    // At each price level: vertical step up, then horizontal line to next price
    for (let i = reversedBids.length - 1; i >= 0; i--) {
      const point = reversedBids[i];

      // Vertical step UP to this level's cumulative
      points.push(`L ${scaleX(point.price)} ${scaleY(point.cumulative)}`);

      // Horizontal line TO next price (lower price) at current cumulative
      if (i > 0) {
        const nextPrice = reversedBids[i - 1].price;
        points.push(`L ${scaleX(nextPrice)} ${scaleY(point.cumulative)}`);
      }
    }

    // Extend to left edge at max cumulative
    const maxCumulative = reversedBids[0].cumulative;
    points.push(`L ${scaleX(chartMinPrice)} ${scaleY(maxCumulative)}`);

    // Go down to baseline at left edge
    points.push(`L ${scaleX(chartMinPrice)} ${scaleY(0)}`);

    // Close path back to start (midpoint)
    points.push('Z');

    return points.join(' ');
  };

  // Build stepped path for asks (red)
  // For asks: start from midpoint, step towards RIGHT increasing cumulative
  const buildAskPath = () => {
    if (askPoints.length === 0) return '';

    const points: string[] = [];

    // Start at midpoint at baseline (curves meet in the middle)
    points.push(`M ${scaleX(midPrice)} ${scaleY(0)}`);

    // Go to best ask at baseline
    points.push(`L ${scaleX(askPoints[0].price)} ${scaleY(0)}`);

    // Draw stepped line from best ask to highest price (left to right)
    for (let i = 0; i < askPoints.length; i++) {
      const point = askPoints[i];

      // Vertical step UP to this level's cumulative
      points.push(`L ${scaleX(point.price)} ${scaleY(point.cumulative)}`);

      // Horizontal line TO next price at current cumulative
      if (i < askPoints.length - 1) {
        const nextPrice = askPoints[i + 1].price;
        points.push(`L ${scaleX(nextPrice)} ${scaleY(point.cumulative)}`);
      }
    }

    // Extend the last level to the right edge
    const lastPoint = askPoints[askPoints.length - 1];
    points.push(`L ${scaleX(chartMaxPrice)} ${scaleY(lastPoint.cumulative)}`);

    // Go down to baseline at right edge
    points.push(`L ${scaleX(chartMaxPrice)} ${scaleY(0)}`);

    // Close path back to start (midpoint)
    points.push('Z');

    return points.join(' ');
  };

  // Generate price axis labels
  const priceLabels = [];
  const numPriceLabels = 5;
  for (let i = 0; i <= numPriceLabels; i++) {
    const price = chartMinPrice + (chartPriceRange * i) / numPriceLabels;
    priceLabels.push({ price, x: scaleX(price) });
  }

  // Generate volume axis labels
  const volumeLabels = [];
  const numVolumeLabels = 4;
  for (let i = 0; i <= numVolumeLabels; i++) {
    const volume = (maxVolume * i) / numVolumeLabels;
    volumeLabels.push({ volume, y: scaleY(volume) });
  }

  return (
    <div className="h-full bg-black flex flex-col p-2">
      {/* Spread indicator */}
      <div className="flex justify-center gap-4 text-[10px] mb-1">
        <span className="text-green-500">Bid: €{formatPrice(bestBid)}</span>
        <span className="text-white/40">|</span>
        <span className="text-red-500">Ask: €{formatPrice(bestAsk)}</span>
        <span className="text-white/40">|</span>
        <span className="text-white/40">Depth: {formatVolume(maxVolume)}</span>
      </div>

      <div className="flex-1 relative min-h-0">
        <svg
          viewBox={`0 0 ${width} ${height}`}
          preserveAspectRatio="none"
          className="w-full h-full"
          onMouseMove={handleMouseMove}
          onMouseLeave={handleMouseLeave}
        >
          {/* Grid lines */}
          <g className="text-white/10">
            {volumeLabels.map((label, i) => (
              <line
                key={`h-${i}`}
                x1={padding.left}
                y1={label.y}
                x2={width - padding.right}
                y2={label.y}
                stroke="currentColor"
                strokeWidth="0.2"
              />
            ))}
          </g>

          {/* Mid price line */}
          <line
            x1={scaleX(midPrice)}
            y1={padding.top}
            x2={scaleX(midPrice)}
            y2={height - padding.bottom}
            stroke="rgba(255,255,255,0.4)"
            strokeWidth="0.5"
            strokeDasharray="2,2"
          />

          {/* Bid area (green) */}
          <path
            d={buildBidPath()}
            fill="rgba(34, 197, 94, 0.25)"
            stroke="#22c55e"
            strokeWidth="1.5"
            vectorEffect="non-scaling-stroke"
          />

          {/* Ask area (red) */}
          <path
            d={buildAskPath()}
            fill="rgba(239, 68, 68, 0.25)"
            stroke="#ef4444"
            strokeWidth="1.5"
            vectorEffect="non-scaling-stroke"
          />
        </svg>

        {/* Price labels (bottom) */}
        <div className="absolute bottom-0 left-0 right-0 flex justify-between text-[9px] text-white/40 px-1">
          {priceLabels.map((label, i) => (
            <span key={i}>€{formatPrice(label.price)}</span>
          ))}
        </div>

        {/* Volume labels (left) */}
        <div className="absolute top-0 left-1 bottom-4 flex flex-col justify-between text-[9px] text-white/40">
          {volumeLabels.slice().reverse().map((label, i) => (
            <span key={i}>{formatVolume(label.volume)}</span>
          ))}
        </div>

        {/* Tooltip */}
        {tooltip && (
          <div
            className="fixed z-50 bg-gray-900 border border-white/20 rounded px-2 py-1 text-[10px] pointer-events-none"
            style={{
              left: tooltip.x + 10,
              top: tooltip.y - 30,
            }}
          >
            <div className={tooltip.side === 'bid' ? 'text-green-500' : 'text-red-500'}>
              €{formatPrice(tooltip.price)}
            </div>
            <div className="text-white/60">
              Vol: {formatVolume(tooltip.cumulative)}
            </div>
          </div>
        )}
      </div>
    </div>
  );
});

export default DepthChart;
