import { useEffect, useRef, useState, memo, useCallback } from 'react';
import type { Trade } from '../types';

const API_URL = (import.meta.env.VITE_API_URL || 'http://localhost:3000').trim();

interface Candle {
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
}

interface OHLCVBar {
  open_time: string;
  open: string;
  high: string;
  low: string;
  close: string;
}

interface PriceChartProps {
  trades: Trade[];
}

type Timeframe = '1m' | '5m' | '15m' | '1h';

const TIMEFRAME_SECONDS: Record<Timeframe, number> = {
  '1m': 60,
  '5m': 300,
  '15m': 900,
  '1h': 3600,
};

const PriceChart = memo(function PriceChart({ trades }: PriceChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<unknown>(null);
  const seriesRef = useRef<unknown>(null);
  const candlesRef = useRef<Map<number, Candle>>(new Map());
  const lastProcessedTradeRef = useRef<number>(0);
  const chartTypeRef = useRef<'candles' | 'line'>('candles');

  const [timeframe, setTimeframe] = useState<Timeframe>('1m');
  const [chartType, setChartType] = useState<'candles' | 'line'>('candles');
  const [isReady, setIsReady] = useState(false);
  const [hasCandles, setHasCandles] = useState(false);

  // Keep ref in sync with state
  useEffect(() => {
    chartTypeRef.current = chartType;
  }, [chartType]);

  // Update chart with current candles - no dependencies on chartType
  const updateChart = useCallback(() => {
    const chart = chartRef.current as ReturnType<typeof window.LightweightCharts.createChart> | null;
    const series = seriesRef.current as ReturnType<ReturnType<typeof window.LightweightCharts.createChart>['addSeries']> | null;
    if (!chart || !series) return;

    const candles = Array.from(candlesRef.current.values()).sort((a, b) => a.time - b.time);

    if (chartTypeRef.current === 'candles') {
      series.setData(candles);
    } else {
      series.setData(candles.map(c => ({ time: c.time, value: c.close })));
    }

    setHasCandles(candles.length > 0);

    if (candles.length > 0) {
      chart.timeScale().scrollToRealTime();
    }
  }, []);

  // Create series based on current chart type
  const createSeries = useCallback((type: 'candles' | 'line') => {
    const chart = chartRef.current as ReturnType<typeof window.LightweightCharts.createChart> | null;
    if (!chart) return;

    const LWC = window.LightweightCharts;

    if (seriesRef.current) {
      chart.removeSeries(seriesRef.current as Parameters<typeof chart.removeSeries>[0]);
    }

    if (type === 'candles') {
      seriesRef.current = chart.addSeries(LWC.CandlestickSeries, {
        upColor: '#22c55e',
        downColor: '#ef4444',
        borderUpColor: '#22c55e',
        borderDownColor: '#ef4444',
        wickUpColor: '#22c55e',
        wickDownColor: '#ef4444',
      });
    } else {
      seriesRef.current = chart.addSeries(LWC.LineSeries, {
        color: '#3b82f6',
        lineWidth: 2,
      });
    }
  }, []);

  // Wait for LightweightCharts library
  useEffect(() => {
    function check() {
      if (window.LightweightCharts) {
        setIsReady(true);
      } else {
        setTimeout(check, 50);
      }
    }
    check();
  }, []);

  // Create chart
  useEffect(() => {
    if (!isReady || !containerRef.current) return;

    const container = containerRef.current;
    const LWC = window.LightweightCharts;

    const chart = LWC.createChart(container, {
      layout: {
        background: { color: '#000000' },
        textColor: '#888888',
      },
      grid: {
        vertLines: { color: 'rgba(255, 255, 255, 0.05)' },
        horzLines: { color: 'rgba(255, 255, 255, 0.05)' },
      },
      width: container.clientWidth,
      height: container.clientHeight,
      timeScale: {
        timeVisible: true,
        secondsVisible: timeframe === '1m',
        borderColor: 'rgba(255, 255, 255, 0.1)',
        barSpacing: 8,
        minBarSpacing: 4,
        rightOffset: 5,
      },
      rightPriceScale: {
        borderColor: 'rgba(255, 255, 255, 0.1)',
      },
      crosshair: {
        vertLine: { color: 'rgba(255, 255, 255, 0.2)' },
        horzLine: { color: 'rgba(255, 255, 255, 0.2)' },
      },
    });

    chartRef.current = chart;

    // Create initial series with current chart type
    createSeries(chartTypeRef.current);

    const resizeObserver = new ResizeObserver(() => {
      chart.applyOptions({
        width: container.clientWidth,
        height: container.clientHeight,
      });
    });
    resizeObserver.observe(container);

    return () => {
      resizeObserver.disconnect();
      chart.remove();
      chartRef.current = null;
      seriesRef.current = null;
    };
  }, [isReady, createSeries, timeframe]);

  // Recreate series when chart type changes
  useEffect(() => {
    if (!chartRef.current || !isReady) return;
    createSeries(chartType);
    updateChart();
  }, [chartType, isReady, createSeries, updateChart]);

  // Fetch initial OHLCV data when timeframe changes
  useEffect(() => {
    let cancelled = false;

    // Clear candles when timeframe changes and update chart to show empty state
    candlesRef.current.clear();
    lastProcessedTradeRef.current = 0;
    // Update chart to reflect cleared state (will set hasCandles to false)
    if (chartRef.current && seriesRef.current) {
      updateChart();
    }

    async function fetchData() {
      try {
        const res = await fetch(
          `${API_URL}/api/ohlcv?symbol=${encodeURIComponent('KCN/EUR')}&interval=${timeframe}&limit=500`
        );
        if (!res.ok || cancelled) return;

        const json = await res.json();
        if (cancelled) return;

        const bars: OHLCVBar[] = json.data || [];
        for (const bar of bars) {
          const time = Math.floor(new Date(bar.open_time).getTime() / 1000);
          candlesRef.current.set(time, {
            time,
            open: parseFloat(bar.open),
            high: parseFloat(bar.high),
            low: parseFloat(bar.low),
            close: parseFloat(bar.close),
          });
        }

        // Only update chart if chart is ready
        if (chartRef.current && seriesRef.current) {
          updateChart();
        }
      } catch (err) {
        console.error('Failed to fetch OHLCV:', err);
      }
    }

    fetchData();
    return () => { cancelled = true; };
  }, [timeframe, updateChart]);

  // Process new trades into candles
  useEffect(() => {
    if (trades.length === 0) return;

    const tfSeconds = TIMEFRAME_SECONDS[timeframe];
    let updated = false;

    for (const trade of trades) {
      // Skip already processed trades
      if (trade.timestamp <= lastProcessedTradeRef.current) continue;

      const tradeTimeSec = Math.floor(trade.timestamp / 1000);
      const candleTime = Math.floor(tradeTimeSec / tfSeconds) * tfSeconds;

      const existing = candlesRef.current.get(candleTime);
      if (existing) {
        existing.high = Math.max(existing.high, trade.price);
        existing.low = Math.min(existing.low, trade.price);
        existing.close = trade.price;
      } else {
        candlesRef.current.set(candleTime, {
          time: candleTime,
          open: trade.price,
          high: trade.price,
          low: trade.price,
          close: trade.price,
        });
      }
      updated = true;
    }

    if (trades.length > 0) {
      lastProcessedTradeRef.current = Math.max(
        lastProcessedTradeRef.current,
        ...trades.map(t => t.timestamp)
      );
    }

    if (updated && chartRef.current && seriesRef.current) {
      updateChart();
    }
  }, [trades, timeframe, updateChart]);

  if (!isReady) {
    return (
      <div className="h-full bg-black flex items-center justify-center text-white/40">
        Loading chart...
      </div>
    );
  }

  return (
    <div className="h-full bg-black flex flex-col relative">
      {/* Controls */}
      <div className="flex justify-between items-center px-4 py-2 border-b border-white/10">
        <div className="flex gap-1 text-xs">
          {(['1m', '5m', '15m', '1h'] as Timeframe[]).map((tf) => (
            <button
              key={tf}
              onClick={() => setTimeframe(tf)}
              className={`px-2.5 py-1 rounded transition-colors ${
                timeframe === tf
                  ? 'text-white bg-white/15'
                  : 'text-white/50 hover:text-white/80 hover:bg-white/5'
              }`}
            >
              {tf}
            </button>
          ))}
        </div>
        <div className="flex gap-1 text-xs">
          <button
            onClick={() => setChartType('line')}
            className={`px-2.5 py-1 rounded transition-colors ${
              chartType === 'line'
                ? 'text-white bg-white/15'
                : 'text-white/50 hover:text-white/80 hover:bg-white/5'
            }`}
          >
            Line
          </button>
          <button
            onClick={() => setChartType('candles')}
            className={`px-2.5 py-1 rounded transition-colors ${
              chartType === 'candles'
                ? 'text-white bg-white/15'
                : 'text-white/50 hover:text-white/80 hover:bg-white/5'
            }`}
          >
            Candles
          </button>
        </div>
      </div>

      {/* Chart container */}
      <div ref={containerRef} className="flex-1 min-h-0" />

      {/* Empty state overlay */}
      {!hasCandles && (
        <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
          <span className="text-white/30 text-sm">Waiting for trades...</span>
        </div>
      )}
    </div>
  );
});

export default PriceChart;

declare global {
  interface Window {
    LightweightCharts: {
      createChart: (container: HTMLElement, options: Record<string, unknown>) => {
        addSeries: (type: unknown, options: Record<string, unknown>) => {
          setData: (data: unknown[]) => void;
          update: (data: unknown) => void;
        };
        removeSeries: (series: unknown) => void;
        remove: () => void;
        applyOptions: (options: Record<string, unknown>) => void;
        timeScale: () => { fitContent: () => void; scrollToRealTime: () => void };
      };
      CandlestickSeries: unknown;
      LineSeries: unknown;
    };
  }
}
