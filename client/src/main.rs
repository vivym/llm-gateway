use std::{
    ops::ControlFlow,
    sync::{atomic::AtomicBool, Arc},
};

use clap::Parser;
use futures::{stream::StreamExt, SinkExt};
use mimalloc::MiMalloc;
use reqwest::header::{HeaderMap, AUTHORIZATION};
use reqwest_eventsource::{Error as EventSourceError, Event, EventSource};
use thiserror::Error;
use tokio::{signal, sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tungstenite::ClientRequestBuilder;

use common::{
    logging::{init_logging, LogFormat, LogLevel},
    schemas::{
        APIRequest, APIRequestBody, APIResponse, APIResponseBody, ChatCompletion,
        ChatCompletionChunk, ErrorCode, ErrorResponse, ListModelsResponse,
    },
};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// The URL for the WebSocket gateway.
    #[clap(long, env, default_value = "ws://localhost:3000/ws")]
    gateway_url: String,
    /// The token for the WebSocket gateway.
    #[clap(long, env)]
    token: String,
    /// The base URL for the API backend.
    #[clap(long, env, default_value = "https://api.openai.com")]
    api_url: String,
    #[clap(long, env, default_value = "null")]
    api_key: Option<String>,
    /// The timeout for the API backend.
    #[clap(long, env, default_value = "120")]
    timeout: u64,
    /// The interval for the API backend health check.
    #[clap(long, env, default_value = "3")]
    healthz_timeout: u64,
    /// The interval for the API backend health check.
    #[clap(long, env, default_value = "3")]
    healthz_interval: u64,
    /// The interval for heartbeating.
    #[clap(long, env, default_value = "3")]
    heartbeat_interval: u64,
    /// The number of retries for the API backend health check.
    #[clap(long, env, default_value = "5")]
    retry: u64,
    /// Whether to disable the health check.
    #[clap(long, env)]
    disable_health_check: bool,
    /// The number of threads for the client.
    #[clap(long, env)]
    num_threads: Option<usize>,
    #[clap(long, env)]
    otlp_endpoint: Option<String>,
    #[clap(long, env, default_value = "info")]
    log_level: LogLevel,
    #[clap(long, env, default_value = "text")]
    log_format: LogFormat,
    #[clap(long, env, default_value_t = true)]
    log_colorize: bool,
}

