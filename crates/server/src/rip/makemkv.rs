//! MakeMKV and udev at the process boundary: parsers for `udevadm info`
//! output and MakeMKV's robot-mode (`--robot`) line protocol, the data types
//! a rip records in its manifest, and the `DiscTools` trait the rip worker
//! drives — implemented over the real binaries here and by a fake in the
//! integration tests.
//!
//! Robot-mode output is line-oriented: `TCOUNT:n`, `CINFO:id,code,"value"`,
//! `TINFO:title,id,code,"value"`, `SINFO:title,stream,id,code,"value"`,
//! `MSG:code,flags,count,"text","format",params...`, `DRV:...`, and while
//! ripping the progress lines `PRGC`/`PRGT` (current and total operation
//! names) and `PRGV:current,total,max`.  Attribute ids come from MakeMKV's
//! `apdefs.h`.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// A udev property query reads the udev database, not the drive.
pub const MEDIA_TIMEOUT: Duration = Duration::from_secs(10);
/// A disc scan reads the whole structure; Blu-rays with AACS take minutes.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A full-disc rip of a dual-layer Blu-ray on a slow drive.
pub const RIP_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
/// Tray operations are near-instant; a stuck drive should not hang the job.
pub const TRAY_TIMEOUT: Duration = Duration::from_secs(60);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// How many trailing `MSG` texts a failed rip reports.
const LAST_MESSAGES: usize = 20;
/// How much of a failed command's output an error message carries.
const TAIL_CHARS: usize = 500;

const ATTR_NAME: u32 = 2;
const ATTR_CHAPTER_COUNT: u32 = 8;
const ATTR_DURATION: u32 = 9;
const ATTR_DISK_SIZE_BYTES: u32 = 11;
const ATTR_ORIGINAL_TITLE_ID: u32 = 24;
const ATTR_SEGMENTS_MAP: u32 = 26;
const ATTR_OUTPUT_FILE_NAME: u32 = 27;
const ATTR_VOLUME_NAME: u32 = 32;
/// "MakeMKV vX.Y.Z ... started".
const MSG_STARTED: u32 = 1005;

/// What udev knows about the disc in a drive, straight from its property
/// database (`udevadm info --query=property`).  Strings are kept raw; the
/// label has udev's `\xNN` escapes decoded.
#[derive(
  Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
pub struct DiscFacts {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub label: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub fs_uuid: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub fs_type: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub volume_set_id: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub application_id: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub publisher_id: Option<String>,
  #[serde(default)]
  pub media_dvd: bool,
  #[serde(default)]
  pub media_bd: bool,
}

/// One title as MakeMKV's disc analysis describes it.  These are the
/// structural facts a fingerprint is built from: read off the disc and
/// deterministic per pressing, unlike anything about the output MKV.
#[derive(
  Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
pub struct TitleInfo {
  pub index: u32,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub output_file: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub duration_secs: Option<u64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub chapters: Option<u32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub size_bytes: Option<u64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub segments_map: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub original_title_id: Option<String>,
}

/// The parsed `makemkvcon --robot info` dump.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InfoDump {
  pub title_count: Option<u32>,
  /// `CINFO` attribute 2; Blu-rays often carry the real title here.
  pub disc_name: Option<String>,
  /// `CINFO` attribute 32.
  pub volume_name: Option<String>,
  pub makemkv_version: Option<String>,
  pub titles: Vec<TitleInfo>,
  /// The raw `TINFO` lines in order, the input to the info-hash disc id.
  pub tinfo_lines: Vec<String>,
}

/// One line of rip progress, as the worker consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RipEvent {
  Progress { current: u64, total: u64, max: u64 },
  CurrentOp(String),
  TotalOp(String),
  Message { code: u32, text: String },
}

/// What a finished rip hands back beyond its files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RipOutput {
  pub last_messages: Vec<String>,
}

