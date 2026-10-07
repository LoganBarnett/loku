//! The `[rip]` configuration table: which drive to watch, which discs-kind
//! library receives rips, and the re-insert policy.
//!
//! The file-level entry is a candidate; `resolve` turns it into a validated
//! `RipConfig` whose library reference is guaranteed to name a configured
//! discs-kind library, following the "types as validation" rule.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

use crate::library::{Library, LibraryFileEntry, LibraryKind};

/// What to do when a disc whose directory already exists is inserted.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum RipPolicy {
  /// Leave the existing directory alone.
  Skip,
  /// Refresh only the sidecars (`disc-info.txt`, `disc.json`), a matter of
  /// seconds, without touching the MKVs.
  Rescan,
  /// Rip again into staging, move the existing tree aside, and swap the new
  /// one in.
  Rerip,
}

impl RipPolicy {
  pub fn as_str(self) -> &'static str {
    match self {
      RipPolicy::Skip => "skip",
      RipPolicy::Rescan => "rescan",
      RipPolicy::Rerip => "rerip",
    }
  }

  pub fn parse(value: &str) -> Option<Self> {
    match value {
      "skip" => Some(RipPolicy::Skip),
      "rescan" => Some(RipPolicy::Rescan),
      "rerip" => Some(RipPolicy::Rerip),
      _ => None,
    }
  }
}

/// `[rip]` as written in the config file: every field but the library is
/// optional, and nothing is validated yet.
#[derive(Debug, Clone, Deserialize)]
pub struct RipFileEntry {
  pub device: Option<PathBuf>,
  pub library: String,
  pub policy: Option<RipPolicy>,
  pub eject: Option<bool>,
  pub poll_interval_secs: Option<u64>,
  pub replaced_retention_days: Option<u64>,
}

/// The validated rip configuration: defaults applied and the library
/// resolved to a configured discs-kind root.
#[derive(Debug, Clone)]
pub struct RipConfig {
  pub device: PathBuf,
  pub library: Library,
  pub policy: RipPolicy,
  pub eject: bool,
  pub poll_interval: Duration,
  pub replaced_retention: Duration,
}

#[derive(Debug, Error)]
pub enum RipConfigError {
  #[error(
    "rip.library '{name}' is not a configured library; configured names: \
     [{configured}]"
  )]
  UnknownLibrary { name: String, configured: String },

  #[error("rip.library '{name}' is kind {kind}; ripping requires a discs-kind library")]
  NotDiscsKind { name: String, kind: &'static str },

  #[error("rip.device '{device}' must be an absolute device path")]
  RelativeDevice { device: String },

  #[error("rip.poll_interval_secs must be at least 1")]
  ZeroPollInterval,
}

const DEFAULT_DEVICE: &str = "/dev/sr0";
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
const DEFAULT_REPLACED_RETENTION_DAYS: u64 = 14;
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// Validate the file entry against the configured libraries.  Each error
/// names the field and the fix, since the config layer surfaces its text.
pub fn resolve(
  entry: Option<&RipFileEntry>,
  candidates: &[LibraryFileEntry],
) -> Result<Option<RipConfig>, RipConfigError> {
  entry
    .map(|entry| resolve_entry(entry, candidates))
    .transpose()
}

fn resolve_entry(
  entry: &RipFileEntry,
  candidates: &[LibraryFileEntry],
) -> Result<RipConfig, RipConfigError> {
  let library = candidates
    .iter()
    .find(|candidate| candidate.name == entry.library)
    .ok_or_else(|| RipConfigError::UnknownLibrary {
      name: entry.library.clone(),
      configured: candidates
        .iter()
        .map(|candidate| candidate.name.as_str())
        .collect::<Vec<_>>()
        .join(", "),
    })?;
  if library.kind != LibraryKind::Discs {
    return Err(RipConfigError::NotDiscsKind {
      name: entry.library.clone(),
      kind: library.kind.as_str(),
    });
  }
  let device = entry
    .device
    .clone()
    .unwrap_or_else(|| PathBuf::from(DEFAULT_DEVICE));
  if !device.is_absolute() {
    return Err(RipConfigError::RelativeDevice {
      device: device.display().to_string(),
    });
  }
  let poll_secs = entry
    .poll_interval_secs
    .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);
  if poll_secs == 0 {
    return Err(RipConfigError::ZeroPollInterval);
  }
  Ok(RipConfig {
    device,
    library: Library {
      name: library.name.clone(),
      path: library.path.clone(),
      kind: library.kind,
    },
    policy: entry.policy.unwrap_or(RipPolicy::Skip),
    eject: entry.eject.unwrap_or(true),
    poll_interval: Duration::from_secs(poll_secs),
    replaced_retention: Duration::from_secs(
      entry
        .replaced_retention_days
        .unwrap_or(DEFAULT_REPLACED_RETENTION_DAYS)
        .saturating_mul(SECS_PER_DAY),
    ),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn candidates() -> Vec<LibraryFileEntry> {
    vec![
      LibraryFileEntry {
        name: "downloads".to_string(),
        path: PathBuf::from("/media/videos"),
        kind: LibraryKind::Downloads,
      },
      LibraryFileEntry {
        name: "movies".to_string(),
        path: PathBuf::from("/media/rips"),
        kind: LibraryKind::Discs,
      },
    ]
  }

  fn entry(library: &str) -> RipFileEntry {
    RipFileEntry {
      device: None,
      library: library.to_string(),
      policy: None,
      eject: None,
      poll_interval_secs: None,
      replaced_retention_days: None,
    }
  }

  #[test]
  fn absent_entry_disables_ripping() {
    assert!(resolve(None, &candidates()).unwrap().is_none());
  }

  #[test]
  fn defaults_apply() {
    let config = resolve(Some(&entry("movies")), &candidates())
      .unwrap()
      .unwrap();
    assert_eq!(config.device, PathBuf::from("/dev/sr0"));
    assert_eq!(config.library.name, "movies");
    assert_eq!(config.policy, RipPolicy::Skip);
    assert!(config.eject);
    assert_eq!(config.poll_interval, Duration::from_secs(5));
    assert_eq!(
      config.replaced_retention,
      Duration::from_secs(14 * SECS_PER_DAY)
    );
  }

  #[test]
  fn unknown_library_is_rejected_with_the_configured_names() {
    let error = resolve(Some(&entry("nope")), &candidates())
      .unwrap_err()
      .to_string();
    assert!(error.contains("'nope'"), "{error}");
    assert!(error.contains("downloads, movies"), "{error}");
  }

  #[test]
  fn downloads_library_is_rejected() {
    let error = resolve(Some(&entry("downloads")), &candidates())
      .unwrap_err()
      .to_string();
    assert!(error.contains("kind downloads"), "{error}");
  }

  #[test]
  fn relative_device_and_zero_poll_are_rejected() {
    let relative = RipFileEntry {
      device: Some(PathBuf::from("sr0")),
      ..entry("movies")
    };
    assert!(resolve(Some(&relative), &candidates())
      .unwrap_err()
      .to_string()
      .contains("absolute"));
    let zero = RipFileEntry {
      poll_interval_secs: Some(0),
      ..entry("movies")
    };
    assert!(resolve(Some(&zero), &candidates())
      .unwrap_err()
      .to_string()
      .contains("poll_interval_secs"));
  }

  #[test]
  fn policy_round_trips() {
    for policy in [RipPolicy::Skip, RipPolicy::Rescan, RipPolicy::Rerip] {
      assert_eq!(RipPolicy::parse(policy.as_str()), Some(policy));
    }
    assert_eq!(RipPolicy::parse("nuke"), None);
  }
}
