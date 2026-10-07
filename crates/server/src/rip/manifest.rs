//! The per-disc manifest (`disc.json`) and raw info dump (`disc-info.txt`)
//! a rip leaves beside its MKVs.
//!
//! The manifest is the persistent record of everything known at rip time:
//! the udev facts and disc id that named the directory, MakeMKV's title
//! table, and the structural fingerprint.  It is what makes a disc
//! directory self-describing to the scanner and, later, to identity
//! resolution — reconstructing any of it afterwards would mean re-scanning
//! the disc.  Both files are written through a `.part` sibling and renamed
//! into place so the watcher never sees a half-written sidecar.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::identity::fingerprint::StoredFingerprint;
use crate::rip::config::RipPolicy;
use crate::rip::makemkv::{DiscFacts, TitleInfo};
use crate::rip::naming::{IdSource, INFO_FILE, MANIFEST_FILE};

/// Bumped when the manifest's meaning changes; readers tolerate older
/// layouts through serde defaults.
pub const MANIFEST_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiscManifest {
  pub schema: u32,
  /// The device the disc was read from.
  pub device: String,
  /// The `<label>__<id>` directory name the rip was placed under.
  pub dir_name: String,
  pub label: String,
  pub id: String,
  pub id_source: IdSource,
  pub scanned_at: i64,
  /// Absent only when a rescan found no earlier manifest to carry it from.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub ripped_at: Option<i64>,
  pub policy: RipPolicy,
  pub udev: DiscFacts,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub makemkv_version: Option<String>,
  /// MakeMKV's disc name (`CINFO` attribute 2).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub disc_name: Option<String>,
  /// MakeMKV's volume name (`CINFO` attribute 32).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub volume_name: Option<String>,
  pub title_count: u32,
  #[serde(default)]
  pub titles: Vec<TitleInfo>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub fingerprint: Option<StoredFingerprint>,
}

#[derive(Debug, Error)]
pub enum ManifestError {
  #[error("Failed to serialize the disc manifest for '{path}': {source}")]
  ManifestSerialize {
    path: String,
    source: serde_json::Error,
  },

  #[error("Failed to write the disc sidecar '{path}': {source}")]
  ManifestWrite {
    path: String,
    source: std::io::Error,
  },

  #[error("Failed to move the disc sidecar into place at '{path}': {source}")]
  ManifestRename {
    path: String,
    source: std::io::Error,
  },

  #[error("Failed to read the disc manifest '{path}': {source}")]
  ManifestRead {
    path: String,
    source: std::io::Error,
  },

  #[error("Failed to parse the disc manifest '{path}': {source}")]
  ManifestParse {
    path: String,
    source: serde_json::Error,
  },
}

/// Write `disc.json` into the directory.
pub async fn write_manifest(
  dir: &Path,
  manifest: &DiscManifest,
) -> Result<(), ManifestError> {
  let path = dir.join(MANIFEST_FILE);
  let json = serde_json::to_vec_pretty(manifest).map_err(|source| {
    ManifestError::ManifestSerialize {
      path: path.to_string_lossy().to_string(),
      source,
    }
  })?;
  write_via_part(&path, &json).await
}

/// Write `disc-info.txt` (the raw `makemkvcon -r info` output) into the
/// directory.
pub async fn write_info_dump(
  dir: &Path,
  text: &str,
) -> Result<(), ManifestError> {
  write_via_part(&dir.join(INFO_FILE), text.as_bytes()).await
}

async fn write_via_part(
  path: &Path,
  bytes: &[u8],
) -> Result<(), ManifestError> {
  let part = part_path(path);
  tokio::fs::write(&part, bytes).await.map_err(|source| {
    ManifestError::ManifestWrite {
      path: part.to_string_lossy().to_string(),
      source,
    }
  })?;
  tokio::fs::rename(&part, path).await.map_err(|source| {
    ManifestError::ManifestRename {
      path: path.to_string_lossy().to_string(),
      source,
    }
  })
}

fn part_path(path: &Path) -> PathBuf {
  let mut name = path.as_os_str().to_os_string();
  name.push(".part");
  PathBuf::from(name)
}

