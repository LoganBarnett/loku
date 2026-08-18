//! Index maintenance: the initial scan plus filesystem-event-driven
//! incremental updates.
//!
//! One spawned task owns all index writes in order — the initial full scan
//! first, then debounced directory reconciles as events arrive — so a full
//! scan and a watcher flush can never interleave.  Watchers are registered
//! *before* the scan starts: events raced by the scan simply queue in the
//! channel and replay afterwards, so nothing falls into a gap.
//!
//! Reconciling writes only to the database, never the filesystem, so a
//! flush triggered by our own derived-file writes terminates in one cycle
//! rather than feeding back.
//!
//! The watchers rely on local-filesystem semantics (FSEvents/inotify); both
//! deployment roots are local disks on the serving host, with NFS consumers
//! mounting *from* it.  If a remote-mounted library ever appears, add a
//! periodic full rescan instead of trusting events.

use futures_util::stream::{self, StreamExt};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tap::Tap;
use thiserror::Error;
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::index::scan;
use crate::index::store::{now_secs, Index};
use crate::library::Library;
use crate::media::ffmpeg::FfmpegRunner;
use crate::media::probe;

/// Quiet period after the last event before a flush; long enough to absorb
/// the bursts a download or rip finalization produces.
const DEBOUNCE_QUIET: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum WatchError {
  #[error("Failed to create the filesystem watcher: {source}")]
  WatcherCreate { source: notify::Error },

  #[error("Failed to watch library root '{path}': {source}")]
  WatchRoot { path: String, source: notify::Error },
}

/// Register watchers on every library root, then spawn the maintenance task
/// (initial scan followed by the event loop).  Returns after registration so
/// watcher misconfiguration surfaces at startup.
pub fn spawn_maintenance(
  index: Index,
  libraries: Vec<Library>,
  last_scan_completed: Arc<AtomicI64>,
  ffmpeg: Option<Arc<dyn FfmpegRunner>>,
  worker_wake: Arc<Notify>,
) -> Result<(), WatchError> {
  let (sender, receiver) = mpsc::unbounded_channel::<notify::Event>();
  let mut watcher = notify::recommended_watcher(
    move |result: Result<notify::Event, notify::Error>| match result {
      // The send only fails once the maintenance task is gone, at which
      // point events have nowhere to go anyway.
      Ok(event) => drop(sender.send(event)),
      Err(error) => warn!(%error, "Filesystem watcher reported an error"),
    },
  )
  .map_err(|source| WatchError::WatcherCreate { source })?;

  // Watch canonical roots: macOS FSEvents reports resolved paths (e.g.
  // /private/tmp for /tmp), and the event-to-library mapping does prefix
  // matches against these same values.
  let roots: Vec<(Library, PathBuf)> = libraries
    .iter()
    .map(|library| {
      let canonical = library
        .path
        .canonicalize()
        .unwrap_or_else(|_| library.path.clone());
      (library.clone(), canonical)
    })
    .collect();
  roots.iter().try_for_each(|(library, canonical)| {
    watcher
      .watch(canonical, RecursiveMode::Recursive)
      .map_err(|source| WatchError::WatchRoot {
        path: library.path.to_string_lossy().to_string(),
        source,
      })
  })?;

  tokio::spawn(run(
    watcher,
    receiver,
    Maintenance {
      index,
      libraries,
      roots,
      last_scan_completed,
      ffmpeg,
      worker_wake,
    },
  ));
  Ok(())
}

/// Everything the maintenance loop operates on, bundled so the loop and its
/// spawner stay readable as the collaborator set grows.
struct Maintenance {
  index: Index,
  libraries: Vec<Library>,
  roots: Vec<(Library, PathBuf)>,
  last_scan_completed: Arc<AtomicI64>,
  ffmpeg: Option<Arc<dyn FfmpegRunner>>,
  worker_wake: Arc<Notify>,
}

/// Fallback for when filesystem watching is unavailable (e.g. inotify
/// limits): run the initial scan and probe pass in the background, after
/// which the index only updates on restart.
pub fn spawn_scan_only(
  index: Index,
  libraries: Vec<Library>,
  last_scan_completed: Arc<AtomicI64>,
  ffmpeg: Option<Arc<dyn FfmpegRunner>>,
  worker_wake: Arc<Notify>,
) {
  tokio::spawn(async move {
    initial_scan(&index, &libraries, &last_scan_completed).await;
    probe_if_available(&index, &libraries, ffmpeg.as_deref()).await;
    worker_wake.notify_one();
  });
}

/// Run the full scan and record its completion for the health check.
async fn initial_scan(
  index: &Index,
  libraries: &[Library],
  last_scan_completed: &AtomicI64,
) {
  match scan::scan_all(index, libraries).await {
    Ok(stats) => {
      last_scan_completed.store(now_secs(), Ordering::Relaxed);
      info!(
        directories = stats.directories,
        videos = stats.videos,
        removed = stats.removed,
        "Initial library scan complete"
      );
    }
    Err(error) => {
      error!(%error, "Initial library scan failed");
    }
  }
}

