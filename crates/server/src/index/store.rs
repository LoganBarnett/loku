//! Async handle to the SQLite media index and its typed queries.
//!
//! All access funnels through one `tokio_rusqlite::Connection` — a dedicated
//! connection thread — whose serialization is a feature at this scale: no
//! `SQLITE_BUSY` juggling, and scan transactions interleave cleanly with
//! reader calls.

use rusqlite::{params, OptionalExtension};
use schemars::JsonSchema;
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tracing::warn;

use crate::identity::tmdb::Candidate;
use crate::identity::{Identity, IdentitySource, IdentityState, TmdbKind};
use crate::index::schema;
use crate::media::ffmpeg::ProbeResult;
use crate::rip::config::RipPolicy;
use crate::rip::naming::IdSource;
use crate::rip::worker::{RipState, RipTrigger};

/// Async handle to the media index.  Cloning shares the underlying
/// connection thread.
#[derive(Clone)]
pub struct Index {
  conn: tokio_rusqlite::Connection,
}

#[derive(Debug, Error)]
pub enum IndexError {
  #[error("Failed to create the state directory '{path}': {source}")]
  StateDirCreate {
    path: String,
    source: std::io::Error,
  },

  #[error("Failed to open the media index database '{path}': {source}")]
  IndexOpen {
    path: String,
    source: rusqlite::Error,
  },

  #[error("Failed to initialize the media index schema: {source}")]
  IndexSchema { source: tokio_rusqlite::Error },

  #[error("Media index query failed while {context}: {source}")]
  IndexQuery {
    context: &'static str,
    source: tokio_rusqlite::Error,
  },

  #[error(
    "Failed to serialize a disc's identity candidates for storage: {source}"
  )]
  CandidatesSerialize { source: serde_json::Error },
}

/// Where a video's display title came from, in decreasing precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
  Nfo,
  InfoJson,
  RipName,
  /// The disc directory's label (from its manifest), for titles inside a
  /// per-disc directory that carries no NFO yet.
  DiscLabel,
}

impl TitleSource {
  fn as_str(self) -> &'static str {
    match self {
      TitleSource::Nfo => "nfo",
      TitleSource::InfoJson => "info_json",
      TitleSource::RipName => "rip_name",
      TitleSource::DiscLabel => "disc_label",
    }
  }

  fn parse(value: &str) -> Option<Self> {
    match value {
      "nfo" => Some(TitleSource::Nfo),
      "info_json" => Some(TitleSource::InfoJson),
      "rip_name" => Some(TitleSource::RipName),
      "disc_label" => Some(TitleSource::DiscLabel),
      _ => None,
    }
  }
}

/// Where a video's codec facts came from.  Absent means the item still needs
/// an ffprobe pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodecSource {
  InfoJson,
  Ffprobe,
}

impl CodecSource {
  fn as_str(self) -> &'static str {
    match self {
      CodecSource::InfoJson => "info_json",
      CodecSource::Ffprobe => "ffprobe",
    }
  }

  fn parse(value: &str) -> Option<Self> {
    match value {
      "info_json" => Some(CodecSource::InfoJson),
      "ffprobe" => Some(CodecSource::Ffprobe),
      _ => None,
    }
  }
}

/// One indexed video, as written by the scanner and read back by the search
/// and browse queries.
#[derive(Debug, Clone)]
pub struct VideoRecord {
  pub library: String,
  pub rel_path: String,
  pub parent: String,
  pub file_name: String,
  pub size: i64,
  pub mtime: i64,
  pub title: Option<String>,
  pub title_source: Option<TitleSource>,
  pub duration_secs: Option<f64>,
  pub upload_date: Option<String>,
  pub year: Option<i32>,
  pub description: Option<String>,
  pub channel: Option<String>,
  pub channel_url: Option<String>,
  pub webpage_url: Option<String>,
  pub view_count: Option<i64>,
  pub genres: Vec<String>,
  pub container: String,
  pub vcodec: Option<String>,
  pub acodec: Option<String>,
  pub vprofile: Option<String>,
  pub field_order: Option<String>,
  pub codec_source: Option<CodecSource>,
  pub thumb_rel_path: Option<String>,
  pub compat_rel_path: Option<String>,
  pub disc_set: Option<String>,
  pub disc_title_index: Option<i64>,
  /// The recorded derivation failure for the file's current content, joined
  /// in from `derivation_failures` at query time.
  pub derivation_error: Option<String>,
}

/// The identity an NFO on disk asserts for a disc, as the scanner reads it
/// back.  This is what makes a resolved disc self-healing: the index row is
/// rebuilt from the sidecar, never the other way round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfoIdentity {
  pub kind: TmdbKind,
  pub id: u64,
  /// The NFO file's modification time, standing in for when the identity
  /// was recorded.
  pub recorded_at: i64,
}

/// A disc directory as the scanner sees it: the facts `disc.json` and the
/// directory-level NFO carry.  The identity worker's own columns are not
/// part of this; the upsert preserves them across rescans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedDisc {
  pub library: String,
  pub rel_path: String,
  pub parent: String,
  pub label: String,
  pub disc_id: String,
  pub id_source: Option<IdSource>,
  pub title: String,
  /// Whether the title came from the directory NFO or is a guess from the
  /// label; titles inside the directory inherit it.  Not persisted: the
  /// identity state carries the same fact for readers.
  pub title_source: TitleSource,
  pub year: Option<i32>,
  pub poster_rel_path: Option<String>,
  pub fingerprint: Option<String>,
  pub fingerprint_titles: u32,
  pub identity: Option<NfoIdentity>,
}

/// One indexed disc directory, as read back by the browse and identity
/// queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscRecord {
  pub library: String,
  pub rel_path: String,
  pub parent: String,
  pub label: String,
  pub disc_id: String,
  pub id_source: Option<IdSource>,
  pub title: String,
  pub year: Option<i32>,
  pub poster_rel_path: Option<String>,
  pub fingerprint: Option<String>,
  pub fingerprint_titles: u32,
  pub identity_state: IdentityState,
  pub identity_source: Option<IdentitySource>,
  pub identity_error: Option<String>,
  /// The stored candidate list, as JSON; the identity layer decodes it.
  pub candidates_json: Option<String>,
  pub identity_attempted_at: Option<i64>,
  pub identified_at: Option<i64>,
  pub tmdb_id: Option<u64>,
  pub tmdb_kind: Option<TmdbKind>,
}

impl DiscRecord {
  /// The stored candidates; nothing stored is an empty list, and stored
  /// text that no longer decodes is reported and treated the same way.
  pub fn candidates(&self) -> Vec<Candidate> {
    self
      .candidates_json
      .as_deref()
      .map_or_else(Vec::new, |json| {
        serde_json::from_str(json).unwrap_or_else(|error| {
          warn!(
            library = %self.library,
            path = %self.rel_path,
            %error,
            "Stored identity candidates are unreadable; treating as none"
          );
          Vec::new()
        })
      })
  }
}

/// How an identity attempt ended, as the worker or an operator records it.
#[derive(Debug, Clone, PartialEq)]
pub enum IdentityOutcome {
  Resolved {
    identity: Identity,
    source: IdentitySource,
  },
  NeedsConfirmation {
    candidates: Vec<Candidate>,
  },
  /// Definitive: an operator pick is the way out.
  Failed {
    error: String,
  },
  /// Transient: stays unresolved and is retried after a backoff.
  Deferred {
    error: String,
  },
}

