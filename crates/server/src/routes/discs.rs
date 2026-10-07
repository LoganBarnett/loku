//! The disc identity API: which discs still need a human, a fresh search
//! for the operator's own query, and the confirmation that commits a pick.

use aide::transform::TransformOperation;
use axum::{
  extract::{Path, State},
  http::StatusCode,
  response::{IntoResponse, Response},
  Json,
};
use futures_util::stream::{self, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tracing::warn;

use crate::identity::naming::{self, TitleHint};
use crate::identity::resolver::{self, CommitError, ResolveError, Resolver};
use crate::identity::tmdb::{image_url, Candidate, ImageSize, TmdbError};
use crate::identity::{IdentitySource, IdentityState, TmdbKind};
use crate::index::store::{DiscRecord, IndexError};
use crate::library::Library;
use crate::rip::manifest;
use crate::web_base::AppState;

#[derive(Debug, Serialize, JsonSchema)]
pub struct UnresolvedDiscs {
  /// Whether a TMDB key is configured; without one nothing resolves and
  /// the identify endpoints refuse.
  pub identity_enabled: bool,
  pub discs: Vec<UnresolvedDisc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UnresolvedDisc {
  pub library: String,
  pub path: String,
  pub label: String,
  pub disc_id: String,
  pub identity_state: IdentityState,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
  /// The search query the disc's own naming suggests (may be empty).
  pub query: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub year_hint: Option<i32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub kind_hint: Option<TmdbKind>,
  /// Whether the structural fingerprint carries enough titles to trust a
  /// cache match on its own.
  pub fingerprint_strong: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub attempted_at: Option<i64>,
  pub candidates: Vec<CandidateItem>,
}

/// A candidate with its poster resolved to a URL the browser can load.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CandidateItem {
  #[serde(flatten)]
  pub candidate: Candidate,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub poster_url: Option<String>,
}

impl From<Candidate> for CandidateItem {
  fn from(candidate: Candidate) -> Self {
    let poster_url = candidate
      .poster_path
      .as_deref()
      .map(|path| image_url(ImageSize::W185, path));
    CandidateItem {
      candidate,
      poster_url,
    }
  }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdentifyRequest {
  pub path: String,
  /// Defaults to the disc's own title hint.
  pub query: Option<String>,
  pub year: Option<i32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct IdentifyResponse {
  pub library: String,
  pub path: String,
  pub query: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub year: Option<i32>,
  pub candidates: Vec<CandidateItem>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetIdentityRequest {
  pub path: String,
  pub tmdb_kind: TmdbKind,
  pub tmdb_id: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetIdentityResponse {
  pub library: String,
  /// Where the disc lives now (renamed to its resolved title).
  pub path: String,
  pub previous_path: String,
  pub title: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub year: Option<i32>,
  pub renamed: bool,
  #[serde(skip_serializing_if = "Vec::is_empty", default)]
  pub artwork_warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub(crate) enum DiscIdentityError {
  #[error("Library '{name}' is not configured")]
  UnknownLibrary { name: String },

  #[error("Disc '{path}' not found in library '{library}'")]
  DiscNotFound { library: String, path: String },

  #[error("TMDB API key not configured; identity resolution is disabled")]
  IdentityDisabled,

  #[error("The search query is empty")]
  EmptyQuery,

  #[error(transparent)]
  Tmdb(#[from] TmdbError),

  #[error(transparent)]
  Resolve(#[from] ResolveError),

  #[error(transparent)]
  Commit(#[from] CommitError),

  #[error(transparent)]
  Index(#[from] IndexError),
}

impl aide::operation::OperationOutput for DiscIdentityError {
  type Inner = Self;
}

impl IntoResponse for DiscIdentityError {
  fn into_response(self) -> Response {
    let status = match &self {
      DiscIdentityError::UnknownLibrary { .. }
      | DiscIdentityError::DiscNotFound { .. } => StatusCode::NOT_FOUND,
      DiscIdentityError::IdentityDisabled => StatusCode::SERVICE_UNAVAILABLE,
      DiscIdentityError::EmptyQuery => StatusCode::BAD_REQUEST,
      DiscIdentityError::Tmdb(_)
      | DiscIdentityError::Resolve(ResolveError::Tmdb(_))
      | DiscIdentityError::Commit(CommitError::Tmdb(_)) => {
        StatusCode::BAD_GATEWAY
      }
      DiscIdentityError::Resolve(_)
      | DiscIdentityError::Commit(_)
      | DiscIdentityError::Index(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, self.to_string()).into_response()
  }
}

pub(crate) fn unresolved_docs(op: TransformOperation) -> TransformOperation {
  op.description(
    "Discs whose identity is unresolved, awaiting confirmation, or failed, \
     with the candidates on offer.",
  )
  .response::<200, Json<UnresolvedDiscs>>()
  .response_with::<500, (), _>(|r| r.description("Index query failed."))
}

pub(crate) async fn unresolved_handler(
  State(state): State<AppState>,
) -> Result<Json<UnresolvedDiscs>, DiscIdentityError> {
  let discs = stream::iter(state.index.unresolved_discs().await?)
    .then(|disc| async {
      let hint = match state.library(&disc.library) {
        Some(library) => hint_for(library, &disc).await,
        None => None,
      };
      unresolved_item(disc, hint)
    })
    .collect::<Vec<_>>()
    .await;
  Ok(Json(UnresolvedDiscs {
    identity_enabled: state.identity.is_some(),
    discs,
  }))
}

fn unresolved_item(
  disc: DiscRecord,
  hint: Option<TitleHint>,
) -> UnresolvedDisc {
  UnresolvedDisc {
    library: disc.library.clone(),
    path: disc.rel_path.clone(),
    label: disc.label.clone(),
    disc_id: disc.disc_id.clone(),
    identity_state: disc.identity_state,
    error: disc.identity_error.clone(),
    query: hint.as_ref().map(|h| h.query.clone()).unwrap_or_default(),
    year_hint: hint.as_ref().and_then(|h| h.year),
    kind_hint: hint.as_ref().and_then(|h| h.kind_hint),
    fingerprint_strong: disc.fingerprint_titles
      >= u32::try_from(crate::identity::fingerprint::STRONG_MIN_TITLES)
        .unwrap_or(u32::MAX),
    attempted_at: disc.identity_attempted_at,
    candidates: disc
      .candidates()
      .into_iter()
      .map(CandidateItem::from)
      .collect(),
  }
}

/// The disc's title hint from its manifest, falling back to its label.  A
/// manifest that cannot be read is reported and leaves only the label.
async fn hint_for(library: &Library, disc: &DiscRecord) -> Option<TitleHint> {
  manifest::read_manifest(&library.path.join(&disc.rel_path))
    .await
    .unwrap_or_else(|error| {
      warn!(
        path = %disc.rel_path,
        %error,
        "Failed to read the disc manifest for its title hint"
      );
      None
    })
    .map_or_else(
      || naming::title_hint(None, None, Some(&disc.label)),
      |manifest| resolver::hint_of(&manifest, &disc.label),
    )
}

pub(crate) fn identify_docs(op: TransformOperation) -> TransformOperation {
  op.description(
    "Search TMDB for a disc under a query of the operator's choosing; \
     nothing is committed.",
  )
  .response::<200, Json<IdentifyResponse>>()
  .response_with::<400, (), _>(|r| r.description("Empty query."))
  .response_with::<404, (), _>(|r| r.description("Unknown library or disc."))
  .response_with::<502, (), _>(|r| r.description("TMDB request failed."))
  .response_with::<503, (), _>(|r| {
    r.description("No TMDB API key is configured.")
  })
}

pub(crate) async fn identify_handler(
  State(state): State<AppState>,
  Path(library_name): Path<String>,
  Json(request): Json<IdentifyRequest>,
) -> Result<Json<IdentifyResponse>, DiscIdentityError> {
  let (library, disc, resolver) =
    locate(&state, &library_name, &request.path).await?;
  let query = match request.query.map(|q| q.trim().to_string()) {
    Some(query) if !query.is_empty() => query,
    _ => hint_for(&library, &disc)
      .await
      .map(|hint| hint.query)
      .unwrap_or_default(),
  };
  if query.is_empty() {
    return Err(DiscIdentityError::EmptyQuery);
  }
  let candidates = resolver.search(&query, request.year).await?;
  Ok(Json(IdentifyResponse {
    library: library.name,
    path: disc.rel_path,
    query,
    year: request.year,
    candidates: candidates.into_iter().map(CandidateItem::from).collect(),
  }))
}

pub(crate) fn identity_docs(op: TransformOperation) -> TransformOperation {
  op.description(
    "Commit a TMDB identity for a disc: write its NFO and artwork, rename \
     its directory to the resolved title, and remember the pressing.",
  )
  .response::<200, Json<SetIdentityResponse>>()
  .response_with::<404, (), _>(|r| r.description("Unknown library or disc."))
  .response_with::<500, (), _>(|r| {
    r.description("Writing the sidecars or updating the index failed.")
  })
  .response_with::<502, (), _>(|r| r.description("TMDB request failed."))
  .response_with::<503, (), _>(|r| {
    r.description("No TMDB API key is configured.")
  })
}

pub(crate) async fn identity_handler(
  State(state): State<AppState>,
  Path(library_name): Path<String>,
  Json(request): Json<SetIdentityRequest>,
) -> Result<Json<SetIdentityResponse>, DiscIdentityError> {
  let (library, disc, resolver) =
    locate(&state, &library_name, &request.path).await?;
  let ctx = resolver.context(&library, &disc).await?;
  let committed = resolver
    .commit(
      &state.index,
      &ctx,
      request.tmdb_kind,
      request.tmdb_id,
      IdentitySource::Attended,
    )
    .await?;
  Ok(Json(SetIdentityResponse {
    library: library.name,
    path: committed.rel_path,
    previous_path: disc.rel_path,
    title: committed.identity.title,
    year: committed.identity.year,
    renamed: committed.renamed,
    artwork_warnings: committed.artwork_warnings,
  }))
}

/// The library, the disc row, and the resolver — or the error explaining
/// which one is missing.
async fn locate(
  state: &AppState,
  library_name: &str,
  path: &str,
) -> Result<(Library, DiscRecord, Arc<Resolver>), DiscIdentityError> {
  let library = state.library(library_name).cloned().ok_or_else(|| {
    DiscIdentityError::UnknownLibrary {
      name: library_name.to_string(),
    }
  })?;
  let disc = state.index.disc(library_name, path).await?.ok_or_else(|| {
    DiscIdentityError::DiscNotFound {
      library: library_name.to_string(),
      path: path.to_string(),
    }
  })?;
  let resolver = state
    .identity
    .clone()
    .ok_or(DiscIdentityError::IdentityDisabled)?;
  Ok((library, disc, resolver))
}
