//! Sidecar discovery beside master video files: thumbnails, compat copies,
//! and yt-dlp `info.json` metadata.  Shared by the browse route (live
//! listings) and the index scanner.

use serde_json::Value;
use std::{ffi::OsStr, fs, path::Path};
use tracing::warn;

pub(crate) const VIDEO_EXTENSIONS: &[&str] =
  &["mp4", "mkv", "webm", "avi", "mov"];
pub(crate) const THUMB_EXTENSIONS: &[&str] = &["jpg", "webp", "png"];

pub(crate) fn sidecar_path(
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

pub(crate) fn find_compat(
  parent: &Path,
  stem: &OsStr,
  canonical_root: &Path,
) -> Option<String> {
  let p = sidecar_path(parent, stem, ".compat.mp4");
  if p.exists() {
    // The .ok() discard is deliberate: strip_prefix failing means the
    // sidecar sits outside the library root (a symlinked escape), and a
    // sidecar Loku cannot serve is the same as no sidecar.
    p.strip_prefix(canonical_root)
      .ok()
      .map(|r| r.to_string_lossy().to_string())
  } else {
    None
  }
}

pub(crate) fn find_thumbnail(
  parent: &Path,
  stem: &OsStr,
  canonical_root: &Path,
) -> Option<String> {
  THUMB_EXTENSIONS.iter().find_map(|ext| {
    let thumb = sidecar_path(parent, stem, &format!(".{ext}"));
    if thumb.exists() {
      // The .ok() discard is deliberate: strip_prefix failing means the
      // sidecar sits outside the library root (a symlinked escape), and a
      // sidecar Loku cannot serve is the same as no sidecar.
      thumb
        .strip_prefix(canonical_root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
    } else {
      None
    }
  })
}

pub(crate) struct InfoJson {
  pub title: Option<String>,
  pub duration_secs: Option<f64>,
  pub upload_date: Option<String>,
  pub description: Option<String>,
  pub channel: Option<String>,
  pub channel_url: Option<String>,
  pub webpage_url: Option<String>,
  pub view_count: Option<u64>,
  pub vcodec: Option<String>,
  pub acodec: Option<String>,
}

pub(crate) fn read_info_json(parent: &Path, stem: &OsStr) -> InfoJson {
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
