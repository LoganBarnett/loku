//! The probe pass: fill codec facts for indexed videos that have none.
//!
//! yt-dlp downloads carry codecs in `info.json`; disc rips (and anything
//! else without a sidecar) get them from ffprobe here.  Results are cached
//! keyed on (size, mtime), so a file is probed once per content change no
//! matter how many scans or rebuilds happen around it.

use futures_util::stream::{self, StreamExt};
use tracing::warn;

use crate::index::store::{Index, ProbeCandidate};
use crate::library::Library;
use crate::media::ffmpeg::{FfmpegRunner, ProbeResult};

/// Probe every video still lacking codec facts.  Serial by design — one
/// ffprobe at a time keeps the load negligible.  Individual failures warn
/// and move on; a transient failure retries on the next pass, while a
/// definitive one (the file itself is unprobeable) records an empty result
/// so the file is not re-probed forever.  Returns how many videos gained
/// facts.
pub async fn probe_pending(
  index: &Index,
  libraries: &[Library],
  runner: &dyn FfmpegRunner,
) -> usize {
  match index.videos_needing_probe().await {
    Ok(candidates) => {
      stream::iter(candidates)
        .fold(0, |resolved, candidate| async move {
          resolved + probe_candidate(index, libraries, runner, candidate).await
        })
        .await
    }
    Err(error) => {
      warn!(%error, "Skipping probe pass; could not list unprobed videos");
      0
    }
  }
}

/// Resolve one candidate — from the cache when its exact content (same size
/// and mtime) was probed before, else by running ffprobe.  Returns how many
/// videos gained facts (one or zero), so the caller can fold a total.
async fn probe_candidate(
  index: &Index,
  libraries: &[Library],
  runner: &dyn FfmpegRunner,
  candidate: ProbeCandidate,
) -> usize {
  let Some(library) = libraries.iter().find(|l| l.name == candidate.library)
  else {
    return 0;
  };
  match index.cached_probe(&candidate).await {
    Ok(Some(cached)) => recorded(index, &candidate, &cached).await,
    Ok(None) => {
      let path = library.path.join(&candidate.rel_path);
      match runner.probe(&path).await {
        Ok(result) => recorded(index, &candidate, &result).await,
        Err(error) if error.is_definitive() => {
          warn!(path = %candidate.rel_path, %error, "File is unprobeable; recording empty codec facts");
          // An empty result marks the row as probed, so it degrades to
          // compat-or-bust instead of being retried every pass.  It does
          // not count as a video gaining facts.
          recorded(index, &candidate, &ProbeResult::default()).await;
          0
        }
        Err(error) => {
          warn!(path = %candidate.rel_path, %error, "Probe failed transiently; will retry next pass");
          0
        }
      }
    }
    Err(error) => {
      warn!(path = %candidate.rel_path, %error, "Failed to consult the probe cache");
      0
    }
  }
}

/// Apply a probe result to the index, returning how many rows gained facts
/// (one, or zero when recording itself failed — which warns and leaves the
/// row for the next pass).
async fn recorded(
  index: &Index,
  candidate: &ProbeCandidate,
  probe: &ProbeResult,
) -> usize {
  match index.record_probe(candidate, probe).await {
    Ok(()) => 1,
    Err(error) => {
      warn!(path = %candidate.rel_path, %error, "Failed to record probe result");
      0
    }
  }
}