fn main() -> Result<(), ClientError> {
    match dotenvy::dotenv() {
        Ok(_) | Err(dotenvy::Error::Io(_)) => {}
        Err(e) => panic!("Failed to load .env file: {:?}", e),
    }

    let args = Args::parse();
    let Args {
        gateway_url,
        token,
        api_url,
        api_key,
        timeout,
        healthz_timeout,
        healthz_interval,
        heartbeat_interval,
        retry,
        disable_health_check,
        num_threads,
        otlp_endpoint,
        log_level,
        log_format,
        log_colorize,
    } = args;
    let api_url_for_healthz = api_url.clone();

    init_logging(
        env!("CARGO_PKG_NAME"),
        otlp_endpoint,
        log_level,
        log_format,
        log_colorize,
    );

    let mut headers = HeaderMap::new();
    if let Some(api_key) = api_key {
        let auth_toke = format!("Bearer {}", api_key);
        headers.insert(AUTHORIZATION, auth_toke.parse().unwrap());
    }

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(healthz_timeout))
        .default_headers(headers.clone())
        .build()?;

    let num_threads = num_threads.unwrap_or_else(|| num_cpus::get() * 2);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_threads)
        .enable_all()
        .build()?
        .block_on(async move {
            let closing = Arc::new(AtomicBool::new(false));

            let mut task = tokio::spawn(start(
                gateway_url,
                token,
                api_url,
                timeout,
                healthz_interval,
                heartbeat_interval,
                retry,
                disable_health_check,
                headers,
                http_client,
                api_url_for_healthz,
                closing.clone(),
            ));

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
                res = (&mut task) => {
                    let _ = res?;
                },
                _ = ctrl_c => {
                    tracing::info!("Received Ctrl-C, starting graceful shutdown");
                    closing.store(true, std::sync::atomic::Ordering::SeqCst);
                    let _ = task.await?;
                },
                _ = terminate => {
                    tracing::info!("Received terminate signal, starting graceful shutdown");
                    closing.store(true, std::sync::atomic::Ordering::SeqCst);
                    let _ = task.await?;
                },
            }

            opentelemetry::global::shutdown_tracer_provider();

            Result::<(), ClientError>::Ok(())
        })?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn start(
    gateway_url: String,
    token: String,
    api_url: String,
    timeout: u64,
    healthz_interval: u64,
    heartbeat_interval: u64,
    retry: u64,
    disable_health_check: bool,
    headers: HeaderMap,
    http_client: reqwest::Client,
    api_url_for_healthz: String,
    closing: Arc<AtomicBool>,
) -> Result<(), ClientError> {
    if !disable_health_check {
        check_health(&http_client, &api_url).await?;
    }

    let models = list_models(&http_client, &api_url).await?;
    tracing::info!("Models: {:?}", models);

    let builder = ClientRequestBuilder::new(gateway_url.parse().unwrap())
        .with_header("Authorization", format!("Bearer {}", token));
    let (ws_stream, _) = connect_async(builder).await?;

    tracing::info!("Connected to gateway: {}", gateway_url);

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    if let Some(Ok(msg)) = ws_receiver.next().await {
        match msg {
            Message::Ping(_) => {
                tracing::info!("Ping received from {}", gateway_url);
            }
            _ => {
                tracing::error!(
                    "Unexpected message received from {}: {:?}",
                    gateway_url,
                    msg
                );
                return Err(ClientError::HandShake(
                    "Unexpected message received".to_string(),
                ));
            }
        }
    } else {
        tracing::error!("Server {} abruptly disconnected", gateway_url);
        return Err(ClientError::HandShake(
            "Server abruptly disconnected".to_string(),
        ));
    }

    ws_sender.send(Message::Pong(token.into())).await?;

    ws_sender
        .send(Message::Text(serde_json::to_string(&models)?))
        .await?;

    tracing::info!("Model info sent");

    // TODO: make the channel size configurable
    let (resp_sender, mut resp_receiver) = mpsc::channel::<Response>(1024);
    let resp_sender_hb = resp_sender.clone();

    let closing_send = closing.clone();
    let closing_recv = closing.clone();
    let closing_healthz = closing.clone();
    let closing_hearbeat = closing.clone();

    let mut send_task: JoinHandle<Result<(), ClientError>> = tokio::spawn(async move {
        while !closing_send.load(std::sync::atomic::Ordering::Relaxed) {
            match resp_receiver.recv().await {
                Some(resp) => match resp {
                    Response::Api(resp) => {
                        let resp = serde_json::to_string(&resp)?;
                        ws_sender.send(Message::Text(resp)).await?;
                    }
                    Response::Heartbeat => {
                        let now = chrono::Utc::now();
                        let body = now.timestamp().to_be_bytes().to_vec();
                        ws_sender.send(Message::Binary(body)).await?;
                    }
                },
                None => {
                    tracing::info!("resp_receiver closed, exiting");
                    break;
                }
            }
        }

        Ok(())
    });

    let mut recv_task: JoinHandle<Result<(), ClientError>> = tokio::spawn(async move {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout))
            .default_headers(headers)
            .build()?;

        while !closing_recv.load(std::sync::atomic::Ordering::Relaxed) {
            match ws_receiver.next().await {
                Some(Ok(msg)) => match process_message(msg).await {
                    ControlFlow::Continue(Some(APIRequest { id, body })) => {
                        handle_api_request(id, body, &api_url, &http_client, &resp_sender).await?;
                    }
                    ControlFlow::Continue(None) => {}
                    ControlFlow::Break(()) => {
                        tracing::info!("Received WS close message, exiting");
                        break;
                    }
                },
                Some(Err(e)) => {
                    tracing::error!("Failed to receive message: {}, exiting", e);
                    break;
                }
                None => {
                    tracing::info!("ws_receiver closed, exiting");
                    break;
                }
            }
        }

        Ok(())
    });

    let mut healthz_task: JoinHandle<Result<(), ClientError>> = if disable_health_check {
        tokio::spawn(async move {
            while !closing_healthz.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_secs(healthz_interval)).await;
            }

            Ok(())
        })
    } else {
        tokio::spawn(async move {
            let mut count = 0;
            while !closing_healthz.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_secs(healthz_interval)).await;
                if let Err(e) = check_health(&http_client, &api_url_for_healthz).await {
                    count += 1;

                    if count <= retry {
                        tracing::error!("Failed to check health: {}, retrying #{}", e, count);
                    } else {
                        tracing::error!("Failed to check health {} times, exiting", retry);
                        break;
                    }
                }
            }

            Ok(())
        })
    };

    let mut heartbeat_task: JoinHandle<Result<(), ClientError>> = tokio::spawn(async move {
        while !closing_hearbeat.load(std::sync::atomic::Ordering::Relaxed) {
            resp_sender_hb
                .send(Response::Heartbeat)
                .await
                .map_err(|e| {
                    let msg = format!("Failed to send heartbeat: {}", e);
                    ClientError::ChannelSend(msg)
                })?;

            tokio::time::sleep(std::time::Duration::from_secs(heartbeat_interval)).await;
        }

        Ok(())
    });

    // wait for either task to finish and kill the other task
    tokio::select! {
        res = (&mut send_task) => {
            closing.store(true, std::sync::atomic::Ordering::SeqCst);

            let _ = res?;
        },
        res = (&mut recv_task) => {
            closing.store(true, std::sync::atomic::Ordering::SeqCst);

            let _ = res?;
        },
        res = (&mut healthz_task) => {
            closing.store(true, std::sync::atomic::Ordering::SeqCst);

            let _ = res?;
        },
        res = (&mut heartbeat_task) => {
            closing.store(true, std::sync::atomic::Ordering::SeqCst);

            let _ = res?;
        },
    }

    Ok(())
}