/// Parse `udevadm info --query=property` output.  `None` means no media is
/// in the drive (or the drive is not an optical drive at all).
pub fn parse_udev_properties(text: &str) -> Option<DiscFacts> {
  let props: HashMap<&str, &str> = text
    .lines()
    .filter_map(|line| line.split_once('='))
    .collect();
  (props.get("ID_CDROM_MEDIA") == Some(&"1")).then(|| DiscFacts {
    label: props
      .get("ID_FS_LABEL_ENC")
      .map(|value| decode_udev_escapes(value))
      .or_else(|| non_empty(props.get("ID_FS_LABEL")))
      .filter(|value| !value.is_empty()),
    fs_uuid: non_empty(props.get("ID_FS_UUID")),
    fs_type: non_empty(props.get("ID_FS_TYPE")),
    volume_set_id: non_empty(props.get("ID_FS_VOLUME_SET_ID")),
    application_id: non_empty(props.get("ID_FS_APPLICATION_ID")),
    publisher_id: non_empty(props.get("ID_FS_PUBLISHER_ID")),
    media_dvd: props.get("ID_CDROM_MEDIA_DVD") == Some(&"1"),
    media_bd: props.get("ID_CDROM_MEDIA_BD") == Some(&"1"),
  })
}

fn non_empty(value: Option<&&str>) -> Option<String> {
  value.map(|v| v.to_string()).filter(|v| !v.is_empty())
}

/// Decode udev's `\xNN` byte escapes, as used in the `*_ENC` properties.
pub fn decode_udev_escapes(value: &str) -> String {
  let bytes = value.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    // The .ok() discards are deliberate: a malformed escape (no two hex
    // digits after the `\x`) is copied through literally.
    let escaped = (bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'x'))
      .then(|| bytes.get(i + 2..i + 4))
      .flatten()
      .and_then(|hex| std::str::from_utf8(hex).ok())
      .and_then(|hex| u8::from_str_radix(hex, 16).ok());
    if let Some(byte) = escaped {
      out.push(byte);
      i += 4;
    } else {
      out.push(bytes[i]);
      i += 1;
    }
  }
  String::from_utf8_lossy(&out).to_string()
}

/// Split a robot-mode line's comma-separated fields, honouring quotes and
/// backslash escapes inside them (values such as disc names carry commas).
fn split_fields(rest: &str) -> Vec<String> {
  let (mut fields, current, _, _) = rest.chars().fold(
    (Vec::new(), String::new(), false, false),
    |(mut fields, mut current, in_quotes, escaped), c| match (
      in_quotes, escaped, c,
    ) {
      (true, true, c) => {
        current.push(c);
        (fields, current, true, false)
      }
      (true, false, '\\') => (fields, current, true, true),
      (true, false, '"') => (fields, current, false, false),
      (true, false, c) => {
        current.push(c);
        (fields, current, true, false)
      }
      (false, _, '"') => (fields, current, true, false),
      (false, _, ',') => {
        fields.push(current);
        (fields, String::new(), false, false)
      }
      (false, _, c) => {
        current.push(c);
        (fields, current, false, false)
      }
    },
  );
  fields.push(current);
  fields
}

/// Parse a `makemkvcon --robot info` dump.  Malformed lines are skipped,
/// never fatal: the dump is also persisted verbatim, so nothing is lost.
pub fn parse_info(text: &str) -> InfoDump {
  text.lines().fold(InfoDump::default(), |mut dump, line| {
    if let Some(rest) = line.strip_prefix("TCOUNT:") {
      // The .ok() discard is deliberate: an unparseable count is reported
      // by the worker as a missing title count, not as a parse error.
      dump.title_count = rest.trim().parse().ok();
    } else if let Some(rest) = line.strip_prefix("CINFO:") {
      let fields = split_fields(rest);
      match (attribute_id(&fields, 0), fields.get(2)) {
        (Some(ATTR_NAME), Some(value)) => dump.disc_name = non_blank(value),
        (Some(ATTR_VOLUME_NAME), Some(value)) => {
          dump.volume_name = non_blank(value);
        }
        _ => {}
      }
    } else if let Some(rest) = line.strip_prefix("TINFO:") {
      dump.tinfo_lines.push(line.to_string());
      apply_tinfo(&mut dump.titles, &split_fields(rest));
    } else if let Some(rest) = line.strip_prefix("MSG:") {
      let fields = split_fields(rest);
      if attribute_id(&fields, 0) == Some(MSG_STARTED) {
        dump.makemkv_version = fields.get(5).and_then(|p| version_in(p));
      }
    }
    dump
  })
}

