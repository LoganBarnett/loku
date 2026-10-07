//! The fingerprint-to-identity cache: one table in its own SQLite file.
//!
//! The media index is dropped and rebuilt on any schema change, which would
//! wipe a cache whose value is the *next* encounter of a disc whose original
//! directory may be long gone; the cache therefore lives in its own file
//! with its own version.  Loss only costs a TMDB search per pressing, so it
//! still needs no migrations.

use rusqlite::{params, OptionalExtension};
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

use crate::identity::{Identity, IdentitySource, TmdbKind};
use crate::index::store::now_secs;

const CACHE_VERSION: i32 = 1;

const CREATE_SQL: &str = "
CREATE TABLE disc_identities (
  fingerprint TEXT PRIMARY KEY,
  tmdb_kind TEXT NOT NULL CHECK (tmdb_kind IN ('movie', 'tv')),
  tmdb_id INTEGER NOT NULL,
  title TEXT NOT NULL,
  year INTEGER,
  resolved_at INTEGER NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('search', 'attended'))
);
";

#[derive(Debug, Error)]
pub enum IdentityCacheError {
  #[error("Failed to open the disc identity cache '{path}': {source}")]
  CacheOpen {
    path: String,
    source: rusqlite::Error,
  },

  #[error("Failed to initialize the disc identity cache schema: {source}")]
  CacheSchema { source: tokio_rusqlite::Error },

  #[error("Disc identity cache query failed while {context}: {source}")]
  CacheQuery {
    context: &'static str,
    source: tokio_rusqlite::Error,
  },
}

/// A remembered resolution: the identity plus how it was first established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedIdentity {
  pub identity: Identity,
  pub source: IdentitySource,
  pub resolved_at: i64,
}

#[derive(Clone)]
pub struct IdentityCache {
  conn: tokio_rusqlite::Connection,
}

impl IdentityCache {
  /// Open (creating if needed) `identity.db` inside the state directory.
  pub async fn open(state_dir: &Path) -> Result<Self, IdentityCacheError> {
    let path = state_dir.join("identity.db");
    let conn =
      tokio_rusqlite::Connection::open(&path)
        .await
        .map_err(|source| IdentityCacheError::CacheOpen {
          path: path.to_string_lossy().to_string(),
          source,
        })?;
    Self::init(conn).await
  }

  pub async fn open_in_memory() -> Result<Self, IdentityCacheError> {
    let conn =
      tokio_rusqlite::Connection::open_in_memory()
        .await
        .map_err(|source| IdentityCacheError::CacheOpen {
          path: ":memory:".to_string(),
          source,
        })?;
    Self::init(conn).await
  }

  async fn init(
    conn: tokio_rusqlite::Connection,
  ) -> Result<Self, IdentityCacheError> {
    conn
      .call(|conn| {
        conn.busy_timeout(Duration::from_secs(5))?;
        let version: i32 =
          conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != CACHE_VERSION {
          let tx = conn.transaction()?;
          tx.execute_batch("DROP TABLE IF EXISTS disc_identities;")?;
          tx.execute_batch(CREATE_SQL)?;
          // CACHE_VERSION is a compile-time integer, so formatting it into
          // the pragma is not an injection surface.
          tx.execute_batch(&format!("PRAGMA user_version = {CACHE_VERSION};"))?;
          tx.commit()?;
        }
        Ok(())
      })
      .await
      .map_err(|source| IdentityCacheError::CacheSchema { source })?;
    Ok(Self { conn })
  }

