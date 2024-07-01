use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_channel::{Receiver, Sender};
use dashmap::DashMap;
use thiserror::Error;
use tokio::sync::mpsc;

use common::schemas::{APIRequest, APIRequestBody, APIResponse, ModelInfo};

#[derive(Debug)]
pub(crate) struct APIRequestWithResponse {
    pub body: APIRequest,
    pub response: mpsc::Sender<APIResponse>,
    pub timestamp: i64,
}

#[derive(Clone)]
pub(crate) struct ClientManager {
    clients: Arc<DashMap<String, ClientInfo>>,
    req_count: Arc<AtomicUsize>,
}

pub(crate) struct ClientInfo {
    pub model_info: ModelInfo,
    pub sender: Sender<APIRequestWithResponse>,
    pub receiver: Receiver<APIRequestWithResponse>,
}

impl ClientManager {
    pub fn new() -> Self {
        Self {
            clients: Arc::new(DashMap::new()),
            req_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn add_client(&self, model_info: ModelInfo) -> Receiver<APIRequestWithResponse> {
        let kv = self
            .clients
            .entry(model_info.id.clone())
            .or_insert_with(|| {
                let (tx, rx) = async_channel::bounded(32);
                ClientInfo {
                    model_info,
                    sender: tx,
                    receiver: rx,
                }
            });

        kv.value().receiver.clone()
    }

    pub async fn send_request(
        &self,
        model_id: String,
        request_body: APIRequestBody,
        response: mpsc::Sender<APIResponse>,
    ) -> Result<(), ClientManagerError> {
        if let Some(client_info) = self.clients.get(&model_id) {
            if client_info.sender.receiver_count() <= 1 {
                tracing::info!("Dropping client: {}", model_id);
                drop(client_info);
                self.clients
                    .remove_if(&model_id, |_, c| c.sender.receiver_count() <= 1);
                return Err(ClientManagerError::ModelNotFound(model_id));
            }

            let req_count = self.req_count.fetch_add(1, Ordering::SeqCst);
            let req_with_resp = APIRequestWithResponse {
                body: APIRequest {
                    id: req_count,
                    body: request_body,
                },
                response,
                timestamp: chrono::Utc::now().timestamp(),
            };

            if client_info.sender.send(req_with_resp).await.is_err() {
                return Err(ClientManagerError::InternalError);
            }

            Ok(())
        } else {
            Err(ClientManagerError::ModelNotFound(model_id))
        }
    }

    pub fn try_remove_client(&self, model_id: &str) {
        if let Some(client_info) = self.clients.get(model_id) {
            if client_info.sender.receiver_count() <= 1 {
                tracing::info!("Dropping client: {}", model_id);
                drop(client_info);
                self.clients
                    .remove_if(model_id, |_, c| c.sender.receiver_count() <= 1);
            }
        }
    }

    pub async fn check_healthz(&self) -> bool {
        // TODO: implement health check
        true
    }

    pub fn list_models(&self) -> Vec<ModelInfo> {
        self.clients
            .iter()
            .map(|kv| kv.value().model_info.clone())
            .collect()
    }
}

#[derive(Error, Debug)]
pub(crate) enum ClientManagerError {
    #[error("Model not found: {0}")]
    ModelNotFound(String),
    #[error("Internal error")]
    InternalError,
}