fn non_blank(value: &str) -> Option<String> {
  let trimmed = value.trim();
  (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A numeric field, absent when missing or not a number.
fn attribute_id(fields: &[String], position: usize) -> Option<u32> {
  // The .ok() discard is deliberate: a non-numeric id means a line this
  // parser does not understand, which the liberal-parse policy skips.
  fields
    .get(position)
    .and_then(|field| field.trim().parse().ok())
}

/// Fold one `TINFO:title,id,code,"value"` record into the title table.
fn apply_tinfo(titles: &mut Vec<TitleInfo>, fields: &[String]) {
  let (Some(index), Some(attribute), Some(value)) =
    (attribute_id(fields, 0), attribute_id(fields, 1), fields.get(3))
  else {
    return;
  };
  let position = titles
    .iter()
    .position(|title| title.index == index)
    .unwrap_or_else(|| {
      titles.push(TitleInfo {
        index,
        ..TitleInfo::default()
      });
      titles.len() - 1
    });
  let Some(title) = titles.get_mut(position) else {
    return;
  };
  // The .ok() discards are deliberate: a malformed numeric attribute leaves
  // that one fact absent rather than discarding the whole title.
  match attribute {
    ATTR_CHAPTER_COUNT => title.chapters = value.trim().parse().ok(),
    ATTR_DURATION => title.duration_secs = duration_secs(value),
    ATTR_DISK_SIZE_BYTES => title.size_bytes = value.trim().parse().ok(),
    ATTR_ORIGINAL_TITLE_ID => title.original_title_id = non_blank(value),
    ATTR_SEGMENTS_MAP => title.segments_map = non_blank(value),
    ATTR_OUTPUT_FILE_NAME => title.output_file = non_blank(value),
    _ => {}
  }
}

/// MakeMKV's `h:mm:ss` (or `mm:ss`) duration as seconds.  The .ok() discard
/// is deliberate: a non-numeric part means no duration, which is the answer.
pub fn duration_secs(text: &str) -> Option<u64> {
  let parts: Vec<u64> = text
    .trim()
    .split(':')
    .map(|part| part.parse::<u64>())
    .collect::<Result<_, _>>()
    .ok()?;
  matches!(parts.len(), 2 | 3)
    .then(|| parts.iter().fold(0, |acc, part| acc * 60 + part))
}

/// The version token in "MakeMKV v1.18.2 linux(x64-release)".
fn version_in(text: &str) -> Option<String> {
  text
    .split_whitespace()
    .find(|token| {
      token
        .strip_prefix('v')
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    })
    .map(|token| token.trim_start_matches('v').to_string())
}

/// Parse one line of `makemkvcon mkv` output into a progress event.
pub fn parse_progress_line(line: &str) -> Option<RipEvent> {
  if let Some(rest) = line.strip_prefix("PRGV:") {
    let fields = split_fields(rest);
    let value = |position: usize| -> Option<u64> {
      // The .ok() discard is deliberate: a malformed progress line is noise
      // the worker ignores rather than a failure of the rip.
      fields.get(position).and_then(|f| f.trim().parse().ok())
    };
    Some(RipEvent::Progress {
      current: value(0)?,
      total: value(1)?,
      max: value(2)?,
    })
  } else if let Some(rest) = line.strip_prefix("PRGC:") {
    split_fields(rest)
      .get(2)
      .map(|name| RipEvent::CurrentOp(name.clone()))
  } else if let Some(rest) = line.strip_prefix("PRGT:") {
    split_fields(rest)
      .get(2)
      .map(|name| RipEvent::TotalOp(name.clone()))
  } else if let Some(rest) = line.strip_prefix("MSG:") {
    let fields = split_fields(rest);
    Some(RipEvent::Message {
      code: attribute_id(&fields, 0)?,
      text: fields.get(3)?.clone(),
    })
  } else {
    None
  }
}

/// Overall progress in permille, from a `PRGV` line's total and max.
pub fn progress_permille(total: u64, max: u64) -> u32 {
  if max == 0 {
    0
  } else {
    u32::try_from((total.saturating_mul(1000) / max).min(1000)).unwrap_or(1000)
  }
}

#[derive(Debug, Error)]
pub enum RipToolsError {
  #[error("Rip tool '{tool}' is not available: {source}")]
  ToolMissing {
    tool: &'static str,
    source: std::io::Error,
  },

  #[error(
    "Rip tool '{tool}' did not answer a startup check within {timeout_secs}s"
  )]
  ToolCheckTimeout {
    tool: &'static str,
    timeout_secs: u64,
  },

  #[error("Rip tool '{tool}' failed its version check ({status})")]
  ToolVersionFailed { tool: &'static str, status: String },
}

#[derive(Debug, Error)]
pub enum MediaQueryError {
  #[error("Failed to spawn udevadm to query '{device}': {source}")]
  UdevadmSpawn {
    device: String,
    source: std::io::Error,
  },

  #[error("udevadm timed out after {timeout_secs}s querying '{device}'")]
  UdevadmTimeout { device: String, timeout_secs: u64 },

  #[error("udevadm failed querying '{device}' ({status}): {stderr_tail}")]
  UdevadmExit {
    device: String,
    status: String,
    stderr_tail: String,
  },
}

#[derive(Debug, Error)]
pub enum DiscScanError {
  #[error("Failed to spawn makemkvcon to scan '{device}': {source}")]
  MakemkvScanSpawn {
    device: String,
    source: std::io::Error,
  },

  #[error("makemkvcon timed out after {timeout_secs}s scanning '{device}'")]
  MakemkvScanTimeout { device: String, timeout_secs: u64 },

  #[error("makemkvcon failed scanning '{device}' ({status}): {output_tail}")]
  MakemkvScanExit {
    device: String,
    status: String,
    output_tail: String,
  },
}

#[derive(Debug, Error)]
pub enum DiscRipError {
  #[error("Failed to spawn makemkvcon to rip '{device}': {source}")]
  MakemkvRipSpawn {
    device: String,
    source: std::io::Error,
  },

  #[error(
    "makemkvcon's output pipe was not available while ripping '{device}'"
  )]
  MakemkvRipOutputUnavailable { device: String },

  #[error(
    "Failed to read makemkvcon's output while ripping '{device}': {source}"
  )]
  MakemkvRipOutputRead {
    device: String,
    source: std::io::Error,
  },

  #[error("makemkvcon timed out after {timeout_secs}s ripping '{device}'")]
  MakemkvRipTimeout { device: String, timeout_secs: u64 },

  #[error(
    "makemkvcon failed ripping '{device}' ({status}); last messages: {last_messages}"
  )]
  MakemkvRipExit {
    device: String,
    status: String,
    last_messages: String,
  },
}

