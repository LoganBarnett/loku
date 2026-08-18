//! Media index schema.
//!
//! The index is a *derived* structure: everything in `videos` (and its FTS
//! mirror) is rebuildable from the library filesystems and their sidecars.
//! The only durable state of consequence is `disc_set_overrides` — the
//! operator's "this is the main title" picks — plus two caches
//! (`probe_cache`, `derivation_failures`) whose loss merely costs recompute.
//! That is why versioned migrations are deliberately absent: on any
//! `user_version` mismatch everything is dropped and rescanned, and the
//! override loss is a two-click recovery.  Revisit if durable state grows.

use std::time::Duration;

pub const SCHEMA_VERSION: i32 = 1;

const CREATE_SQL: &str = "
CREATE TABLE videos (
  id INTEGER PRIMARY KEY,
  library TEXT NOT NULL,
  rel_path TEXT NOT NULL,
  parent TEXT NOT NULL,
  file_name TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  title TEXT,
  title_source TEXT,
  duration_secs REAL,
  upload_date TEXT,
  year INTEGER,
  description TEXT,
  channel TEXT,
  channel_url TEXT,
  webpage_url TEXT,
  view_count INTEGER,
  genres TEXT NOT NULL DEFAULT '[]',
  container TEXT NOT NULL,
  vcodec TEXT,
  acodec TEXT,
  vprofile TEXT,
  field_order TEXT,
  codec_source TEXT,
  thumb_rel_path TEXT,
  compat_rel_path TEXT,
  disc_set TEXT,
  disc_title_index INTEGER,
  last_seen INTEGER NOT NULL DEFAULT 0,
  UNIQUE (library, rel_path)
);
CREATE INDEX idx_videos_library_parent ON videos (library, parent);

CREATE VIRTUAL TABLE videos_fts USING fts5(
  title, description, channel, file_name,
  content='videos', content_rowid='id',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER videos_ai AFTER INSERT ON videos BEGIN
  INSERT INTO videos_fts (rowid, title, description, channel, file_name)
  VALUES (new.id, new.title, new.description, new.channel, new.file_name);
END;
CREATE TRIGGER videos_ad AFTER DELETE ON videos BEGIN
  INSERT INTO videos_fts (videos_fts, rowid, title, description, channel,
                          file_name)
  VALUES ('delete', old.id, old.title, old.description, old.channel,
          old.file_name);
END;
CREATE TRIGGER videos_au AFTER UPDATE ON videos BEGIN
  INSERT INTO videos_fts (videos_fts, rowid, title, description, channel,
                          file_name)
  VALUES ('delete', old.id, old.title, old.description, old.channel,
          old.file_name);
  INSERT INTO videos_fts (rowid, title, description, channel, file_name)
  VALUES (new.id, new.title, new.description, new.channel, new.file_name);
END;

CREATE TABLE probe_cache (
  library TEXT NOT NULL,
  rel_path TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  vcodec TEXT,
  acodec TEXT,
  vprofile TEXT,
  field_order TEXT,
  duration_secs REAL,
  width INTEGER,
  height INTEGER,
  PRIMARY KEY (library, rel_path)
);

CREATE TABLE disc_set_overrides (
  library TEXT NOT NULL,
  disc_set TEXT NOT NULL,
  main_rel_path TEXT NOT NULL,
  PRIMARY KEY (library, disc_set)
);

CREATE TABLE derivation_failures (
  library TEXT NOT NULL,
  rel_path TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  error TEXT NOT NULL,
  failed_at INTEGER NOT NULL,
  PRIMARY KEY (library, rel_path)
);

CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
";

const DROP_SQL: &str = "
DROP TRIGGER IF EXISTS videos_ai;
DROP TRIGGER IF EXISTS videos_ad;
DROP TRIGGER IF EXISTS videos_au;
DROP TABLE IF EXISTS videos_fts;
DROP TABLE IF EXISTS videos;
DROP TABLE IF EXISTS probe_cache;
DROP TABLE IF EXISTS disc_set_overrides;
DROP TABLE IF EXISTS derivation_failures;
DROP TABLE IF EXISTS meta;
";

/// Prepare a freshly opened connection: pragmas, then drop-and-recreate the
/// whole schema when the on-disk `user_version` does not match.
pub fn initialize(
  conn: &mut rusqlite::Connection,
) -> Result<(), rusqlite::Error> {
  // WAL keeps readers unblocked during scan transactions.  The pragma
  // returns the resulting mode as a row (and in-memory databases stay on
  // their own journal), so it is issued as a query rather than an update.
  conn.query_row("PRAGMA journal_mode = WAL", [], |_row| Ok(()))?;
  conn.execute_batch("PRAGMA foreign_keys = ON;")?;
  conn.busy_timeout(Duration::from_secs(5))?;
  let version: i32 =
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
  if version != SCHEMA_VERSION {
    let tx = conn.transaction()?;
    tx.execute_batch(DROP_SQL)?;
    tx.execute_batch(CREATE_SQL)?;
    // SCHEMA_VERSION is a compile-time integer, so formatting it into the
    // pragma is not an injection surface.
    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    tx.commit()?;
  }
  Ok(())
}
