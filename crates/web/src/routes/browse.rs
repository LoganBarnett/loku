use aide::transform::TransformOperation;
use axum::{
  extract::{Query, State},
  http::StatusCode,
  response::{IntoResponse, Response},
  Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{ffi::OsStr, fs, path::Path};
use thiserror::Error;

use crate::web_base::AppState;

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "avi", "mov"];
const THUMB_EXTENSIONS: &[&str] = &["jpg", "webp", "png"];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowseQuery {
  #[serde(default)]
  pub path: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DirListing {
  pub path: String,
  pub entries: Vec<Entry>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Entry {
  Directory {
    name: String,
    path: String,
  },
  Video {
    name: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thumb_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upload_date: Option<String>,
  },
}

#[derive(Debug, Error)]
pub(crate) enum BrowseError {
  #[error("Path traversal attempt: '{path}' escapes the library root")]
  PathTraversal { path: String },

  #[error("Directory '{path}' not found in library")]
  DirectoryNotFound { path: String },

  #[error("Failed to read library directory '{path}': {source}")]
  LibraryDirectoryRead {
    path: String,
    source: std::io::Error,
  },
}

impl aide::operation::OperationOutput for BrowseError {
  type Inner = Self;
}

impl IntoResponse for BrowseError {
  fn into_response(self) -> Response {
    let status = match &self {
      BrowseError::PathTraversal { .. } => StatusCode::BAD_REQUEST,
      BrowseError::DirectoryNotFound { .. } => StatusCode::NOT_FOUND,
      BrowseError::LibraryDirectoryRead { .. } => {
        StatusCode::INTERNAL_SERVER_ERROR
      }
    };
    (status, self.to_string()).into_response()
  }
}

pub(crate) fn browse_docs(op: TransformOperation) -> TransformOperation {
  op.description("List videos and subdirectories under a library path.")
    .response::<200, Json<DirListing>>()
    .response_with::<400, (), _>(|r| r.description("Path traversal attempt."))
    .response_with::<404, (), _>(|r| r.description("Directory not found."))
    .response_with::<500, (), _>(|r| r.description("Failed to read directory."))
}

pub(crate) async fn handler(
  State(state): State<AppState>,
  Query(params): Query<BrowseQuery>,
) -> Result<Json<DirListing>, BrowseError> {
  let library_root = &state.library_path;

  // Canonicalize the library root so that prefix checks work correctly even
  // when it contains symlinks or relative components.
  let canonical_root = library_root
    .canonicalize()
    .unwrap_or_else(|_| library_root.clone());

  // Strip any leading slash so that joining works regardless of input form.
  let rel_path = params.path.trim_start_matches('/');
  let target = library_root.join(rel_path);

  let canonical_target =
    target
      .canonicalize()
      .map_err(|_| BrowseError::DirectoryNotFound {
        path: params.path.clone(),
      })?;

  if !canonical_target.starts_with(&canonical_root) {
    return Err(BrowseError::PathTraversal {
      path: params.path.clone(),
    });
  }

  let read_dir = fs::read_dir(&canonical_target).map_err(|source| {
    BrowseError::LibraryDirectoryRead {
      path: params.path.clone(),
      source,
    }
  })?;

  let mut dirs: Vec<Entry> = Vec::new();
  let mut videos: Vec<Entry> = Vec::new();

  for entry_result in read_dir {
    let Ok(entry) = entry_result else { continue };
    let Ok(file_type) = entry.file_type() else {
      continue;
    };
    let entry_path = entry.path();
    let name = entry.file_name().to_string_lossy().to_string();

    let rel_entry_path = entry_path
      .strip_prefix(&canonical_root)
      .unwrap_or(&entry_path)
      .to_string_lossy()
      .to_string();

    if file_type.is_dir() {
      dirs.push(Entry::Directory {
        name,
        path: rel_entry_path,
      });
    } else if file_type.is_file() {
      let ext = entry_path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_lowercase();

      if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        let stem = entry_path.file_stem().unwrap_or_default();
        let parent = entry_path.parent().unwrap_or(Path::new(""));

        let thumb_path = find_thumbnail(parent, stem, &canonical_root);
        let (title, duration_secs, upload_date) = read_info_json(parent, stem);

        videos.push(Entry::Video {
          name,
          path: rel_entry_path,
          thumb_path,
          title,
          duration_secs,
          upload_date,
        });
      }
    }
  }

  dirs.sort_by(|a, b| {
    if let (
      Entry::Directory { name: na, .. },
      Entry::Directory { name: nb, .. },
    ) = (a, b)
    {
      na.cmp(nb)
    } else {
      std::cmp::Ordering::Equal
    }
  });

  videos.sort_by(|a, b| {
    if let (Entry::Video { name: na, .. }, Entry::Video { name: nb, .. }) =
      (a, b)
    {
      na.cmp(nb)
    } else {
      std::cmp::Ordering::Equal
    }
  });

  let mut entries = dirs;
  entries.extend(videos);

  Ok(Json(DirListing {
    path: params.path,
    entries,
  }))
}

fn find_thumbnail(
  parent: &Path,
  stem: &OsStr,
  canonical_root: &Path,
) -> Option<String> {
  THUMB_EXTENSIONS.iter().find_map(|ext| {
    let thumb = parent.join(stem).with_extension(ext);
    if thumb.exists() {
      thumb
        .strip_prefix(canonical_root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
    } else {
      None
    }
  })
}

fn read_info_json(
  parent: &Path,
  stem: &OsStr,
) -> (Option<String>, Option<f64>, Option<String>) {
  let info_path = parent.join(stem).with_extension("info.json");

  let Ok(contents) = fs::read_to_string(&info_path) else {
    return (None, None, None);
  };

  let Ok(json) = serde_json::from_str::<Value>(&contents) else {
    return (None, None, None);
  };

  let title = json
    .get("title")
    .and_then(Value::as_str)
    .map(str::to_string);
  let duration_secs = json.get("duration").and_then(Value::as_f64);
  let upload_date = json
    .get("upload_date")
    .and_then(Value::as_str)
    .map(str::to_string);

  (title, duration_secs, upload_date)
}