async fn process_message(msg: Message) -> ControlFlow<(), Option<APIRequest>> {
    match msg {
        Message::Text(t) => {
            tracing::debug!("Received text message: {}", t);
            match serde_json::from_str(t.as_ref()) {
                Ok(request) => ControlFlow::Continue(Some(request)),
                Err(e) => {
                    tracing::error!("Failed to parse request: {}", e);
                    ControlFlow::Continue(None)
                }
            }
        }
        Message::Binary(b) => {
            tracing::info!("Received binary message: {} bytes, ignore it.", b.len());
            ControlFlow::Continue(None)
        }
        Message::Ping(_) => {
            tracing::info!("Received ping message");
            ControlFlow::Continue(None)
        }
        Message::Pong(_) => {
            tracing::info!("Received pong message");
            ControlFlow::Continue(None)
        }
        Message::Close(c) => {
            tracing::info!("Received close message: {:?}", c);
            ControlFlow::Break(())
        }
        _ => {
            tracing::warn!("Received unknown message type");
            ControlFlow::Continue(None)
        }
    }
}

async fn check_health(client: &reqwest::Client, api_url: &str) -> Result<(), UnhealthyError> {
    let health_url = format!("{}/health", api_url);
    let res = client.get(&health_url).send().await?;

    if res.status().is_success() {
        Ok(())
    } else {
        let msg = res.text().await?;
        tracing::error!("API server is not healthy: {}", msg);
        Err(UnhealthyError::Unhealthy(msg))
    }
}

async fn list_models(
    client: &reqwest::Client,
    api_url: &str,
) -> Result<ListModelsResponse, ClientError> {
    let url = format!("{}/v1/models", api_url);
    let res = client.get(&url).send().await?;

    if res.status().is_success() {
        let body = res.text().await?;
        Ok(serde_json::from_str::<ListModelsResponse>(&body)?)
    } else {
        let msg = res.text().await?;
        tracing::error!("Failed to get model info: {}", msg);
        Err(ClientError::Unhealthy(UnhealthyError::Unhealthy(msg)))
    }
}

