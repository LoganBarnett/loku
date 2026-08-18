//! Named library roots.
//!
//! Loku serves one or more libraries, each a directory tree holding a single
//! dataset kind.  The kinds differ in how items are named and what sidecar
//! metadata they carry (yt-dlp `info.json` versus disc-rip conventions), so
//! the kind is typed configuration rather than a path convention.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The dataset shape a library holds.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum LibraryKind {
  /// yt-dlp style downloads carrying `info.json` sidecars.
  Downloads,
  /// MakeMKV disc rips, named by the ripper's title conventions.
  Discs,
}

/// A validated library root: the name is URL-safe and unique, and the path
/// existed at config-resolution time.
#[derive(Debug, Clone)]
pub struct Library {
  pub name: String,
  pub path: PathBuf,
  pub kind: LibraryKind,
}

/// A library entry as written in the config file, before validation — the
/// candidate type for [`Library`].
#[derive(Debug, Clone, Deserialize)]
pub struct LibraryFileEntry {
  pub name: String,
  pub path: PathBuf,
  pub kind: LibraryKind,
}

/// Whether a library name is safe to embed as a URL path segment (it becomes
/// the `/files/{name}` mount point) without any escaping concerns.
pub fn valid_library_name(name: &str) -> bool {
  !name.is_empty()
    && name.chars().all(|c| {
      c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
    })
}