/// A rip job as it is opened: what is known before the disc scan names it.
#[derive(Debug, Clone)]
pub struct NewRip {
  pub device: String,
  pub library: String,
  pub policy: RipPolicy,
  pub trigger: RipTrigger,
}

/// One row of rip history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RipRecord {
  pub id: i64,
  pub device: String,
  pub library: String,
  pub dir_rel_path: Option<String>,
  pub label: Option<String>,
  pub disc_id: Option<String>,
  pub policy: RipPolicy,
  pub trigger: RipTrigger,
  pub state: RipState,
  pub progress_permille: u32,
  pub current_op: Option<String>,
  pub started_at: i64,
  pub finished_at: Option<i64>,
  pub error: Option<String>,
}

#[derive(Debug)]
pub struct SearchResults {
  pub total: u64,
  pub items: Vec<VideoRecord>,
}

/// One directory's worth of browse data.
#[derive(Debug)]
pub struct BrowseData {
  pub videos: Vec<VideoRecord>,
  pub subdirectories: Vec<String>,
  /// The disc directories directly beneath the browse path, each with the
  /// titles it holds, in disc order.
  pub discs: Vec<(DiscRecord, Vec<VideoRecord>)>,
  /// `disc_set` → main title relative path, for the whole library.
  pub overrides: std::collections::HashMap<String, String>,
}

/// A video awaiting a codec probe, with the (size, mtime) pair that keys the
/// probe cache.
#[derive(Debug, Clone)]
pub struct ProbeCandidate {
  pub library: String,
  pub rel_path: String,
  pub size: i64,
  pub mtime: i64,
}

// Rescans rebuild rows from sidecars alone, so a scanner-produced row for an
// info.json-less file (a disc rip) carries no codec facts.  The CASE rules in
// the conflict clause keep previously probed facts (and probe-filled
// durations) as long as the file content is unchanged (same size and mtime),
// while a *changed* file resets them to NULL so the probe pass re-examines
// it — the probe cache misses on the new (size, mtime) and a real ffprobe
// runs.
const UPSERT_SQL: &str = "
INSERT INTO videos (
  library, rel_path, parent, file_name, size, mtime,
  title, title_source, duration_secs, upload_date, year,
  description, channel, channel_url, webpage_url, view_count, genres,
  container, vcodec, acodec, vprofile, field_order, codec_source,
  thumb_rel_path, compat_rel_path, disc_set, disc_title_index, last_seen
) VALUES (
  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
  ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28
)
ON CONFLICT (library, rel_path) DO UPDATE SET
  parent = excluded.parent,
  file_name = excluded.file_name,
  size = excluded.size,
  mtime = excluded.mtime,
  title = excluded.title,
  title_source = excluded.title_source,
  duration_secs = CASE
    WHEN excluded.duration_secs IS NOT NULL THEN excluded.duration_secs
    WHEN excluded.size = videos.size AND excluded.mtime = videos.mtime
      THEN videos.duration_secs
    ELSE NULL END,
  upload_date = excluded.upload_date,
  year = excluded.year,
  description = excluded.description,
  channel = excluded.channel,
  channel_url = excluded.channel_url,
  webpage_url = excluded.webpage_url,
  view_count = excluded.view_count,
  genres = excluded.genres,
  container = excluded.container,
  vcodec = CASE
    WHEN excluded.codec_source IS NOT NULL THEN excluded.vcodec
    WHEN excluded.size = videos.size AND excluded.mtime = videos.mtime
      THEN videos.vcodec
    ELSE NULL END,
  acodec = CASE
    WHEN excluded.codec_source IS NOT NULL THEN excluded.acodec
    WHEN excluded.size = videos.size AND excluded.mtime = videos.mtime
      THEN videos.acodec
    ELSE NULL END,
  vprofile = CASE
    WHEN excluded.codec_source IS NOT NULL THEN excluded.vprofile
    WHEN excluded.size = videos.size AND excluded.mtime = videos.mtime
      THEN videos.vprofile
    ELSE NULL END,
  field_order = CASE
    WHEN excluded.codec_source IS NOT NULL THEN excluded.field_order
    WHEN excluded.size = videos.size AND excluded.mtime = videos.mtime
      THEN videos.field_order
    ELSE NULL END,
  codec_source = CASE
    WHEN excluded.codec_source IS NOT NULL THEN excluded.codec_source
    WHEN excluded.size = videos.size AND excluded.mtime = videos.mtime
      THEN videos.codec_source
    ELSE NULL END,
  thumb_rel_path = excluded.thumb_rel_path,
  compat_rel_path = excluded.compat_rel_path,
  disc_set = excluded.disc_set,
  disc_title_index = excluded.disc_title_index,
  last_seen = excluded.last_seen
";

// Columns are qualified with the `v` alias because the FTS join would
// otherwise make the mirrored columns (title, file_name, …) ambiguous.
// Every query using this constant must alias the videos table as `v` and
// include the derivation-failures join in FAILURE_JOIN (aliased `f`).
const RECORD_COLUMNS: &str = "
  v.library, v.rel_path, v.parent, v.file_name, v.size, v.mtime,
  v.title, v.title_source, v.duration_secs, v.upload_date, v.year,
  v.description, v.channel, v.channel_url, v.webpage_url, v.view_count,
  v.genres, v.container, v.vcodec, v.acodec, v.vprofile, v.field_order,
  v.codec_source, v.thumb_rel_path, v.compat_rel_path, v.disc_set,
  v.disc_title_index, f.error AS derivation_error
";

// The failure join keyed on current content: a failure recorded for an
// older size/mtime no longer applies.
const FAILURE_JOIN: &str = "
  LEFT JOIN derivation_failures f
    ON f.library = v.library AND f.rel_path = v.rel_path
   AND f.size = v.size AND f.mtime = v.mtime
";

// The scanner owns the descriptive columns and overwrites them on every
// reconcile; the identity worker owns its progress columns (a pending
// confirmation, a recorded failure, retry stamps), which the CASE rules keep
// unless the sidecar on disk now proves the disc resolved.  A resolved state
// coming from the NFO wins outright and clears the worker's leftovers, so
// deleting the NFO re-queues the disc and writing one settles it.
const DISC_UPSERT_SQL: &str = "
INSERT INTO discs (
  library, rel_path, parent, label, disc_id, id_source, title, year,
  poster_rel_path, fingerprint, fingerprint_titles, identity_state,
  identity_source, tmdb_id, tmdb_kind, identified_at, last_seen
) VALUES (
  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17
)
ON CONFLICT (library, rel_path) DO UPDATE SET
  parent = excluded.parent,
  label = excluded.label,
  disc_id = excluded.disc_id,
  id_source = excluded.id_source,
  title = excluded.title,
  year = excluded.year,
  poster_rel_path = excluded.poster_rel_path,
  fingerprint = excluded.fingerprint,
  fingerprint_titles = excluded.fingerprint_titles,
  identity_state = CASE
    WHEN excluded.identity_state = 'resolved' THEN 'resolved'
    WHEN discs.identity_state IN ('needs_confirmation', 'failed')
      THEN discs.identity_state
    ELSE 'unresolved' END,
  identity_source = CASE
    WHEN excluded.identity_state = 'resolved'
      THEN COALESCE(discs.identity_source, excluded.identity_source)
    ELSE NULL END,
  tmdb_id = CASE
    WHEN excluded.identity_state = 'resolved' THEN excluded.tmdb_id
    ELSE NULL END,
  tmdb_kind = CASE
    WHEN excluded.identity_state = 'resolved' THEN excluded.tmdb_kind
    ELSE NULL END,
  identity_error = CASE
    WHEN excluded.identity_state = 'resolved' THEN NULL
    ELSE discs.identity_error END,
  candidates_json = CASE
    WHEN excluded.identity_state = 'resolved' THEN NULL
    ELSE discs.candidates_json END,
  identity_attempted_at = CASE
    WHEN excluded.identity_state = 'resolved' THEN NULL
    ELSE discs.identity_attempted_at END,
  identified_at = CASE
    WHEN excluded.identity_state = 'resolved'
      THEN COALESCE(discs.identified_at, excluded.identified_at)
    ELSE NULL END,
  last_seen = excluded.last_seen
