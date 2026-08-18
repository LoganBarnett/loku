//! The serial derivation worker: produces compat copies and thumbnails for
//! indexed videos that need them, one job at a time.
//!
//! Serial by construction — a single task processes one ffmpeg child at a
//! time, so a transcode can never starve the box.  Idempotency comes from
//! on-disk truth: output goes to a `.part` file renamed into place only on
//! success (atomic within the same directory), the candidate query is keyed
//! on what exists (no compat yet, not failed at this exact size and mtime),
//! and a crash mid-job leaves only an ignored `.part` that the next attempt
//! deletes.  Nothing can be "stuck in processing" across a restart because
//! the processing state lives only in this task's watch channel.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{watch, Notify};
use tracing::{info, warn};

use crate::index::store::{Index, VideoRecord};
use crate::library::{Library, LibraryKind};
use crate::media::compat::{self, CompatInputs, CompatPlan};
use crate::media::ffmpeg::{DeriveError, FfmpegRunner};
use crate::media::sidecars::sidecar_path;

/// The item currently being derived, published for status display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveDerivation {
  pub library: String,
  pub rel_path: String,
}

#[derive(Debug, Error)]
enum JobError {
  #[error(transparent)]
  Derive(#[from] DeriveError),

  #[error(
    "Failed to move finished derivation into place at '{path}': {source}"
  )]
  FinalizeRename {
    path: String,
    source: std::io::Error,
  },
}

/// Spawn the worker task.  Returns the status receiver (the sender side uses
/// `send_replace`, so the worker runs fine even before anything subscribes).
pub fn spawn(
  index: Index,
  libraries: Vec<Library>,
  runner: Arc<dyn FfmpegRunner>,
  wake: Arc<Notify>,
) -> watch::Receiver<Option<ActiveDerivation>> {
  let (sender, receiver) = watch::channel(None);
  tokio::spawn(async move {
    loop {
      let worked =
        process_next(&index, &libraries, runner.as_ref(), &sender).await;
      if !worked {
        wake.notified().await;
      }
    }
  });
  receiver
}

/// Take on the next job, if any: a compat derivation (which also fills a
/// missing thumbnail) or a thumbnail alone for an already-playable video.
/// Returns whether an attempt was made — a recorded failure counts, since
/// the failure row excludes the item from the next candidate pass.
pub async fn process_next(
  index: &Index,
  libraries: &[Library],
  runner: &dyn FfmpegRunner,
  status: &watch::Sender<Option<ActiveDerivation>>,
) -> bool {
  let candidates = match index.derivation_candidates().await {
    Ok(candidates) => candidates,
    Err(error) => {
      warn!(%error, "Skipping derivation pass; could not list candidates");
      return false;
    }
  };

  // Disc rips first: they are the items with no playable representation at
  // all, so they are where the payoff lands.
  let ordered_libraries = libraries
    .iter()
    .filter(|l| l.kind == LibraryKind::Discs)
    .chain(libraries.iter().filter(|l| l.kind != LibraryKind::Discs));

  let job = ordered_libraries
    .flat_map(|library| {
      candidates
        .iter()
        .filter(move |record| record.library == library.name)
        .map(move |record| (library, record))
    })
    .find_map(|(library, record)| {
      match compat::decide(&CompatInputs {
        container: &record.container,
        vcodec: record.vcodec.as_deref(),
        vprofile: record.vprofile.as_deref(),
        acodec: record.acodec.as_deref(),
        field_order: record.field_order.as_deref(),
        has_compat: record.compat_rel_path.is_some(),
      }) {
        compat::DerivationDecision::Produce(plan) => {
          Some((library, record.clone(), Some(plan)))
        }
        compat::DerivationDecision::NotNeeded
          if record.thumb_rel_path.is_none() =>
        {
          Some((library, record.clone(), None))
        }
        _ => None,
      }
    });

  let Some((library, record, plan)) = job else {
    return false;
  };

  status.send_replace(Some(ActiveDerivation {
    library: record.library.clone(),
    rel_path: record.rel_path.clone(),
  }));
  run_job(index, library, &record, plan, runner).await;
  status.send_replace(None);
  true
}

