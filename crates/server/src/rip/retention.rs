//! The retention sweep: replaced and failed rip trees are moved aside under
//! the library's hidden `.rip/` directory rather than deleted at the moment
//! they stop being current, and this daily sweep deletes them once they are
//! older than the configured retention.  The delay is the safety margin: a
//! rerip that turns out worse than the original, or a failure worth
//! inspecting, is still there for a couple of weeks.

use futures_util::stream::{self, StreamExt, TryStreamExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

use crate::index::store::now_secs;
use crate::rip::naming::{self, FAILED_DIR, REPLACED_DIR, RIP_DIR};

/// The service runs persistently, so a daily pass is plenty.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepStats {
  pub removed: usize,
  pub kept: usize,
  /// Entries without an archival timestamp; never touched.
  pub unparseable: usize,
}

impl SweepStats {
  fn merged(self, other: SweepStats) -> SweepStats {
    SweepStats {
      removed: self.removed + other.removed,
      kept: self.kept + other.kept,
      unparseable: self.unparseable + other.unparseable,
    }
  }
}

/// Pure: which archived names are past retention as of `now_secs`.
pub fn expired<'a>(
  names: impl IntoIterator<Item = &'a str>,
  now_secs: i64,
  retention: Duration,
) -> Vec<&'a str> {
  let cutoff =
    now_secs - i64::try_from(retention.as_secs()).unwrap_or(i64::MAX);
  names
    .into_iter()
    .filter(|name| {
      naming::archived_timestamp(name).is_some_and(|ts| ts <= cutoff)
    })
    .collect()
}

/// One pass over `.rip/replaced` and `.rip/failed`.
pub async fn sweep(library_root: &Path, retention: Duration) -> SweepStats {
  let now = now_secs();
  let rip_dir = library_root.join(RIP_DIR);
  let replaced = sweep_dir(&rip_dir.join(REPLACED_DIR), now, retention).await;
  let failed = sweep_dir(&rip_dir.join(FAILED_DIR), now, retention).await;
  replaced.merged(failed)
}

async fn sweep_dir(dir: &Path, now: i64, retention: Duration) -> SweepStats {
  match archived_entries(dir).await {
    Ok(names) => sweep_names(dir, names, now, retention).await,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      SweepStats::default()
    }
    Err(error) => {
      warn!(path = %dir.display(), %error, "Failed to list rip archive during retention sweep");
      SweepStats::default()
    }
  }
}

/// Delete the expired entries among the listed names and account for the
/// rest.
async fn sweep_names(
  dir: &Path,
  names: Vec<String>,
  now: i64,
  retention: Duration,
) -> SweepStats {
  let unparseable: Vec<&String> = names
    .iter()
    .filter(|name| naming::archived_timestamp(name).is_none())
    .collect();
  if !unparseable.is_empty() {
    warn!(
      path = %dir.display(),
      entries = ?unparseable,
      "Rip archive holds entries without an archival timestamp; leaving them alone"
    );
  }
  let doomed = expired(names.iter().map(String::as_str), now, retention);
  let doomed_paths: Vec<PathBuf> =
    doomed.iter().map(|name| dir.join(name)).collect();
  let removed = stream::iter(doomed_paths)
    .fold(0, |removed, path| async move {
      removed + usize::from(remove_archived(path).await)
    })
    .await;
  SweepStats {
    removed,
    kept: names.len() - unparseable.len() - doomed.len(),
    unparseable: unparseable.len(),
  }
}

/// Delete one archived tree; a failure only warns, so the sweep goes on.
async fn remove_archived(path: PathBuf) -> bool {
  match tokio::fs::remove_dir_all(&path).await {
    Ok(()) => {
      info!(path = %path.display(), "Removed archived rip tree past retention");
      true
    }
    Err(error) => {
      warn!(path = %path.display(), %error, "Failed to remove archived rip tree");
      false
    }
  }
}

async fn archived_entries(dir: &Path) -> std::io::Result<Vec<String>> {
  let entries = tokio::fs::read_dir(dir).await?;
  stream::try_unfold(entries, |mut entries| async move {
    entries.next_entry().await.map(|next| {
      next
        .map(|entry| (entry.file_name().to_string_lossy().to_string(), entries))
    })
  })
  .try_collect()
  .await
}

/// Sweep now, then once a day for the life of the process.
pub fn spawn(library_root: PathBuf, retention: Duration) {
  tokio::spawn(async move {
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    loop {
      ticker.tick().await;
      let stats = sweep(&library_root, retention).await;
      if stats.removed > 0 || stats.unparseable > 0 {
        info!(
          removed = stats.removed,
          kept = stats.kept,
          unparseable = stats.unparseable,
          "Rip retention sweep complete"
        );
      }
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn expired_selects_by_archival_timestamp() {
    let day: u64 = 24 * 60 * 60;
    let now = i64::try_from(100 * day).unwrap();
    let names = [
      "old__id__1",
      "recent__id__8639900",
      "unparseable",
      "exact__id__7776000",
    ];
    let expired = expired(names, now, Duration::from_secs(10 * day));
    assert_eq!(expired, vec!["old__id__1", "exact__id__7776000"]);
  }

  #[tokio::test]
  async fn sweep_removes_only_expired_archives() {
    let root = tempfile::tempdir().unwrap();
    let replaced = root.path().join(".rip/replaced");
    std::fs::create_dir_all(replaced.join("old__id__1")).unwrap();
    std::fs::write(replaced.join("old__id__1/title.mkv"), b"x").unwrap();
    let fresh = format!("fresh__id__{}", now_secs());
    std::fs::create_dir_all(replaced.join(&fresh)).unwrap();
    std::fs::create_dir_all(replaced.join("mystery")).unwrap();

    let stats = sweep(root.path(), Duration::from_secs(60)).await;
    assert_eq!(
      stats,
      SweepStats {
        removed: 1,
        kept: 1,
        unparseable: 1
      }
    );
    assert!(!replaced.join("old__id__1").exists());
    assert!(replaced.join(&fresh).exists());
    assert!(replaced.join("mystery").exists());
  }

  #[tokio::test]
  async fn sweep_without_archives_is_quiet() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
      sweep(root.path(), Duration::from_secs(1)).await,
      SweepStats::default()
    );
  }
}