#[derive(Debug, Error)]
pub enum TrayError {
  #[error("Failed to spawn eject for '{device}': {source}")]
  EjectSpawn {
    device: String,
    source: std::io::Error,
  },

  #[error("eject timed out after {timeout_secs}s on '{device}'")]
  EjectTimeout { device: String, timeout_secs: u64 },

  #[error("eject failed on '{device}' ({status}): {stderr_tail}")]
  EjectExit {
    device: String,
    status: String,
    stderr_tail: String,
  },
}

/// The drive and MakeMKV operations the rip worker needs, dyn-safe so tests
/// inject a fake without genericizing the worker.
#[async_trait]
pub trait DiscTools: Send + Sync {
  /// What is in the drive right now; `None` when no media is loaded.
  async fn media(
    &self,
    device: &Path,
  ) -> Result<Option<DiscFacts>, MediaQueryError>;

  /// The raw `makemkvcon --robot info` dump for the loaded disc.
  async fn scan(&self, device: &Path) -> Result<String, DiscScanError>;

  /// Rip every title into `staging`, streaming progress as it goes.  A
  /// closed `events` receiver is not an error: the caller may stop
  /// listening before the rip ends.
  async fn rip(
    &self,
    device: &Path,
    staging: &Path,
    events: mpsc::UnboundedSender<RipEvent>,
  ) -> Result<RipOutput, DiscRipError>;

