use axum::{response::IntoResponse, Extension, Json};
use tracing::instrument;

use crate::manager::ClientManager;
use common::schemas::model::ListModelsResponse;

#[utoipa::path(
    get,
    tag = env!("CARGO_PKG_NAME"),
    path = "/v1/models",
    responses(
        (
            status = 200,
            description = "List of Models",
            content(
                ("application/json" = ListModelsResponse),
            ),
        )
    ),
)]
#[instrument(skip_all)]
pub async fn list_models(Extension(manager): Extension<ClientManager>) -> impl IntoResponse {
    let models = manager.list_models();

    Json(ListModelsResponse {
        object: "list".to_owned(),
        data: models,
    })
    .into_response()
}
