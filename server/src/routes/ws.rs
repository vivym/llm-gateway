use std::{net::SocketAddr, ops::ControlFlow, sync::{atomic::{AtomicBool, AtomicI64, Ordering}, Arc}};

use axum::{
    extract::{
        connect_info::ConnectInfo,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    Extension,
};
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use tracing::instrument;
use streamunordered::{StreamUnordered, StreamYield};

use common::schemas::{model::ListModelsResponse, APIResponse};
use crate::manager::ClientManager;

#[instrument(skip_all)]
pub(crate) async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Extension(manager): Extension<ClientManager>,
) -> impl IntoResponse {
    tracing::info!("WebSocket connection from {}", addr);
    ws.on_upgrade(move |socket| handle_socket(socket, addr, manager))
}

async fn handle_socket(
    mut socket: WebSocket,
    who: SocketAddr,
    manager: ClientManager,
) {
    if socket.send(Message::Ping("Hello".into())).await.is_ok() {
        tracing::info!("Ping sent to {}", who);
    } else {
        tracing::error!("Failed to send ping to {}", who);
        return;
    }

    if let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Pong(_) => {
                tracing::info!("Pong received from {}", who);
            },
            _ => {
                tracing::error!("Unexpected message received from {}: {:?}", who, msg);
                return;
            },
        }
    } else {
        tracing::error!("Client {} abruptly disconnected", who);
        return;
    }

    let mut req_receivers = StreamUnordered::new();

    // Receive model info
    if let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(t) => {
                match serde_json::from_str::<ListModelsResponse>(t.as_ref()) {
                    Ok(models) => {
                        tracing::info!("Received model info from {}: {:?}", who, models);
                        for model_info in models.data.into_iter() {
                            let rx = manager.add_client(model_info);
                            req_receivers.insert(rx);
                        }
                    },
                    Err(e) => {
                        tracing::error!("Failed to parse model info from {}: {}, Error: {}", who, t, e);
                        return;
                    },
                }
            },
            _ => {
                tracing::error!("Unexpected message received from {}: {:?}", who, msg);
                return;
            },
        }
    } else {
        tracing::error!("Disconnected from {} while waiting for model info", who);
        return;
    }

    if req_receivers.is_empty() {
        tracing::warn!("No models found for {}", who);
        return;
    }

    let (mut ws_sender, mut ws_receiver) = socket.split();

    let resp_senders = Arc::new(DashMap::new());
    let resp_senders_clone = resp_senders.clone();

    let closing = Arc::new(AtomicBool::new(false));
    let closing_send = closing.clone();
    let closing_recv = closing.clone();
    let closing_hearbeat = closing.clone();

    let last_heartbeat = Arc::new(AtomicI64::new(0));
    let last_heartbeat_clone = last_heartbeat.clone();

    let mut send_task = tokio::spawn(async move {
        while let Some((item, _)) = req_receivers.next().await {
            if let StreamYield::Item(req) = item {
                let now = chrono::Utc::now().timestamp();
                if (now - req.timestamp).abs() > 10 * 60 {
                    tracing::warn!("Ignoring request with timestamp drift of more than 10 minutes: {:?}", req);
                    continue;
                }

                let body = serde_json::to_string(&req.body)
                    .expect("Failed to serialize request body to JSON.");

                if let Err(e) = ws_sender.send(Message::Text(body)).await {
                    tracing::error!("Failed to send message to {}: {}", who, e);
                } else {
                    resp_senders.insert(req.body.id, req.response);
                }
            } else {
                tracing::warn!("Yielded item is not a request: {:?}", item);
            }

            if closing_send.load(Ordering::Relaxed) {
                break;
            }
        }

        tracing::info!("Send task completed for {}", who);
    });

    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match process_message(msg, who) {
                ControlFlow::Continue(Response::Api(resp)) => {
                    if let Some(sender) = resp_senders_clone.get(&resp.id) {
                        let id: usize = resp.id;
                        let should_remove = resp.body.is_done_or_error() || resp.once;
                        if sender.send(resp).await.is_err() {
                            tracing::warn!("Response receiver has been closed for response: {}", id);
                        }
                        if should_remove {
                            sender.closed().await;
                            drop(sender); // drop the sender reference, otherwise the next line will deadlock
                            resp_senders_clone.remove(&id);
                        }
                    } else {
                        tracing::error!("No response sender found for response: {:?}", resp);
                    }
                },
                ControlFlow::Continue(Response::Heartbeat(client_time)) => {
                    let now = chrono::Utc::now().timestamp();
                    if (now - client_time).abs() > 10 {
                        tracing::warn!("Client {} has a time drift of more than 10 seconds", who);
                    }
                    last_heartbeat_clone.store(now, Ordering::Relaxed);
                    tracing::info!("Received heartbeat from {}", who);
                },
                ControlFlow::Continue(Response::Ignore) => {},
                ControlFlow::Break(()) => break,
            }

            if closing_recv.load(Ordering::Relaxed) {
                break;
            }
        }

        tracing::info!("Recv task completed for {}", who);
    });

    let heartbeat_task = tokio::spawn(async move {
        while !closing_hearbeat.load(Ordering::Relaxed) {
            let last_heartbeat = last_heartbeat.load(Ordering::Relaxed);

            if last_heartbeat > 0 {
                let now = chrono::Utc::now().timestamp();

                // TODO: make this configurable
                if (now - last_heartbeat).abs() > 10 {
                    tracing::error!("Client {} has not sent a heartbeat in more than 10 seconds", who);
                    break;
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }

        tracing::info!("Heartbeat task completed for {}", who);
    });

    tokio::select! {
        _ = (&mut send_task) => {
            closing.store(true, Ordering::SeqCst);
        },
        _ = recv_task => {
            closing.store(true, Ordering::SeqCst);
            send_task.abort();
        },
        _ = heartbeat_task => {
            closing.store(true, Ordering::SeqCst);
            send_task.abort();
        },
    }

    tracing::info!("Connection closed to {}", who);
}

fn process_message(msg: Message, who: SocketAddr) -> ControlFlow<(), Response> {
    match msg {
        Message::Text(t) => {
            tracing::info!("Received text message from {}: {}", who, t);
            match serde_json::from_str(t.as_ref()) {
                Ok(resp) => ControlFlow::Continue(Response::Api(resp)),
                Err(e) => {
                    tracing::error!("Failed to parse response from {}: {}", who, e);
                    ControlFlow::Continue(Response::Ignore)
                },
            }
        },
        Message::Binary(b) => {
            if b.len() == 8 {
                // convert b to [u8; 8]
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&b);
                let hb = i64::from_be_bytes(buf);
                ControlFlow::Continue(Response::Heartbeat(hb))
            } else {
                ControlFlow::Continue(Response::Ignore)
            }
        },
        Message::Ping(_) => {
            tracing::info!("Received ping message from {}", who);
            ControlFlow::Continue(Response::Ignore)
        },
        Message::Pong(_) => {
            tracing::info!("Received pong message from {}", who);
            ControlFlow::Continue(Response::Ignore)
        },
        Message::Close(c) => {
            tracing::info!("Received close message from {}: {:?}", who, c);
            ControlFlow::Break(())
        },
    }
}

enum Response {
    Api(APIResponse),
    Heartbeat(i64),
    Ignore,
}