  async fn eject(&self, device: &Path) -> Result<(), TrayError>;

  /// Release the tray lock without ejecting, so a failed rip leaves the
  /// operator able to remove the disc.
  async fn unlock(&self, device: &Path) -> Result<(), TrayError>;
}

/// Drives the real `udevadm`, `makemkvcon`, and `eject` binaries found on
/// `PATH` (the NixOS module provides them).
pub struct RealDiscTools {
  udevadm: PathBuf,
  makemkvcon: PathBuf,
  eject: PathBuf,
}

impl RealDiscTools {
  /// Locate the tools and verify each starts, so a missing binary degrades
  /// the server at startup instead of failing every disc.
  pub async fn detect() -> Result<Self, RipToolsError> {
    let tools = Self {
      udevadm: PathBuf::from("udevadm"),
      makemkvcon: PathBuf::from("makemkvcon"),
      eject: PathBuf::from("eject"),
    };
    version_check("udevadm", &tools.udevadm).await?;
    version_check("eject", &tools.eject).await?;
    spawn_check("makemkvcon", &tools.makemkvcon).await?;
    Ok(tools)
  }
}

async fn version_check(
  tool: &'static str,
  binary: &Path,
) -> Result<(), RipToolsError> {
  let output = tokio::time::timeout(
    VERSION_TIMEOUT,
    Command::new(binary)
      .arg("--version")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .output(),
  )
  .await
  .map_err(|_elapsed| RipToolsError::ToolCheckTimeout {
    tool,
    timeout_secs: VERSION_TIMEOUT.as_secs(),
  })?
  .map_err(|source| RipToolsError::ToolMissing { tool, source })?;
  if output.status.success() {
    Ok(())
  } else {
    Err(RipToolsError::ToolVersionFailed {
      tool,
      status: output.status.to_string(),
    })
  }
}

/// `makemkvcon` has no version flag; being able to start it at all is the
/// check, whatever it exits with when given no command.
async fn spawn_check(
  tool: &'static str,
  binary: &Path,
) -> Result<(), RipToolsError> {
  tokio::time::timeout(
    VERSION_TIMEOUT,
    Command::new(binary)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .output(),
  )
  .await
  .map_err(|_elapsed| RipToolsError::ToolCheckTimeout {
    tool,
    timeout_secs: VERSION_TIMEOUT.as_secs(),
  })?
  .map(|_output| ())
  .map_err(|source| RipToolsError::ToolMissing { tool, source })
}