/// Run the probe pass when the media tools are available; without them the
/// index simply keeps serving with unknown codecs (compat-or-bust items).
async fn probe_if_available(
  index: &Index,
  libraries: &[Library],
  ffmpeg: Option<&dyn FfmpegRunner>,
) {
  if let Some(runner) = ffmpeg {
    let resolved = probe::probe_pending(index, libraries, runner).await;
    if resolved > 0 {
      info!(resolved, "Codec probe pass complete");
    }
  }
}

/// The maintenance loop.  Owns the watcher (dropping it would silence the
/// event stream).
///
/// The probe pass runs inline after the scan and after each flush, so a long
/// first probe of a large backlog delays event *processing* — never event
/// *delivery*, since events queue in the channel and flush afterwards.  The
/// scan-complete health signal fires before probing starts.
async fn run(
  _watcher: RecommendedWatcher,
  mut receiver: mpsc::UnboundedReceiver<notify::Event>,
  ctx: Maintenance,
) {
  initial_scan(&ctx.index, &ctx.libraries, &ctx.last_scan_completed).await;
  probe_if_available(&ctx.index, &ctx.libraries, ctx.ffmpeg.as_deref()).await;
  ctx.worker_wake.notify_one();

  let mut debounce = Debounce::new(DEBOUNCE_QUIET);
  loop {
    let deadline = debounce.deadline();
    tokio::select! {
      received = receiver.recv() => {
        match received {
          Some(event) => {
            let now = Instant::now();
            dirty_directories(&event, &ctx.roots)
              .into_iter()
              .for_each(|key| debounce.record(key, now));
          }
          None => break,
        }
      }
      // A pending deadline wakes the loop to flush; with nothing pending the
      // branch is disabled and the loop just waits for events.
      () = async {
        match deadline {
          Some(at) => tokio::time::sleep_until(at).await,
          None => std::future::pending().await,
        }
      } => {
        flush(&ctx.index, &ctx.roots, debounce.take()).await;
        probe_if_available(&ctx.index, &ctx.libraries, ctx.ffmpeg.as_deref())
          .await;
        ctx.worker_wake.notify_one();
      }
    }
  }
}

/// Map one filesystem event to the (library, directory) candidates that need
/// attention.  Each path contributes both itself and its parent: the path
/// itself may be a created or *vanished* directory (whose deletion event is
/// often the only signal a whole tree is gone), while the parent covers the
/// ordinary changed-file case.  Non-directory candidates are cheap no-ops at
/// flush time.  Access-only events and in-progress `.part` files are
/// ignored.
fn dirty_directories(
  event: &notify::Event,
  roots: &[(Library, PathBuf)],
) -> Vec<(usize, PathBuf)> {
  if matches!(event.kind, notify::EventKind::Access(_)) {
    return Vec::new();
  }
  event
    .paths
    .iter()
    .filter(|path| {
      path
        .extension()
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("part"))
    })
    .flat_map(|path| {
      std::iter::once(path.clone()).chain(path.parent().map(Path::to_path_buf))
    })
    .filter_map(|dir| {
      roots
        .iter()
        .position(|(_, root)| dir.starts_with(root))
        .map(|library_index| (library_index, dir))
    })
    .collect()
}

/// Process the dirty candidates.  Candidates that still exist as directories
/// get their whole subtree re-reconciled (covering trees moved in as one
/// event); vanished candidates have any rows beneath them removed (a no-op
/// for plain file paths).  Live and vanished sets are pruned separately: a
/// live ancestor's reconcile only visits *existing* directories, so it can
/// never stand in for a vanished descendant's removal.  Failures warn and
/// move on — one broken directory must not stall index maintenance.
async fn flush(
  index: &Index,
  roots: &[(Library, PathBuf)],
  dirty: Vec<(usize, PathBuf)>,
) {
  let generation = match index.current_generation().await {
    Ok(generation) => generation,
    Err(error) => {
      warn!(%error, "Skipping index flush; could not read scan generation");
      return;
    }
  };
  let (live, vanished): (Vec<_>, Vec<_>) =
    dirty.into_iter().partition(|(_, dir)| dir.is_dir());

  stream::iter(prune_nested(vanished))
    .for_each(|(library_index, dir)| {
      remove_vanished(index, roots, library_index, dir)
    })
    .await;
  stream::iter(prune_nested(live))
    .for_each(|(library_index, dir)| {
      reconcile_live(index, roots, generation, library_index, dir)
    })
    .await;
}

