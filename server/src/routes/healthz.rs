use axum::{
    extract::Extension,
    http::StatusCode,
    Json,
};
use tracing::instrument;

use common::schemas::{ErrorCode, ErrorResponse};
use crate::manager::ClientManager;

#[utoipa::path(
    get,
    tag = env!("CARGO_PKG_NAME"),
    path = "/healthz",
    responses(
        (status = 200, description = "Everything is working fine"),
        (
            status = 503,
            description = "Service is unavailable",
            body = ErrorResponse,
            example = json ! ({
                "error": 1000,
                "message": "Health check failed",
            }),
        ),
    ),
)]
#[instrument(skip(manager))]
pub(crate) async fn healthz(
    Extension(manager): Extension<ClientManager>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    match manager.check_healthz().await {
        true => Ok(()),
        false => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: ErrorCode::Unhealthy,
                message: "Health check failed".to_string(),
            }),
        )),
    }
}
