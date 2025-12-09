use accounts::db;
use accounts::models;
use accounts::routes;

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Json, Router,
};
use serde::Serialize;
use std::env;
use axum::http::HeaderValue;
use tower_http::cors::CorsLayer;

use accounts::AppState;
use models::{User, token::TokenService};
use routes::{auth_routes, balance_routes, ohlcv_routes, order_routes, faucet_routes, internal_routes};

#[derive(Serialize)]
struct HealthResponse {
    status: String,
}

#[derive(Serialize)]
struct MeResponse {
    id: String,
    email: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("accounts=debug".parse()?)
                .add_directive("tower_http=debug".parse()?),
        )
        .init();

    // Config from env
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/accounts".to_string());
    let jwt_secret = env::var("JWT_SECRET")
        .unwrap_or_else(|_| "dev-secret-change-in-production".to_string());
    let bind_addr = env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3001".to_string());

    // Create database connection pool
    tracing::info!("Connecting to database...");
    let pool = db::create_pool(&database_url).await?;

    // Run migrations
    tracing::info!("Running migrations...");
    db::run_migrations(&pool).await?;

    // Spawn the midnight cleanup task
    accounts::scheduler::spawn_cleanup_task(pool.clone());

    // Create mail service
    let mail = accounts::mail::create_mail_service();

    let state = AppState {
        pool,
        jwt_secret,
        mail,
    };

    // Build router
    let app = Router::new()
        .route("/health", get(health))
        .nest("/auth", auth_routes())
        .nest(
            "/api/me",
            Router::new()
                .route("/", get(me))
                .layer(middleware::from_fn_with_state(state.clone(), auth_middleware)),
        )
        .nest(
            "/api/balances",
            balance_routes()
                .layer(middleware::from_fn_with_state(state.clone(), auth_middleware)),
        )
        .nest(
            "/api/orders",
            order_routes()
                .layer(middleware::from_fn_with_state(state.clone(), auth_middleware)),
        )
        .nest(
            "/api/faucet",
            faucet_routes()
                .layer(middleware::from_fn_with_state(state.clone(), auth_middleware)),
        )
        .nest("/api/ohlcv", ohlcv_routes())
        .nest("/internal", internal_routes())
        .layer({
            use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, ACCEPT};
            use axum::http::Method;

            // CORS configuration - support credentials for cookie-based auth
            // Note: credentials mode requires explicit origins, methods, and headers (no wildcards)
            let allowed_headers = [AUTHORIZATION, CONTENT_TYPE, ACCEPT];
            let allowed_methods = [Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS];

            if let Ok(origins) = env::var("CORS_ALLOWED_ORIGINS") {
                let allowed: Vec<HeaderValue> = origins
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
                CorsLayer::new()
                    .allow_origin(allowed)
                    .allow_methods(allowed_methods)
                    .allow_headers(allowed_headers)
                    .allow_credentials(true)
            } else {
                // Development: allow common localhost origins
                let dev_origins: Vec<HeaderValue> = [
                    "http://localhost:5173",
                    "http://localhost:5174",
                    "http://localhost:5175",
                    "http://localhost:5176",
                    "http://localhost:5177",
                    "http://localhost:5178",
                    "http://localhost:5179",
                    "http://localhost:3000",
                    "http://127.0.0.1:5173",
                    "http://127.0.0.1:5174",
                    "http://127.0.0.1:5175",
                    "http://127.0.0.1:5176",
                    "http://127.0.0.1:5177",
                    "http://127.0.0.1:5178",
                    "http://127.0.0.1:5179",
                    "http://127.0.0.1:3000",
                ]
                .iter()
                .filter_map(|o| o.parse().ok())
                .collect();
                CorsLayer::new()
                    .allow_origin(dev_origins)
                    .allow_methods(allowed_methods)
                    .allow_headers(allowed_headers)
                    .allow_credentials(true)
            }
        })
        .with_state(state);

    // Start server
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("Accounts service listening on {}", bind_addr);

    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}

async fn me(
    axum::Extension(user): axum::Extension<User>,
) -> Json<MeResponse> {
    Json(MeResponse {
        id: user.id.to_string(),
        email: user.email,
    })
}

async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Missing authorization header".into() }))
        })?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Invalid authorization header".into() }))
        })?;

    let claims = TokenService::verify_access_token(token, &state.jwt_secret)
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Invalid or expired token".into() }))
        })?;

    let user_id: uuid::Uuid = claims.sub.parse().map_err(|_| {
        (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Invalid token".into() }))
    })?;

    let user = User::find_by_id(&state.pool, user_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to find user: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Internal error".into() }))
        })?
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "User not found".into() }))
        })?;

    req.extensions_mut().insert(user);

    Ok(next.run(req).await)
}
