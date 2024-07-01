use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use thiserror::Error;

use crate::server::AppState;
use common::schemas::ErrorCode;

pub(crate) async fn auth_middleware(
    State(AppState {
        access_token,
        client_token,
        ..
    }): State<AppState>,
    req: Request,
    next: Next,
) -> Result<impl IntoResponse, AuthError> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|auth_header| auth_header.to_str().ok())
        .and_then(|auth_header| auth_header.strip_prefix("Bearer "));

    let token = token.ok_or(AuthError::MissingAuthorizationHeader)?;

    tracing::info!("uri: {}", req.uri().path());
    if req.uri().path() == "/ws" {
        tracing::info!("token: {}, client_token: {}", token, client_token);
        if token != client_token {
            return Err(AuthError::InvalidAccessToken);
        }
    } else if token != access_token {
        return Err(AuthError::InvalidAccessToken);
    }

    Ok(next.run(req).await)
}

#[derive(Debug, Error)]
pub(crate) enum AuthError {
    #[error("Missing Authorization header")]
    MissingAuthorizationHeader,
    #[error("Invalid access token")]
    InvalidAccessToken,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> axum::http::Response<axum::body::Body> {
        let (status, code, err_msg) = match self {
            AuthError::MissingAuthorizationHeader => (
                StatusCode::UNAUTHORIZED,
                ErrorCode::MissingAuthorizationHeader,
                "Missing Authorization header",
            ),
            AuthError::InvalidAccessToken => (
                axum::http::StatusCode::UNAUTHORIZED,
                ErrorCode::InvalidAccessToken,
                "Invalid access token",
            ),
        };

        (status, Json(json!({"code": code, "msg": err_msg}))).into_response()
    }
}
