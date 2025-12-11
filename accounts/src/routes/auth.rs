use axum::{
    extract::State,
    http::StatusCode,
    routing::post,
    Json, Router,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use time::Duration;

use crate::models::{Otp, RefreshToken, User};
use crate::models::token::TokenService;
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct RequestOtpPayload {
    email: String,
}

#[derive(Debug, Serialize)]
pub struct RequestOtpResponse {
    message: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyOtpPayload {
    email: String,
    code: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    access_token: String,
    user: UserResponse,
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    id: String,
    email: String,
}

#[derive(Debug, Serialize)]
pub struct RefreshResponse {
    access_token: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    error: String,
}

pub fn auth_routes() -> Router<AppState> {
    let mut router = Router::new()
        .route("/request-otp", post(request_otp))
        .route("/verify-otp", post(verify_otp))
        .route("/signup", post(signup))
        .route("/refresh", post(refresh_token))
        .route("/logout", post(logout));

    // Dev/bot login endpoint that bypasses OTP
    // Enabled in development OR when ENABLE_BOT_LOGIN=true (for production bots)
    let is_dev = std::env::var("ENVIRONMENT").unwrap_or_default() == "development";
    let bot_login_enabled = std::env::var("ENABLE_BOT_LOGIN").unwrap_or_default() == "true";
    if is_dev || bot_login_enabled {
        router = router.route("/dev-login", post(dev_login));
    }

    router
}

async fn request_otp(
    State(state): State<AppState>,
    Json(payload): Json<RequestOtpPayload>,
) -> Result<Json<RequestOtpResponse>, (StatusCode, Json<ErrorResponse>)> {
    let email = payload.email.trim().to_lowercase();

    if !email.contains('@') {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: "Invalid email".into() }),
        ));
    }

    let otp = Otp::create(&state.pool, &email)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create OTP: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to create OTP".into() }))
        })?;

    // Send OTP via configured mail provider
    if let Err(e) = state.mail.send_otp(&email, &otp.code).await {
        tracing::error!("Failed to send OTP email: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: "Failed to send OTP".into() }),
        ));
    }

    Ok(Json(RequestOtpResponse {
        message: "OTP sent to your email".into(),
    }))
}

async fn verify_otp(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(payload): Json<VerifyOtpPayload>,
) -> Result<(CookieJar, Json<AuthResponse>), (StatusCode, Json<ErrorResponse>)> {
    let email = payload.email.trim().to_lowercase();
    let code = payload.code.trim();

    let valid = Otp::verify(&state.pool, &email, code)
        .await
        .map_err(|e| {
            tracing::error!("Failed to verify OTP: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to verify OTP".into() }))
        })?;

    if !valid {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse { error: "Invalid email or code".into() }),
        ));
    }

    // Find existing user only (sign-in requires existing account)
    let user = User::find_by_email(&state.pool, &email)
        .await
        .map_err(|e| {
            tracing::error!("Failed to find user: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Authentication failed".into() }))
        })?
        .ok_or_else(|| {
            // Generic error to prevent enumeration
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Invalid email or code".into() }))
        })?;

    // Create tokens
    let access_token = TokenService::create_access_token(user.id, &user.email, &state.jwt_secret);
    let (_, refresh_token) = RefreshToken::create(&state.pool, user.id, &state.jwt_secret)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create refresh token: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to create session".into() }))
        })?;

    // Set refresh token as HTTP-only cookie
    let cookie = Cookie::build(("refresh_token", refresh_token))
        .path("/")
        .http_only(true)
        .secure(false) // Set to true in production with HTTPS
        .same_site(SameSite::Lax)
        .max_age(Duration::days(30));

    Ok((
        jar.add(cookie),
        Json(AuthResponse {
            access_token,
            user: UserResponse {
                id: user.id.to_string(),
                email: user.email,
            },
        }),
    ))
}

async fn signup(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(payload): Json<VerifyOtpPayload>,
) -> Result<(CookieJar, Json<AuthResponse>), (StatusCode, Json<ErrorResponse>)> {
    let email = payload.email.trim().to_lowercase();
    let code = payload.code.trim();

    let valid = Otp::verify(&state.pool, &email, code)
        .await
        .map_err(|e| {
            tracing::error!("Failed to verify OTP: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to verify OTP".into() }))
        })?;

    if !valid {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse { error: "Invalid email or code".into() }),
        ));
    }

    // Check if user already exists
    let existing = User::find_by_email(&state.pool, &email)
        .await
        .map_err(|e| {
            tracing::error!("Failed to check user: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Registration failed".into() }))
        })?;

    if existing.is_some() {
        // Generic error to prevent enumeration
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse { error: "Invalid email or code".into() }),
        ));
    }

    // Create new user
    let user = User::create(&state.pool, &email)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create user: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to create account".into() }))
        })?;

    // Create tokens
    let access_token = TokenService::create_access_token(user.id, &user.email, &state.jwt_secret);
    let (_, refresh_token) = RefreshToken::create(&state.pool, user.id, &state.jwt_secret)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create refresh token: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to create session".into() }))
        })?;

    // Set refresh token as HTTP-only cookie
    let cookie = Cookie::build(("refresh_token", refresh_token))
        .path("/")
        .http_only(true)
        .secure(false)
        .same_site(SameSite::Lax)
        .max_age(Duration::days(30));

    Ok((
        jar.add(cookie),
        Json(AuthResponse {
            access_token,
            user: UserResponse {
                id: user.id.to_string(),
                email: user.email,
            },
        }),
    ))
}

