use std::net::SocketAddr;

use axum::{
    extract::Extension,
    http::{self, Method},
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use axum_tracing_opentelemetry::middleware::OtelAxumLayer;
use thiserror::Error;
use tokio::signal;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::instrument;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::{
    manager::ClientManager,
    middlewares::auth::auth_middleware,
    routes::{chat::chat_completions, healthz::healthz, models::list_models, ws::ws_handler},
};

#[derive(Clone)]
pub struct AppState {
    pub access_token: String,
    pub client_token: String,
}

#[instrument]
async fn not_found(uri: http::Uri) -> impl IntoResponse {
    (
        http::StatusCode::NOT_FOUND,
        format!("{} not found", uri.path()),
    )
}

pub async fn run(
    addr: SocketAddr,
    allow_origin: Option<AllowOrigin>,
    access_token: String,
    client_token: String,
) -> Result<(), WebServerError> {
    let allow_origin = allow_origin.unwrap_or_else(AllowOrigin::any);
    let cors_layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([http::header::AUTHORIZATION, http::header::CONTENT_TYPE])
        .allow_origin(allow_origin);

    // OpenAPI documentation
    #[derive(OpenApi)]
    #[openapi(
        paths(
            crate::routes::healthz::healthz,
            crate::routes::models::list_models,
            crate::routes::chat::chat_completions,
        ),
        components(schemas(
            common::schemas::ErrorCode,
            common::schemas::ErrorResponse,
            common::schemas::model::ModelInfo,
            common::schemas::model::ListModelsResponse,
            common::schemas::chat::ChatRequest,
            common::schemas::chat::StreamOptions,
            common::schemas::chat::Message,
            common::schemas::chat::GrammarType,
            common::schemas::chat::ChatCompletion,
            common::schemas::chat::ChatCompletionChoice,
            common::schemas::chat::ChatCompletionMessage,
            common::schemas::chat::ChatCompletionLogprobs,
            common::schemas::chat::ChatCompletionLogprob,
            common::schemas::chat::ChatCompletionTopLogprob,
            common::schemas::chat::Usage,
            common::schemas::chat::ChatCompletionChunk,
            common::schemas::chat::ChatCompletionChunkChoice,
            common::schemas::chat::ChatCompletionDelta,
            common::schemas::chat::TextMessage,
            common::schemas::chat::ToolCallDelta,
            common::schemas::chat::DeltaToolCall,
            common::schemas::chat::Function,
            common::schemas::model::ModelInfo,
        )),
        tags(
            (
                name = "llm-gateway-server",
                description = "An API gateway for OpenAI-compatible servers."
            )
        ),
    )]
    struct ApiDoc;

    let doc = ApiDoc::openapi();

    let swagger_ui = SwaggerUi::new("/docs").url("/api-doc/openapi.json", doc);

    let state = AppState {
        access_token,
        client_token,
    };

    let app = Router::new()
        .route("/", get(healthz))
        .route("/healthz", get(healthz))
        .route("/ping", get(healthz))
        .route("/ws", get(ws_handler))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .layer(Extension(ClientManager::new()))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
        .merge(swagger_ui)
        .fallback(not_found)
        .layer(OtelAxumLayer::default())
        .layer(cors_layer);

    let app = app.into_make_service_with_connect_info::<SocketAddr>();

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|err| WebServerError::Axum(Box::new(err)))?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install terminate signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Signal received, starting graceful shutdown");
    opentelemetry::global::shutdown_tracer_provider();

    // TODO: gracefully clean up clients
}

#[derive(Debug, Error)]
pub enum WebServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Axum error: {0}")]
    Axum(#[from] axum::BoxError),
    #[error("Invalid access token")]
    InvalidAccessToken,
    #[error("Invalid client token")]
    InvalidClientToken,
}
