//! Naming for per-disc rip directories: filesystem-safe components, the
//! stable disc id, and the `<label>__<id>` convention that keeps box-set
//! discs from overwriting one another.
//!
//! The id is a directory handle, not a matching key: it only has to be
//! unique per pressing and identical on every re-insert of the same disc, so
//! the directory doubles as the "already ripped" check.  Identity resolution
//! keys on the structural fingerprint instead.  The preferred id is the disc
//! filesystem's UUID as udev reports it (the volume creation timestamp for
//! ISO 9660, the volume set identifier for UDF): it is readable before the
//! slow MakeMKV scan and does not drift with MakeMKV versions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;

/// The hidden directory under a discs library holding rip staging, replaced
/// trees, and failed attempts.  Hidden paths are invisible to the index and
/// the watcher, so nothing in here can surface as a library item mid-rip.
pub const RIP_DIR: &str = ".rip";
/// Rip in progress; deleted before every new attempt.
pub const STAGING_DIR: &str = "staging";
/// The previous tree after a rerip, kept until the retention sweep.
pub const REPLACED_DIR: &str = "replaced";
/// A failed attempt, quarantined for inspection until the retention sweep.
pub const FAILED_DIR: &str = "failed";
/// The per-disc manifest written beside the MKVs.
pub const MANIFEST_FILE: &str = "disc.json";
/// The raw `makemkvcon -r info` dump written beside the MKVs.
pub const INFO_FILE: &str = "disc-info.txt";

/// Length cap for a sanitized component.  Long labels are rare, and the cap
/// keeps `<label>__<id>` well inside every filesystem's name limit.
const COMPONENT_MAX_CHARS: usize = 64;

/// How many hex digits of the title-table hash make up an info-hash id.
const INFO_HASH_CHARS: usize = 16;

/// Where a disc id came from, in decreasing order of stability.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum IdSource {
  /// The disc filesystem's UUID as udev reports it.
  Uuid,
  /// A hash of the MakeMKV title table, for discs udev could not identify.
  InfoHash,
  /// The rip timestamp; carries no disc identity at all.
  Timestamp,
}

impl IdSource {
  pub fn as_str(self) -> &'static str {
    match self {
      IdSource::Uuid => "uuid",
      IdSource::InfoHash => "info_hash",
      IdSource::Timestamp => "timestamp",
    }
  }

  pub fn parse(value: &str) -> Option<Self> {
    match value {
      "uuid" => Some(IdSource::Uuid),
      "info_hash" => Some(IdSource::InfoHash),
      "timestamp" => Some(IdSource::Timestamp),
      _ => None,
    }
  }
}

/// A disc id together with the fallback rung that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscIdentity {
  pub id: String,
  pub source: IdSource,
}

