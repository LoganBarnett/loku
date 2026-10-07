//! The resolver chain: ordered strategies, cheap and deterministic first,
//! each either settling the identity, contributing candidates for an
//! operator to confirm, or passing.  The chain is a seam: TheDiscDB
//! structural matching and barcode lookup slot in ahead of the text search
//! when they land, without changing the worker or the API.

use async_trait::async_trait;
use futures_util::stream::{self, StreamExt, TryStreamExt};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::identity::cache::{IdentityCache, IdentityCacheError};
use crate::identity::fingerprint::{self, Fingerprint};
use crate::identity::naming::{self, TitleHint};
use crate::identity::output::{self, NfoDoc, OutputError};
use crate::identity::tmdb::{Candidate, TmdbClient, TmdbError};
use crate::identity::{Identity, IdentitySource, TmdbKind};
use crate::index::scan::{self, ScanError};
use crate::index::store::{DiscRecord, IdentityOutcome, Index, IndexError};
use crate::library::Library;
use crate::rip::manifest::{self, DiscManifest, ManifestError};

/// How many candidates are kept for an operator to choose from.
pub const MAX_CANDIDATES: usize = 8;
/// A second-place candidate this much less popular than the first is not a
/// real contender.
const RUNNER_UP_RATIO: f64 = 0.5;
/// How many collision suffixes a rename tries before keeping the old name.
const RENAME_ATTEMPTS: u32 = 8;

/// What the search decided, purely from the candidates.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
  Accept(Candidate),
  Confirm(Vec<Candidate>),
}

/// Accept only a match that leaves no real doubt: a lone related hit, or a
/// top hit whose title matches the query exactly (year too, when the label
/// carried one) with no exact-match twin and no close runner-up that the
/// year hint does not already rule out.  A season marker narrows the field
/// to series first.  Everything else goes to the operator with the top few
/// candidates.
pub fn decide(hint: &TitleHint, candidates: Vec<Candidate>) -> Decision {
  let pool = hint.kind_hint.map_or_else(
    || candidates.clone(),
    |kind| {
      let same: Vec<Candidate> = candidates
        .iter()
        .filter(|c| c.kind == kind)
        .cloned()
        .collect();
      if same.is_empty() {
        candidates.clone()
      } else {
        same
      }
    },
  );
  let query = naming::normalized_title(&hint.query);
  let exact = |c: &Candidate| {
    naming::normalized_title(&c.title) == query
      || c
        .original_title
        .as_deref()
        .is_some_and(|t| naming::normalized_title(t) == query)
  };
  let related = |c: &Candidate| {
    let title = naming::normalized_title(&c.title);
    !query.is_empty() && (title.contains(&query) || query.contains(&title))
  };
  let year_ok = |c: &Candidate| hint.year.is_none() || hint.year == c.year;
  match pool.as_slice() {
    [only] if related(only) && year_ok(only) => Decision::Accept(only.clone()),
    [top, second, ..]
      if exact(top)
        && year_ok(top)
        && !(hint.year.is_none() && exact(second))
        && (!year_ok(second)
          || second.popularity < RUNNER_UP_RATIO * top.popularity) =>
    {
      Decision::Accept(top.clone())
    }
    _ => Decision::Confirm(pool.into_iter().take(MAX_CANDIDATES).collect()),
  }
}

/// Everything a strategy may need about one disc.
#[derive(Debug, Clone)]
pub struct DiscContext {
  pub library: Library,
  pub rel_path: String,
  pub dir: PathBuf,
  pub disc_id: String,
  pub fingerprint: Fingerprint,
  pub hint: Option<TitleHint>,
}

pub enum StrategyOutcome {
  Identified {
    identity: Identity,
    source: IdentitySource,
  },
  Candidates(Vec<Candidate>),
  Pass,
}

#[async_trait]
pub trait Strategy: Send + Sync {
  /// Try to settle the disc, seeing what earlier strategies proposed.
  async fn attempt(
    &self,
    ctx: &DiscContext,
    prior: &[Candidate],
  ) -> Result<StrategyOutcome, ResolveError>;
}

/// A fingerprint cache hit: proof for a strong fingerprint, a leading
/// suggestion for a weak one.
struct CacheStrategy {
  cache: IdentityCache,
}

