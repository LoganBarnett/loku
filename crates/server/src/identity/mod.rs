//! Disc identity: resolving a ripped disc to a TMDB title so the library can
//! carry a real name, year, and artwork instead of a volume label.

pub mod cache;
pub mod fingerprint;
pub mod naming;
pub mod output;
pub mod resolver;
pub mod tmdb;
pub mod worker;

use rust_template_foundation::Server;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tracing::warn;

use crate::config::Config;
use crate::index::store::Index;
use crate::index::wakes::Wakes;
use crate::web_base::IdentityCheck;
use resolver::Resolver;

/// A resolved TMDB identity: the catalogue entry a disc is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Identity {
  pub kind: TmdbKind,
  pub id: u64,
  pub title: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub year: Option<i32>,
}

/// The TMDB catalogue a disc resolves into.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TmdbKind {
  Movie,
  Tv,
}

impl TmdbKind {
  pub fn as_str(self) -> &'static str {
    match self {
      TmdbKind::Movie => "movie",
      TmdbKind::Tv => "tv",
    }
  }

  pub fn parse(value: &str) -> Option<Self> {
    match value {
      "movie" => Some(TmdbKind::Movie),
      "tv" => Some(TmdbKind::Tv),
      _ => None,
    }
  }
}

/// Where a disc stands on having a resolved identity.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum IdentityState {
  /// Nothing has resolved it yet; the identity worker will try.
  Unresolved,
  /// Automatic resolution was not confident; candidates await an operator.
  NeedsConfirmation,
  /// A TMDB identity is recorded (and written to the NFO on disk).
  Resolved,
  /// Resolution hit a definitive error; an operator pick clears it.
  Failed,
}

impl IdentityState {
  pub fn as_str(self) -> &'static str {
    match self {
      IdentityState::Unresolved => "unresolved",
      IdentityState::NeedsConfirmation => "needs_confirmation",
      IdentityState::Resolved => "resolved",
      IdentityState::Failed => "failed",
    }
  }

  pub fn parse(value: &str) -> Option<Self> {
    match value {
      "unresolved" => Some(IdentityState::Unresolved),
      "needs_confirmation" => Some(IdentityState::NeedsConfirmation),
      "resolved" => Some(IdentityState::Resolved),
      "failed" => Some(IdentityState::Failed),
      _ => None,
    }
  }
}

/// How a resolved identity was established.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
  /// A fingerprint cache hit from an earlier encounter of the pressing.
  Cache,
  /// A confident automatic TMDB search match.
  Search,
  /// An operator's pick.
  Attended,
  /// Read back from an NFO already on disk.
  Nfo,
}

impl IdentitySource {
  pub fn as_str(self) -> &'static str {
    match self {
      IdentitySource::Cache => "cache",
      IdentitySource::Search => "search",
      IdentitySource::Attended => "attended",
      IdentitySource::Nfo => "nfo",
    }
  }

  pub fn parse(value: &str) -> Option<Self> {
    match value {
      "cache" => Some(IdentitySource::Cache),
      "search" => Some(IdentitySource::Search),
      "attended" => Some(IdentitySource::Attended),
      "nfo" => Some(IdentitySource::Nfo),
      _ => None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn enums_round_trip_through_their_strings() {
    for kind in [TmdbKind::Movie, TmdbKind::Tv] {
      assert_eq!(TmdbKind::parse(kind.as_str()), Some(kind));
    }
    for state in [
      IdentityState::Unresolved,
      IdentityState::NeedsConfirmation,
      IdentityState::Resolved,
      IdentityState::Failed,
    ] {
      assert_eq!(IdentityState::parse(state.as_str()), Some(state));
    }
    for source in [
      IdentitySource::Cache,
      IdentitySource::Search,
      IdentitySource::Attended,
      IdentitySource::Nfo,
    ] {
      assert_eq!(IdentitySource::parse(source.as_str()), Some(source));
    }
    assert_eq!(TmdbKind::parse("person"), None);
  }
}

#[derive(Debug, Error)]
pub enum IdentityStartupError {
  #[error(transparent)]
  Cache(#[from] cache::IdentityCacheError),

  #[error(transparent)]
  Tmdb(#[from] tmdb::TmdbError),
}

/// Open the fingerprint cache and build the TMDB client: everything the
/// resolver needs to run.
pub async fn bootstrap(
  state_dir: &Path,
  api_key: &str,
  language: &str,
) -> Result<resolver::Resolver, IdentityStartupError> {
  let cache = cache::IdentityCache::open(state_dir).await?;
  let client = tmdb::RealTmdb::new(api_key, language)?;
  Ok(resolver::Resolver::new(
    Arc::new(client) as Arc<dyn tmdb::TmdbClient>,
    cache,
  ))
}

/// Bring up identity resolution when a TMDB key is configured.
pub async fn subsystem(
  config: &Config,
  server: &Server,
  index: &Index,
  wakes: &Wakes,
) -> Option<Arc<Resolver>> {
  let registry = &server.base_state().health_registry;
  match config.tmdb.as_ref() {
    None => {
      registry
        .register(
          "identity",
          IdentityCheck {
            disabled_reason: Some("TMDB API key not configured".to_string()),
          },
        )
        .await;
      None
    }
    Some(tmdb) => {
      match bootstrap(&config.state_dir, &tmdb.api_key, &tmdb.language).await {
        Ok(resolver) => {
          registry
            .register(
              "identity",
              IdentityCheck {
                disabled_reason: None,
              },
            )
            .await;
          let resolver = Arc::new(resolver);
          worker::spawn(
            index.clone(),
            config.libraries.clone(),
            resolver.clone(),
            wakes.identity.clone(),
          );
          Some(resolver)
        }
        Err(error) => {
          warn!(%error, "Identity resolution unavailable; discs will stay unidentified");
          registry
            .register(
              "identity",
              IdentityCheck {
                disabled_reason: Some(error.to_string()),
              },
            )
            .await;
          None
        }
      }
    }
  }
}
