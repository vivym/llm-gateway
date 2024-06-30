use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Clone, Deserialize, ToSchema, Serialize, Debug)]
pub struct ModelInfo {
    /// The model identifier, which can be referenced in the API endpoints.
    #[schema(example = "gpt3.5-turbo")]
    pub id: String,

    /// The Unix timestamp (in seconds) when the model was created.
    #[schema(example = "1700000000")]
    pub created: u64,

    /// The object type, which is always "model".
    #[schema(example = "model")]
    pub object: String,

    /// The organization that owns the model.
    #[schema(example = "rizzup")]
    pub owned_by: String,
}

#[derive(Clone, Deserialize, ToSchema, Serialize, Debug)]
pub struct ListModelsResponse {
    /// The object type, which is always "list".
    #[schema(example = "list")]
    pub object: String,
    /// Available models.
    pub data: Vec<ModelInfo>,
}
