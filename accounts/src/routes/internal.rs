use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{delete, post},
    Json, Router,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::{EntryType, Fill, LedgerEntry, Order, OrderType, PlaceOrderRequest, Side, Trade, OHLCV};
use crate::AppState;

/// Internal API routes (called by gateway/matching engine, not end users)
pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/settle", post(settle_fill))
        .route("/cancel", post(cancel_order_internal))
        .route("/orders", post(place_order_internal))
        .route("/orders/:order_id", delete(cancel_order_by_id_internal))
        .route("/mint", post(mint_internal))
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

/// Request for internal order placement (from gateway WebSocket)
#[derive(Debug, Deserialize)]
pub struct PlaceOrderInternalRequest {
    pub symbol: String,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Decimal>,
    pub quantity: Option<Decimal>,
    pub quote_amount: Option<Decimal>,
    pub max_slippage_price: Option<Decimal>,
}

/// Response for internal order placement
#[derive(Debug, Serialize)]
pub struct PlaceOrderInternalResponse {
    pub order: OrderResponseInternal,
}

#[derive(Debug, Serialize)]
pub struct OrderResponseInternal {
    pub id: String,
    pub quantity: String,
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

    // Update OHLCV candles directly for reliable persistence
    if let Err(e) = OHLCV::update_from_trade(
        &state.pool,
        &req.symbol,
        req.price,
        req.quantity,
        req.timestamp,
    ).await {
        // Log error but don't fail the settlement - OHLCV is secondary
        tracing::warn!("Failed to update OHLCV for trade: {}", e);
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

/// Request for internal mint (for bots/testing)
#[derive(Debug, Deserialize)]
pub struct MintInternalRequest {
    pub asset: String,
    pub amount: Decimal,
}

/// Response for internal mint
#[derive(Debug, Serialize)]
pub struct MintInternalResponse {
    pub success: bool,
    pub asset: String,
    pub amount: String,
    pub new_balance: String,
}

/// Internal endpoint to mint tokens for a user (for bots/testing)
/// The user_id is passed via X-User-Id header
async fn mint_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MintInternalRequest>,
) -> Result<Json<MintInternalResponse>, (StatusCode, Json<SettleErrorResponse>)> {
    // Extract user_id from header
    let user_id = headers
        .get("X-User-Id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<Uuid>().ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(SettleErrorResponse {
                    error: "Missing or invalid X-User-Id header".into(),
                    code: "INVALID_USER_ID".into(),
                }),
            )
        })?;

    tracing::info!(
        "Internal mint: user_id={}, asset={}, amount={}",
        user_id,
        req.asset,
        req.amount
    );

    // Credit the asset directly
    let entry = LedgerEntry::append(
        &state.pool,
        user_id,
        &req.asset,
        req.amount,
        EntryType::Deposit,
        None,
        Some("Internal mint"),
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to mint: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SettleErrorResponse {
                error: "Failed to mint tokens".into(),
                code: "MINT_FAILED".into(),
            }),
        )
    })?;

    Ok(Json(MintInternalResponse {
        success: true,
        asset: req.asset,
        amount: req.amount.to_string(),
        new_balance: entry.balance_after.to_string(),
    }))
}

