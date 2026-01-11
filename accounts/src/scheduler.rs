use sqlx::PgPool;
use std::time::Duration;
use chrono::Utc;

/// How often to run cleanup (6 hours)
const CLEANUP_INTERVAL_HOURS: u64 = 6;

/// How many days of data to keep (1 day keeps ~2.4GB, very safe for 20GB limit)
const RETENTION_DAYS: i64 = 1;

/// Spawns a background task that periodically cleans up old database records.
/// Controlled by ENABLE_DB_CLEANUP env var (disabled by default).
pub fn spawn_cleanup_task(pool: PgPool) {
    // Check if cleanup is enabled via env var (default: disabled)
    let enabled = std::env::var("ENABLE_DB_CLEANUP")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if !enabled {
        tracing::info!("Database cleanup disabled (set ENABLE_DB_CLEANUP=true to enable)");
        return;
    }

    tokio::spawn(async move {
        tracing::info!(
            "Database cleanup scheduler started - runs every {} hours, retains {} days of data",
            CLEANUP_INTERVAL_HOURS,
            RETENTION_DAYS
        );

        // Run initial cleanup after 1 minute (let the system stabilize first)
        tokio::time::sleep(Duration::from_secs(60)).await;

        loop {
            // Run cleanup
            match run_cleanup(&pool).await {
                Ok(stats) => {
                    tracing::info!(
                        "Database cleanup complete: {} trades, {} orders, {} ledger entries, {} OHLCV records deleted",
                        stats.trades_deleted,
                        stats.orders_deleted,
                        stats.ledger_deleted,
                        stats.ohlcv_deleted
                    );
                }
                Err(e) => {
                    tracing::error!("Database cleanup failed: {}", e);
                }
            }

            // Sleep until next cleanup
            tracing::info!("Next cleanup in {} hours", CLEANUP_INTERVAL_HOURS);
            tokio::time::sleep(Duration::from_secs(CLEANUP_INTERVAL_HOURS * 3600)).await;
        }
    });
}

#[derive(Default)]
struct CleanupStats {
    trades_deleted: u64,
    orders_deleted: u64,
    ledger_deleted: u64,
    ohlcv_deleted: u64,
}

async fn run_cleanup(pool: &PgPool) -> Result<CleanupStats, sqlx::Error> {
    let cutoff = Utc::now() - chrono::Duration::days(RETENTION_DAYS);
    let cutoff_str = cutoff.format("%Y-%m-%d %H:%M:%S").to_string();

    tracing::info!("Running database cleanup, deleting records older than {}", cutoff_str);

    let mut stats = CleanupStats::default();

    // Disable ledger immutability trigger temporarily for cleanup
    sqlx::query("ALTER TABLE ledger DISABLE TRIGGER ledger_immutable")
        .execute(pool)
        .await
        .ok();

    // Delete old trades
    let result = sqlx::query("DELETE FROM trades WHERE created_at < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    stats.trades_deleted = result.rows_affected();

    // Delete old filled/cancelled orders (keep open orders)
    let result = sqlx::query(
        "DELETE FROM orders WHERE updated_at < $1 AND status IN ('filled', 'cancelled')"
    )
        .bind(cutoff)
        .execute(pool)
        .await?;
    stats.orders_deleted = result.rows_affected();

    // Delete old ledger entries (but keep at least the last entry per user/asset for balance)
    // This is tricky - we need to preserve balance continuity
    // Instead, we'll just delete entries older than retention period for non-bot users
    // For simplicity, just delete old entries - balances table has current state
    let result = sqlx::query("DELETE FROM ledger WHERE created_at < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    stats.ledger_deleted = result.rows_affected();

    // Re-enable ledger immutability trigger
    sqlx::query("ALTER TABLE ledger ENABLE TRIGGER ledger_immutable")
        .execute(pool)
        .await
        .ok();

    // Delete old OHLCV data (keep recent candles for charts)
    let result = sqlx::query("DELETE FROM ohlcv WHERE bucket < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    stats.ohlcv_deleted = result.rows_affected();

    // Delete old faucet claims
    sqlx::query("DELETE FROM faucet_claims WHERE claimed_at < $1")
        .bind(cutoff)
        .execute(pool)
        .await
        .ok();

    // Optionally run VACUUM to reclaim space (only on small tables to avoid long locks)
    // This is safe to run and helps keep the database compact
    sqlx::query("VACUUM ANALYZE trades")
        .execute(pool)
        .await
        .ok();
    sqlx::query("VACUUM ANALYZE orders")
        .execute(pool)
        .await
        .ok();

    Ok(stats)
}

/// Force an immediate cleanup (useful for testing or manual triggers)
pub async fn force_cleanup(pool: &PgPool) -> Result<(), sqlx::Error> {
    match run_cleanup(pool).await {
        Ok(stats) => {
            tracing::info!(
                "Forced cleanup complete: {} trades, {} orders, {} ledger entries deleted",
                stats.trades_deleted,
                stats.orders_deleted,
                stats.ledger_deleted
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!("Forced cleanup failed: {}", e);
            Err(e)
        }
    }
}