";

const RIP_COLUMNS: &str = "
  id, device, library, dir_rel_path, label, disc_id, policy, trigger, state,
  progress_permille, current_op, started_at, finished_at, error
";

const DISC_COLUMNS: &str = "
  library, rel_path, parent, label, disc_id, id_source, title, year,
  poster_rel_path, fingerprint, fingerprint_titles, identity_state,
  identity_source, identity_error, candidates_json, identity_attempted_at,
  identified_at, tmdb_id, tmdb_kind
";

impl Index {
  /// Open (creating if needed) the index database inside the state
  /// directory.
  pub async fn open(state_dir: &Path) -> Result<Self, IndexError> {
    std::fs::create_dir_all(state_dir).map_err(|source| {
      IndexError::StateDirCreate {
        path: state_dir.to_string_lossy().to_string(),
        source,
      }
    })?;
    let path = state_dir.join("index.db");
    let conn =
      tokio_rusqlite::Connection::open(&path)
        .await
        .map_err(|source| IndexError::IndexOpen {
          path: path.to_string_lossy().to_string(),
          source,
        })?;
    Self::init(conn).await
  }

  /// Open a throwaway in-memory index — the form integration tests use, so
  /// routers need no on-disk state directory.
  pub async fn open_in_memory() -> Result<Self, IndexError> {
    let conn =
      tokio_rusqlite::Connection::open_in_memory()
        .await
        .map_err(|source| IndexError::IndexOpen {
          path: ":memory:".to_string(),
          source,
        })?;
    Self::init(conn).await
  }

  async fn init(conn: tokio_rusqlite::Connection) -> Result<Self, IndexError> {
    conn
      .call(schema::initialize)
      .await
      .map_err(|source| IndexError::IndexSchema { source })?;
    Ok(Self { conn })
  }

