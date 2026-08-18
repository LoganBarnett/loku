//! Library scanning: walk each library root and reconcile the media index
//! with what is actually on disk.  Sidecar files (NFO, `info.json`,
//! thumbnails, compat copies) remain the source of truth — the index is a
//! derived, rebuildable structure.

use futures_util::stream::{self, StreamExt, TryStreamExt};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use thiserror::Error;
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::index::store::{
  CodecSource, Index, IndexError, TitleSource, VideoRecord,
};
use crate::library::{Library, LibraryKind};
use crate::media::sidecars::{
  find_compat, find_thumbnail, read_info_json, VIDEO_EXTENSIONS,
};
use loku_lib::disc;
use loku_lib::nfo::{self, Nfo};

#[derive(Debug, Default)]
pub struct ScanStats {
  pub directories: usize,
  pub videos: usize,
  pub removed: usize,
}

impl ScanStats {
  /// Combine two scans' tallies.
  fn merged(self, other: ScanStats) -> ScanStats {
    ScanStats {
      directories: self.directories + other.directories,
      videos: self.videos + other.videos,
      removed: self.removed + other.removed,
    }
  }
}

#[derive(Debug, Error)]
pub enum ScanError {
  #[error("Failed to canonicalize library root '{path}': {source}")]
  LibraryRootCanonicalize {
    path: String,
    source: std::io::Error,
  },

  #[error("Library scan worker task failed: {source}")]
  ScanTaskJoin { source: tokio::task::JoinError },

