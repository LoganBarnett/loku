//! The ffmpeg/ffprobe boundary: a dyn-safe async trait so the rest of the
//! system (scanner probe pass, and later the compat-derivation worker)
//! neither knows nor cares whether a real binary or a test fake answers.

use async_trait::async_trait;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;

use crate::media::compat::{AudioAction, CompatPlan, VideoAction};

/// A probe should be near-instant; anything past this is a hung binary or a
/// pathological file, and the worker must not stall behind it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// `-version` checks are a liveness test, not real work.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// A full-length transcode of a long movie on modest hardware takes hours;
/// this only exists to reap a genuinely hung encode.
const DERIVE_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

/// Thumbnail extraction decodes a handful of frames.
const THUMBNAIL_TIMEOUT: Duration = Duration::from_secs(60);

/// Codec and container facts ffprobe reports for one media file.  All fields
/// optional: a damaged file may yield any subset, and an all-`None` result is
/// the "probed, nothing usable" marker.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProbeResult {
  /// ffprobe `codec_name` (`h264`, `hevc`, `mpeg2video`, `vc1`, …).
  pub vcodec: Option<String>,
  /// ffprobe `codec_name` for the first audio stream (`aac`, `ac3`, `dts`).
  pub acodec: Option<String>,
  /// ffprobe `profile` for the video stream (`High`, `High 10`, `Main`).
  pub vprofile: Option<String>,
  /// ffprobe `field_order`: `progressive`, or interlaced variants
  /// (`tt`/`bb`/`tb`/`bt`) that will need deinterlacing in a compat encode.
  pub field_order: Option<String>,
  pub duration_secs: Option<f64>,
  pub width: Option<i64>,
  pub height: Option<i64>,
}

#[derive(Debug, Error)]
pub enum ProbeError {
  #[error("Failed to spawn ffprobe for '{path}': {source}")]
  FfprobeSpawn {
    path: String,
    source: std::io::Error,
  },

  #[error("ffprobe timed out after {timeout_secs}s probing '{path}'")]
  FfprobeTimeout { path: String, timeout_secs: u64 },

  #[error("ffprobe failed on '{path}' ({status}): {stderr_tail}")]
  FfprobeExit {
    path: String,
    status: String,
    stderr_tail: String,
  },

  #[error("Failed to parse ffprobe output for '{path}': {source}")]
  FfprobeOutputParse {
    path: String,
    source: serde_json::Error,
  },
}

impl ProbeError {
  /// Whether the failure is a fact about the *file* (safe to record so the
  /// file is not re-probed every pass) rather than a transient condition of
  /// the environment (worth retrying on the next pass).
  pub fn is_definitive(&self) -> bool {
    matches!(
      self,
      ProbeError::FfprobeExit { .. } | ProbeError::FfprobeOutputParse { .. }
    )
  }
}

#[derive(Debug, Error)]
pub enum MediaToolsError {
  #[error("{tool} is not runnable: {source}")]
  ToolMissing {
    tool: &'static str,
    source: std::io::Error,
  },

  #[error("{tool} -version timed out after {timeout_secs}s")]
  ToolVersionTimeout {
    tool: &'static str,
    timeout_secs: u64,
  },

  #[error("{tool} -version exited with {status}")]
  ToolVersionFailed { tool: &'static str, status: String },
}

#[derive(Debug, Error)]
pub enum DeriveError {
  #[error("Failed to spawn ffmpeg for '{path}': {source}")]
  FfmpegSpawn {
    path: String,
    source: std::io::Error,
  },

  #[error("ffmpeg timed out after {timeout_secs}s deriving from '{path}'")]
  FfmpegTimeout { path: String, timeout_secs: u64 },

  #[error("ffmpeg failed on '{path}' ({status}): {stderr_tail}")]
  FfmpegExit {
    path: String,
    status: String,
    stderr_tail: String,
  },
}

/// The operations Loku drives external media tooling for.  Implemented by
/// [`RealFfmpeg`] in production and by fakes in tests.
#[async_trait]
pub trait FfmpegRunner: Send + Sync {
  async fn probe(&self, path: &Path) -> Result<ProbeResult, ProbeError>;

  /// Produce a browser-compatible MP4 at `dest` from `source` per the plan.
  async fn derive_compat(
    &self,
    source: &Path,
    dest: &Path,
    plan: &CompatPlan,
  ) -> Result<(), DeriveError>;

  /// Extract a single representative frame from `source` as a JPEG at
  /// `dest`.
  async fn extract_thumbnail(
    &self,
    source: &Path,
    dest: &Path,
    at_secs: f64,
  ) -> Result<(), DeriveError>;
}

/// Drives the real `ffprobe`/`ffmpeg` binaries found on `PATH` (the NixOS
/// module and dev shell both provide them).
pub struct RealFfmpeg {
  ffprobe: PathBuf,
  ffmpeg: PathBuf,
}

impl RealFfmpeg {
  /// Verify both binaries are runnable, so degraded mode is decided once at
  /// startup instead of surfacing as per-file failures later.
  pub async fn detect() -> Result<Self, MediaToolsError> {
    let runner = Self {
      ffprobe: PathBuf::from("ffprobe"),
      ffmpeg: PathBuf::from("ffmpeg"),
    };
    version_check("ffprobe", &runner.ffprobe).await?;
    version_check("ffmpeg", &runner.ffmpeg).await?;
    Ok(runner)
  }