/// Remove a vanished directory's rows (the whole subtree, since no per-file
/// events arrive for a deleted tree's contents).
async fn remove_vanished(
  index: &Index,
  roots: &[(Library, PathBuf)],
  library_index: usize,
  dir: PathBuf,
) {
  let Some((library, root)) = roots.get(library_index) else {
    return;
  };
  let rel = dir
    .strip_prefix(root)
    .unwrap_or(&dir)
    .to_string_lossy()
    .to_string();
  if let Err(error) = index.remove_tree(&library.name, &rel).await {
    warn!(
      path = %dir.display(),
      %error,
      "Failed to remove vanished directory from the index"
    );
  }
}

/// Re-reconcile a live directory's whole subtree.
async fn reconcile_live(
  index: &Index,
  roots: &[(Library, PathBuf)],
  generation: i64,
  library_index: usize,
  dir: PathBuf,
) {
  let Some((library, _)) = roots.get(library_index) else {
    return;
  };
  stream::iter(walk_dirs(&dir))
    .for_each(|subdir| async move {
      if let Err(error) =
        scan::reconcile_directory(index, library, &subdir, generation).await
      {
        warn!(
          path = %subdir.display(),
          %error,
          "Failed to reconcile directory after filesystem event"
        );
      }
    })
    .await;
}

/// Drop dirty directories that are descendants of other dirty directories —
/// the ancestor's subtree reconcile already covers them.  Sorting first puts
/// every ancestor immediately before its descendants, so covering only ever
/// has to look at the last kept entry.
fn prune_nested(dirty: Vec<(usize, PathBuf)>) -> Vec<(usize, PathBuf)> {
  dirty
    .tap_mut(|dirty| {
      dirty.sort();
      dirty.dedup();
    })
    .into_iter()
    .fold(Vec::new(), |mut kept, (library_index, dir)| {
      let covered = kept.last().is_some_and(|(kept_index, kept_dir)| {
        *kept_index == library_index && dir.starts_with(kept_dir)
      });
      if !covered {
        kept.push((library_index, dir));
      }
      kept
    })
}

fn walk_dirs(dir: &Path) -> Vec<PathBuf> {
  walkdir::WalkDir::new(dir)
    .follow_links(false)
    .into_iter()
    .filter_map(|entry| match entry {
      Ok(e) if e.file_type().is_dir() => Some(e.into_path()),
      Ok(_) => None,
      Err(error) => {
        warn!(%error, "Skipping unreadable entry during event reconcile");
        None
      }
    })
    .collect()
}

/// Pure debounce state: absorbs dirty directories and exposes the deadline
/// at which they should flush.  Kept free of I/O so the policy is unit
/// testable with synthetic instants.
struct Debounce {
  quiet: Duration,
  pending: HashSet<(usize, PathBuf)>,
  deadline: Option<Instant>,
}

impl Debounce {
  fn new(quiet: Duration) -> Self {
    Self {
      quiet,
      pending: HashSet::new(),
      deadline: None,
    }
  }

  /// Absorb one dirty directory; every event pushes the flush deadline out
  /// to a full quiet period from now.
  fn record(&mut self, key: (usize, PathBuf), now: Instant) {
    self.pending.insert(key);
    self.deadline = Some(now + self.quiet);
  }

  fn deadline(&self) -> Option<Instant> {
    self.deadline
  }

  /// Hand over everything pending and reset.
  fn take(&mut self) -> Vec<(usize, PathBuf)> {
    self.deadline = None;
    self.pending.drain().collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn debounce_extends_deadline_on_each_event() {
    let mut debounce = Debounce::new(Duration::from_secs(2));
    let start = Instant::now();
    debounce.record((0, PathBuf::from("a")), start);
    let first = debounce.deadline().unwrap();
    debounce.record((0, PathBuf::from("b")), start + Duration::from_secs(1));
    let second = debounce.deadline().unwrap();
    assert!(second > first, "a later event must push the deadline out");
  }

  #[test]
  fn debounce_collapses_duplicate_directories() {
    let mut debounce = Debounce::new(Duration::from_secs(2));
    let now = Instant::now();
    debounce.record((0, PathBuf::from("a")), now);
    debounce.record((0, PathBuf::from("a")), now);
    debounce.record((1, PathBuf::from("a")), now);
    let taken = debounce.take();
    assert_eq!(taken.len(), 2, "same dir in same library collapses");
    assert!(debounce.deadline().is_none(), "take resets the deadline");
  }

  #[test]
  fn prune_nested_drops_covered_descendants() {
    let pruned = prune_nested(vec![
      (0, PathBuf::from("/lib/a/b")),
      (0, PathBuf::from("/lib/a")),
      (0, PathBuf::from("/lib/c")),
      (1, PathBuf::from("/lib/a/b")),
    ]);
    assert_eq!(
      pruned,
      vec![
        (0, PathBuf::from("/lib/a")),
        (0, PathBuf::from("/lib/c")),
        (1, PathBuf::from("/lib/a/b")),
      ],
      "descendants collapse into ancestors, but only within one library"
    );
  }
}