#[async_trait]
impl Strategy for CacheStrategy {
  async fn attempt(
    &self,
    ctx: &DiscContext,
    _prior: &[Candidate],
  ) -> Result<StrategyOutcome, ResolveError> {
    Ok(self.cache.lookup(&ctx.fingerprint.sha256).await?.map_or(
      StrategyOutcome::Pass,
      |hit| {
        if ctx.fingerprint.is_strong() {
          StrategyOutcome::Identified {
            identity: hit.identity,
            source: IdentitySource::Cache,
          }
        } else {
          StrategyOutcome::Candidates(vec![candidate_of(&hit.identity)])
        }
      },
    ))
  }
}

fn candidate_of(identity: &Identity) -> Candidate {
  Candidate {
    kind: identity.kind,
    id: identity.id,
    title: identity.title.clone(),
    original_title: None,
    year: identity.year,
    overview: None,
    poster_path: None,
    popularity: 0.0,
    vote_count: 0,
  }
}

/// The TMDB text search over the disc's title hint.
struct SearchStrategy {
  client: Arc<dyn TmdbClient>,
}

#[async_trait]
impl Strategy for SearchStrategy {
  async fn attempt(
    &self,
    ctx: &DiscContext,
    prior: &[Candidate],
  ) -> Result<StrategyOutcome, ResolveError> {
    match ctx.hint.as_ref() {
      None => Ok(StrategyOutcome::Candidates(prior.to_vec())),
      Some(hint) => {
        let found = self.client.search(&hint.query, hint.year).await?;
        // A year from the label can be wrong (a re-release date, say); an
        // empty year-filtered search is retried without it before giving up.
        let found = if found.is_empty() && hint.year.is_some() {
          self.client.search(&hint.query, None).await?
        } else {
          found
        };
        Ok(match decide(hint, found) {
          Decision::Accept(candidate) => {
            // A weak cache hit that the search contradicts is exactly the
            // case the cache alone must not settle.
            if prior
              .iter()
              .any(|p| (p.kind, p.id) != (candidate.kind, candidate.id))
            {
              StrategyOutcome::Candidates(merge(prior, vec![candidate]))
            } else {
              StrategyOutcome::Identified {
                source: if prior.is_empty() {
                  IdentitySource::Search
                } else {
                  IdentitySource::Cache
                },
                identity: identity_of(&candidate),
              }
            }
          }
          Decision::Confirm(candidates) => {
            StrategyOutcome::Candidates(merge(prior, candidates))
          }
        })
      }
    }
  }
}

fn identity_of(candidate: &Candidate) -> Identity {
  Identity {
    kind: candidate.kind,
    id: candidate.id,
    title: candidate.title.clone(),
    year: candidate.year,
  }
}

/// Prior candidates first, then new ones not already present.
fn merge(prior: &[Candidate], found: Vec<Candidate>) -> Vec<Candidate> {
  found
    .into_iter()
    .fold(prior.to_vec(), |mut merged, candidate| {
      if !merged
        .iter()
        .any(|c| (c.kind, c.id) == (candidate.kind, candidate.id))
      {
        merged.push(candidate);
      }
      merged
    })
    .into_iter()
    .take(MAX_CANDIDATES)
    .collect()
}

#[derive(Debug, Error)]
pub enum ResolveError {
  #[error("Failed to read the disc manifest at '{path}' for identity resolution: {source}")]
  ManifestRead { path: String, source: ManifestError },

  #[error("Disc directory '{path}' has no manifest to resolve from")]
  ManifestMissing { path: String },

