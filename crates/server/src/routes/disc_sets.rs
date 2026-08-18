use aide::transform::TransformOperation;
use axum::{
  extract::State,
  http::StatusCode,
  response::{IntoResponse, Response},
  Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::index::store::IndexError;
use crate::web_base::AppState;

/// The operator's "this is the main title" pick for a disc set.  The only
/// mutating endpoint; when OIDC is configured it sits behind the same login
/// as everything else.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetMainTitleRequest {
  pub library: String,
  /// The set's grouping key, as reported by browse entries.
  pub disc_set: String,
  /// The library-relative path of the title to treat as the set's main
  /// feature.
  pub path: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetMainTitleResponse {
  pub library: String,
  pub disc_set: String,
  pub main_path: String,
}

#[derive(Debug, Error)]
pub(crate) enum DiscSetError {
  #[error("Library '{name}' is not configured")]
  UnknownLibrary { name: String },

  #[error("Video '{path}' not found in library '{library}'")]
  ItemNotFound { library: String, path: String },

  #[error("Video '{path}' does not belong to disc set '{disc_set}'")]
  NotInSet { path: String, disc_set: String },

  #[error(transparent)]
  Index(#[from] IndexError),
}

impl aide::operation::OperationOutput for DiscSetError {
  type Inner = Self;
}

impl IntoResponse for DiscSetError {
  fn into_response(self) -> Response {
    let status = match &self {
      DiscSetError::UnknownLibrary { .. }
      | DiscSetError::ItemNotFound { .. } => StatusCode::NOT_FOUND,
      DiscSetError::NotInSet { .. } => StatusCode::BAD_REQUEST,
      DiscSetError::Index(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, self.to_string()).into_response()
  }
}

pub(crate) fn set_main_docs(op: TransformOperation) -> TransformOperation {
  op.description("Pick the main title of a disc set.")
    .response::<200, Json<SetMainTitleResponse>>()
    .response_with::<400, (), _>(|r| {
      r.description("The video does not belong to that disc set.")
    })
    .response_with::<404, (), _>(|r| r.description("Unknown library or video."))
    .response_with::<500, (), _>(|r| r.description("Index query failed."))
}

pub(crate) async fn set_main_handler(
  State(state): State<AppState>,
  Json(request): Json<SetMainTitleRequest>,
) -> Result<Json<SetMainTitleResponse>, DiscSetError> {
  state.library(&request.library).ok_or_else(|| {
    DiscSetError::UnknownLibrary {
      name: request.library.clone(),
    }
  })?;
  let record = state
    .index
    .video(&request.library, &request.path)
    .await?
    .ok_or_else(|| DiscSetError::ItemNotFound {
      library: request.library.clone(),
      path: request.path.clone(),
    })?;
  if record.disc_set.as_deref() != Some(request.disc_set.as_str()) {
    return Err(DiscSetError::NotInSet {
      path: request.path,
      disc_set: request.disc_set,
    });
  }
  state
    .index
    .set_main_title(&request.library, &request.disc_set, &request.path)
    .await?;
  Ok(Json(SetMainTitleResponse {
    library: request.library,
    disc_set: request.disc_set,
    main_path: request.path,
  }))
}
