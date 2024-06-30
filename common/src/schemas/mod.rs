pub mod chat;
pub mod model;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use utoipa::ToSchema;

pub use chat::{ChatRequest, ChatCompletion, ChatCompletionChunk};
pub use model::{ModelInfo, ListModelsResponse};

#[derive(Serialize_repr, Deserialize_repr, PartialEq, Debug, ToSchema)]
#[repr(u16)]
pub enum ErrorCode {
    /// The model was not found.
    ModelNotFound = 1000,
    /// Internal server error.
    InternalError = 2000,
    /// The service is unhealthy.
    Unhealthy = 3000,
    /// Backend service error.
    BackendError = 4000,
}

#[derive(Serialize, Deserialize, ToSchema, Debug)]
pub struct ErrorResponse {
    pub error: ErrorCode,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct APIRequest {
    pub id: usize,
    pub body: APIRequestBody,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum APIRequestBody {
    OpenAIChat(ChatRequest),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct APIResponse {
    pub id: usize,
    pub body: APIResponseBody,
    pub once: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum APIResponseBody {
    Success(APIResponseBodySuccess),
    Error(ErrorResponse),
    Done,
}

impl APIResponseBody {
    pub fn from_openai_chat_completion(chat_completion: ChatCompletion) -> Self {
        Self::Success(APIResponseBodySuccess::OpenAIChatCompletion(chat_completion))
    }

    pub fn from_openai_chat_completion_chunk(chat_completion_chunk: ChatCompletionChunk) -> Self {
        Self::Success(APIResponseBodySuccess::OpenAIChatCompletionChunk(chat_completion_chunk))
    }

    pub fn from_error(error: ErrorResponse) -> Self {
        Self::Error(error)
    }

    pub fn is_done_or_error(&self) -> bool {
        matches!(self, Self::Done | Self::Error(_))
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum APIResponseBodySuccess {
    OpenAIChatCompletion(ChatCompletion),
    OpenAIChatCompletionChunk(ChatCompletionChunk),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum APIResponseError {
    ErrorResponse(ErrorResponse),
}