  /// Advance and return the scan generation counter.  Each full scan stamps
  /// the rows it sees with its generation, which is what lets
  /// [`Index::remove_unseen`] sweep vanished files afterwards.
  pub async fn next_generation(&self) -> Result<i64, IndexError> {
    self
      .call("advancing the scan generation", |conn| {
        let tx = conn.transaction()?;
        let current: i64 = tx
          .query_row(
            "SELECT value FROM meta WHERE key = 'scan_generation'",
            [],
            |row| row.get::<_, String>(0),
          )
          // A junk stored value degrading to zero merely restarts the
          // counter, which only widens the next sweep — never data loss.
          .map(|v| v.parse().unwrap_or(0))
          // Only "no row yet" (a fresh database) means generation zero;
          // real query failures must propagate.
          .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(0),
            other => Err(other),
          })?;
        let next = current + 1;
        tx.execute(
          "INSERT INTO meta (key, value) VALUES ('scan_generation', ?1)
           ON CONFLICT (key) DO UPDATE SET value = excluded.value",
          params![next.to_string()],
        )?;
        tx.commit()?;
        Ok(next)
      })
      .await
  }

  /// The generation stamped by the most recent [`Index::next_generation`],
  /// or zero when no scan has run.  Watcher-driven reconciles use it so
  /// their rows keep the current stamp.
  pub async fn current_generation(&self) -> Result<i64, IndexError> {
    self
      .call("reading the scan generation", |conn| {
        conn
          .query_row(
            "SELECT value FROM meta WHERE key = 'scan_generation'",
            [],
            |row| row.get::<_, String>(0),
          )
          // A junk stored value degrading to zero merely restarts the
          // counter, which only widens the next sweep — never data loss.
          .map(|v| v.parse().unwrap_or(0))
          .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(0),
            other => Err(other),
          })
      })
      .await
  }

  /// Upsert one directory's worth of records and delete rows for files that
  /// vanished from that directory — one transaction, so readers never see a
  /// half-reconciled directory.  The directory's own disc row (when it is a
  /// per-disc rip directory) rides the same transaction, keyed on the
  /// directory path.
  pub async fn reconcile_directory(
    &self,
    library: &str,
    parent: &str,
    records: Vec<VideoRecord>,
    disc: Option<ScannedDisc>,
    generation: i64,
  ) -> Result<(), IndexError> {
    let library = library.to_string();
    let parent = parent.to_string();
    self
      .call("reconciling a library directory", move |conn| {
        let tx = conn.transaction()?;
        {
          let mut upsert = tx.prepare_cached(UPSERT_SQL)?;
          records.iter().try_for_each(|record| {
            upsert
              .execute(params![
                record.library,
                record.rel_path,
                record.parent,
                record.file_name,
                record.size,
                record.mtime,
                record.title,
                record.title_source.map(TitleSource::as_str),
                record.duration_secs,
                record.upload_date,
                record.year,
                record.description,
                record.channel,
                record.channel_url,
                record.webpage_url,
                record.view_count,
                serde_json::to_string(&record.genres).unwrap_or_else(|_| {
                  // A Vec<String> cannot fail JSON serialization in
                  // practice; degrading to an empty list keeps this path
                  // total without a panicking assertion.
                  "[]".to_string()
                }),
                record.container,
                record.vcodec,
                record.acodec,
                record.vprofile,
                record.field_order,
                record.codec_source.map(CodecSource::as_str),
                record.thumb_rel_path,
                record.compat_rel_path,
                record.disc_set,
                record.disc_title_index,
                generation,
              ])
              // The affected-row count is not a fact worth keeping; only
              // failure matters here.
              .map(|_rows| ())
          })?;
        }
        let seen: Vec<&str> =
          records.iter().map(|r| r.rel_path.as_str()).collect();
        if seen.is_empty() {
          tx.execute(
            "DELETE FROM videos WHERE library = ?1 AND parent = ?2",
            params![library, parent],
          )?;
        } else {
          let placeholders = seen
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(", ");
          let sql = format!(
            "DELETE FROM videos WHERE library = ?1 AND parent = ?2 \
             AND rel_path NOT IN ({placeholders})"
          );
          let mut stmt = tx.prepare(&sql)?;
          stmt.execute(rusqlite::params_from_iter(
            [library.as_str(), parent.as_str()].into_iter().chain(seen),
          ))?;
        }
        match disc {
          Some(disc) => {
            let identity_state = disc
              .identity
              .as_ref()
              .map_or(IdentityState::Unresolved, |_| IdentityState::Resolved);
            tx.execute(
              DISC_UPSERT_SQL,
              params![
                disc.library,
                disc.rel_path,
                disc.parent,
                disc.label,
                disc.disc_id,
                disc.id_source.map(IdSource::as_str),
                disc.title,
                disc.year,
                disc.poster_rel_path,
                disc.fingerprint,
                disc.fingerprint_titles,
                identity_state.as_str(),
                disc.identity.as_ref().map(|_| IdentitySource::Nfo.as_str()),
                disc.identity.as_ref().and_then(|identity| {
                  // The .ok() discard is deliberate: an id beyond i64 is not
                  // a TMDB id, and absent beats absurd in the index.
                  i64::try_from(identity.id).ok()
                }),
                disc
                  .identity
                  .as_ref()
                  .map(|identity| identity.kind.as_str()),
                disc.identity.as_ref().map(|identity| identity.recorded_at),
                generation,
              ],
            )?;
          }
          None => {
            tx.execute(
              "DELETE FROM discs WHERE library = ?1 AND rel_path = ?2",
              params![library, parent],
            )?;
          }
        }
        tx.commit()?;
        Ok(())
      })
      .await
  }

  /// Delete every row in a library whose stamp predates the given
  /// generation — the end-of-scan sweep that removes files (and whole
  /// directories) that vanished between scans.
  pub async fn remove_unseen(
    &self,
    library: &str,
    generation: i64,
  ) -> Result<usize, IndexError> {
    let library = library.to_string();
    self
      .call("sweeping vanished files", move |conn| {
        conn.execute(
          "DELETE FROM discs WHERE library = ?1 AND last_seen < ?2",
          params![library, generation],
        )?;
        conn.execute(
          "DELETE FROM videos WHERE library = ?1 AND last_seen < ?2",
          params![library, generation],
        )
      })
      .await
  }

  /// Delete every row whose parent directory is `dir_rel` or lies anywhere
  /// beneath it — used when a whole directory vanishes, since no per-file
  /// events arrive for the tree's contents.
  pub async fn remove_tree(
    &self,
    library: &str,
    dir_rel: &str,
  ) -> Result<usize, IndexError> {
    let library = library.to_string();
    let dir_rel = dir_rel.to_string();
    self
      .call("removing a vanished directory tree", move |conn| {
        conn.execute(
          "DELETE FROM discs
           WHERE library = ?1
             AND (rel_path = ?2 OR rel_path LIKE ?3 ESCAPE '\\')",
          params![library, dir_rel, format!("{}/%", escape_like(&dir_rel))],
        )?;
        conn.execute(
          "DELETE FROM videos
           WHERE library = ?1
             AND (parent = ?2 OR parent LIKE ?3 ESCAPE '\\')",
          params![library, dir_rel, format!("{}/%", escape_like(&dir_rel))],
        )
      })
      .await
  }

  /// Every video still lacking codec facts (no info.json and not yet
  /// probed).
  pub async fn videos_needing_probe(
    &self,
  ) -> Result<Vec<ProbeCandidate>, IndexError> {
    self
      .call("listing videos needing a probe", |conn| {
        let mut stmt = conn.prepare_cached(
          "SELECT library, rel_path, size, mtime FROM videos
           WHERE codec_source IS NULL
           ORDER BY library, rel_path",
        )?;
        let candidates = stmt
          .query_map([], |row| {
            Ok(ProbeCandidate {
              library: row.get(0)?,
              rel_path: row.get(1)?,
              size: row.get(2)?,
              mtime: row.get(3)?,
            })
          })?
          .collect::<Result<Vec<_>, _>>()?;
        Ok(candidates)
      })
      .await
  }

  /// A prior probe of this exact content, if one is cached for the
  /// candidate's (size, mtime).
  pub async fn cached_probe(
    &self,
    candidate: &ProbeCandidate,
  ) -> Result<Option<ProbeResult>, IndexError> {
    let candidate = candidate.clone();
    self
      .call("consulting the probe cache", move |conn| {
        conn
          .query_row(
            "SELECT vcodec, acodec, vprofile, field_order, duration_secs,
                    width, height
             FROM probe_cache
             WHERE library = ?1 AND rel_path = ?2
               AND size = ?3 AND mtime = ?4",
            params![
              candidate.library,
              candidate.rel_path,
              candidate.size,
              candidate.mtime
            ],
            |row| {
              Ok(ProbeResult {
                vcodec: row.get(0)?,
                acodec: row.get(1)?,
                vprofile: row.get(2)?,
                field_order: row.get(3)?,
                duration_secs: row.get(4)?,
                width: row.get(5)?,
                height: row.get(6)?,
              })
            },
          )
          .optional()
      })
      .await
  }

  /// Store a probe outcome: refresh the cache and apply the facts to the
  /// video row (marking it probed).  The video's duration keeps a more
  /// precise sidecar-provided value when one exists.
  pub async fn record_probe(
    &self,
    candidate: &ProbeCandidate,
    probe: &ProbeResult,
  ) -> Result<(), IndexError> {
    let candidate = candidate.clone();
    let probe = probe.clone();
    self
      .call("recording a probe result", move |conn| {
        let tx = conn.transaction()?;
        tx.execute(
          "INSERT INTO probe_cache (
             library, rel_path, size, mtime, vcodec, acodec, vprofile,
             field_order, duration_secs, width, height
           ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
           ON CONFLICT (library, rel_path) DO UPDATE SET
             size = excluded.size,
             mtime = excluded.mtime,
             vcodec = excluded.vcodec,
             acodec = excluded.acodec,
             vprofile = excluded.vprofile,
             field_order = excluded.field_order,
             duration_secs = excluded.duration_secs,
             width = excluded.width,
             height = excluded.height",
          params![
            candidate.library,
            candidate.rel_path,
            candidate.size,
            candidate.mtime,
            probe.vcodec,
            probe.acodec,
            probe.vprofile,
            probe.field_order,
            probe.duration_secs,
            probe.width,
            probe.height,
          ],
        )?;
        tx.execute(
          "UPDATE videos SET
             vcodec = ?3,
             acodec = ?4,
             vprofile = ?5,
             field_order = ?6,
             duration_secs = COALESCE(duration_secs, ?7),
             codec_source = 'ffprobe'
           WHERE library = ?1 AND rel_path = ?2",
          params![
            candidate.library,
            candidate.rel_path,
            probe.vcodec,
            probe.acodec,
            probe.vprofile,
            probe.field_order,
            probe.duration_secs,
          ],
        )?;
        tx.commit()?;
        Ok(())
      })
      .await
  }

  /// Videos that may need derivation work: no compat copy yet, codec facts
  /// known, and no recorded failure for the file's current content.  The
  /// worker applies the actual planning logic; this only narrows the field.
  pub async fn derivation_candidates(
    &self,
  ) -> Result<Vec<VideoRecord>, IndexError> {
    self
      .call("listing derivation candidates", |conn| {
        let mut stmt = conn.prepare_cached(&format!(
          "SELECT {RECORD_COLUMNS}
           FROM videos v
           {FAILURE_JOIN}
           WHERE v.compat_rel_path IS NULL
             AND v.codec_source IS NOT NULL
             AND f.rel_path IS NULL
           ORDER BY v.library, v.rel_path"
        ))?;
        let candidates = stmt
          .query_map([], record_from_row)?
          .collect::<Result<Vec<_>, _>>()?;
        Ok(candidates)
      })
      .await
  }

  /// Record a derivation failure for the file's current content; the item
  /// is retried only after the file itself changes (new size or mtime).
  pub async fn record_derivation_failure(
    &self,
    library: &str,
    rel_path: &str,
    size: i64,
    mtime: i64,
    error: &str,
  ) -> Result<(), IndexError> {
    let library = library.to_string();
    let rel_path = rel_path.to_string();
    let error = error.to_string();
    self
      .call("recording a derivation failure", move |conn| {
        conn.execute(
          "INSERT INTO derivation_failures (
             library, rel_path, size, mtime, error, failed_at
           ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
           ON CONFLICT (library, rel_path) DO UPDATE SET
             size = excluded.size,
             mtime = excluded.mtime,
             error = excluded.error,
             failed_at = excluded.failed_at",
          params![library, rel_path, size, mtime, error, now_secs()],
        )?;
        Ok(())
      })
      .await
  }

  /// Everything one directory listing needs: the videos directly in the
  /// directory, the immediate subdirectory names (derived from deeper
  /// parents, so only video-bearing directories appear), and the library's
  /// main-title overrides.
  pub async fn browse_directory(
    &self,
    library: &str,
    path: &str,
  ) -> Result<BrowseData, IndexError> {
    let library = library.to_string();
    let path = path.to_string();
    self
      .call("listing a library directory", move |conn| {
        let mut stmt = conn.prepare_cached(&format!(
          "SELECT {RECORD_COLUMNS}
           FROM videos v
           {FAILURE_JOIN}
           WHERE v.library = ?1 AND v.parent = ?2
           ORDER BY v.file_name"
        ))?;
        let videos = stmt
          .query_map(params![library, path], record_from_row)?
          .collect::<Result<Vec<_>, _>>()?;

        // Immediate children fall out of the set of video-bearing parent
        // directories beneath the browse path.
        let prefix = if path.is_empty() {
          String::new()
        } else {
          format!("{path}/")
        };
        let mut dir_stmt = conn.prepare_cached(
          "SELECT DISTINCT parent FROM videos
           WHERE library = ?1 AND parent != '' AND parent LIKE ?2 ESCAPE '\\'",
        )?;
        let parents = dir_stmt
          .query_map(
            params![library, format!("{}%", escape_like(&prefix))],
            |row| row.get::<_, String>(0),
          )?
          .collect::<Result<Vec<_>, _>>()?;
        let mut subdirectories: Vec<String> = parents
          .iter()
          .filter_map(|parent| {
            parent
              .strip_prefix(&prefix)
              .and_then(|rest| rest.split('/').next())
              .map(str::to_string)
          })
          .collect();
        subdirectories.sort();
        subdirectories.dedup();

        let mut override_stmt = conn.prepare_cached(
          "SELECT disc_set, main_rel_path FROM disc_set_overrides
           WHERE library = ?1",
        )?;
        let overrides = override_stmt
          .query_map(params![library], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
          })?
          .collect::<Result<std::collections::HashMap<_, _>, _>>()?;

        // Disc directories directly beneath the path, with their titles,
        // so the listing can show each disc as one item without descending.
        let mut disc_stmt = conn.prepare_cached(&format!(
          "SELECT {DISC_COLUMNS} FROM discs
           WHERE library = ?1 AND parent = ?2
           ORDER BY title, rel_path"
        ))?;
        let disc_rows = disc_stmt
          .query_map(params![library, path], disc_from_row)?
          .collect::<Result<Vec<_>, _>>()?;
        let mut disc_video_stmt = conn.prepare_cached(&format!(
          "SELECT {RECORD_COLUMNS}
           FROM videos v
           {FAILURE_JOIN}
           JOIN discs d ON d.library = v.library AND d.rel_path = v.parent
           WHERE v.library = ?1 AND d.parent = ?2
           ORDER BY v.parent, v.disc_title_index, v.file_name"
        ))?;
        let mut disc_videos = disc_video_stmt
          .query_map(params![library, path], record_from_row)?
          .collect::<Result<Vec<_>, _>>()?
          .into_iter()
          .fold(
            std::collections::HashMap::<String, Vec<VideoRecord>>::new(),
            |mut by_parent, record| {
              by_parent
                .entry(record.parent.clone())
                .or_default()
                .push(record);
              by_parent
            },
          );
        let discs = disc_rows
          .into_iter()
          .map(|disc| {
            let titles = disc_videos.remove(&disc.rel_path).unwrap_or_default();
            (disc, titles)
          })
          .collect();

        Ok(BrowseData {
          videos,
          subdirectories,
          discs,
          overrides,
        })
      })
      .await
  }

  /// One video by its opaque (library, relative path) key.
  pub async fn video(
    &self,
    library: &str,
    rel_path: &str,
  ) -> Result<Option<VideoRecord>, IndexError> {
    let library = library.to_string();
    let rel_path = rel_path.to_string();
    self
      .call("looking up a video", move |conn| {
        conn
          .query_row(
            &format!(
              "SELECT {RECORD_COLUMNS}
               FROM videos v
               {FAILURE_JOIN}
               WHERE v.library = ?1 AND v.rel_path = ?2"
            ),
            params![library, rel_path],
            record_from_row,
          )
          .optional()
      })
      .await
  }

  /// One disc directory by its opaque (library, relative path) key.
  pub async fn disc(
    &self,
    library: &str,
    rel_path: &str,
  ) -> Result<Option<DiscRecord>, IndexError> {
    let library = library.to_string();
    let rel_path = rel_path.to_string();
    self
      .call("looking up a disc directory", move |conn| {
        conn
          .query_row(
            &format!(
              "SELECT {DISC_COLUMNS} FROM discs
               WHERE library = ?1 AND rel_path = ?2"
            ),
            params![library, rel_path],
            disc_from_row,
          )
          .optional()
      })
      .await
  }

  /// Discs an operator may need to look at: unresolved, awaiting
  /// confirmation, or failed.
  pub async fn unresolved_discs(&self) -> Result<Vec<DiscRecord>, IndexError> {
    self
      .call("listing unresolved discs", |conn| {
        let mut stmt = conn.prepare_cached(&format!(
          "SELECT {DISC_COLUMNS} FROM discs
           WHERE identity_state IN ('unresolved', 'needs_confirmation', 'failed')
           ORDER BY last_seen DESC, library, rel_path"
        ))?;
        let discs = stmt
          .query_map([], disc_from_row)?
          .collect::<Result<Vec<_>, _>>()?;
        Ok(discs)
      })
      .await
  }

  /// The next disc the identity worker should try: unresolved, with a
  /// fingerprint, and not attempted since `retry_before`.  Never-attempted
  /// discs go first.
  pub async fn next_identity_candidate(
    &self,
    retry_before: i64,
  ) -> Result<Option<DiscRecord>, IndexError> {
    self
      .call("picking the next disc to identify", move |conn| {
        conn
          .query_row(
            &format!(
              "SELECT {DISC_COLUMNS} FROM discs
               WHERE identity_state = 'unresolved'
                 AND fingerprint IS NOT NULL
                 AND (identity_attempted_at IS NULL
                      OR identity_attempted_at <= ?1)
               ORDER BY identity_attempted_at IS NOT NULL, last_seen DESC,
                        library, rel_path
               LIMIT 1"
            ),
            params![retry_before],
            disc_from_row,
          )
          .optional()
      })
      .await
  }

  /// Record how an identity attempt ended.
  pub async fn set_disc_identity(
    &self,
    library: &str,
    rel_path: &str,
    outcome: &IdentityOutcome,
  ) -> Result<(), IndexError> {
    let library = library.to_string();
    let rel_path = rel_path.to_string();
    let outcome = outcome.clone();
    let candidates_json = match &outcome {
      IdentityOutcome::NeedsConfirmation { candidates } => {
        serde_json::to_string(candidates).map(Some)
      }
      _ => Ok(None),
    }
    .map_err(|source| IndexError::CandidatesSerialize { source })?;
    self
      .call("recording a disc identity outcome", move |conn| {
        let now = now_secs();
        match outcome {
          IdentityOutcome::Resolved { identity, source } => conn.execute(
            "UPDATE discs SET
               identity_state = 'resolved',
               identity_source = ?3,
               tmdb_id = ?4,
               tmdb_kind = ?5,
               title = ?6,
               year = ?7,
               identity_error = NULL,
               candidates_json = NULL,
               identity_attempted_at = NULL,
               identified_at = ?8
             WHERE library = ?1 AND rel_path = ?2",
            params![
              library,
              rel_path,
              source.as_str(),
              // The .ok() discard is deliberate: an id beyond i64 is not a
              // TMDB id, and absent beats absurd in the index.
              i64::try_from(identity.id).ok(),
              identity.kind.as_str(),
              identity.title,
              identity.year,
              now,
            ],
          ),
          IdentityOutcome::NeedsConfirmation { .. } => conn.execute(
            "UPDATE discs SET
               identity_state = 'needs_confirmation',
               candidates_json = ?3,
               identity_error = NULL,
               identity_attempted_at = ?4
             WHERE library = ?1 AND rel_path = ?2",
            params![library, rel_path, candidates_json, now,],
          ),
          IdentityOutcome::Failed { error } => conn.execute(
            "UPDATE discs SET
               identity_state = 'failed',
               identity_error = ?3,
               identity_attempted_at = ?4
             WHERE library = ?1 AND rel_path = ?2",
            params![library, rel_path, error, now],
          ),
          IdentityOutcome::Deferred { error } => conn.execute(
            "UPDATE discs SET
               identity_error = ?3,
               identity_attempted_at = ?4
             WHERE library = ?1 AND rel_path = ?2",
            params![library, rel_path, error, now],
          ),
        }?;
        Ok(())
      })
      .await
  }

  /// Move every row keyed under a disc directory to its new path after the
  /// directory was renamed on disk.  Rows the watcher may already have
  /// created at the new path are deleted first — as plain deletes, so the
  /// FTS triggers fire — since the old rows carry the facts the fresh ones
  /// lack.  When the watcher has already swept the old rows, the fresh rows
  /// are the truth and nothing moves, so either ordering converges on one
  /// row per file.
  pub async fn rename_disc_dir(
    &self,
    library: &str,
    old_rel: &str,
    new_rel: &str,
  ) -> Result<(), IndexError> {
    let library = library.to_string();
    let old_rel = old_rel.to_string();
    let new_rel = new_rel.to_string();
    self
      .call("renaming a disc directory in the index", move |conn| {
        let tx = conn.transaction()?;
        let old_exists = tx
          .query_row(
            "SELECT 1 FROM discs WHERE library = ?1 AND rel_path = ?2",
            params![library, old_rel],
            |_row| Ok(()),
          )
          .optional()?
          .is_some();
        if !old_exists {
          return Ok(());
        }
        let old_beneath = format!("{}/%", escape_like(&old_rel));
        let new_beneath = format!("{}/%", escape_like(&new_rel));
        let new_parent = parent_of(&new_rel).to_string();
        tx.execute(
          "DELETE FROM videos WHERE library = ?1
             AND (parent = ?2 OR parent LIKE ?3 ESCAPE '\\')",
          params![library, new_rel, new_beneath],
        )?;
        tx.execute(
          "DELETE FROM discs WHERE library = ?1 AND rel_path = ?2",
          params![library, new_rel],
        )?;
        tx.execute(
          "DELETE FROM probe_cache WHERE library = ?1
             AND rel_path LIKE ?2 ESCAPE '\\'",
          params![library, new_beneath],
        )?;
        tx.execute(
          "DELETE FROM derivation_failures WHERE library = ?1
             AND rel_path LIKE ?2 ESCAPE '\\'",
          params![library, new_beneath],
        )?;
        tx.execute(
          "DELETE FROM disc_set_overrides
           WHERE library = ?1 AND disc_set = ?2",
          params![library, new_rel],
        )?;
        tx.execute(
          "UPDATE discs SET rel_path = ?3, parent = ?4
           WHERE library = ?1 AND rel_path = ?2",
          params![library, old_rel, new_rel, new_parent],
        )?;
        tx.execute(
          "UPDATE videos SET
             rel_path = ?3 || substr(rel_path, length(?2) + 1),
             parent = ?3 || substr(parent, length(?2) + 1),
             disc_set = CASE WHEN disc_set = ?2 THEN ?3 ELSE disc_set END
           WHERE library = ?1
             AND (parent = ?2 OR parent LIKE ?4 ESCAPE '\\')",
          params![library, old_rel, new_rel, old_beneath],
        )?;
        tx.execute(
          "UPDATE probe_cache SET
             rel_path = ?3 || substr(rel_path, length(?2) + 1)
           WHERE library = ?1 AND rel_path LIKE ?4 ESCAPE '\\'",
          params![library, old_rel, new_rel, old_beneath],
        )?;
        tx.execute(
          "UPDATE derivation_failures SET
             rel_path = ?3 || substr(rel_path, length(?2) + 1)
           WHERE library = ?1 AND rel_path LIKE ?4 ESCAPE '\\'",
          params![library, old_rel, new_rel, old_beneath],
        )?;
        tx.execute(
          "UPDATE disc_set_overrides SET
             disc_set = ?3,
             main_rel_path = ?3 || substr(main_rel_path, length(?2) + 1)
           WHERE library = ?1 AND disc_set = ?2",
          params![library, old_rel, new_rel],
        )?;
        tx.commit()?;
        Ok(())
      })
      .await
  }

  /// Record the operator's main-title pick for a disc set — the one piece
  /// of state that is not rebuildable from the filesystem.
  pub async fn set_main_title(
    &self,
    library: &str,
    disc_set: &str,
    main_rel_path: &str,
  ) -> Result<(), IndexError> {
    let library = library.to_string();
    let disc_set = disc_set.to_string();
    let main_rel_path = main_rel_path.to_string();
    self
      .call("recording a main-title pick", move |conn| {
        conn.execute(
          "INSERT INTO disc_set_overrides (library, disc_set, main_rel_path)
           VALUES (?1, ?2, ?3)
           ON CONFLICT (library, disc_set) DO UPDATE SET
             main_rel_path = excluded.main_rel_path",
          params![library, disc_set, main_rel_path],
        )?;
        Ok(())
      })
      .await
  }

  /// Point a video row at its freshly produced compat copy.
  pub async fn set_compat(
    &self,
    library: &str,
    rel_path: &str,
    compat_rel_path: &str,
  ) -> Result<(), IndexError> {
    let library = library.to_string();
    let rel_path = rel_path.to_string();
    let compat_rel_path = compat_rel_path.to_string();
    self
      .call("recording a compat copy", move |conn| {
        conn.execute(
          "UPDATE videos SET compat_rel_path = ?3
           WHERE library = ?1 AND rel_path = ?2",
          params![library, rel_path, compat_rel_path],
        )?;
        Ok(())
      })
      .await
  }

  /// Point a video row at its freshly extracted thumbnail.
  pub async fn set_thumb(
    &self,
    library: &str,
    rel_path: &str,
    thumb_rel_path: &str,
  ) -> Result<(), IndexError> {
    let library = library.to_string();
    let rel_path = rel_path.to_string();
    let thumb_rel_path = thumb_rel_path.to_string();
    self
      .call("recording a thumbnail", move |conn| {
        conn.execute(
          "UPDATE videos SET thumb_rel_path = ?3
           WHERE library = ?1 AND rel_path = ?2",
          params![library, rel_path, thumb_rel_path],
        )?;
        Ok(())
      })
      .await
  }

  /// Record a completed scan in the meta table (for observability; the
  /// health check reads a cached in-process copy instead).
  pub async fn mark_scan_complete(&self) -> Result<(), IndexError> {
    self
      .call("recording scan completion", |conn| {
        conn.execute(
          "INSERT INTO meta (key, value)
           VALUES ('last_scan_completed', ?1)
           ON CONFLICT (key) DO UPDATE SET value = excluded.value",
          params![now_secs().to_string()],
        )?;
        Ok(())
      })
      .await
  }

  /// Full-text search across libraries, ranked by bm25.
  pub async fn search(
    &self,
    fts_query: String,
    library: Option<String>,
    limit: u32,
    offset: u32,
  ) -> Result<SearchResults, IndexError> {
    self
      .call("searching the index", move |conn| {
        let total: u64 = conn.query_row(
          "SELECT count(*)
           FROM videos_fts
           JOIN videos v ON v.id = videos_fts.rowid
           WHERE videos_fts MATCH ?1 AND (?2 IS NULL OR v.library = ?2)",
          params![fts_query, library],
          |row| row.get(0),
        )?;
        let mut stmt = conn.prepare_cached(&format!(
          "SELECT {RECORD_COLUMNS}
           FROM videos_fts
           JOIN videos v ON v.id = videos_fts.rowid
           {FAILURE_JOIN}
           WHERE videos_fts MATCH ?1 AND (?2 IS NULL OR v.library = ?2)
           ORDER BY bm25(videos_fts)
           LIMIT ?3 OFFSET ?4"
        ))?;
        let items = stmt
          .query_map(
            params![fts_query, library, limit, offset],
            record_from_row,
          )?
          .collect::<Result<Vec<_>, _>>()?;
        Ok(SearchResults { total, items })
      })
      .await
  }

  /// Total indexed videos, optionally scoped to a library.  Used by tests
  /// and diagnostics.
  pub async fn video_count(
    &self,
    library: Option<String>,
  ) -> Result<u64, IndexError> {
    self
      .call("counting indexed videos", move |conn| {
        conn.query_row(
          "SELECT count(*) FROM videos
           WHERE (?1 IS NULL OR library = ?1)",
          params![library],
          |row| row.get(0),
        )
      })
      .await
  }

  /// Open a rip job row in the `detected` state.
  pub async fn insert_rip(
    &self,
    new: &NewRip,
  ) -> Result<RipRecord, IndexError> {
    let new = new.clone();
    self
      .call("opening a rip job", move |conn| {
        conn.query_row(
          &format!(
            "INSERT INTO rips (
               device, library, policy, trigger, state, started_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             RETURNING {RIP_COLUMNS}"
          ),
          params![
            new.device,
            new.library,
            new.policy.as_str(),
            new.trigger.as_str(),
            RipState::Detected.as_str(),
            now_secs(),
          ],
          rip_from_row,
        )
      })
      .await
  }

  /// Record the names the disc scan settled on for a job.
  pub async fn describe_rip(
    &self,
    id: i64,
    dir_rel_path: &str,
    label: &str,
    disc_id: &str,
  ) -> Result<(), IndexError> {
    let dir_rel_path = dir_rel_path.to_string();
    let label = label.to_string();
    let disc_id = disc_id.to_string();
    self
      .call("recording a rip job's disc", move |conn| {
        conn.execute(
          "UPDATE rips SET dir_rel_path = ?2, label = ?3, disc_id = ?4
           WHERE id = ?1",
          params![id, dir_rel_path, label, disc_id],
        )?;
        Ok(())
      })
      .await
  }

  pub async fn set_rip_state(
    &self,
    id: i64,
    state: RipState,
  ) -> Result<(), IndexError> {
    self
      .call("recording a rip job's state", move |conn| {
        conn.execute(
          "UPDATE rips SET state = ?2 WHERE id = ?1",
          params![id, state.as_str()],
        )?;
        Ok(())
      })
      .await
  }

  pub async fn rip_progress(
    &self,
    id: i64,
    progress_permille: u32,
    current_op: Option<&str>,
  ) -> Result<(), IndexError> {
    let current_op = current_op.map(str::to_string);
    self
      .call("recording rip progress", move |conn| {
        conn.execute(
          "UPDATE rips SET progress_permille = ?2, current_op = ?3
           WHERE id = ?1",
          params![id, progress_permille, current_op],
        )?;
        Ok(())
      })
      .await
  }

  /// Close a job: terminal state, finish time, and the error when it failed.
  pub async fn finish_rip(
    &self,
    id: i64,
    state: RipState,
    error: Option<&str>,
  ) -> Result<(), IndexError> {
    let error = error.map(str::to_string);
    self
      .call("closing a rip job", move |conn| {
        conn.execute(
          "UPDATE rips SET
             state = ?2,
             finished_at = ?3,
             error = ?4,
             current_op = NULL,
             progress_permille = CASE WHEN ?2 = 'done' THEN 1000
                                      ELSE progress_permille END
           WHERE id = ?1",
          params![id, state.as_str(), now_secs(), error],
        )?;
        Ok(())
      })
      .await
  }

  /// Close every job a previous process left open: the rip died with it.
  pub async fn fail_interrupted_rips(&self) -> Result<usize, IndexError> {
    self
      .call("closing interrupted rip jobs", move |conn| {
        conn.execute(
          "UPDATE rips SET
             state = 'failed',
             finished_at = ?1,
             error = 'interrupted by server restart',
             current_op = NULL
           WHERE state NOT IN ('done', 'skipped', 'failed')",
          params![now_secs()],
        )
      })
      .await
  }

  pub async fn rip(&self, id: i64) -> Result<Option<RipRecord>, IndexError> {
    self
      .call("looking up a rip job", move |conn| {
        conn
          .query_row(
            &format!("SELECT {RIP_COLUMNS} FROM rips WHERE id = ?1"),
            params![id],
            rip_from_row,
          )
          .optional()
      })
      .await
  }

  /// The most recently started jobs, newest first.
  pub async fn recent_rips(
    &self,
    limit: u32,
  ) -> Result<Vec<RipRecord>, IndexError> {
    self
      .call("listing recent rip jobs", move |conn| {
        let mut stmt = conn.prepare_cached(&format!(
          "SELECT {RIP_COLUMNS} FROM rips
           ORDER BY started_at DESC, id DESC
           LIMIT ?1"
        ))?;
        let rows = stmt
          .query_map(params![limit], rip_from_row)?
          .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
      })
      .await
  }

  /// Run a closure on the connection thread, stamping failures with a
  /// semantic context.
  async fn call<T, F>(
    &self,
    context: &'static str,
    f: F,
  ) -> Result<T, IndexError>
  where
    T: Send + 'static,
    F: FnOnce(&mut rusqlite::Connection) -> Result<T, rusqlite::Error>
      + Send
      + 'static,
  {
    self
      .conn
      .call(f)
      .await
      .map_err(|source| IndexError::IndexQuery { context, source })
  }
}