  pub async fn lookup(
    &self,
    fingerprint: &str,
  ) -> Result<Option<CachedIdentity>, IdentityCacheError> {
    let fingerprint = fingerprint.to_string();
    self
      .call("looking up a disc fingerprint", move |conn| {
        conn
          .query_row(
            "SELECT tmdb_kind, tmdb_id, title, year, resolved_at, source
             FROM disc_identities WHERE fingerprint = ?1",
            params![fingerprint],
            |row| {
              Ok(CachedIdentity {
                identity: Identity {
                  // A junk kind can only come from a hand-edited file; the
                  // CHECK constraint keeps that from happening through us.
                  kind: TmdbKind::parse(&row.get::<_, String>(0)?)
                    .unwrap_or(TmdbKind::Movie),
                  // A negative stored id is not a TMDB id; zero is a harmless
                  // miss downstream.
                  id: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                  title: row.get(2)?,
                  year: row.get(3)?,
                },
                resolved_at: row.get(4)?,
                source: IdentitySource::parse(&row.get::<_, String>(5)?)
                  .unwrap_or(IdentitySource::Search),
              })
            },
          )
          .optional()
      })
      .await
  }

  /// Remember (or overwrite) the identity of a pressing.  An attended pick
  /// overwrites an automatic one; that is the point of the operator's word.
  pub async fn record(
    &self,
    fingerprint: &str,
    identity: &Identity,
    source: IdentitySource,
  ) -> Result<(), IdentityCacheError> {
    let fingerprint = fingerprint.to_string();
    let identity = identity.clone();
    self
      .call("recording a disc identity", move |conn| {
        conn.execute(
          "INSERT INTO disc_identities (
             fingerprint, tmdb_kind, tmdb_id, title, year, resolved_at, source
           ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
           ON CONFLICT (fingerprint) DO UPDATE SET
             tmdb_kind = excluded.tmdb_kind,
             tmdb_id = excluded.tmdb_id,
             title = excluded.title,
             year = excluded.year,
             resolved_at = excluded.resolved_at,
             source = excluded.source",
          params![
            fingerprint,
            identity.kind.as_str(),
            // An id beyond i64 is not a TMDB id; storing zero keeps the row
            // harmless.
            i64::try_from(identity.id).unwrap_or(0),
            identity.title,
            identity.year,
            now_secs(),
            source.as_str(),
          ],
        )?;
        Ok(())
      })
      .await
  }

  async fn call<T, F>(
    &self,
    context: &'static str,
    f: F,
  ) -> Result<T, IdentityCacheError>
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
      .map_err(|source| IdentityCacheError::CacheQuery { context, source })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn matrix() -> Identity {
    Identity {
      kind: TmdbKind::Movie,
      id: 603,
      title: "The Matrix".to_string(),
      year: Some(1999),
    }
  }

  #[tokio::test]
  async fn records_and_looks_up_by_fingerprint() {
    let cache = IdentityCache::open_in_memory().await.unwrap();
    assert_eq!(cache.lookup("fp").await.unwrap(), None);
    cache
      .record("fp", &matrix(), IdentitySource::Search)
      .await
      .unwrap();
    let hit = cache.lookup("fp").await.unwrap().unwrap();
    assert_eq!(hit.identity, matrix());
    assert_eq!(hit.source, IdentitySource::Search);
    assert!(hit.resolved_at > 0);
  }

  #[tokio::test]
  async fn an_attended_pick_overwrites_a_search_result() {
    let cache = IdentityCache::open_in_memory().await.unwrap();
    cache
      .record("fp", &matrix(), IdentitySource::Search)
      .await
      .unwrap();
    let corrected = Identity {
      kind: TmdbKind::Tv,
      id: 1,
      title: "Actually A Show".to_string(),
      year: None,
    };
    cache
      .record("fp", &corrected, IdentitySource::Attended)
      .await
      .unwrap();
    let hit = cache.lookup("fp").await.unwrap().unwrap();
    assert_eq!(hit.identity, corrected);
    assert_eq!(hit.source, IdentitySource::Attended);
  }

  #[tokio::test]
  async fn a_file_backed_cache_persists() {
    let dir = tempfile::tempdir().unwrap();
    {
      let cache = IdentityCache::open(dir.path()).await.unwrap();
      cache
        .record("fp", &matrix(), IdentitySource::Search)
        .await
        .unwrap();
    }
    let reopened = IdentityCache::open(dir.path()).await.unwrap();
    assert_eq!(
      reopened.lookup("fp").await.unwrap().unwrap().identity,
      matrix()
    );
  }
}
