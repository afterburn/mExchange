use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct OHLCV {
    pub symbol: String,
    pub interval: String,
    pub open_time: DateTime<Utc>,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub trade_count: i32,
}

impl OHLCV {
    pub async fn get_latest(
        pool: &PgPool,
        symbol: &str,
        interval: &str,
        limit: i64,
    ) -> Result<Vec<Self>, sqlx::Error> {
        sqlx::query_as::<_, Self>(
            "SELECT * FROM ohlcv
             WHERE symbol = $1 AND interval = $2
             ORDER BY open_time DESC
             LIMIT $3"
        )
        .bind(symbol)
        .bind(interval)
        .bind(limit)
        .fetch_all(pool)
        .await
    }

    pub async fn upsert(
        pool: &PgPool,
        symbol: &str,
        interval: &str,
        open_time: DateTime<Utc>,
        open: Decimal,
        high: Decimal,
        low: Decimal,
        close: Decimal,
        volume: Decimal,
        trade_count: i32,
    ) -> Result<Self, sqlx::Error> {
        sqlx::query_as::<_, Self>(
            "INSERT INTO ohlcv (symbol, interval, open_time, open, high, low, close, volume, trade_count)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (symbol, interval, open_time) DO UPDATE SET
                high = GREATEST(ohlcv.high, $5),
                low = LEAST(ohlcv.low, $6),
                close = $7,
                volume = ohlcv.volume + $8,
                trade_count = ohlcv.trade_count + $9
             RETURNING *"
        )
        .bind(symbol)
        .bind(interval)
        .bind(open_time)
        .bind(open)
        .bind(high)
        .bind(low)
        .bind(close)
        .bind(volume)
        .bind(trade_count)
        .fetch_one(pool)
        .await
    }

    /// Update OHLCV bars for all intervals when a trade occurs
    pub async fn update_from_trade(
        pool: &PgPool,
        symbol: &str,
        price: Decimal,
        quantity: Decimal,
        timestamp_ms: i64,
    ) -> Result<(), sqlx::Error> {
        use chrono::TimeZone;

        let timestamp = Utc.timestamp_millis_opt(timestamp_ms).unwrap();

        // Update 1m, 5m, 15m, 1h candles
        let intervals = [
            ("1m", 60),
            ("5m", 300),
            ("15m", 900),
            ("1h", 3600),
        ];

        for (interval, seconds) in intervals {
            let open_time = Utc.timestamp_opt(
                (timestamp.timestamp() / seconds) * seconds,
                0
            ).unwrap();

            Self::upsert(
                pool,
                symbol,
                interval,
                open_time,
                price,    // open (only used on insert)
                price,    // high
                price,    // low
                price,    // close
                quantity, // volume
                1,        // trade_count
            ).await?;
        }

        Ok(())
    }

    /// Get 24h statistics for a symbol
    pub async fn get_24h_stats(
        pool: &PgPool,
        symbol: &str,
    ) -> Result<Option<Stats24h>, sqlx::Error> {
        // Get stats from 1-minute candles over the last 24 hours
        sqlx::query_as::<_, Stats24h>(
            "SELECT
                MAX(high) as high_24h,
                MIN(low) as low_24h,
                SUM(volume) as volume_24h,
                (SELECT close FROM ohlcv WHERE symbol = $1 AND interval = '1m' ORDER BY open_time DESC LIMIT 1) as last_price,
                (SELECT open FROM ohlcv WHERE symbol = $1 AND interval = '1m' AND open_time >= NOW() - INTERVAL '24 hours' ORDER BY open_time ASC LIMIT 1) as open_24h
             FROM ohlcv
             WHERE symbol = $1
               AND interval = '1m'
               AND open_time >= NOW() - INTERVAL '24 hours'"
        )
        .bind(symbol)
        .fetch_optional(pool)
        .await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Stats24h {
    pub high_24h: Option<Decimal>,
    pub low_24h: Option<Decimal>,
    pub volume_24h: Option<Decimal>,
    pub last_price: Option<Decimal>,
    pub open_24h: Option<Decimal>,
}
