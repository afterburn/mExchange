use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::Arc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};

mod ohlcv;

use ohlcv::OhlcvAggregator;

fn main() {
    use std::io::Write;

    std::panic::set_hook(Box::new(|panic_info| {
        let _ = std::io::stderr().write_all(format!("PANIC: {:?}\n", panic_info).as_bytes());
        let _ = std::io::stderr().flush();
        std::process::exit(1);
    }));

    let _ = std::io::stderr().write_all(b"Starting market data service...\n");
    let _ = std::io::stderr().flush();

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let _ =
                std::io::stderr().write_all(format!("Failed to create runtime: {}\n", e).as_bytes());
            let _ = std::io::stderr().flush();
            std::process::exit(1);
        }
    };

    match rt.block_on(tokio_main()) {
        Ok(_) => {}
        Err(e) => {
            let _ = std::io::stderr().write_all(format!("Error: {}\n", e).as_bytes());
            let _ = std::io::stderr().flush();
            std::process::exit(1);
        }
    }
}

#[derive(Clone)]
struct AppState {
    pool: PgPool,
}

async fn tokio_main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "market_data=info".into()),
        )
        .init();

    info!("Starting market data service...");

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/accounts".to_string());
    let gateway_ws_url = std::env::var("GATEWAY_WS_URL")
        .unwrap_or_else(|_| "ws://localhost:3000/ws".to_string());
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3002".to_string());

    info!("Connecting to database...");
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await?;
    info!("Connected to database");

    let aggregator = Arc::new(OhlcvAggregator::new(pool.clone()));

    // Start WebSocket consumer for trades
    start_websocket_consumer(&gateway_ws_url, aggregator.clone()).await;

    // Set up HTTP routes
    let state = AppState { pool };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/ohlcv", get(get_ohlcv))
        .layer(cors)
        .with_state(state);

    info!("Market data service listening on {}", bind_addr);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

// === OHLCV API Types ===

#[derive(Debug, Deserialize)]
struct OHLCVQuery {
    symbol: String,
    interval: String,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    500
}

#[derive(Debug, Serialize)]
struct OHLCVBar {
    open_time: String,
    open: String,
    high: String,
    low: String,
    close: String,
    volume: String,
    trade_count: i32,
}

#[derive(Debug, Serialize)]
struct OHLCVResponse {
    data: Vec<OHLCVBar>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug, sqlx::FromRow)]
struct OhlcvRow {
    open_time: chrono::DateTime<chrono::Utc>,
    open: rust_decimal::Decimal,
    high: rust_decimal::Decimal,
    low: rust_decimal::Decimal,
    close: rust_decimal::Decimal,
    volume: rust_decimal::Decimal,
    trade_count: i32,
}

async fn get_ohlcv(
    State(state): State<AppState>,
    Query(query): Query<OHLCVQuery>,
) -> Result<Json<OHLCVResponse>, (StatusCode, Json<ErrorResponse>)> {
    let limit = query.limit.min(1000).max(1);

    let rows: Vec<OhlcvRow> = sqlx::query_as(
        r#"
        SELECT open_time, open, high, low, close, volume, trade_count
        FROM ohlcv
        WHERE symbol = $1 AND interval = $2
        ORDER BY open_time DESC
        LIMIT $3
        "#,
    )
    .bind(&query.symbol)
    .bind(&query.interval)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        error!("Failed to get OHLCV data: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to get data".into(),
            }),
        )
    })?;

    // Reverse to get chronological order (oldest first)
    let bars: Vec<OHLCVBar> = rows
        .into_iter()
        .rev()
        .map(|row| OHLCVBar {
            open_time: row.open_time.to_rfc3339(),
            open: row.open.to_string(),
            high: row.high.to_string(),
            low: row.low.to_string(),
            close: row.close.to_string(),
            volume: row.volume.to_string(),
            trade_count: row.trade_count,
        })
        .collect();

    Ok(Json(OHLCVResponse { data: bars }))
}

// === WebSocket Consumer ===

#[derive(Debug, Deserialize)]
struct ChannelNotification {
    #[allow(dead_code)]
    channel_name: String,
    notification: NotificationData,
}

#[derive(Debug, Deserialize)]
struct NotificationData {
    trades: Vec<TradeData>,
    #[allow(dead_code)]
    bid_changes: Vec<serde_json::Value>,
    #[allow(dead_code)]
    ask_changes: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct TradeData {
    price: f64,
    quantity: f64,
    #[allow(dead_code)]
    side: String,
    timestamp: u64,
}

async fn start_websocket_consumer(gateway_ws_url: &str, aggregator: Arc<OhlcvAggregator>) {
    let url = gateway_ws_url.to_string();

    tokio::spawn(async move {
        loop {
            info!("Connecting to gateway WebSocket at {}...", url);

            match connect_async(&url).await {
                Ok((ws_stream, _)) => {
                    info!("Connected to gateway WebSocket");
                    let (mut write, mut read) = ws_stream.split();

                    // Subscribe to the orderbook channel to receive trades
                    let subscribe_msg = serde_json::json!({
                        "type": "subscribe",
                        "channel": "book.KCN/EUR.none.10.100ms"
                    });

                    if let Err(e) = write
                        .send(Message::Text(subscribe_msg.to_string()))
                        .await
                    {
                        error!("Failed to send subscribe message: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                        continue;
                    }

                    info!("Subscribed to book.KCN/EUR.none.10.100ms channel");

                    // Process incoming messages
                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(Message::Text(text)) => {
                                // Try to parse as channel notification with trades
                                if let Ok(notification) =
                                    serde_json::from_str::<ChannelNotification>(&text)
                                {
                                    for trade in notification.notification.trades {
                                        let price = rust_decimal::Decimal::try_from(trade.price)
                                            .unwrap_or_default();
                                        let quantity =
                                            rust_decimal::Decimal::try_from(trade.quantity)
                                                .unwrap_or_default();

                                        if let Err(e) = aggregator
                                            .process_trade(
                                                "KCN/EUR",
                                                price,
                                                quantity,
                                                trade.timestamp,
                                            )
                                            .await
                                        {
                                            error!("Failed to process trade for OHLCV: {}", e);
                                        }
                                    }
                                }
                                // Ignore other message types (snapshots, etc.)
                            }
                            Ok(Message::Close(_)) => {
                                warn!("WebSocket connection closed by server");
                                break;
                            }
                            Err(e) => {
                                error!("WebSocket error: {}", e);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to connect to gateway WebSocket: {}", e);
                }
            }

            info!("Reconnecting to gateway WebSocket in 3 seconds...");
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        }
    });
}
