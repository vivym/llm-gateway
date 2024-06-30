use std::convert::Infallible;

use async_stream::__private::AsyncStream;
use axum::{
    extract::Extension,
    http::StatusCode,
    response::{sse::{Event, KeepAlive}, IntoResponse, Response, Sse},
    Json,
};
use tokio::sync::mpsc;
use tracing::instrument;

use common::schemas::{
    APIRequestBody,
    APIResponse,
    APIResponseBody,
    APIResponseBodySuccess,
    ChatRequest,
    ErrorCode,
    ErrorResponse,
};
use crate::manager::{ClientManager, ClientManagerError};

#[utoipa::path(
    post,
    tag = env!("CARGO_PKG_NAME"),
    path = "/v1/chat/completions",
    request_body = ChatRequest,
    responses(
        (
            status = 200,
            description = "Generated Chat Completion",
            content(
                ("application/json" = ChatCompletion),
            ),
        ),
        (
            status = 424,
            description = "Generation Error",
            body = ErrorResponse,
        ),
    )
)]
#[instrument(skip_all)]
pub(crate) async fn chat_completions(
    Extension(manager): Extension<ClientManager>,
    Json(req): Json<ChatRequest>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let stream_mode = req.stream;
    let model_id = req.model.clone();
    let (tx, mut rx) = mpsc::channel::<APIResponse>(
        if stream_mode {
            4
        } else {
            1
        }
    );
    tracing::info!("Request: {:?}", req);

    match manager.send_request(model_id, APIRequestBody::OpenAIChat(req), tx).await {
        Ok(_) => {
            if stream_mode {
                let resp_stream: AsyncStream<Result<Event, Infallible>, _> = async_stream::stream! {
                    loop {
                        match rx.recv().await {
                            Some(APIResponse { body, .. }) => {
                                match body {
                                    APIResponseBody::Success(resp) => match resp {
                                        APIResponseBodySuccess::OpenAIChatCompletionChunk(chunk) => {
                                            tracing::info!("Sending chunk: {:?}", chunk);
                                            let event = Event::default()
                                                .json_data(chunk)
                                                .unwrap_or_else(|err| {
                                                    tracing::error!("Failed to serialize chunk: {:?}", err);
                                                    Event::default()
                                                });

                                            yield Ok(event);
                                        },
                                        _ => {
                                            tracing::error!("Invalid response from backend: {:?}", resp);
                                            break;
                                        },
                                    },
                                    APIResponseBody::Error(err) => {
                                        tracing::error!("Error response from backend: {:?}", err);
                                        let event = Event::default()
                                            .json_data(err)
                                            .unwrap_or_else(|_| Event::default());

                                        yield Ok(event);
                                        break;
                                    },
                                    APIResponseBody::Done => {
                                        tracing::info!("Done response from backend");
                                        break;
                                    },
                                }
                            }
                            None => {
                                tracing::info!("Channel closed");
                                break;
                            },
                        }
                    }

                    tracing::info!("RespStream ended");
                };
                let sse = Sse::new(resp_stream)
                    .keep_alive(KeepAlive::default());
                Ok(sse.into_response())
            } else {
                let resp = rx.recv().await.unwrap();
                match resp.body {
                    APIResponseBody::Success(resp) => match resp {
                        APIResponseBodySuccess::OpenAIChatCompletion(chat_completion) => {
                            Ok(Json(chat_completion).into_response())
                        },
                        _ => Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(ErrorResponse {
                                error: ErrorCode::BackendError,
                                message: "Invalid response from backend".to_string(),
                            }),
                        )),
                    },
                    APIResponseBody::Error(err) => Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(err),
                    )),
                    _ => unreachable!("Invalid response from backend"),
                }
            }
        },
        Err(ClientManagerError::ModelNotFound(model_id)) => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: ErrorCode::ModelNotFound,
                message: format!("Model not found: `{}`", model_id).to_string(),
            }),
        )),
        Err(ClientManagerError::InternalError) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: ErrorCode::InternalError,
                message: "Internal error".to_string(),
            }),
        )),
    }
}