/// Map one `RECORD_COLUMNS` row back into a [`VideoRecord`].
fn record_from_row(
  row: &rusqlite::Row<'_>,
) -> Result<VideoRecord, rusqlite::Error> {
  let genres_json: String = row.get("genres")?;
  Ok(VideoRecord {
    library: row.get("library")?,
    rel_path: row.get("rel_path")?,
    parent: row.get("parent")?,
    file_name: row.get("file_name")?,
    size: row.get("size")?,
    mtime: row.get("mtime")?,
    title: row.get("title")?,
    title_source: row
      .get::<_, Option<String>>("title_source")?
      .as_deref()
      .and_then(TitleSource::parse),
    duration_secs: row.get("duration_secs")?,
    upload_date: row.get("upload_date")?,
    year: row.get("year")?,
    description: row.get("description")?,
    channel: row.get("channel")?,
    channel_url: row.get("channel_url")?,
    webpage_url: row.get("webpage_url")?,
    view_count: row.get("view_count")?,
    // Stored by us as JSON; junk degrades to empty rather than failing the
    // whole query row.
    genres: serde_json::from_str(&genres_json).unwrap_or_default(),
    container: row.get("container")?,
    vcodec: row.get("vcodec")?,
    acodec: row.get("acodec")?,
    vprofile: row.get("vprofile")?,
    field_order: row.get("field_order")?,
    codec_source: row
      .get::<_, Option<String>>("codec_source")?
      .as_deref()
      .and_then(CodecSource::parse),
    thumb_rel_path: row.get("thumb_rel_path")?,
    compat_rel_path: row.get("compat_rel_path")?,
    disc_set: row.get("disc_set")?,
    disc_title_index: row.get("disc_title_index")?,
    derivation_error: row.get("derivation_error")?,
  })
}

