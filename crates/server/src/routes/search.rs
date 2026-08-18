use aide::transform::TransformOperation;
use axum::{
  extract::{Query, State},
  http::StatusCode,
  response::{IntoResponse, Response},
  Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::index::store::{fts_query, IndexError};
use crate::routes::types::VideoItem;
use crate::web_base::AppState;

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchQuery {
  /// Free-text query; tokens match as prefixes across title, description,
  /// channel, and file name.
  pub q: String,
  /// Restrict results to one library; absent searches all libraries.
  pub library: Option<String>,
  /// Page size, capped at 200 (default 50).
  pub limit: Option<u32>,
  /// Offset into the ranked results (default 0).
  pub offset: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchPage {
  pub total: u64,
  pub limit: u32,
  pub offset: u32,
  pub items: Vec<VideoItem>,
}

#[derive(Debug, Error)]
pub(crate) enum SearchError {
  #[error("Library '{name}' is not configured")]
  UnknownLibrary { name: String },

  #[error(transparent)]
  Index(#[from] IndexError),
}

impl aide::operation::OperationOutput for SearchError {
  type Inner = Self;
}

impl IntoResponse for SearchError {
  fn into_response(self) -> Response {
    let status = match &self {
      SearchError::UnknownLibrary { .. } => StatusCode::NOT_FOUND,
      SearchError::Index(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, self.to_string()).into_response()
  }
}

pub(crate) fn search_docs(op: TransformOperation) -> TransformOperation {
  op.description("Full-text search across all libraries, ranked by relevance.")
    .response::<200, Json<SearchPage>>()
    .response_with::<404, (), _>(|r| r.description("Unknown library."))
    .response_with::<500, (), _>(|r| r.description("Index query failed."))
}

pub(crate) async fn handler(
  State(state): State<AppState>,
  Query(params): Query<SearchQuery>,
) -> Result<Json<SearchPage>, SearchError> {
  if let Some(name) = &params.library {
    state
      .library(name)
      .ok_or_else(|| SearchError::UnknownLibrary { name: name.clone() })?;
  }
  let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
  let offset = params.offset.unwrap_or(0);

  // A query with no searchable tokens is a valid request with zero results,
  // not an error — the match expression never sees raw user input.
  let Some(fts) = fts_query(&params.q) else {
    return Ok(Json(SearchPage {
      total: 0,
      limit,
      offset,
      items: Vec::new(),
    }));
  };

  let results = state
    .index
    .search(fts, params.library.clone(), limit, offset)
    .await?;
  let active = state.derivation_active.borrow().clone();
  Ok(Json(SearchPage {
    total: results.total,
    limit,
    offset,
    items: results
      .items
      .into_iter()
      .map(|record| VideoItem::from_record(record, active.as_ref()))
      .collect(),
  }))
}