async fn run_job(
  index: &Index,
  library: &Library,
  record: &VideoRecord,
  plan: Option<CompatPlan>,
  runner: &dyn FfmpegRunner,
) {
  let source = library.path.join(&record.rel_path);
  if let Some(plan) = plan {
    match derive_compat(&source, &plan, runner).await {
      Ok(compat_name) => {
        let compat_rel = sibling_rel(&record.rel_path, &compat_name);
        info!(
          library = %record.library,
          path = %record.rel_path,
          "Compat derivation complete"
        );
        if let Err(error) = index
          .set_compat(&record.library, &record.rel_path, &compat_rel)
          .await
        {
          // The filesystem watcher folds the new file in regardless; the
          // direct update just makes the status visible sooner.
          warn!(path = %record.rel_path, %error, "Failed to record new compat copy");
        }
      }
      Err(error) => {
        warn!(
          library = %record.library,
          path = %record.rel_path,
          %error,
          "Compat derivation failed"
        );
        if let Err(record_error) = index
          .record_derivation_failure(
            &record.library,
            &record.rel_path,
            record.size,
            record.mtime,
            &error.to_string(),
          )
          .await
        {
          warn!(path = %record.rel_path, error = %record_error, "Failed to record derivation failure");
        }
        return;
      }
    }
  }

  if record.thumb_rel_path.is_none() {
    match extract_thumbnail(&source, record.duration_secs, runner).await {
      Ok(thumb_name) => {
        let thumb_rel = sibling_rel(&record.rel_path, &thumb_name);
        if let Err(error) = index
          .set_thumb(&record.library, &record.rel_path, &thumb_rel)
          .await
        {
          warn!(path = %record.rel_path, %error, "Failed to record new thumbnail");
        }
      }
      Err(error) => {
        // Thumbnails are cosmetic; a failure is logged and, for
        // thumbnail-only jobs, recorded so the item is not retried every
        // wake.  (A compat failure above already recorded the item.)
        warn!(
          library = %record.library,
          path = %record.rel_path,
          %error,
          "Thumbnail extraction failed"
        );
        if let Err(record_error) = index
          .record_derivation_failure(
            &record.library,
            &record.rel_path,
            record.size,
            record.mtime,
            &error.to_string(),
          )
          .await
        {
          warn!(path = %record.rel_path, error = %record_error, "Failed to record thumbnail failure");
        }
      }
    }
  }
}

/// Derive the compat copy beside the master via a `.part` rename.  Returns
/// the produced file's name.
async fn derive_compat(
  source: &Path,
  plan: &CompatPlan,
  runner: &dyn FfmpegRunner,
) -> Result<String, JobError> {
  let (part, dest) = sibling_paths(source, ".compat.mp4");
  remove_stale_part(&part).await;
  runner.derive_compat(source, &part, plan).await?;
  tokio::fs::rename(&part, &dest).await.map_err(|source| {
    JobError::FinalizeRename {
      path: dest.to_string_lossy().to_string(),
      source,
    }
  })?;
  Ok(file_name_of(&dest))
}

/// Extract a thumbnail frame at roughly 10% into the video (a plain 1s seek
/// when the duration is unknown, so a short file still yields a frame).
async fn extract_thumbnail(
  source: &Path,
  duration_secs: Option<f64>,
  runner: &dyn FfmpegRunner,
) -> Result<String, JobError> {
  let at_secs = duration_secs.map_or(1.0, |d| (d * 0.1).max(1.0));
  let (part, dest) = sibling_paths(source, ".jpg");
  remove_stale_part(&part).await;
  runner.extract_thumbnail(source, &part, at_secs).await?;
  tokio::fs::rename(&part, &dest).await.map_err(|source| {
    JobError::FinalizeRename {
      path: dest.to_string_lossy().to_string(),
      source,
    }
  })?;
  Ok(file_name_of(&dest))
}

/// The `.part` staging path and final path for a sidecar of `source` with
/// the given suffix, per the stem-appending sidecar convention.
fn sibling_paths(source: &Path, suffix: &str) -> (PathBuf, PathBuf) {
  let parent = source.parent().unwrap_or(Path::new(""));
  let stem = source.file_stem().unwrap_or_default();
  (
    sidecar_path(parent, stem, &format!("{suffix}.part")),
    sidecar_path(parent, stem, suffix),
  )
}

/// Remove a stale `.part` from an interrupted earlier attempt.  Absence is
/// the normal case; any other failure is only worth a warning because the
/// derivation itself will surface a real permission problem immediately.
async fn remove_stale_part(part: &Path) {
  match tokio::fs::remove_file(part).await {
    Ok(()) => {
      info!(path = %part.display(), "Removed stale partial derivation");
    }
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
    Err(error) => {
      warn!(path = %part.display(), %error, "Could not remove stale partial derivation");
    }
  }
}

fn file_name_of(path: &Path) -> String {
  path
    .file_name()
    .unwrap_or_default()
    .to_string_lossy()
    .to_string()
}

/// The library-relative path of a sibling file: the record's relative path
/// with its file name swapped.
fn sibling_rel(rel_path: &str, sibling_name: &str) -> String {
  Path::new(rel_path)
    .with_file_name(sibling_name)
    .to_string_lossy()
    .to_string()
}
