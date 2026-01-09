import { memo } from 'react';
import type { MarketStats as MarketStatsType } from '../types';
import MarketSelector from './MarketSelector';

interface MarketStatsProps {
  stats: MarketStatsType;
}

function formatVolume(volume: number): string {
  if (volume >= 1_000_000_000) {
    return (volume / 1_000_000_000).toFixed(2) + 'B';
  }
  if (volume >= 1_000_000) {
    return (volume / 1_000_000).toFixed(2) + 'M';
  }
  if (volume >= 1_000) {
    return (volume / 1_000).toFixed(2) + 'K';
  }
  return volume.toFixed(2);
}

const MarketStats = memo(function MarketStats({ stats }: MarketStatsProps) {
  const isPositive = stats.priceChange24h >= 0;

  return (
    <div className="border-b border-white/10 bg-black px-3 py-2">
      <div className="flex items-center justify-between gap-6">
        <div className="flex items-center gap-3">
          <MarketSelector
            currentSymbol={stats.symbol}
            currentPrice={stats.lastPrice}
            currentChange24h={stats.priceChangePercent24h}
          />
          <span className="text-sm font-semibold">
            €{stats.lastPrice.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
          </span>
          <span className={`text-xs ${isPositive ? 'text-green-400' : 'text-red-400'}`}>
            {isPositive ? '+' : ''}{stats.priceChangePercent24h.toFixed(2)}%
          </span>
        </div>
        <div className="flex items-center gap-4 text-xs">
          <div className="flex gap-1">
            <span className="text-white/40">H</span>
            <span className="text-white/70">€{stats.high24h.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</span>
          </div>
          <div className="flex gap-1">
            <span className="text-white/40">L</span>
            <span className="text-white/70">€{stats.low24h.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</span>
          </div>
          <div className="flex gap-1">
            <span className="text-white/40">Vol</span>
            <span className="text-white/70">{formatVolume(stats.volume24h)}</span>
          </div>
        </div>
      </div>
    </div>
  );
});

export default MarketStats;

