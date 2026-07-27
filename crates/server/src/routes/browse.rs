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
use tracing::warn;

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

// The `Video` variant carries far more than `Directory`, so
// `large_enum_variant` fires.  Allowed here (not workspace-wide) because the
// size is acceptable for now; factoring `Video` into its own type would shrink
// the enum and give callers that only handle videos a dedicated type.  See
// tasks.org.
#[allow(clippy::large_enum_variant)]
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
    // The exact `<source type>` string for the native file — a web-standard
    // container MIME with an RFC 6381 codecs parameter (e.g. `video/mp4;
    // codecs="av01.0.05M.08, opus"`).  It lets the browser's canPlayType decide
    // authoritatively whether it can play the native file, so an incapable
    // browser skips it without downloading rather than committing to a file it
    // cannot decode.  `None` when the container is non-standard or the codecs
    // are unknown, in which case the player serves only the compat copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    native_type: Option<String>,
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

  #[error("Failed to canonicalize library root '{path}': {source}")]
  LibraryRootCanonicalize {
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
      BrowseError::LibraryDirectoryRead { .. }
      | BrowseError::LibraryRootCanonicalize { .. } => {
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
  let canonical_root = library_root.canonicalize().map_err(|source| {
    BrowseError::LibraryRootCanonicalize {
      path: library_root.to_string_lossy().to_string(),
      source,
    }
  })?;

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
        let native_type = native_source_type(
          &ext,
          info.vcodec.as_deref(),
          info.acodec.as_deref(),
        );

        videos.push(Entry::Video {
          name,
          path: rel_entry_path,
          thumb_path,
          title: info.title,
          duration_secs: info.duration_secs,
          upload_date: info.upload_date,
          compat_path,
          native_type,
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

/// Build the `<source type>` string the player should advertise for the native
/// file, or `None` when the native should not be offered as a typed source.
///
/// The value is a web-standard container MIME plus an RFC 6381 codecs
/// parameter, which is what lets a browser's `canPlayType` decide up front
/// whether it can play the file.  A container MIME alone cannot: `video/mp4`
/// says nothing about whether the bytes inside are H.264 (which Safari
/// hardware-decodes) or AV1 (which most Safari builds cannot decode at all), so
/// Safari would commit to the source and download it before failing.  Naming
/// the codec makes the rejection happen before any bytes move.
///
/// Non-standard containers (MKV, AVI, MOV) have no MIME `canPlayType` accepts,
/// and an unrecognized codec cannot be named honestly, so both return `None` —
/// the player then serves only the compat copy rather than risk a wrong hint.
fn native_source_type(
  ext: &str,
  vcodec: Option<&str>,
  acodec: Option<&str>,
) -> Option<String> {
  let container = match ext {
    "mp4" => "video/mp4",
    "webm" => "video/webm",
    _ => return None,
  };
  // The video codec is the discriminating signal, so a missing or unrecognized
  // one drops the native source entirely rather than emitting a bare container
  // type that reintroduces the download-then-fail behavior.
  let video = video_codec_token(vcodec?)?;
  let codecs = match acodec.and_then(audio_codec_token) {
    Some(audio) => format!("{video}, {audio}"),
    None => video,
  };
  Some(format!("{container}; codecs=\"{codecs}\""))
}

/// Map a yt-dlp `vcodec` value to an RFC 6381 codecs token.
///
/// Modern yt-dlp already emits the full form (`avc1.640028`, `av01.0.05M.08`,
/// `vp09.00.10.08`); that carries the exact profile and level, so pass it
/// through unchanged.  Bare names are the rare fallback: map them to a value
/// the browser accepts, honest about *which* codec even if not its exact
/// profile — the profile in a `type` hint only gates selection, and the browser
/// decodes whatever the file actually contains once selected.
fn video_codec_token(vcodec: &str) -> Option<String> {
  if vcodec.contains('.') {
    Some(vcodec.to_string())
  } else {
    match vcodec {
      // Bare vp8/vp9 name no profile, which is safest: the browser plays any
      // profile the file contains rather than being told a specific one.
      "vp8" => Some("vp8".to_string()),
      "vp9" => Some("vp9".to_string()),
      // A bare "av1" is not a valid token, so synthesize a representative Main
      // profile string; AV1-incapable browsers reject any av01.* form anyway.
      "av1" => Some("av01.0.05M.08".to_string()),
      "h264" | "avc" | "avc1" => Some("avc1.42E01E".to_string()),
      "h265" | "hevc" | "hvc1" | "hev1" => Some("hvc1.1.6.L93.B0".to_string()),
      _ => None,
    }
  }
}

/// Map a yt-dlp `acodec` value to an RFC 6381 codecs token, or `None` to omit
/// the audio token (a video-only codecs string is still valid).  Omitting an
/// unknown audio codec is safe: the video codec already drives the browser's
/// accept/reject decision for the container.
fn audio_codec_token(acodec: &str) -> Option<String> {
  if acodec.contains('.') {
    Some(acodec.to_string())
  } else {
    match acodec {
      "aac" => Some("mp4a.40.2".to_string()),
      "opus" => Some("opus".to_string()),
      "vorbis" => Some("vorbis".to_string()),
      _ => None,
    }
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
  vcodec: Option<String>,
  acodec: Option<String>,
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
    vcodec: None,
    acodec: None,
  };

  let contents = match fs::read_to_string(&info_path) {
    Ok(c) => c,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return default,
    Err(e) => {
      warn!(path = %info_path.display(), error = %e, "Failed to read info.json sidecar");
      return default;
    }
  };

  let json = match serde_json::from_str::<Value>(&contents) {
    Ok(v) => v,
    Err(e) => {
      warn!(path = %info_path.display(), error = %e, "Failed to parse info.json sidecar");
      return default;
    }
  };

  let str_field =
    |key: &str| json.get(key).and_then(Value::as_str).map(str::to_string);

  // yt-dlp writes the literal string "none" for an absent video or audio
  // track (e.g. a video-only or audio-only format), so treat that as absent
  // rather than a codec name.
  let codec_field = |key: &str| {
    json
      .get(key)
      .and_then(Value::as_str)
      .filter(|v| *v != "none")
      .map(str::to_string)
  };

  InfoJson {
    title: str_field("title"),
    duration_secs: json.get("duration").and_then(Value::as_f64),
    upload_date: str_field("upload_date"),
    description: str_field("description"),
    channel: str_field("channel"),
    channel_url: str_field("channel_url"),
    webpage_url: str_field("webpage_url"),
    view_count: json.get("view_count").and_then(Value::as_u64),
    vcodec: codec_field("vcodec"),
    acodec: codec_field("acodec"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn native_type_passes_through_full_rfc6381_codecs() {
    // Modern yt-dlp emits the exact profile/level string; it must survive
    // verbatim so canPlayType matches the real file.
    assert_eq!(
      native_source_type("mp4", Some("avc1.640028"), Some("mp4a.40.2")),
      Some(r#"video/mp4; codecs="avc1.640028, mp4a.40.2""#.to_string()),
    );
    assert_eq!(
      native_source_type("mp4", Some("av01.0.05M.08"), Some("opus")),
      Some(r#"video/mp4; codecs="av01.0.05M.08, opus""#.to_string()),
    );
    assert_eq!(
      native_source_type("webm", Some("vp09.00.10.08"), Some("opus")),
      Some(r#"video/webm; codecs="vp09.00.10.08, opus""#.to_string()),
    );
  }

  #[test]
  fn native_type_maps_bare_codec_names() {
    assert_eq!(
      native_source_type("webm", Some("vp9"), Some("opus")),
      Some(r#"video/webm; codecs="vp9, opus""#.to_string()),
    );
    assert_eq!(
      native_source_type("mp4", Some("h264"), Some("aac")),
      Some(r#"video/mp4; codecs="avc1.42E01E, mp4a.40.2""#.to_string()),
    );
  }

  #[test]
  fn native_type_omits_unknown_audio_but_keeps_video() {
    // An unrecognized audio codec should not sink the whole hint; the video
    // codec alone still lets the browser decide on the container.
    assert_eq!(
      native_source_type("mp4", Some("avc1.640028"), Some("flac")),
      Some(r#"video/mp4; codecs="avc1.640028""#.to_string()),
    );
  }

  #[test]
  fn native_type_absent_for_non_standard_container() {
    // MKV/AVI/MOV have no MIME canPlayType accepts, so no native source is
    // offered regardless of the codecs inside.
    assert_eq!(
      native_source_type("mkv", Some("avc1.640028"), Some("aac")),
      None
    );
    assert_eq!(
      native_source_type("mov", Some("avc1.640028"), Some("aac")),
      None
    );
    assert_eq!(
      native_source_type("avi", Some("avc1.640028"), Some("aac")),
      None
    );
  }

  #[test]
  fn native_type_absent_when_video_codec_unknown_or_missing() {
    // Without a nameable video codec the player must fall back to the compat
    // copy rather than emit a bare container type.
    assert_eq!(native_source_type("mp4", None, Some("aac")), None);
    assert_eq!(native_source_type("mp4", Some("theora"), Some("aac")), None);
  }
}