  #[error(transparent)]
  Tmdb(#[from] TmdbError),

  #[error(transparent)]
  Cache(#[from] IdentityCacheError),
}

#[derive(Debug, Error)]
pub enum CommitError {
  #[error(transparent)]
  Tmdb(#[from] TmdbError),

  #[error(transparent)]
  Output(#[from] OutputError),

  #[error("Failed to rename disc directory '{from}' to '{to}': {source}")]
  DirectoryRename {
    from: String,
    to: String,
    source: std::io::Error,
  },

  #[error(transparent)]
  Index(#[from] IndexError),

  #[error(transparent)]
  IndexScan(#[from] ScanError),

  #[error(transparent)]
  Cache(#[from] IdentityCacheError),
}

/// Why a fold over the strategies stopped before the last one.
enum Halt {
  Settled {
    identity: Identity,
    source: IdentitySource,
  },
  Failed(ResolveError),
}

/// How resolution ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
  Resolved {
    identity: Identity,
    source: IdentitySource,
  },
  NeedsConfirmation {
    candidates: Vec<Candidate>,
  },
}

/// What a commit wrote and where the disc now lives.
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
  pub rel_path: String,
  pub identity: Identity,
  pub renamed: bool,
  pub artwork_warnings: Vec<String>,
}

pub struct Resolver {
  strategies: Vec<Box<dyn Strategy>>,
  client: Arc<dyn TmdbClient>,
  cache: IdentityCache,
  /// Serializes commits: the worker and an operator must not both write
  /// one disc's sidecars.
  commit_lock: Mutex<()>,
}

impl Resolver {
  pub fn new(client: Arc<dyn TmdbClient>, cache: IdentityCache) -> Self {
    Self {
      strategies: vec![
        Box::new(CacheStrategy {
          cache: cache.clone(),
        }),
        Box::new(SearchStrategy {
          client: client.clone(),
        }),
      ],
      client,
      cache,
      commit_lock: Mutex::new(()),
    }
  }

  /// Everything the chain needs about a disc, from its manifest.
  pub async fn context(
    &self,
    library: &Library,
    disc: &DiscRecord,
  ) -> Result<DiscContext, ResolveError> {
    let dir = library.path.join(&disc.rel_path);
    let manifest = manifest::read_manifest(&dir)
      .await
      .map_err(|source| ResolveError::ManifestRead {
        path: dir.display().to_string(),
        source,
      })?
      .ok_or_else(|| ResolveError::ManifestMissing {
        path: dir.display().to_string(),
      })?;
    Ok(DiscContext {
      library: library.clone(),
      rel_path: disc.rel_path.clone(),
      dir,
      disc_id: disc.disc_id.clone(),
      fingerprint: fingerprint::compute(&manifest.titles),
      hint: hint_of(&manifest, &disc.label),
    })
  }

  /// Run the chain.
  pub async fn resolve(
    &self,
    ctx: &DiscContext,
  ) -> Result<Resolution, ResolveError> {
    let outcome = stream::iter(&self.strategies)
      .map(Ok::<_, Halt>)
      // `try_fold` stops only through its error channel, so a settled
      // identity travels as `Err(Halt::Settled)`.
      .try_fold(Vec::new(), |prior, strategy| async move {
        match strategy.attempt(ctx, &prior).await.map_err(Halt::Failed)? {
          StrategyOutcome::Identified { identity, source } => {
            Err(Halt::Settled { identity, source })
          }
          StrategyOutcome::Candidates(candidates) => Ok(candidates),
          StrategyOutcome::Pass => Ok(prior),
        }
      })
      .await;
    match outcome {
      Ok(candidates) => Ok(Resolution::NeedsConfirmation {
        candidates: candidates.into_iter().take(MAX_CANDIDATES).collect(),
      }),
      Err(Halt::Settled { identity, source }) => {
        Ok(Resolution::Resolved { identity, source })
      }
      Err(Halt::Failed(error)) => Err(error),
    }
  }

  /// A fresh search for the operator, nothing committed.
  pub async fn search(
    &self,
    query: &str,
    year: Option<i32>,
  ) -> Result<Vec<Candidate>, TmdbError> {
    self
      .client
      .search(query, year)
      .await
      .map(|candidates| candidates.into_iter().take(MAX_CANDIDATES).collect())
  }

  /// Make an identity stick on disk and in the index.
  pub async fn commit(
    &self,
    index: &Index,
    ctx: &DiscContext,
    kind: TmdbKind,
    id: u64,
    source: IdentitySource,
  ) -> Result<Committed, CommitError> {
    let _serialized = self.commit_lock.lock().await;
    let (identity, xml, poster, fanart) = match kind {
      TmdbKind::Movie => {
        let details = self.client.movie(id).await?;
        let nfo = output::movie_nfo(&details);
        let identity = Identity {
          kind,
          id,
          title: details.title.clone(),
          year: nfo.year,
        };
        let xml = output::nfo_xml(NfoDoc::Movie(&nfo))?;
        (identity, xml, details.poster_path, details.backdrop_path)
      }
      TmdbKind::Tv => {
        let details = self.client.tv(id).await?;
        let nfo = output::tvshow_nfo(&details);
        let identity = Identity {
          kind,
          id,
          title: details.name.clone(),
          year: nfo.year,
        };
        let xml = output::nfo_xml(NfoDoc::TvShow(&nfo))?;
        (identity, xml, details.poster_path, details.backdrop_path)
      }
    };
    output::write_nfo(&ctx.dir, kind, &xml).await?;
    let artwork_warnings: Vec<String> = output::write_artwork(
      &ctx.dir,
      self.client.as_ref(),
      poster.as_deref(),
      fanart.as_deref(),
    )
    .await
    .into_iter()
    .map(|error| error.to_string())
    .collect();
    artwork_warnings.iter().for_each(|warning| {
      warn!(path = %ctx.dir.display(), "{warning}");
    });
    if ctx.fingerprint.is_cacheable() && source != IdentitySource::Cache {
      self
        .cache
        .record(&ctx.fingerprint.sha256, &identity, source)
        .await?;
    }
    let (rel_path, renamed) = rename_into_place(index, ctx, &identity).await?;
    index
      .set_disc_identity(
        &ctx.library.name,
        &rel_path,
        &IdentityOutcome::Resolved {
          identity: identity.clone(),
          source,
        },
      )
      .await?;
    let generation = index.current_generation().await?;
    scan::reconcile_directory(
      index,
      &ctx.library,
      &ctx.library.path.join(&rel_path),
      generation,
    )
    .await?;
    info!(
      library = %ctx.library.name,
      path = %rel_path,
      title = %identity.title,
      ?source,
      "Disc identity committed"
    );
    Ok(Committed {
      rel_path,
      identity,
      renamed,
      artwork_warnings,
    })
  }
}

/// The title hint a manifest offers, falling back to the indexed label.
pub fn hint_of(manifest: &DiscManifest, label: &str) -> Option<TitleHint> {
  naming::title_hint(
    manifest.disc_name.as_deref(),
    manifest.volume_name.as_deref(),
    manifest.udev.label.as_deref().or(Some(label)),
  )
}

/// Rename the directory to its resolved name, dodging an existing sibling
/// with a numeric suffix and giving up (name kept, identity still recorded)
/// when even those are taken.  Returns the directory's rel path afterwards
/// and whether it moved.
async fn rename_into_place(
  index: &Index,
  ctx: &DiscContext,
  identity: &Identity,
) -> Result<(String, bool), CommitError> {
  let parent_rel = ctx
    .rel_path
    .rsplit_once('/')
    .map_or("", |(parent, _)| parent);
  let current_name = ctx
    .rel_path
    .rsplit('/')
    .next()
    .unwrap_or(&ctx.rel_path)
    .to_string();
  let wanted = naming::resolved_dir_name(
    &identity.title,
    identity.year,
    ctx.hint.as_ref(),
    &ctx.disc_id,
  );
  if wanted == current_name {
    Ok((ctx.rel_path.clone(), false))
  } else {
    let parent_dir = ctx.dir.parent().map(PathBuf::from).unwrap_or_default();
    match free_name(&parent_dir, &wanted, &ctx.disc_id) {
      None => {
        warn!(
          path = %ctx.dir.display(),
          wanted = %wanted,
          "Every resolved directory name is taken; keeping the current name"
        );
        Ok((ctx.rel_path.clone(), false))
      }
      Some(new_name) => {
        let new_dir = parent_dir.join(&new_name);
        tokio::fs::rename(&ctx.dir, &new_dir)
          .await
          .map_err(|source| CommitError::DirectoryRename {
            from: ctx.dir.display().to_string(),
            to: new_dir.display().to_string(),
            source,
          })?;
        let new_rel = if parent_rel.is_empty() {
          new_name
        } else {
          format!("{parent_rel}/{new_name}")
        };
        index
          .rename_disc_dir(&ctx.library.name, &ctx.rel_path, &new_rel)
          .await?;
        info!(from = %ctx.rel_path, to = %new_rel, "Renamed disc directory to its resolved title");
        Ok((new_rel, true))
      }
    }
  }
}

/// The wanted name, or the first ` (n)` variant that is free.
fn free_name(
  parent: &std::path::Path,
  wanted: &str,
  disc_id: &str,
) -> Option<String> {
  let base = wanted
    .strip_suffix(&format!("__{disc_id}"))
    .unwrap_or(wanted);
  std::iter::once(wanted.to_string())
    .chain(
      (2..=RENAME_ATTEMPTS + 1).map(|n| format!("{base} ({n})__{disc_id}")),
    )
    .find(|name| !parent.join(name).exists())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn candidate(
    kind: TmdbKind,
    id: u64,
    title: &str,
    year: Option<i32>,
    popularity: f64,
  ) -> Candidate {
    Candidate {
      kind,
      id,
      title: title.to_string(),
      original_title: None,
      year,
      overview: None,
      poster_path: None,
      popularity,
      vote_count: 0,
    }
  }

  fn hint(query: &str, year: Option<i32>, kind: Option<TmdbKind>) -> TitleHint {
    TitleHint {
      query: query.to_string(),
      raw: query.to_string(),
      year,
      kind_hint: kind,
      season: None,
      disc_number: None,
    }
  }

  #[test]
  fn a_lone_related_hit_is_accepted() {
    let only = candidate(
      TmdbKind::Tv,
      1,
      "Voltron: Legendary Defender",
      Some(2016),
      5.0,
    );
    assert_eq!(
      decide(&hint("VOLTRON", None, None), vec![only.clone()]),
      Decision::Accept(only)
    );
    let unrelated = candidate(TmdbKind::Movie, 2, "Something Else", None, 5.0);
    assert!(matches!(
      decide(&hint("DISC", None, None), vec![unrelated]),
      Decision::Confirm(_)
    ));
  }

  #[test]
  fn an_exact_dominant_top_hit_is_accepted() {
    let top = candidate(TmdbKind::Movie, 603, "The Matrix", Some(1999), 80.0);
    let runner =
      candidate(TmdbKind::Movie, 604, "The Matrix Reloaded", Some(2003), 30.0);
    assert_eq!(
      decide(&hint("THE MATRIX", None, None), vec![top.clone(), runner]),
      Decision::Accept(top)
    );
  }

  #[test]
  fn twins_and_close_runners_go_to_the_operator() {
    let remake =
      candidate(TmdbKind::Movie, 1, "Ghostbusters", Some(2016), 50.0);
    let original =
      candidate(TmdbKind::Movie, 2, "Ghostbusters", Some(1984), 45.0);
    assert!(matches!(
      decide(&hint("GHOSTBUSTERS", None, None), vec![remake.clone(), original.clone()]),
      Decision::Confirm(list) if list.len() == 2
    ));
    assert_eq!(
      decide(
        &hint("GHOSTBUSTERS", Some(1984), None),
        vec![original.clone(), remake.clone()]
      ),
      Decision::Accept(original.clone()),
      "a year hint settles the twin"
    );
    let close =
      candidate(TmdbKind::Movie, 3, "Ghostbusters II", Some(2016), 40.0);
    assert!(
      matches!(
        decide(&hint("GHOSTBUSTERS", Some(2016), None), vec![remake, close]),
        Decision::Confirm(_)
      ),
      "a runner-up the year does not rule out keeps the doubt alive"
    );
  }

  #[test]
  fn a_kind_hint_narrows_the_pool() {
    let movie = candidate(TmdbKind::Movie, 1, "Voltron", Some(1984), 90.0);
    let show = candidate(TmdbKind::Tv, 2, "Voltron", Some(2016), 20.0);
    assert_eq!(
      decide(
        &hint("VOLTRON", None, Some(TmdbKind::Tv)),
        vec![movie, show.clone()]
      ),
      Decision::Accept(show)
    );
  }

  #[test]
  fn merge_keeps_prior_first_and_dedups() {
    let a = candidate(TmdbKind::Movie, 1, "A", None, 1.0);
    let b = candidate(TmdbKind::Movie, 2, "B", None, 1.0);
    let merged = merge(std::slice::from_ref(&a), vec![a.clone(), b.clone()]);
    assert_eq!(merged, vec![a, b]);
  }

  #[test]
  fn free_name_dodges_taken_siblings() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("Title (1999)__id")).unwrap();
    std::fs::create_dir(dir.path().join("Title (1999) (2)__id")).unwrap();
    assert_eq!(
      free_name(dir.path(), "Title (1999)__id", "id").as_deref(),
      Some("Title (1999) (3)__id")
    );
    assert_eq!(
      free_name(dir.path(), "Fresh__id", "id").as_deref(),
      Some("Fresh__id")
    );
  }
}
