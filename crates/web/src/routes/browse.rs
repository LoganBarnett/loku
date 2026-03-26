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
    #[serde(skip_serializing_if = "Option::is_none")]
    compat_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    webpage_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    view_count: Option<u64>,
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
        // Skip companion compatibility copies (e.g. foo.compat.mp4).
        if stem.to_string_lossy().ends_with(".compat") {
          continue;
        }
        let parent = entry_path.parent().unwrap_or(Path::new(""));

        let thumb_path = find_thumbnail(parent, stem, &canonical_root);
        let info = read_info_json(parent, stem);
        let compat_path = find_compat(parent, stem, &canonical_root);

        videos.push(Entry::Video {
          name,
          path: rel_entry_path,
          thumb_path,
          title: info.title,
          duration_secs: info.duration_secs,
          upload_date: info.upload_date,
          compat_path,
          description: info.description,
          channel: info.channel,
          channel_url: info.channel_url,
          webpage_url: info.webpage_url,
          view_count: info.view_count,
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

fn sidecar_path(
  parent: &Path,
  stem: &OsStr,
  suffix: &str,
) -> std::path::PathBuf {
  // Append the suffix directly to the stem so that compound-extension names
  // like "foo.mov.webm" (stem "foo.mov") resolve to "foo.mov.webp" rather
  // than "foo.webp" as Path::with_extension would produce.
  let mut name = stem.to_os_string();
  name.push(suffix);
  parent.join(name)
}

fn find_compat(
  parent: &Path,
  stem: &OsStr,
  canonical_root: &Path,
) -> Option<String> {
  let p = sidecar_path(parent, stem, ".compat.mp4");
  if p.exists() {
    p.strip_prefix(canonical_root)
      .ok()
      .map(|r| r.to_string_lossy().to_string())
  } else {
    None
  }
}

fn find_thumbnail(
  parent: &Path,
  stem: &OsStr,
  canonical_root: &Path,
) -> Option<String> {
  THUMB_EXTENSIONS.iter().find_map(|ext| {
    let thumb = sidecar_path(parent, stem, &format!(".{ext}"));
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

struct InfoJson {
  title: Option<String>,
  duration_secs: Option<f64>,
  upload_date: Option<String>,
  description: Option<String>,
  channel: Option<String>,
  channel_url: Option<String>,
  webpage_url: Option<String>,
  view_count: Option<u64>,
}

fn read_info_json(parent: &Path, stem: &OsStr) -> InfoJson {
  let info_path = sidecar_path(parent, stem, ".info.json");

  let default = InfoJson {
    title: None,
    duration_secs: None,
    upload_date: None,
    description: None,
    channel: None,
    channel_url: None,
    webpage_url: None,
    view_count: None,
  };

  let Ok(contents) = fs::read_to_string(&info_path) else {
    return default;
  };

  let Ok(json) = serde_json::from_str::<Value>(&contents) else {
    return default;
  };

  let str_field =
    |key: &str| json.get(key).and_then(Value::as_str).map(str::to_string);

  InfoJson {
    title: str_field("title"),
    duration_secs: json.get("duration").and_then(Value::as_f64),
    upload_date: str_field("upload_date"),
    description: str_field("description"),
    channel: str_field("channel"),
    channel_url: str_field("channel_url"),
    webpage_url: str_field("webpage_url"),
    view_count: json.get("view_count").and_then(Value::as_u64),
  }
}