/// Map one `DISC_COLUMNS` row back into a [`DiscRecord`].
fn disc_from_row(
  row: &rusqlite::Row<'_>,
) -> Result<DiscRecord, rusqlite::Error> {
  Ok(DiscRecord {
    library: row.get("library")?,
    rel_path: row.get("rel_path")?,
    parent: row.get("parent")?,
    label: row.get("label")?,
    disc_id: row.get("disc_id")?,
    id_source: row
      .get::<_, Option<String>>("id_source")?
      .as_deref()
      .and_then(IdSource::parse),
    title: row.get("title")?,
    year: row.get("year")?,
    poster_rel_path: row.get("poster_rel_path")?,
    fingerprint: row.get("fingerprint")?,
    fingerprint_titles: row.get("fingerprint_titles")?,
    // A junk state can only come from a hand-edited database; treating it
    // as unresolved simply re-queues the disc.
    identity_state: IdentityState::parse(
      &row.get::<_, String>("identity_state")?,
    )
    .unwrap_or(IdentityState::Unresolved),
    identity_source: row
      .get::<_, Option<String>>("identity_source")?
      .as_deref()
      .and_then(IdentitySource::parse),
    identity_error: row.get("identity_error")?,
    candidates_json: row.get("candidates_json")?,
    identity_attempted_at: row.get("identity_attempted_at")?,
    identified_at: row.get("identified_at")?,
    // The .ok() discard is deliberate: a negative stored id is not a TMDB
    // id, and absent beats absurd.
    tmdb_id: row
      .get::<_, Option<i64>>("tmdb_id")?
      .and_then(|id| u64::try_from(id).ok()),
    tmdb_kind: row
      .get::<_, Option<String>>("tmdb_kind")?
      .as_deref()
      .and_then(TmdbKind::parse),
  })
}