async fn refresh_token(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Json<RefreshResponse>), (StatusCode, Json<ErrorResponse>)> {
    let refresh_token = jar
        .get("refresh_token")
        .map(|c| c.value().to_string())
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "No refresh token".into() }))
        })?;

    // Verify JWT structure
    let claims = TokenService::verify_refresh_token(&refresh_token, &state.jwt_secret)
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Invalid refresh token".into() }))
        })?;

    // Verify token exists in DB and not revoked
    let token_record = RefreshToken::find_valid(&state.pool, &refresh_token)
        .await
        .map_err(|e| {
            tracing::error!("Failed to find refresh token: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to verify session".into() }))
        })?
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Session expired or revoked".into() }))
        })?;

    // Get user
    let user_id: uuid::Uuid = claims.sub.parse().map_err(|_| {
        (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Invalid token".into() }))
    })?;

    let user = User::find_by_id(&state.pool, user_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to find user: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to find user".into() }))
        })?
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "User not found".into() }))
        })?;

    // Revoke old token and create new one (token rotation)
    RefreshToken::revoke(&state.pool, token_record.id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to revoke old token: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to refresh session".into() }))
        })?;

    let access_token = TokenService::create_access_token(user.id, &user.email, &state.jwt_secret);
    let (_, new_refresh_token) = RefreshToken::create(&state.pool, user.id, &state.jwt_secret)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create new refresh token: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to refresh session".into() }))
        })?;

    let cookie = Cookie::build(("refresh_token", new_refresh_token))
        .path("/")
        .http_only(true)
        .secure(false)
        .same_site(SameSite::Lax)
        .max_age(Duration::days(30));

    Ok((
        jar.add(cookie),
        Json(RefreshResponse { access_token }),
    ))
}

async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<CookieJar, (StatusCode, Json<ErrorResponse>)> {
    if let Some(refresh_token) = jar.get("refresh_token") {
        if let Some(token_record) = RefreshToken::find_valid(&state.pool, refresh_token.value())
            .await
            .unwrap_or(None)
        {
            let _ = RefreshToken::revoke(&state.pool, token_record.id).await;
        }
    }

    let cookie = Cookie::build(("refresh_token", ""))
        .path("/")
        .http_only(true)
        .max_age(Duration::seconds(0));

    Ok(jar.add(cookie))
}

/// Dev-only: Login/signup without OTP verification
/// Only available when ENVIRONMENT=development
#[derive(Debug, Deserialize)]
pub struct DevLoginPayload {
    email: String,
}

async fn dev_login(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(payload): Json<DevLoginPayload>,
) -> Result<(CookieJar, Json<AuthResponse>), (StatusCode, Json<ErrorResponse>)> {
    let email = payload.email.trim().to_lowercase();

    if !email.contains('@') {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: "Invalid email".into() }),
        ));
    }

    // Find or create user
    let user = match User::find_by_email(&state.pool, &email).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            // Create new user
            User::create(&state.pool, &email)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to create user: {}", e);
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to create user".into() }))
                })?
        }
        Err(e) => {
            tracing::error!("Failed to find user: {}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Database error".into() })));
        }
    };

    // Create tokens
    let access_token = TokenService::create_access_token(user.id, &user.email, &state.jwt_secret);
    let (_, refresh_token) = RefreshToken::create(&state.pool, user.id, &state.jwt_secret)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create refresh token: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: "Failed to create session".into() }))
        })?;

    // Set refresh token as HTTP-only cookie
    let cookie = Cookie::build(("refresh_token", refresh_token))
        .path("/")
        .http_only(true)
        .secure(false)
        .same_site(SameSite::Lax)
        .max_age(Duration::days(30));

    tracing::info!("Dev login for user: {} ({})", user.email, user.id);

    Ok((
        jar.add(cookie),
        Json(AuthResponse {
            access_token,
            user: UserResponse {
                id: user.id.to_string(),
                email: user.email,
            },
        }),
    ))
}
