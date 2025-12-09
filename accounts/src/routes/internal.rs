use axum::{
    extract::State,
    http::StatusCode,
    routing::post,
    Json, Router,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::{Fill, Order, Trade, OHLCV};
use crate::AppState;

/// Internal API routes (called by gateway/matching engine, not end users)
pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/settle", post(settle_fill))
        .route("/cancel", post(cancel_order_internal))
}

// === Request/Response Types ===

#[derive(Debug, Deserialize)]
pub struct SettleFillRequest {
    pub symbol: String,
    /// Order ID (UUID) for the buy side
    /// If this UUID is not found in the database, the buy side is treated as an anonymous/bot order
    pub buy_order_id: Uuid,
    /// Order ID (UUID) for the sell side
    /// If this UUID is not found in the database, the sell side is treated as an anonymous/bot order
    pub sell_order_id: Uuid,
    pub price: Decimal,
    pub quantity: Decimal,
    pub timestamp: i64,
}

#[derive(Debug, Serialize)]
pub struct SettleFillResponse {
    pub trade_id: String,
    pub buyer_id: String,
    pub seller_id: String,
    pub settled: bool,
}

#[derive(Debug, Serialize)]
pub struct SettleErrorResponse {
    pub error: String,
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct CancelOrderInternalRequest {
    pub order_id: Uuid,
    pub filled_quantity: Decimal,
}

#[derive(Debug, Serialize)]
pub struct CancelOrderInternalResponse {
    pub success: bool,
    pub order_id: String,
    pub status: String,
}

// === Handlers ===

async fn settle_fill(
    State(state): State<AppState>,
    Json(req): Json<SettleFillRequest>,
) -> Result<Json<SettleFillResponse>, (StatusCode, Json<SettleErrorResponse>)> {
    // Convert to Fill struct - the settlement logic will look up each order
    // and treat "not found" as an anonymous/bot order (skip that side)
    let fill = Fill {
        buy_order_id: req.buy_order_id,
        sell_order_id: req.sell_order_id,
        price: req.price,
        quantity: req.quantity,
        timestamp: req.timestamp,
    };

    let trade = Trade::settle(&state.pool, &req.symbol, &fill)
        .await
        .map_err(|e| {
            let (code, error) = match &e {
                crate::models::SettlementError::OrderNotFound(id) => {
                    ("ORDER_NOT_FOUND", format!("Order not found: {}", id))
                }
                crate::models::SettlementError::AlreadySettled(id) => {
                    ("ALREADY_SETTLED", format!("Already settled: {}", id))
                }
                crate::models::SettlementError::PartialSettlement(msg) => {
                    ("PARTIAL_SETTLEMENT", msg.clone())
                }
                crate::models::SettlementError::InvalidSymbol(symbol) => {
                    ("INVALID_SYMBOL", format!("Invalid symbol format: {}", symbol))
                }
                crate::models::SettlementError::Database(err) => {
                    tracing::error!("Settlement database error: {}", err);
                    ("DATABASE_ERROR", "Database error".to_string())
                }
            };
            (
                StatusCode::BAD_REQUEST,
                Json(SettleErrorResponse {
                    error,
                    code: code.to_string(),
                }),
            )
        })?;

    // Update OHLCV candles for this trade
    if let Err(e) = OHLCV::update_from_trade(
        &state.pool,
        &req.symbol,
        req.price,
        req.quantity,
        req.timestamp,
    ).await {
        tracing::error!("Failed to update OHLCV: {}", e);
    }

    Ok(Json(SettleFillResponse {
        trade_id: trade.id.to_string(),
        buyer_id: trade.buyer_id.map(|id| id.to_string()).unwrap_or_default(),
        seller_id: trade.seller_id.map(|id| id.to_string()).unwrap_or_default(),
        settled: true,
    }))
}

/// Internal endpoint to cancel an order (e.g., unfilled market order portion)
async fn cancel_order_internal(
    State(state): State<AppState>,
    Json(req): Json<CancelOrderInternalRequest>,
) -> Result<Json<CancelOrderInternalResponse>, (StatusCode, Json<SettleErrorResponse>)> {
    tracing::info!(
        "Internal cancel request: order_id={}, filled_quantity={}",
        req.order_id,
        req.filled_quantity
    );

    let order = Order::cancel_internal(&state.pool, req.order_id, req.filled_quantity)
        .await
        .map_err(|e| {
            let (code, error) = match &e {
                crate::models::OrderError::NotFound => {
                    ("ORDER_NOT_FOUND", format!("Order not found: {}", req.order_id))
                }
                crate::models::OrderError::CannotCancel(status) => {
                    ("CANNOT_CANCEL", format!("Cannot cancel order with status: {}", status))
                }
                _ => {
                    tracing::error!("Internal cancel error: {:?}", e);
                    ("INTERNAL_ERROR", "Internal error".to_string())
                }
            };
            (
                StatusCode::BAD_REQUEST,
                Json(SettleErrorResponse {
                    error,
                    code: code.to_string(),
                }),
            )
        })?;

    Ok(Json(CancelOrderInternalResponse {
        success: true,
        order_id: order.id.to_string(),
        status: order.status,
    }))
}