  /// The ffmpeg binary path, for the derivation worker.
  pub fn ffmpeg_path(&self) -> &Path {
    &self.ffmpeg
  }
}

#[async_trait]
impl FfmpegRunner for RealFfmpeg {
  async fn probe(&self, path: &Path) -> Result<ProbeResult, ProbeError> {
    let display_path = path.to_string_lossy().to_string();
    let output = tokio::time::timeout(
      PROBE_TIMEOUT,
      Command::new(&self.ffprobe)
        .arg("-loglevel")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_streams")
        .arg("-show_format")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // On timeout the future is dropped, which must kill the child
        // rather than orphan it.
        .kill_on_drop(true)
        .output(),
    )
    .await
    .map_err(|_elapsed| ProbeError::FfprobeTimeout {
      path: display_path.clone(),
      timeout_secs: PROBE_TIMEOUT.as_secs(),
    })?
    .map_err(|source| ProbeError::FfprobeSpawn {
      path: display_path.clone(),
      source,
    })?;

    if !output.status.success() {
      return Err(ProbeError::FfprobeExit {
        path: display_path,
        status: output.status.to_string(),
        stderr_tail: tail(&String::from_utf8_lossy(&output.stderr)),
      });
    }

    serde_json::from_slice::<FfprobeOutput>(&output.stdout)
      .map(ProbeResult::from)
      .map_err(|source| ProbeError::FfprobeOutputParse {
        path: display_path,
        source,
      })
  }

  async fn derive_compat(
    &self,
    source: &Path,
    dest: &Path,
    plan: &CompatPlan,
  ) -> Result<(), DeriveError> {
    // ffmpeg has no long-form aliases for these flags: -y = overwrite
    // output, -i = input, -map = stream selection (0:v:0 first video, 0:a:0?
    // first audio if present), -sn/-dn = drop subtitle/data streams (MP4
    // handles disc subtitle formats poorly and the master keeps them),
    // -c:v/-c:a = codec selection, -vf = video filter, -preset/-crf =
    // libx264 speed/quality trade-off, -b:a = audio bitrate, -movflags
    // +faststart = relocate the index atom so playback starts before the
    // whole file downloads.
    let mut command = Command::new(&self.ffmpeg);
    command
      .arg("-loglevel")
      .arg("error")
      .arg("-y")
      .arg("-i")
      .arg(source);
    match &plan.video {
      VideoAction::Copy => {
        command.arg("-c:v").arg("copy");
      }
      VideoAction::Transcode { deinterlace } => {
        if *deinterlace {
          command.arg("-vf").arg("bwdif");
        }
        command
          .arg("-c:v")
          .arg("libx264")
          .arg("-preset")
          .arg("medium")
          .arg("-crf")
          .arg("20");
      }
    }
    match &plan.audio {
      AudioAction::Copy => {
        command.arg("-c:a").arg("copy");
      }
      AudioAction::Transcode => {
        command.arg("-c:a").arg("aac").arg("-b:a").arg("192k");
      }
    }
    command
      .arg("-map")
      .arg("0:v:0")
      .arg("-map")
      .arg("0:a:0?")
      .arg("-sn")
      .arg("-dn")
      .arg("-movflags")
      .arg("+faststart")
      .arg(dest);
    run_ffmpeg(command, source, DERIVE_TIMEOUT).await
  }

  async fn extract_thumbnail(
    &self,
    source: &Path,
    dest: &Path,
    at_secs: f64,
  ) -> Result<(), DeriveError> {
    // Flags without long forms: -ss = seek before decode, -i = input,
    // -frames:v 1 = emit one video frame, -vf scale = bound the thumbnail
    // width (height follows aspect, kept even for codec safety), -q:v =
    // JPEG quality, -y = overwrite.
    let mut command = Command::new(&self.ffmpeg);
    command
      .arg("-loglevel")
      .arg("error")
      .arg("-y")
      .arg("-ss")
      .arg(format!("{at_secs:.3}"))
      .arg("-i")
      .arg(source)
      .arg("-frames:v")
      .arg("1")
      .arg("-vf")
      .arg("scale=640:-2")
      .arg("-q:v")
      .arg("3")
      .arg(dest);
    run_ffmpeg(command, source, THUMBNAIL_TIMEOUT).await
  }
}

/// Run a prepared ffmpeg command with a timeout, mapping failures to
/// semantic errors.
async fn run_ffmpeg(
  mut command: Command,
  source: &Path,
  timeout: Duration,
) -> Result<(), DeriveError> {
  let display_path = source.to_string_lossy().to_string();
  let output = tokio::time::timeout(
    timeout,
    command
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::piped())
      // On timeout or shutdown the future is dropped, which must kill the
      // encoder rather than orphan it.
      .kill_on_drop(true)
      .output(),
  )
  .await
  .map_err(|_elapsed| DeriveError::FfmpegTimeout {
    path: display_path.clone(),
    timeout_secs: timeout.as_secs(),
  })?
  .map_err(|source| DeriveError::FfmpegSpawn {
    path: display_path.clone(),
    source,
  })?;