/// Read the directory's manifest, if it has one.
pub async fn read_manifest(
  dir: &Path,
) -> Result<Option<DiscManifest>, ManifestError> {
  let path = dir.join(MANIFEST_FILE);
  match tokio::fs::read(&path).await {
    Ok(bytes) => parse_manifest(&path, &bytes).map(Some),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(source) => Err(ManifestError::ManifestRead {
      path: path.to_string_lossy().to_string(),
      source,
    }),
  }
}

fn parse_manifest(
  path: &Path,
  bytes: &[u8],
) -> Result<DiscManifest, ManifestError> {
  serde_json::from_slice(bytes).map_err(|source| ManifestError::ManifestParse {
    path: path.to_string_lossy().to_string(),
    source,
  })
}

/// Read the directory's manifest, if it has one.  Blocking, for the scanner's
/// `spawn_blocking` context.
pub fn read_manifest_sync(
  dir: &Path,
) -> Result<Option<DiscManifest>, ManifestError> {
  let path = dir.join(MANIFEST_FILE);
  match std::fs::read(&path) {
    Ok(bytes) => parse_manifest(&path, &bytes).map(Some),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(source) => Err(ManifestError::ManifestRead {
      path: path.to_string_lossy().to_string(),
      source,
    }),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn sample() -> DiscManifest {
    DiscManifest {
      schema: MANIFEST_SCHEMA,
      device: "/dev/sr0".to_string(),
      dir_name: "LBL__uuid".to_string(),
      label: "LBL".to_string(),
      id: "uuid".to_string(),
      id_source: IdSource::Uuid,
      scanned_at: 10,
      ripped_at: Some(20),
      policy: RipPolicy::Skip,
      udev: DiscFacts {
        label: Some("LBL".to_string()),
        fs_uuid: Some("uuid".to_string()),
        media_dvd: true,
        ..DiscFacts::default()
      },
      makemkv_version: Some("1.18.2".to_string()),
      disc_name: Some("Name".to_string()),
      volume_name: None,
      title_count: 1,
      titles: vec![TitleInfo {
        index: 0,
        output_file: Some("title_t00.mkv".to_string()),
        duration_secs: Some(5535),
        chapters: Some(12),
        size_bytes: Some(1),
        segments_map: Some("1".to_string()),
        original_title_id: None,
      }],
      fingerprint: None,
    }
  }

  #[test]
  fn manifest_round_trips_through_json() {
    let json = serde_json::to_string(&sample()).unwrap();
    let back: DiscManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, sample());
  }

  #[test]
  fn manifest_tolerates_missing_optional_fields() {
    let minimal = r#"{"schema":1,"device":"/dev/sr0","dir_name":"a__b",
      "label":"a","id":"b","id_source":"uuid","scanned_at":1,
      "policy":"skip","udev":{},"title_count":0}"#;
    let parsed: DiscManifest = serde_json::from_str(minimal).unwrap();
    assert!(parsed.titles.is_empty());
    assert!(parsed.fingerprint.is_none());
  }

  #[tokio::test]
  async fn sidecars_write_through_part_files() {
    let dir = tempfile::tempdir().unwrap();
    write_manifest(dir.path(), &sample()).await.unwrap();
    write_info_dump(dir.path(), "TCOUNT:1\n").await.unwrap();
    assert!(!dir.path().join("disc.json.part").exists());
    assert!(!dir.path().join("disc-info.txt.part").exists());
    assert_eq!(
      read_manifest_sync(dir.path()).unwrap(),
      Some(sample()),
      "the manifest reads back as written"
    );
    assert_eq!(
      std::fs::read_to_string(dir.path().join("disc-info.txt")).unwrap(),
      "TCOUNT:1\n"
    );
  }

  #[test]
  fn missing_manifest_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_manifest_sync(dir.path()).unwrap(), None);
  }

  #[test]
  fn malformed_manifest_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("disc.json"), b"{not json").unwrap();
    assert!(matches!(
      read_manifest_sync(dir.path()),
      Err(ManifestError::ManifestParse { .. })
    ));
  }
}