/// Reduce arbitrary text to a filesystem-safe name component: ASCII
/// alphanumerics, `-`, and `.` survive; everything else (slashes,
/// whitespace, control characters, non-ASCII) becomes `_`, runs of `_`
/// collapse, and leading or trailing `_`/`.` are trimmed so the result is
/// never hidden and never contains the `__` separator.  `None` when nothing
/// survives.
pub fn sanitize_component(raw: &str) -> Option<String> {
  let mapped: String = raw
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
        c
      } else {
        '_'
      }
    })
    .collect();
  let collapsed = mapped
    .split('_')
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("_");
  let capped: String = collapsed
    .trim_matches(['_', '.'])
    .chars()
    .take(COMPONENT_MAX_CHARS)
    .collect();
  let trimmed = capped.trim_end_matches(['_', '.']);
  (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The disc id fallback chain: the udev UUID, else a hash of the MakeMKV
/// title table, else the rip timestamp.  Only the first two identify the
/// pressing; a timestamp id is a last resort the policy layer refuses to
/// reuse.
pub fn disc_identity(
  fs_uuid: Option<&str>,
  tinfo_lines: &[String],
  now_secs: i64,
) -> DiscIdentity {
  fs_uuid
    .and_then(sanitize_component)
    .map(|id| DiscIdentity {
      id,
      source: IdSource::Uuid,
    })
    .or_else(|| {
      (!tinfo_lines.is_empty()).then(|| DiscIdentity {
        id: info_hash(tinfo_lines),
        source: IdSource::InfoHash,
      })
    })
    .unwrap_or_else(|| DiscIdentity {
      id: format!("ts{now_secs}"),
      source: IdSource::Timestamp,
    })
}

/// SHA-256 over the title-table lines, truncated: the lines are a
/// deterministic function of the pressing (per-title duration, size, and
/// segment map), so the same disc hashes the same on every insert.
fn info_hash(lines: &[String]) -> String {
  let digest = Sha256::digest(lines.join("\n").as_bytes());
  format!("{digest:x}")
    .chars()
    .take(INFO_HASH_CHARS)
    .collect()
}

/// The label half of the directory name, in precedence order: the udev
/// volume label, then MakeMKV's volume name and disc name, then a fixed
/// placeholder so a label-less disc still gets a well-formed directory.
pub fn disc_label(
  udev_label: Option<&str>,
  volume_name: Option<&str>,
  disc_name: Option<&str>,
) -> String {
  [udev_label, volume_name, disc_name]
    .into_iter()
    .flatten()
    .find_map(sanitize_component)
    .unwrap_or_else(|| "DISC".to_string())
}

/// `<label>__<id>`: the per-disc directory name.
pub fn dir_name(label: &str, id: &str) -> String {
  format!("{label}__{id}")
}

/// Split a per-disc directory name into its label and id halves.  The label
/// half is whatever precedes the last `__`, so a resolved
/// `<Title (Year)>__<id>` name splits the same way.
pub fn split_dir_name(name: &str) -> Option<(&str, &str)> {
  name.rsplit_once("__")
}

/// The name a replaced or failed tree is archived under: the directory name
/// plus the archival timestamp, which the retention sweep reads back.
pub fn archived_name(dir_name: &str, ts: i64) -> String {
  format!("{dir_name}__{ts}")
}

/// The archival timestamp an archived name carries, if it has one.
pub fn archived_timestamp(name: &str) -> Option<i64> {
  // The .ok() discard is deliberate: a suffix that is not an integer means
  // the entry was not archived by Loku, which the caller treats as "leave
  // it alone" rather than as an error.
  split_dir_name(name).and_then(|(_, ts)| ts.parse().ok())
}

/// Whether a path component is hidden by the dot convention.  Hidden
/// directories are where rip staging lives, so the index and the watcher
/// skip them wholesale.
pub fn is_hidden_component(name: &OsStr) -> bool {
  name.to_string_lossy().starts_with('.')
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sanitize_replaces_hostile_characters() {
    assert_eq!(
      sanitize_component("Amélie/Disc 1").as_deref(),
      Some("Am_lie_Disc_1")
    );
    assert_eq!(
      sanitize_component("back\\slash:colon").as_deref(),
      Some("back_slash_colon")
    );
    assert_eq!(
      sanitize_component("VOLTRON_S1_D1").as_deref(),
      Some("VOLTRON_S1_D1")
    );
  }

  #[test]
  fn sanitize_collapses_and_trims_separators() {
    assert_eq!(sanitize_component("A__B").as_deref(), Some("A_B"));
    assert_eq!(sanitize_component("__leading").as_deref(), Some("leading"));
    assert_eq!(sanitize_component("trailing._").as_deref(), Some("trailing"));
    assert_eq!(sanitize_component(".hidden").as_deref(), Some("hidden"));
  }

  #[test]
  fn sanitize_rejects_empty_results() {
    assert_eq!(sanitize_component(""), None);
    assert_eq!(sanitize_component("   "), None);
    assert_eq!(sanitize_component("///"), None);
    assert_eq!(sanitize_component(".."), None);
  }

  #[test]
  fn sanitize_caps_length_without_trailing_separator() {
    let long = "a".repeat(63) + "_" + &"b".repeat(200);
    let result = sanitize_component(&long).unwrap();
    assert_eq!(result.len(), 63, "the cap must not end on a separator");
    assert_eq!(result, "a".repeat(63));
  }

  #[test]
  fn identity_prefers_uuid_then_hash_then_timestamp() {
    let lines = vec!["TINFO:0,9,0,\"1:32:15\"".to_string()];
    assert_eq!(
      disc_identity(Some("2016-05-12-10-15-33-00"), &lines, 5),
      DiscIdentity {
        id: "2016-05-12-10-15-33-00".to_string(),
        source: IdSource::Uuid,
      }
    );
    let hashed = disc_identity(None, &lines, 5);
    assert_eq!(hashed.source, IdSource::InfoHash);
    assert_eq!(hashed.id.len(), INFO_HASH_CHARS);
    assert_eq!(
      disc_identity(None, &[], 5),
      DiscIdentity {
        id: "ts5".to_string(),
        source: IdSource::Timestamp,
      }
    );
  }

  #[test]
  fn identity_treats_unusable_uuid_as_absent() {
    let hashed = disc_identity(Some("///"), &["x".to_string()], 5);
    assert_eq!(hashed.source, IdSource::InfoHash);
  }

  #[test]
  fn info_hash_is_deterministic() {
    let lines = vec!["a".to_string(), "b".to_string()];
    assert_eq!(info_hash(&lines), info_hash(&lines.clone()));
    assert_eq!(info_hash(&lines), "7e18f737311b2dc3");
    assert_ne!(info_hash(&lines), info_hash(&["b".to_string()]));
  }

  #[test]
  fn label_falls_through_sources_to_placeholder() {
    assert_eq!(disc_label(Some("LBL"), Some("VOL"), Some("NAME")), "LBL");
    assert_eq!(disc_label(Some(""), Some("VOL"), None), "VOL");
    assert_eq!(disc_label(None, None, Some("The Name")), "The_Name");
    assert_eq!(disc_label(None, None, None), "DISC");
  }

  #[test]
  fn dir_names_round_trip() {
    let name = dir_name("VOLTRON_S1_D1", "2016-05-12");
    assert_eq!(name, "VOLTRON_S1_D1__2016-05-12");
    assert_eq!(split_dir_name(&name), Some(("VOLTRON_S1_D1", "2016-05-12")));
    assert_eq!(
      split_dir_name("The Matrix (1999)__abc"),
      Some(("The Matrix (1999)", "abc"))
    );
    assert_eq!(split_dir_name("no-separator"), None);
  }

  #[test]
  fn archived_names_carry_their_timestamp() {
    let archived = archived_name("LBL__id", 1_757_400_000);
    assert_eq!(archived, "LBL__id__1757400000");
    assert_eq!(archived_timestamp(&archived), Some(1_757_400_000));
    assert_eq!(archived_timestamp("LBL__id"), None);
    assert_eq!(archived_timestamp("plain"), None);
  }

  #[test]
  fn hidden_components_start_with_a_dot() {
    assert!(is_hidden_component(OsStr::new(".rip")));
    assert!(!is_hidden_component(OsStr::new("Movie__id")));
  }

  #[test]
  fn id_source_round_trips() {
    for source in [IdSource::Uuid, IdSource::InfoHash, IdSource::Timestamp] {
      assert_eq!(IdSource::parse(source.as_str()), Some(source));
    }
    assert_eq!(IdSource::parse("junk"), None);
  }
}
