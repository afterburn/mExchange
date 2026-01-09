import { memo } from 'react';
import type { Trade } from '../types';

interface RecentTradesProps {
  trades: Trade[];
}

// Memoized trade row using pre-formatted strings (no Date object creation)
const TradeRow = memo(function TradeRow({ trade }: { trade: Trade }) {
  return (
    <div className="flex justify-between items-center px-3 py-0.5 hover:bg-white/5 text-xs transition-colors">
      <span className="text-white/90 font-medium w-16">
        {trade.priceStr}
      </span>
      <span className="text-white/70 w-14 text-right">{trade.quantityStr}</span>
      <span className="text-white/50 w-14 text-right">{trade.timeStr}</span>
    </div>
  );
});

const RecentTrades = memo(function RecentTrades({ trades }: RecentTradesProps) {
  // Debug: log trades when they change
  console.log('[RecentTrades] Rendering with trades:', trades.length, trades.slice(0, 2));

  return (
    <div className="flex flex-col h-full">
      <div className="flex justify-between text-[10px] text-white/40 px-3 py-1.5 border-b border-white/10 font-medium">
        <span className="w-16">Price ({trades.length})</span>
        <span className="w-14 text-right">Qty</span>
        <span className="w-14 text-right">Time</span>
      </div>

      <div className="flex-1 overflow-y-auto">
        {trades.length === 0 ? (
          <div className="flex items-center justify-center h-full text-white/30 text-xs">
            No recent trades
          </div>
        ) : (
          <div className="flex flex-col py-0.5">
            {trades.map((trade, index) => (
              <TradeRow key={`${trade.id}-${index}`} trade={trade} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
});

export default RecentTrades;
