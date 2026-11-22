//! Authentication API endpoints

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::AppState;
use crate::auth::accounts::AuthError;

/// Build auth router
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/validate", get(validate))
}

/// Registration request
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
}

/// Authentication response (for register and login)
#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub account_id: String,
    pub username: String,
    pub access_level: String,
}

/// Error response
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Register a new account
/// Rules for new accounts: usernames are 3-32 ASCII letters, digits, `_`
/// or `-`; passwords 6 characters (as the web client requires) to 1 KiB
fn validate_credentials(username: &str, password: &str) -> Result<(), String> {
    let len = username.chars().count();
    if !(3..=32).contains(&len) {
        return Err("username must be 3-32 characters".to_string());
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("username may contain only letters, digits, _ and -".to_string());
    }
    if password.chars().count() < 6 {
        return Err("password must be at least 6 characters".to_string());
    }
    if password.len() > crate::auth::accounts::MAX_PASSWORD_BYTES {
        return Err(format!(
            "password must be at most {} bytes",
            crate::auth::accounts::MAX_PASSWORD_BYTES
        ));
    }
    Ok(())
}

async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> impl IntoResponse {
    if let Err(error) = validate_credentials(&req.username, &req.password) {
        return (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })).into_response();
    }

    let service = state.accounts();

    match service.create_account(&req.username, &req.password).await {
        Ok((account, token)) => (
            StatusCode::CREATED,
            Json(AuthResponse {
                token,
                account_id: account.id,
                username: account.username,
                access_level: account.access_level,
            }),
        )
            .into_response(),
        Err(AuthError::UsernameExists) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "username already exists".to_string(),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// Login request
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Login with username and password
async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> impl IntoResponse {
    // Before any password hashing
    if let Err(retry) = state.auth_by_user.check(&req.username.to_lowercase()) {
        return super::ratelimit::too_many(retry);
    }
    let service = state.accounts();

    match service.login(&req.username, &req.password).await {
        Ok((account, token)) => (
            StatusCode::OK,
            Json(AuthResponse {
                token,
                account_id: account.id,
                username: account.username,
                access_level: account.access_level,
            }),
        )
            .into_response(),
        Err(AuthError::InvalidCredentials) => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "invalid credentials".to_string(),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// Logout request
#[derive(Debug, Deserialize)]
pub struct LogoutRequest {
    pub token: String,
}

/// Logout response
#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    pub success: bool,
}

/// Logout by invalidating token
async fn logout(
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> impl IntoResponse {
    let service = state.accounts();

    match service.logout(&req.token).await {
        Ok(success) => (StatusCode::OK, Json(LogoutResponse { success })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// Validate query params
#[derive(Debug, Deserialize)]
pub struct ValidateQuery {
    pub token: String,
}

/// Validate response
#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_level: Option<String>,
}

/// Validate a token
async fn validate(
    State(state): State<AppState>,
    Query(params): Query<ValidateQuery>,
) -> impl IntoResponse {
    let service = state.accounts();

    match service.validate_token(&params.token).await {
        Ok(Some(account)) => Json(ValidateResponse {
            valid: true,
            account_id: Some(account.id),
            username: Some(account.username),
            access_level: Some(account.access_level),
        }),
        Ok(None) => Json(ValidateResponse {
            valid: false,
            account_id: None,
            username: None,
            access_level: None,
        }),
        Err(_) => Json(ValidateResponse {
            valid: false,
            account_id: None,
            username: None,
            access_level: None,
        }),
    }
}