  if output.status.success() {
    Ok(())
  } else {
    Err(DeriveError::FfmpegExit {
      path: display_path,
      status: output.status.to_string(),
      stderr_tail: tail(&String::from_utf8_lossy(&output.stderr)),
    })
  }
}

async fn version_check(
  tool: &'static str,
  binary: &Path,
) -> Result<(), MediaToolsError> {
  let output = tokio::time::timeout(
    VERSION_TIMEOUT,
    Command::new(binary)
      .arg("-version")
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .output(),
  )
  .await
  .map_err(|_elapsed| MediaToolsError::ToolVersionTimeout {
    tool,
    timeout_secs: VERSION_TIMEOUT.as_secs(),
  })?
  .map_err(|source| MediaToolsError::ToolMissing { tool, source })?;

  if output.status.success() {
    Ok(())
  } else {
    Err(MediaToolsError::ToolVersionFailed {
      tool,
      status: output.status.to_string(),
    })
  }
}

/// The last stretch of stderr — enough to diagnose, small enough to log.
fn tail(text: &str) -> String {
  const TAIL_CHARS: usize = 500;
  text
    .char_indices()
    .rev()
    .nth(TAIL_CHARS.saturating_sub(1))
    .map_or_else(|| text.to_string(), |(idx, _)| text[idx..].to_string())
}

/// Candidate types for ffprobe's JSON, deserialized liberally: every field
/// optional, unknown fields ignored.
#[derive(Debug, Deserialize)]
struct FfprobeOutput {
  #[serde(default)]
  streams: Vec<FfprobeStream>,
  format: Option<FfprobeFormat>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
  codec_type: Option<String>,
  codec_name: Option<String>,
  profile: Option<String>,
  field_order: Option<String>,
  width: Option<i64>,
  height: Option<i64>,
  disposition: Option<FfprobeDisposition>,
}

#[derive(Debug, Deserialize)]
struct FfprobeDisposition {
  attached_pic: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
  /// ffprobe emits the duration as a decimal string.
  duration: Option<String>,
}

impl From<FfprobeOutput> for ProbeResult {
  fn from(output: FfprobeOutput) -> Self {
    // Embedded cover art shows up as a video stream flagged attached_pic;
    // it must not shadow the actual movie stream.
    let video = output.streams.iter().find(|s| {
      s.codec_type.as_deref() == Some("video")
        && s
          .disposition
          .as_ref()
          .and_then(|d| d.attached_pic)
          .unwrap_or(0)
          == 0
    });
    let audio = output
      .streams
      .iter()
      .find(|s| s.codec_type.as_deref() == Some("audio"));
    ProbeResult {
      vcodec: video.and_then(|s| s.codec_name.clone()),
      acodec: audio.and_then(|s| s.codec_name.clone()),
      vprofile: video.and_then(|s| s.profile.clone()),
      field_order: video.and_then(|s| s.field_order.clone()),
      // The .ok() discard is deliberate: an unparseable duration string is
      // ffprobe mess that degrades to absent, per the liberal-parse policy.
      duration_secs: output
        .format
        .and_then(|f| f.duration)
        .and_then(|d| d.parse().ok()),
      width: video.and_then(|s| s.width),
      height: video.and_then(|s| s.height),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn probe_result_takes_first_real_video_stream() {
    let raw = r#"{
      "streams": [
        {"codec_type": "video", "codec_name": "mjpeg",
         "disposition": {"attached_pic": 1}},
        {"codec_type": "video", "codec_name": "h264", "profile": "High",
         "field_order": "progressive", "width": 1920, "height": 1080,
         "disposition": {"attached_pic": 0}},
        {"codec_type": "audio", "codec_name": "ac3"}
      ],
      "format": {"duration": "5400.021000"}
    }"#;
    let parsed: FfprobeOutput = serde_json::from_str(raw).unwrap();
    let result = ProbeResult::from(parsed);
    assert_eq!(result.vcodec.as_deref(), Some("h264"));
    assert_eq!(result.acodec.as_deref(), Some("ac3"));
    assert_eq!(result.vprofile.as_deref(), Some("High"));
    assert_eq!(result.field_order.as_deref(), Some("progressive"));
    assert_eq!(result.width, Some(1920));
    assert_eq!(result.height, Some(1080));
    assert!((result.duration_secs.unwrap() - 5400.021).abs() < 0.001);
  }

  #[test]
  fn probe_result_tolerates_sparse_output() {
    let parsed: FfprobeOutput = serde_json::from_str("{}").unwrap();
    assert_eq!(ProbeResult::from(parsed), ProbeResult::default());
  }

  #[test]
  fn tail_keeps_the_end_of_long_text() {
    let long = "a".repeat(600) + "END";
    let tailed = tail(&long);
    assert!(tailed.len() <= 500);
    assert!(tailed.ends_with("END"));
    assert_eq!(tail("short"), "short");
  }
}