#[async_trait]
impl DiscTools for RealDiscTools {
  async fn media(
    &self,
    device: &Path,
  ) -> Result<Option<DiscFacts>, MediaQueryError> {
    let display = device.display().to_string();
    let output = tokio::time::timeout(
      MEDIA_TIMEOUT,
      Command::new(&self.udevadm)
        .arg("info")
        .arg("--query=property")
        .arg(format!("--name={display}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output(),
    )
    .await
    .map_err(|_elapsed| MediaQueryError::UdevadmTimeout {
      device: display.clone(),
      timeout_secs: MEDIA_TIMEOUT.as_secs(),
    })?
    .map_err(|source| MediaQueryError::UdevadmSpawn {
      device: display.clone(),
      source,
    })?;
    if output.status.success() {
      Ok(parse_udev_properties(&String::from_utf8_lossy(&output.stdout)))
    } else {
      Err(MediaQueryError::UdevadmExit {
        device: display,
        status: output.status.to_string(),
        stderr_tail: tail(&String::from_utf8_lossy(&output.stderr)),
      })
    }
  }

  async fn scan(&self, device: &Path) -> Result<String, DiscScanError> {
    let display = device.display().to_string();
    let output = tokio::time::timeout(
      SCAN_TIMEOUT,
      Command::new(&self.makemkvcon)
        .arg("--robot")
        // One megabyte of read cache is plenty for a structure scan.
        .arg("--cache=1")
        .arg("info")
        .arg(format!("dev:{display}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output(),
    )
    .await
    .map_err(|_elapsed| DiscScanError::MakemkvScanTimeout {
      device: display.clone(),
      timeout_secs: SCAN_TIMEOUT.as_secs(),
    })?
    .map_err(|source| DiscScanError::MakemkvScanSpawn {
      device: display.clone(),
      source,
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if output.status.success() {
      Ok(stdout)
    } else {
      // Robot mode reports on stdout; stderr is usually empty, so the tail
      // covers both streams.
      let combined =
        format!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
      Err(DiscScanError::MakemkvScanExit {
        device: display,
        status: output.status.to_string(),
        output_tail: tail(&combined),
      })
    }
  }

  async fn rip(
    &self,
    device: &Path,
    staging: &Path,
    events: mpsc::UnboundedSender<RipEvent>,
  ) -> Result<RipOutput, DiscRipError> {
    let display = device.display().to_string();
    let mut child = Command::new(&self.makemkvcon)
      .arg("--robot")
      .arg("--messages=-stdout")
      .arg("--progress=-same")
      // A large read cache keeps a slow drive streaming.
      .arg("--cache=512")
      // The device is named explicitly; no drive enumeration needed.
      .arg("--noscan")
      .arg("mkv")
      .arg(format!("dev:{display}"))
      .arg("all")
      .arg(staging)
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      // On timeout or shutdown the future is dropped, which must kill the
      // ripper rather than orphan it.
      .kill_on_drop(true)
      .spawn()
      .map_err(|source| DiscRipError::MakemkvRipSpawn {
        device: display.clone(),
        source,
      })?;
    let stdout = child.stdout.take().ok_or_else(|| {
      DiscRipError::MakemkvRipOutputUnavailable {
        device: display.clone(),
      }
    })?;
    let mut lines = BufReader::new(stdout).lines();
    let mut last_messages = VecDeque::with_capacity(LAST_MESSAGES);
    let run = async {
      while let Some(line) = lines.next_line().await.map_err(|source| {
        DiscRipError::MakemkvRipOutputRead {
          device: display.clone(),
          source,
        }
      })? {
        if let Some(event) = parse_progress_line(&line) {
          if let RipEvent::Message { code, text } = &event {
            remember(&mut last_messages, text);
            log_message(*code, text);
          }
          // A closed receiver only means nobody is listening any more.
          drop(events.send(event));
        }
      }
      child
        .wait()
        .await
        .map_err(|source| DiscRipError::MakemkvRipOutputRead {
          device: display.clone(),
          source,
        })
    };
    let status =
      tokio::time::timeout(RIP_TIMEOUT, run)
        .await
        .map_err(|_elapsed| DiscRipError::MakemkvRipTimeout {
          device: display.clone(),
          timeout_secs: RIP_TIMEOUT.as_secs(),
        })??;
    let last_messages: Vec<String> = last_messages.into_iter().collect();
    if status.success() {
      Ok(RipOutput { last_messages })
    } else {
      Err(DiscRipError::MakemkvRipExit {
        device: display,
        status: status.to_string(),
        last_messages: last_messages.join(" | "),
      })
    }
  }

  async fn eject(&self, device: &Path) -> Result<(), TrayError> {
    run_eject(&self.eject, device, &[]).await
  }

  async fn unlock(&self, device: &Path) -> Result<(), TrayError> {
    run_eject(&self.eject, device, &["--manualeject", "off"]).await
  }
}

fn remember(ring: &mut VecDeque<String>, text: &str) {
  if ring.len() == LAST_MESSAGES {
    ring.pop_front();
  }
  ring.push_back(text.to_string());
}

/// MakeMKV's message flags are undocumented, so severity is a wording
/// heuristic: anything mentioning an error or failure is worth a warning;
/// the rest is diagnostic chatter.
fn log_message(code: u32, text: &str) {
  let lowered = text.to_ascii_lowercase();
  if lowered.contains("error") || lowered.contains("fail") {
    warn!(code, "makemkvcon: {text}");
  } else {
    debug!(code, "makemkvcon: {text}");
  }
}

async fn run_eject(
  binary: &Path,
  device: &Path,
  args: &[&str],
) -> Result<(), TrayError> {
  let display = device.display().to_string();
  let output = tokio::time::timeout(
    TRAY_TIMEOUT,
    Command::new(binary)
      .args(args)
      .arg(device)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::piped())
      .kill_on_drop(true)
      .output(),
  )
  .await
  .map_err(|_elapsed| TrayError::EjectTimeout {
    device: display.clone(),
    timeout_secs: TRAY_TIMEOUT.as_secs(),
  })?
  .map_err(|source| TrayError::EjectSpawn {
    device: display.clone(),
    source,
  })?;
  if output.status.success() {
    Ok(())
  } else {
    Err(TrayError::EjectExit {
      device: display,
      status: output.status.to_string(),
      stderr_tail: tail(&String::from_utf8_lossy(&output.stderr)),
    })
  }
}

/// The last few hundred characters of a command's output, for error
/// messages.
fn tail(text: &str) -> String {
  let trimmed = text.trim();
  let start = trimmed
    .char_indices()
    .rev()
    .nth(TAIL_CHARS.saturating_sub(1))
    .map_or(0, |(index, _)| index);
  trimmed[start..].to_string()
}

#[cfg(test)]
mod tests {
  use super::*;

  const UDEV_WITH_DISC: &str = "\
DEVNAME=/dev/sr0
ID_CDROM_MEDIA=1
ID_CDROM_MEDIA_DVD=1
ID_FS_LABEL=VOLTRON_S1_D1
ID_FS_LABEL_ENC=VOLTRON\\x20S1\\x20D1
ID_FS_UUID=2016-05-12-10-15-33-00
ID_FS_TYPE=udf
ID_FS_VOLUME_SET_ID=abc
ID_FS_APPLICATION_ID=SONIC
ID_FS_PUBLISHER_ID=
";

  #[test]
  fn udev_properties_describe_a_loaded_disc() {
    let facts = parse_udev_properties(UDEV_WITH_DISC).expect("media present");
    assert_eq!(facts.label.as_deref(), Some("VOLTRON S1 D1"));
    assert_eq!(facts.fs_uuid.as_deref(), Some("2016-05-12-10-15-33-00"));
    assert_eq!(facts.fs_type.as_deref(), Some("udf"));
    assert_eq!(facts.volume_set_id.as_deref(), Some("abc"));
    assert_eq!(facts.application_id.as_deref(), Some("SONIC"));
    assert_eq!(facts.publisher_id, None, "empty values are absent");
    assert!(facts.media_dvd);
    assert!(!facts.media_bd);
  }

  #[test]
  fn udev_properties_without_media_are_none() {
    assert_eq!(parse_udev_properties("DEVNAME=/dev/sr0\n"), None);
    assert_eq!(
      parse_udev_properties("DEVNAME=/dev/sr0\nID_CDROM_MEDIA=0\n"),
      None
    );
  }

  #[test]
  fn udev_label_falls_back_to_the_raw_property() {
    let facts =
      parse_udev_properties("ID_CDROM_MEDIA=1\nID_FS_LABEL=PLAIN\n").unwrap();
    assert_eq!(facts.label.as_deref(), Some("PLAIN"));
    let facts = parse_udev_properties("ID_CDROM_MEDIA=1\n").unwrap();
    assert_eq!(facts.label, None);
  }

  #[test]
  fn udev_escapes_decode_bytes() {
    assert_eq!(decode_udev_escapes("A\\x20B"), "A B");
    assert_eq!(decode_udev_escapes("caf\\xc3\\xa9"), "café");
    assert_eq!(decode_udev_escapes("trailing\\x2"), "trailing\\x2");
    assert_eq!(decode_udev_escapes("plain"), "plain");
  }

  const INFO: &str = "\
MSG:1005,0,1,\"MakeMKV v1.18.2 linux(x64-release) started\",\"%1 started\",\"MakeMKV v1.18.2 linux(x64-release)\"
MSG:3007,0,0,\"Using direct disc access mode\",\"Using direct disc access mode\"
DRV:0,2,999,12,\"BD-RE ASUS\",\"VOLTRON_S1_D1\",\"/dev/sr0\"
TCOUNT:2
CINFO:1,6209,\"DVD disc\"
CINFO:2,0,\"Voltron, Season 1: Disc 1\"
CINFO:32,0,\"VOLTRON_S1_D1\"
TINFO:0,2,0,\"Title 0\"
TINFO:0,8,0,\"5\"
TINFO:0,9,0,\"0:21:40\"
TINFO:0,11,0,\"1234567890\"
TINFO:0,24,0,\"1\"
TINFO:0,26,0,\"1\"
TINFO:0,27,0,\"title_t00.mkv\"
SINFO:0,0,1,6201,\"Video\"
TINFO:1,8,0,\"6\"
TINFO:1,9,0,\"1:32:15\"
TINFO:1,27,0,\"Weird \\\"Quoted\\\", Name_t01.mkv\"
garbage line
TINFO:junk
";

  #[test]
  fn info_dump_parses_titles_and_disc_facts() {
    let dump = parse_info(INFO);
    assert_eq!(dump.title_count, Some(2));
    assert_eq!(dump.disc_name.as_deref(), Some("Voltron, Season 1: Disc 1"));
    assert_eq!(dump.volume_name.as_deref(), Some("VOLTRON_S1_D1"));
    assert_eq!(dump.makemkv_version.as_deref(), Some("1.18.2"));
    assert_eq!(dump.tinfo_lines.len(), 11, "every TINFO line is kept raw");
    assert_eq!(dump.titles.len(), 2);
    assert_eq!(
      dump.titles[0],
      TitleInfo {
        index: 0,
        output_file: Some("title_t00.mkv".to_string()),
        duration_secs: Some(1300),
        chapters: Some(5),
        size_bytes: Some(1_234_567_890),
        segments_map: Some("1".to_string()),
        original_title_id: Some("1".to_string()),
      }
    );
    assert_eq!(dump.titles[1].duration_secs, Some(5535));
    assert_eq!(
      dump.titles[1].output_file.as_deref(),
      Some("Weird \"Quoted\", Name_t01.mkv"),
      "quoted commas and escaped quotes survive"
    );
  }

  #[test]
  fn info_dump_tolerates_missing_count() {
    let dump = parse_info("CINFO:2,0,\"Name\"\n");
    assert_eq!(dump.title_count, None);
    assert!(dump.titles.is_empty());
  }

  #[test]
  fn durations_parse_both_forms() {
    assert_eq!(duration_secs("1:32:15"), Some(5535));
    assert_eq!(duration_secs("21:40"), Some(1300));
    assert_eq!(duration_secs("0:00:00"), Some(0));
    assert_eq!(duration_secs("nope"), None);
    assert_eq!(duration_secs("1:2:3:4"), None);
  }

  #[test]
  fn progress_lines_parse() {
    assert_eq!(
      parse_progress_line("PRGV:1234,5678,65536"),
      Some(RipEvent::Progress {
        current: 1234,
        total: 5678,
        max: 65536
      })
    );
    assert_eq!(
      parse_progress_line("PRGC:5017,0,\"Saving to MKV file\""),
      Some(RipEvent::CurrentOp("Saving to MKV file".to_string()))
    );
    assert_eq!(
      parse_progress_line("PRGT:5018,0,\"Saving all titles to MKV files\""),
      Some(RipEvent::TotalOp("Saving all titles to MKV files".to_string()))
    );
    assert_eq!(
      parse_progress_line(
        "MSG:5003,0,2,\"Copy complete. 2 titles saved.\",\"%1\",\"x\""
      ),
      Some(RipEvent::Message {
        code: 5003,
        text: "Copy complete. 2 titles saved.".to_string()
      })
    );
    assert_eq!(parse_progress_line("PRGV:oops"), None);
    assert_eq!(parse_progress_line("DRV:0,2,999"), None);
  }

  #[test]
  fn permille_is_bounded() {
    assert_eq!(progress_permille(0, 0), 0);
    assert_eq!(progress_permille(32768, 65536), 500);
    assert_eq!(progress_permille(70000, 65536), 1000);
  }

  #[test]
  fn tail_keeps_the_end() {
    let long = "a".repeat(600) + "END";
    let tailed = tail(&long);
    assert_eq!(tailed.len(), TAIL_CHARS);
    assert!(tailed.ends_with("END"));
    assert_eq!(tail("  short  "), "short");
  }
}
