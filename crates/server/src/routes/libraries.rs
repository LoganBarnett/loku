use aide::transform::TransformOperation;
use axum::{extract::State, Json};
use schemars::JsonSchema;
use serde::Serialize;

use crate::library::LibraryKind;
use crate::web_base::AppState;

/// A configured library as exposed to clients: the name keys browse, search,
/// and `/files/{name}` URLs; the kind lets the UI present each dataset
/// appropriately.  Paths are server-internal and deliberately not exposed.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LibraryInfo {
  pub name: String,
  pub kind: LibraryKind,
}

pub(crate) fn libraries_docs(op: TransformOperation) -> TransformOperation {
  op.description("List the configured library roots.")
    .response::<200, Json<Vec<LibraryInfo>>>()
}

pub(crate) async fn handler(
  State(state): State<AppState>,
) -> Json<Vec<LibraryInfo>> {
  Json(
    state
      .libraries
      .iter()
      .map(|l| LibraryInfo {
        name: l.name.clone(),
        kind: l.kind,
      })
      .collect(),
  )
}