  #[error(transparent)]
  Index(#[from] IndexError),
}

/// Scan every library under a fresh generation, then record completion.
pub async fn scan_all(
  index: &Index,
  libraries: &[Library],
) -> Result<ScanStats, ScanError> {
  let generation = index.next_generation().await?;
  let stats = stream::iter(libraries)
    .map(Ok::<_, ScanError>)
    .try_fold(ScanStats::default(), |stats, library| async move {
      let library_stats = scan_library(index, library, generation).await?;
      info!(
        library = %library.name,
        directories = library_stats.directories,
        videos = library_stats.videos,
        removed = library_stats.removed,
        "Library scan complete"
      );
      Ok(stats.merged(library_stats))
    })
    .await?;
  index.mark_scan_complete().await?;
  Ok(stats)
}

/// Scan one library root: reconcile each directory, then sweep rows whose
/// generation stamp shows their files vanished.
pub async fn scan_library(
  index: &Index,
  library: &Library,
  generation: i64,
) -> Result<ScanStats, ScanError> {
  let root = library.path.canonicalize().map_err(|source| {
    ScanError::LibraryRootCanonicalize {
      path: library.path.to_string_lossy().to_string(),
      source,
    }
  })?;

  let directories = {
    let root = root.clone();
    tokio::task::spawn_blocking(move || walk_directories(&root))
      .await
      .map_err(|source| ScanError::ScanTaskJoin { source })?
  };

  let scanned = stream::iter(directories)
    .map(Ok::<_, ScanError>)
    .try_fold(ScanStats::default(), |stats, dir| {
      let root = root.clone();
      let blocking_library = library.clone();
      async move {
        let (parent_rel, records) = tokio::task::spawn_blocking(move || {
          (
            relative_of(&dir, &root),
            collect_directory(&blocking_library, &root, &dir),
          )
        })
        .await
        .map_err(|source| ScanError::ScanTaskJoin { source })?;
        let videos = records.len();
        index
          .reconcile_directory(&library.name, &parent_rel, records, generation)
          .await?;
        Ok(stats.merged(ScanStats {
          directories: 1,
          videos,
          removed: 0,
        }))
      }
    })
    .await?;

  let removed = index.remove_unseen(&library.name, generation).await?;
  Ok(scanned.merged(ScanStats {
    removed,
    ..ScanStats::default()
  }))
}

/// Reconcile a single directory — the unit of work the filesystem watcher
/// re-runs when it sees changes.  Missing directories reconcile to empty,
/// which is exactly right for deletions.
pub async fn reconcile_directory(
  index: &Index,
  library: &Library,
  dir: &Path,
  generation: i64,
) -> Result<(), ScanError> {
  let root = library.path.canonicalize().map_err(|source| {
    ScanError::LibraryRootCanonicalize {
      path: library.path.to_string_lossy().to_string(),
      source,
    }
  })?;
  let dir = dir.to_path_buf();
  let (parent_rel, records) = {
    let root = root.clone();
    let library = library.clone();
    tokio::task::spawn_blocking(move || {
      let parent_rel = relative_of(&dir, &root);
      let records = collect_directory(&library, &root, &dir);
      (parent_rel, records)
    })
    .await
    .map_err(|source| ScanError::ScanTaskJoin { source })?
  };
  index
    .reconcile_directory(&library.name, &parent_rel, records, generation)
    .await?;
  Ok(())
}

/// Every directory under the root, the root itself included.  Symlinks are
/// not followed: a link out of the library must not pull foreign trees into
/// the index (the browse route enforces the same boundary).
fn walk_directories(root: &Path) -> Vec<PathBuf> {
  WalkDir::new(root)
    .follow_links(false)
    .into_iter()
    .filter_map(|entry| match entry {
      Ok(e) if e.file_type().is_dir() => Some(e.into_path()),
      Ok(_) => None,
      Err(error) => {
        warn!(%error, "Skipping unreadable entry during library walk");
        None
      }
    })
    .collect()
}

fn relative_of(path: &Path, root: &Path) -> String {
  path
    .strip_prefix(root)
    .unwrap_or(path)
    .to_string_lossy()
    .to_string()
}

/// Build records for the master videos directly inside one directory.
fn collect_directory(
  library: &Library,
  root: &Path,
  dir: &Path,
) -> Vec<VideoRecord> {
  let entries = match fs::read_dir(dir) {
    Ok(read) => read,
    Err(error) => {
      warn!(path = %dir.display(), %error, "Failed to read directory during scan");
      return Vec::new();
    }
  };

  let video_paths: Vec<PathBuf> = entries
    .filter_map(|entry| match entry {
      Ok(entry) => Some(entry.path()),
      Err(error) => {
        warn!(path = %dir.display(), %error, "Skipping unreadable directory entry during scan");
        None
      }
    })
    .filter(|path| path.is_file() && is_master_video(path))
    .collect();

  // Kodi's folder-level movie.nfo rule assumes one movie per folder; with
  // several videos present the fallback would misattribute one movie's
  // metadata to every file, so it only applies to lone videos.
  let allow_folder_nfo = video_paths.len() == 1;

  video_paths
    .iter()
    .filter_map(|path| build_record(library, root, path, allow_folder_nfo))
    .collect()
}

fn is_master_video(path: &Path) -> bool {
  let ext = path
    .extension()
    .and_then(OsStr::to_str)
    .unwrap_or("")
    .to_lowercase();
  VIDEO_EXTENSIONS.contains(&ext.as_str())
    && path
      .file_stem()
      .is_some_and(|stem| !stem.to_string_lossy().ends_with(".compat"))
}

fn build_record(
  library: &Library,
  root: &Path,
  path: &Path,
  allow_folder_nfo: bool,
) -> Option<VideoRecord> {
  let metadata = match path.metadata() {
    Ok(m) => m,
    Err(error) => {
      warn!(path = %path.display(), %error, "Failed to stat video during scan");
      return None;
    }
  };
  let file_name = path.file_name()?.to_string_lossy().to_string();
  let stem = path.file_stem()?;
  let parent = path.parent().unwrap_or(Path::new(""));
  let container = path
    .extension()
    .and_then(OsStr::to_str)
    .unwrap_or("")
    .to_lowercase();

  let info = read_info_json(parent, stem);
  let nfo = read_nfo(path, allow_folder_nfo);
  let nfo_facts = nfo.map(nfo_facts).unwrap_or_default();

  let rip_name = (library.kind == LibraryKind::Discs)
    .then(|| disc::parse_rip_name(&file_name));

  let (title, title_source) = nfo_facts
    .title
    .clone()
    .map(|t| (t, TitleSource::Nfo))
    .or_else(|| info.title.clone().map(|t| (t, TitleSource::InfoJson)))
    .or_else(|| {
      rip_name
        .as_ref()
        .map(|r| (disc::display_title(&r.prefix), TitleSource::RipName))
    })
    .map_or((None, None), |(t, s)| (Some(t), Some(s)));

  let codec_source = (info.vcodec.is_some() || info.acodec.is_some())
    .then_some(CodecSource::InfoJson);

  Some(VideoRecord {
    library: library.name.clone(),
    rel_path: relative_of(path, root),
    parent: relative_of(parent, root),
    file_name,
    size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
    mtime: mtime_secs(&metadata),
    title,
    title_source,
    duration_secs: info.duration_secs.or(nfo_facts.runtime_secs),
    upload_date: info.upload_date,
    year: nfo_facts.year,
    description: nfo_facts.description.or(info.description),
    channel: info.channel,
    channel_url: info.channel_url,
    webpage_url: info.webpage_url,
    // The .ok() discard is deliberate: a view count beyond i64 is absurd
    // input, and absent beats absurd in the index.
    view_count: info.view_count.and_then(|count| i64::try_from(count).ok()),
    genres: nfo_facts.genres,
    container,
    vcodec: info.vcodec,
    acodec: info.acodec,
    vprofile: None,
    field_order: None,
    codec_source,
    thumb_rel_path: find_thumbnail(parent, stem, root),
    compat_rel_path: find_compat(parent, stem, root),
    disc_set: rip_name.as_ref().map(|r| r.prefix.clone()),
    disc_title_index: rip_name.and_then(|r| r.title_index).map(i64::from),
    // Failures are join-time facts from the failures table, never part of a
    // scanned record.
    derivation_error: None,
  })
}

/// Facts a video's own NFO contributes to its record.
#[derive(Debug, Default)]
struct NfoFacts {
  title: Option<String>,
  year: Option<i32>,
  genres: Vec<String>,
  description: Option<String>,
  runtime_secs: Option<f64>,
}

fn nfo_facts(nfo: Nfo) -> NfoFacts {
  match nfo {
    Nfo::Movie(movie) => NfoFacts {
      title: movie.title,
      year: movie.year,
      genres: movie.genres,
      description: movie.plot,
      runtime_secs: movie.runtime.map(|minutes| f64::from(minutes) * 60.0),
    },
    Nfo::Episode(episode) => NfoFacts {
      title: episode.title,
      year: None,
      genres: Vec::new(),
      description: episode.plot,
      runtime_secs: episode.runtime.map(|minutes| f64::from(minutes) * 60.0),
    },
    Nfo::TvShow(show) => NfoFacts {
      title: show.title,
      year: show.year,
      genres: show.genres,
      description: show.plot,
      runtime_secs: None,
    },
    Nfo::Reference(_) => NfoFacts::default(),
  }
}

/// Read and parse the video's NFO per the Kodi lookup rules, skipping the
/// folder-level fallback when it would be ambiguous.
fn read_nfo(video_path: &Path, allow_folder_nfo: bool) -> Option<Nfo> {
  nfo::nfo_candidates(video_path)
    .into_iter()
    .enumerate()
    .filter(|(i, _)| *i == 0 || allow_folder_nfo)
    .find_map(|(_, candidate)| {
      let bytes = match fs::read(&candidate) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
          return None;
        }
        Err(error) => {
          warn!(path = %candidate.display(), %error, "Failed to read NFO sidecar");
          return None;
        }
      };
      match nfo::parse_nfo(&String::from_utf8_lossy(&bytes)) {
        Ok(parsed) => Some(parsed),
        Err(error) => {
          warn!(path = %candidate.display(), %error, "Failed to parse NFO sidecar");
          None
        }
      }
    })
}

/// Modification time as Unix seconds.  The .ok() discards are the
/// change-detection policy, not lost errors: a platform without mtime
/// support or a pre-1970 clock degrades to zero, which at worst makes
/// change detection re-index the file — never data loss.
fn mtime_secs(metadata: &fs::Metadata) -> i64 {
  metadata
    .modified()
    .ok()
    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
    .map_or(0, |duration: Duration| {
      i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
    })
}
