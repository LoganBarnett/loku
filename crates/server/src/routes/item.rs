use aide::transform::TransformOperation;
use axum::{
  extract::{Query, State},
  http::StatusCode,
  response::{IntoResponse, Response},
  Json,
};
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;

use crate::index::store::IndexError;
use crate::routes::types::VideoItem;
use crate::web_base::AppState;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ItemQuery {
  pub library: String,
  /// The video's library-relative path, exactly as browse and search report
  /// it.
  pub path: String,
}

#[derive(Debug, Error)]
pub(crate) enum ItemError {
  #[error("Library '{name}' is not configured")]
  UnknownLibrary { name: String },

  #[error("Video '{path}' not found in library '{library}'")]
  ItemNotFound { library: String, path: String },

  #[error(transparent)]
  Index(#[from] IndexError),
}

impl aide::operation::OperationOutput for ItemError {
  type Inner = Self;
}

impl IntoResponse for ItemError {
  fn into_response(self) -> Response {
    let status = match &self {
      ItemError::UnknownLibrary { .. } | ItemError::ItemNotFound { .. } => {
        StatusCode::NOT_FOUND
      }
      ItemError::Index(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, self.to_string()).into_response()
  }
}

pub(crate) fn item_docs(op: TransformOperation) -> TransformOperation {
  op.description("Fetch one video with its full metadata.")
    .response::<200, Json<VideoItem>>()
    .response_with::<404, (), _>(|r| r.description("Unknown library or video."))
    .response_with::<500, (), _>(|r| r.description("Index query failed."))
}

pub(crate) async fn handler(
  State(state): State<AppState>,
  Query(params): Query<ItemQuery>,
) -> Result<Json<VideoItem>, ItemError> {
  state
    .library(&params.library)
    .ok_or_else(|| ItemError::UnknownLibrary {
      name: params.library.clone(),
    })?;
  // The path is an opaque index key here — no filesystem access, so no
  // traversal surface.
  let record = state
    .index
    .video(&params.library, params.path.trim_start_matches('/'))
    .await?
    .ok_or_else(|| ItemError::ItemNotFound {
      library: params.library.clone(),
      path: params.path.clone(),
    })?;
  let active = state.derivation_active.borrow().clone();
  Ok(Json(VideoItem::from_record(record, active.as_ref())))
}