/// Internal endpoint to place an order on behalf of an authenticated user
/// The user_id is passed via X-User-Id header (gateway has already validated the JWT)
async fn place_order_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PlaceOrderInternalRequest>,
) -> Result<Json<PlaceOrderInternalResponse>, (StatusCode, Json<SettleErrorResponse>)> {
    // Extract user_id from header (gateway has already authenticated)
    let user_id = headers
        .get("X-User-Id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<Uuid>().ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(SettleErrorResponse {
                    error: "Missing or invalid X-User-Id header".into(),
                    code: "INVALID_USER_ID".into(),
                }),
            )
        })?;

    tracing::info!(
        "Internal place order: user_id={}, symbol={}, side={:?}, type={:?}",
        user_id,
        req.symbol,
        req.side,
        req.order_type
    );

    // Determine quantity - either from quantity field or calculated from quote_amount
    let (quantity, quote_amount) = match (req.quantity, req.quote_amount) {
        // Quote currency order: "spend X EUR to buy KCN"
        (None, Some(qa)) => {
            // Only valid for market buy orders
            if req.order_type != OrderType::Market || req.side != Side::Bid {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(SettleErrorResponse {
                        error: "Quote currency orders are only supported for market buy orders".into(),
                        code: "INVALID_ORDER_TYPE".into(),
                    }),
                ));
            }

            let price = req.max_slippage_price.unwrap_or_else(|| Decimal::from(1000));
            let calculated_qty = (qa / price).round_dp(8);
            (calculated_qty, Some(qa))
        }
        // Normal order with quantity
        (Some(qty), _) => (qty, None),
        // Neither provided
        (None, None) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(SettleErrorResponse {
                    error: "Either quantity or quote_amount must be provided".into(),
                    code: "MISSING_QUANTITY".into(),
                }),
            ));
        }
    };

    let result = Order::place(
        &state.pool,
        user_id,
        PlaceOrderRequest {
            symbol: req.symbol,
            side: req.side,
            order_type: req.order_type,
            price: req.price,
            quantity,
            quote_amount,
            max_slippage_price: req.max_slippage_price,
        },
    )
    .await
    .map_err(|e| {
        let (code, error) = match &e {
            crate::models::OrderError::InsufficientBalance { available, required } => {
                ("INSUFFICIENT_BALANCE", format!("Insufficient balance: available={}, required={}", available, required))
            }
            crate::models::OrderError::LimitOrderRequiresPrice => {
                ("LIMIT_REQUIRES_PRICE", "Limit order requires price".into())
            }
            crate::models::OrderError::InvalidSymbol => {
                ("INVALID_SYMBOL", "Invalid symbol format".into())
            }
            _ => {
                tracing::error!("Internal place order error: {:?}", e);
                ("INTERNAL_ERROR", "Internal error".into())
            }
        };
        (
            StatusCode::BAD_REQUEST,
            Json(SettleErrorResponse { error, code: code.into() }),
        )
    })?;

    Ok(Json(PlaceOrderInternalResponse {
        order: OrderResponseInternal {
            id: result.order.id.to_string(),
            quantity: result.order.quantity.to_string(),
        },
    }))
}

/// Internal endpoint to cancel an order by ID
/// The user_id is passed via X-User-Id header
async fn cancel_order_by_id_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(order_id): axum::extract::Path<Uuid>,
) -> Result<Json<CancelOrderInternalResponse>, (StatusCode, Json<SettleErrorResponse>)> {
    // Extract user_id from header
    let user_id = headers
        .get("X-User-Id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<Uuid>().ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(SettleErrorResponse {
                    error: "Missing or invalid X-User-Id header".into(),
                    code: "INVALID_USER_ID".into(),
                }),
            )
        })?;

    tracing::info!("Internal cancel by ID: user_id={}, order_id={}", user_id, order_id);

    // Cancel using the user's cancel method (verifies ownership)
    let order = Order::cancel(&state.pool, user_id, order_id)
        .await
        .map_err(|e| {
            let (code, error) = match &e {
                crate::models::OrderError::NotFound => {
                    ("ORDER_NOT_FOUND", format!("Order not found: {}", order_id))
                }
                crate::models::OrderError::CannotCancel(status) => {
                    ("CANNOT_CANCEL", format!("Cannot cancel order with status: {}", status))
                }
                _ => {
                    tracing::error!("Internal cancel error: {:?}", e);
                    ("INTERNAL_ERROR", "Internal error".into())
                }
            };
            (
                StatusCode::BAD_REQUEST,
                Json(SettleErrorResponse { error, code: code.into() }),
            )
        })?;

    Ok(Json(CancelOrderInternalResponse {
        success: true,
        order_id: order.id.to_string(),
        status: order.status,
    }))
}
