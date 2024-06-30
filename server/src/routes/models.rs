use axum::{response::IntoResponse, Extension, Json};
use tracing::instrument;

use common::schemas::model::ListModelsResponse;
use crate::manager::ClientManager;

#[instrument(skip_all)]
pub async fn list_models(
    Extension(manager): Extension<ClientManager>,
) -> impl IntoResponse {
    let models = manager.list_models();

    Json(ListModelsResponse {
        object: "list".to_owned(),
        data: models,
    }).into_response()
}
