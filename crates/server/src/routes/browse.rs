//! The browse API: one directory listing per request, served from the media
//! index.
//!
//! Listings are index-backed, so entries carry the full metadata the scanner
//! resolved (NFO titles, probed codecs, derivation status) and paths act as
//! opaque database keys — traversal cannot reach anything the scanner did
//! not index.  The only filesystem touch is an existence check that keeps
//! the 404-versus-empty distinction (an index of video-bearing directories
//! cannot tell those apart) and preserves the traversal guard on the
//! query's path parameter.

use aide::transform::TransformOperation;
use axum::{
  extract::{Query, State},
  http::StatusCode,
  response::{IntoResponse, Response},
  Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use tap::Tap;
use thiserror::Error;

use crate::index::store::{BrowseData, IndexError, VideoRecord};
use crate::library::{Library, LibraryKind};
use crate::media::worker::ActiveDerivation;
use crate::routes::types::VideoItem;
use crate::web_base::AppState;
use loku_lib::disc;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowseQuery {
  /// Which library to browse; defaults to the first configured library so
  /// single-library setups need no parameter.
  pub library: Option<String>,
  #[serde(default)]
  pub path: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DirListing {
  /// The library the listing came from — the resolved name, so clients that
  /// omitted the parameter learn which library the default landed on.
  pub library: String,
  pub path: String,
  pub entries: Vec<Entry>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DirEntry {
  pub name: String,
  pub path: String,
}

/// A multi-title disc rip presented as one item: the presumed main title
/// plus every title on the disc.  Singleton rips stay plain videos.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DiscSetEntry {
  /// The set's grouping key (the shared filename prefix), used when posting
  /// a main-title override.
  pub disc_set: String,
  /// Cleaned-up display title for the set as a whole.
  pub display_title: String,
  pub main: VideoItem,
  /// Every title in the set, main included, in disc order.
  pub titles: Vec<VideoItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
  Directory(DirEntry),
  Video(Box<VideoItem>),
  DiscSet(Box<DiscSetEntry>),
}

impl Entry {
  /// The name listings sort by, directory-first grouping aside.
  fn sort_name(&self) -> &str {
    match self {
      Entry::Directory(dir) => &dir.name,
      Entry::Video(video) => &video.name,
      Entry::DiscSet(set) => &set.display_title,
    }
  }
}

#[derive(Debug, Error)]
pub(crate) enum BrowseError {
  #[error("Library '{name}' is not configured")]
  UnknownLibrary { name: String },

  #[error("Path traversal attempt: '{path}' escapes the library root")]
  PathTraversal { path: String },

  #[error("Directory '{path}' not found in library")]
  DirectoryNotFound { path: String },

  #[error(transparent)]
  Index(#[from] IndexError),
}

impl aide::operation::OperationOutput for BrowseError {
  type Inner = Self;
}

impl IntoResponse for BrowseError {
  fn into_response(self) -> Response {
    let status = match &self {
      BrowseError::PathTraversal { .. } => StatusCode::BAD_REQUEST,
      BrowseError::UnknownLibrary { .. }
      | BrowseError::DirectoryNotFound { .. } => StatusCode::NOT_FOUND,
      BrowseError::Index(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, self.to_string()).into_response()
  }
}

pub(crate) fn browse_docs(op: TransformOperation) -> TransformOperation {
  op.description(
    "List videos, disc sets, and subdirectories under a library path.",
  )
  .response::<200, Json<DirListing>>()
  .response_with::<400, (), _>(|r| r.description("Path traversal attempt."))
  .response_with::<404, (), _>(|r| {
    r.description("Unknown library or directory not found.")
  })
  .response_with::<500, (), _>(|r| r.description("Index query failed."))
}

pub(crate) async fn handler(
  State(state): State<AppState>,
  Query(params): Query<BrowseQuery>,
) -> Result<Json<DirListing>, BrowseError> {
  let library = params
    .library
    .as_deref()
    .map_or_else(|| state.libraries.first(), |name| state.library(name))
    .ok_or_else(|| BrowseError::UnknownLibrary {
      name: params.library.clone().unwrap_or_default(),
    })?;

  let rel_path = params.path.trim_start_matches('/');
  ensure_directory_exists(library, rel_path, &params.path)?;

  let data = state
    .index
    .browse_directory(&library.name, rel_path)
    .await?;
  let active = state.derivation_active.borrow().clone();
  let entries = assemble_entries(library, rel_path, data, active.as_ref());

  Ok(Json(DirListing {
    library: library.name.clone(),
    path: params.path,
    entries,
  }))
}

/// The one filesystem touch: reject paths that escape the library root and
/// distinguish an empty-but-real directory (200 with no entries) from a
/// nonexistent one (404).  Listing content itself comes from the index.
fn ensure_directory_exists(
  library: &Library,
  rel_path: &str,
  requested: &str,
) -> Result<(), BrowseError> {
  let canonical_root = library.path.canonicalize().map_err(|_| {
    BrowseError::DirectoryNotFound {
      path: requested.to_string(),
    }
  })?;
  let canonical_target =
    library.path.join(rel_path).canonicalize().map_err(|_| {
      BrowseError::DirectoryNotFound {
        path: requested.to_string(),
      }
    })?;
  if !canonical_target.starts_with(&canonical_root) {
    return Err(BrowseError::PathTraversal {
      path: requested.to_string(),
    });
  }
  if !canonical_target.is_dir() {
    return Err(BrowseError::DirectoryNotFound {
      path: requested.to_string(),
    });
  }
  Ok(())
}

/// Assemble the listing: subdirectories first (alphabetical), then videos
/// and disc sets sorted together by display name.  Multi-title disc sets in
/// discs-kind libraries collapse into one entry each; singleton "sets" are
/// just videos, since a one-item card would be noise.
fn assemble_entries(
  library: &Library,
  rel_path: &str,
  data: BrowseData,
  active: Option<&ActiveDerivation>,
) -> Vec<Entry> {
  let (sets, singles) = disc_set_groups(library.kind, data.videos);
  let (multi_sets, singleton_sets): (BTreeMap<_, _>, BTreeMap<_, _>) =
    sets.into_iter().partition(|(_, records)| records.len() > 1);

  let items = multi_sets
    .into_iter()
    .filter_map(|(set_name, records)| {
      disc_set_entry(set_name, records, &data.overrides, active)
    })
    .chain(
      singles
        .into_iter()
        .chain(singleton_sets.into_values().flatten())
        .map(|record| {
          Entry::Video(Box::new(VideoItem::from_record(record, active)))
        }),
    )
    .collect::<Vec<_>>()
    .tap_mut(|items| items.sort_by(|a, b| a.sort_name().cmp(b.sort_name())));

  directory_entries(rel_path, data.subdirectories)
    .chain(items)
    .collect()
}

/// Subdirectory entries with their paths joined onto the browse path.
fn directory_entries(
  rel_path: &str,
  subdirectories: Vec<String>,
) -> impl Iterator<Item = Entry> + '_ {
  subdirectories.into_iter().map(move |name| {
    Entry::Directory(DirEntry {
      path: child_rel_path(rel_path, &name),
      name,
    })
  })
}

/// The library-relative path of a child name under a directory path, where
/// the empty string is the library root.
fn child_rel_path(rel_path: &str, name: &str) -> String {
  if rel_path.is_empty() {
    name.to_string()
  } else {
    format!("{rel_path}/{name}")
  }
}

/// Split a directory's videos into disc-set groups and standalone videos.
/// Only discs-kind libraries group; everything else browses flat.
fn disc_set_groups(
  kind: LibraryKind,
  videos: Vec<VideoRecord>,
) -> (BTreeMap<String, Vec<VideoRecord>>, Vec<VideoRecord>) {
  videos.into_iter().fold(
    (BTreeMap::new(), Vec::new()),
    |(mut sets, mut singles), record| {
      match (kind, record.disc_set.clone()) {
        (LibraryKind::Discs, Some(set)) => {
          sets.entry(set).or_default().push(record);
        }
        _ => singles.push(record),
      }
      (sets, singles)
    },
  )
}

/// Collapse one multi-title disc set into its entry.  The main title is the
/// operator's override when present, else the largest file — the standing
/// heuristic for "the actual movie" on a disc.
fn disc_set_entry(
  set_name: String,
  records: Vec<VideoRecord>,
  overrides: &HashMap<String, String>,
  active: Option<&ActiveDerivation>,
) -> Option<Entry> {
  let main_rel = overrides
    .get(&set_name)
    .cloned()
    .or_else(|| {
      records
        .iter()
        .max_by_key(|record| record.size)
        .map(|record| record.rel_path.clone())
    })
    .unwrap_or_default();
  let titles: Vec<VideoItem> = records
    .tap_mut(|records| {
      records.sort_by_key(|r| (r.disc_title_index, r.rel_path.clone()));
    })
    .into_iter()
    .map(|record| VideoItem::from_record(record, active))
    .collect();
  // A stale override may name a vanished file; the first title stands in.
  // Grouping never yields an empty set, but `first` keeps this total (and
  // the return `Option`al) rather than asserting that at a distance.
  titles
    .iter()
    .find(|item| item.path == main_rel)
    .or_else(|| titles.first())
    .cloned()
    .map(|main| {
      Entry::DiscSet(Box::new(DiscSetEntry {
        display_title: main
          .title
          .clone()
          .unwrap_or_else(|| disc::display_title(&set_name)),
        disc_set: set_name,
        main,
        titles,
      }))
    })
}