/// Map one `RIP_COLUMNS` row back into a [`RipRecord`].  Junk enum values
/// can only come from a hand-edited database; each degrades to the most
/// conservative reading rather than failing the whole query.
fn rip_from_row(row: &rusqlite::Row<'_>) -> Result<RipRecord, rusqlite::Error> {
  Ok(RipRecord {
    id: row.get("id")?,
    device: row.get("device")?,
    library: row.get("library")?,
    dir_rel_path: row.get("dir_rel_path")?,
    label: row.get("label")?,
    disc_id: row.get("disc_id")?,
    policy: RipPolicy::parse(&row.get::<_, String>("policy")?)
      .unwrap_or(RipPolicy::Skip),
    trigger: RipTrigger::parse(&row.get::<_, String>("trigger")?)
      .unwrap_or(RipTrigger::Auto),
    state: RipState::parse(&row.get::<_, String>("state")?)
      .unwrap_or(RipState::Failed),
    progress_permille: row.get("progress_permille")?,
    current_op: row.get("current_op")?,
    started_at: row.get("started_at")?,
    finished_at: row.get("finished_at")?,
    error: row.get("error")?,
  })
}

/// The parent directory of a library-relative path, where the empty string
/// is the library root.
fn parent_of(rel_path: &str) -> &str {
  rel_path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// Escape SQL LIKE wildcards so a literal path prefix cannot widen a match.
fn escape_like(input: &str) -> String {
  input
    .replace('\\', "\\\\")
    .replace('%', "\\%")
    .replace('_', "\\_")
}

/// Turn raw user input into an FTS5 MATCH expression, or `None` when the
/// input carries no searchable tokens.  Every token is wrapped as a quoted
/// phrase-prefix (`"tok"*`) with embedded quotes doubled, so FTS operator
/// syntax (`NEAR(`, `-`, unbalanced quotes) can never reach the parser as
/// syntax.
pub fn fts_query(user_input: &str) -> Option<String> {
  let tokens: Vec<String> = user_input
    .split_whitespace()
    .map(|token| format!("\"{}\"*", token.replace('"', "\"\"")))
    .collect();
  (!tokens.is_empty()).then(|| tokens.join(" "))
}

/// Seconds since the Unix epoch; a clock set before 1970 degrades to zero
/// rather than panicking.
pub fn now_secs() -> i64 {
  i64::try_from(
    SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or(Duration::ZERO)
      .as_secs(),
  )
  .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn fts_query_quotes_tokens_as_phrase_prefixes() {
    assert_eq!(fts_query("matrix"), Some("\"matrix\"*".to_string()));
    assert_eq!(
      fts_query("the matrix"),
      Some("\"the\"* \"matrix\"*".to_string())
    );
  }

  #[test]
  fn fts_query_neutralizes_operator_syntax() {
    assert_eq!(fts_query("NEAR("), Some("\"NEAR(\"*".to_string()));
    assert_eq!(fts_query("-negated"), Some("\"-negated\"*".to_string()));
    assert_eq!(fts_query("a\"quote"), Some("\"a\"\"quote\"*".to_string()));
    assert_eq!(fts_query("\""), Some("\"\"\"\"*".to_string()));
  }

  #[test]
  fn fts_query_empty_input_yields_none() {
    assert_eq!(fts_query(""), None);
    assert_eq!(fts_query("   \t "), None);
  }

  #[test]
  fn fts_query_handles_emoji() {
    assert_eq!(fts_query("🎬 clip"), Some("\"🎬\"* \"clip\"*".to_string()));
  }
}
