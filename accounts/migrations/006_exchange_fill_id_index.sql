-- Add unique index on exchange_fill_id for idempotency
-- This index is used for:
-- 1. Fast idempotency lookups (Trade::exists_by_fill_id, Trade::get_by_fill_id)
-- 2. ON CONFLICT handling in Trade::settle
--
-- Note: Migration 004 created a partial index (idx_trades_exchange_fill)
-- which doesn't work with ON CONFLICT. We need a non-partial unique index.
-- Drop the old partial index first to avoid confusion.
DROP INDEX IF EXISTS idx_trades_exchange_fill;

-- Create non-partial unique index that works with ON CONFLICT (exchange_fill_id)
CREATE UNIQUE INDEX IF NOT EXISTS idx_trades_exchange_fill_id ON trades(exchange_fill_id);