async fn handle_api_request(
    id: usize,
    body: APIRequestBody,
    api_url: &str,
    http_client: &reqwest::Client,
    resp_sender: &mpsc::Sender<Response>,
) -> Result<(), ClientError> {
    let (url, inner_body) = match &body {
        APIRequestBody::OpenAIChat(body) => (format!("{}/v1/chat/completions", api_url), body),
    };

    tracing::debug!("Request ID: {}, URL: {}, Body: {:?}", id, url, inner_body);
    let request_builder = http_client.post(url).json(inner_body);

    match body {
        APIRequestBody::OpenAIChat(request) => {
            if request.stream {
                let mut es = EventSource::new(request_builder)?;
                let mut done = false;

                while let Some(event) = es.next().await {
                    let resp = match event {
                        Ok(Event::Open) => {
                            tracing::debug!("EventSource opened");
                            None
                        }
                        Ok(Event::Message(msg)) => {
                            tracing::debug!("EventSource message: {:?}", msg);
                            if msg.data == "[DONE]" {
                                done = true;
                                tracing::debug!("EventSource done");
                                Some(APIResponse {
                                    id,
                                    body: APIResponseBody::Done,
                                    once: false,
                                })
                            } else {
                                match serde_json::from_str::<ChatCompletionChunk>(&msg.data) {
                                    Ok(chunk) => Some(APIResponse {
                                        id,
                                        body: APIResponseBody::from_openai_chat_completion_chunk(
                                            chunk,
                                        ),
                                        once: false,
                                    }),
                                    Err(e) => {
                                        tracing::error!("Failed to parse response: {}", e);
                                        done = true;
                                        Some(APIResponse {
                                            id,
                                            body: APIResponseBody::from_error(ErrorResponse {
                                                error: ErrorCode::BackendError,
                                                message: "Failed to parse response".to_string(),
                                            }),
                                            once: false,
                                        })
                                    }
                                }
                            }
                        }
                        Err(EventSourceError::StreamEnded) if !done => {
                            tracing::error!("EventSource stream ended unexpectedly");
                            done = true;
                            Some(APIResponse {
                                id,
                                body: APIResponseBody::from_error(ErrorResponse {
                                    error: ErrorCode::BackendError,
                                    message: "EventSource stream ended unexpectedly".to_string(),
                                }),
                                once: false,
                            })
                        }
                        Err(err) => {
                            tracing::error!("EventSource error: {:?}", err);
                            done = true;
                            Some(APIResponse {
                                id,
                                body: APIResponseBody::from_error(ErrorResponse {
                                    error: ErrorCode::BackendError,
                                    message: "EventSource internal error".to_string(),
                                }),
                                once: false,
                            })
                        }
                    };

                    if let Some(resp) = resp {
                        resp_sender.send(Response::Api(resp)).await.map_err(|e| {
                            let msg = format!("Failed to send response: {}", e);
                            ClientError::ChannelSend(msg)
                        })?;
                    }

                    if done {
                        es.close();
                        break;
                    }
                }
            } else {
                let resp = match request_builder.send().await {
                    Ok(res) => match res.text().await {
                        Ok(body) => {
                            match serde_json::from_str::<ChatCompletion>(&body) {
                                Ok(resp) => APIResponse {
                                    id,
                                    body: APIResponseBody::from_openai_chat_completion(resp),
                                    once: true,
                                },
                                Err(e) => {
                                    tracing::error!("Request ID: {}, Failed to parse response body: {}, Error: {}", id, body, e);
                                    let msg = format!(
                                        "Request ID: {}, Failed to parse response body.",
                                        id
                                    );
                                    APIResponse {
                                        id,
                                        body: APIResponseBody::from_error(ErrorResponse {
                                            error: ErrorCode::BackendError,
                                            message: msg,
                                        }),
                                        once: true,
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                "Request ID: {}, Failed to read response body: {}",
                                id,
                                e
                            );
                            let msg = format!("Request ID: {}, Failed to read response body.", id);
                            APIResponse {
                                id,
                                body: APIResponseBody::from_error(ErrorResponse {
                                    error: ErrorCode::BackendError,
                                    message: msg,
                                }),
                                once: true,
                            }
                        }
                    },
                    Err(e) => {
                        tracing::error!("Request ID: {}, Backend error: {}", id, e);
                        // Hide the actual error message from the user
                        let msg = format!("Request ID: {}, Backend error.", id);
                        APIResponse {
                            id,
                            body: APIResponseBody::from_error(ErrorResponse {
                                error: ErrorCode::BackendError,
                                message: msg,
                            }),
                            once: true,
                        }
                    }
                };

                resp_sender.send(Response::Api(resp)).await.map_err(|e| {
                    let msg = format!("Failed to send response: {}", e);
                    ClientError::ChannelSend(msg)
                })?;
            }
        }
    }

    Ok(())
}

enum Response {
    Api(APIResponse),
    Heartbeat,
}

#[derive(Error, Debug)]
enum ClientError {
    #[error("Tokio runtime failed to start: {0}")]
    Tokio(#[from] std::io::Error),
    #[error("Handshake error: {0}")]
    HandShake(String),
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("TaskJoin error: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),
    #[error("{0}")]
    Unhealthy(#[from] UnhealthyError),
    #[error("WebSocket Error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("JSON Error: {0}")]
    SerdeJson(#[from] serde_json::Error),
    #[error("Reqwest Eventsource CannotCloneRequestError: {0}")]
    CannotCloneRequest(#[from] reqwest_eventsource::CannotCloneRequestError),
    #[error("ChannelSend error: {0}")]
    ChannelSend(String),
}

#[derive(Error, Debug)]
enum UnhealthyError {
    #[error("API server is not healthy: {0}")]
    Unhealthy(String),
    #[error("Failed to check health: {0}")]
    FailedToCheck(#[from] reqwest::Error),
}
