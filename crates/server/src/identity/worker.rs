//! The serial identity worker: resolves unresolved discs one at a time,
//! discovered by query and woken by the scanner (which is what creates the
//! rows).  Transient TMDB trouble defers a disc with a backoff; because a
//! deferred disc has nobody to wake the worker when its backoff ends, the
//! wait is also bounded by a poll interval.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Notify};
use tracing::{info, warn};

use crate::identity::resolver::{
  CommitError, Resolution, ResolveError, Resolver,
};
use crate::index::store::{now_secs, DiscRecord, IdentityOutcome, Index};
use crate::library::Library;

/// How long a disc waits after a transient failure before another try.
pub const RETRY_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// The longest the worker sleeps without a wake, so backoffs expire.
pub const RETRY_POLL: Duration = Duration::from_secs(5 * 60);

/// The disc being resolved right now, published for status display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveIdentification {
  pub library: String,
  pub rel_path: String,
}

/// Spawn the worker task.
pub fn spawn(
  index: Index,
  libraries: Vec<Library>,
  resolver: Arc<Resolver>,
  wake: Arc<Notify>,
) -> watch::Receiver<Option<ActiveIdentification>> {
  let (sender, receiver) = watch::channel(None);
  tokio::spawn(async move {
    loop {
      let worked = process_next(
        &index,
        &libraries,
        resolver.as_ref(),
        &sender,
        RETRY_BACKOFF,
      )
      .await;
      if !worked {
        // The timeout result is discarded deliberately: expiring is the
        // poll that lets deferred discs come back around, not an error.
        drop(tokio::time::timeout(RETRY_POLL, wake.notified()).await);
      }
    }
  });
  receiver
}

/// Resolve the next eligible disc.  Returns whether an attempt was made — a
/// recorded outcome of any kind counts, since it takes the disc out of the
/// next candidate pass (or defers it).  Public so tests can step the worker
/// without spawning it.
pub async fn process_next(
  index: &Index,
  libraries: &[Library],
  resolver: &Resolver,
  status: &watch::Sender<Option<ActiveIdentification>>,
  retry_backoff: Duration,
) -> bool {
  let retry_before =
    now_secs() - i64::try_from(retry_backoff.as_secs()).unwrap_or(i64::MAX);
  let Some(disc) = index
    .next_identity_candidate(retry_before)
    .await
    .unwrap_or_else(|error| {
      warn!(%error, "Skipping identity pass; could not list unresolved discs");
      None
    })
  else {
    return false;
  };
  status.send_replace(Some(ActiveIdentification {
    library: disc.library.clone(),
    rel_path: disc.rel_path.clone(),
  }));
  let outcome = match libraries.iter().find(|l| l.name == disc.library) {
    Some(library) => attempt(index, library, resolver, &disc).await,
    None => IdentityOutcome::Failed {
      error: format!("library '{}' is not configured", disc.library),
    },
  };
  record(index, &disc, &outcome).await;
  status.send_replace(None);
  true
}

async fn attempt(
  index: &Index,
  library: &Library,
  resolver: &Resolver,
  disc: &DiscRecord,
) -> IdentityOutcome {
  match resolver.context(library, disc).await {
    Ok(ctx) => resolve_and_commit(index, resolver, &ctx).await,
    Err(error) => resolve_failure(error),
  }
}

async fn resolve_and_commit(
  index: &Index,
  resolver: &Resolver,
  ctx: &crate::identity::resolver::DiscContext,
) -> IdentityOutcome {
  match resolver.resolve(ctx).await {
    Ok(Resolution::Resolved { identity, source }) => {
      match resolver
        .commit(index, ctx, identity.kind, identity.id, source)
        .await
      {
        Ok(committed) => IdentityOutcome::Resolved {
          identity: committed.identity,
          source,
        },
        Err(CommitError::Tmdb(error)) if error.is_transient() => {
          IdentityOutcome::Deferred {
            error: error.to_string(),
          }
        }
        Err(error) => IdentityOutcome::Failed {
          error: error.to_string(),
        },
      }
    }
    Ok(Resolution::NeedsConfirmation { candidates }) => {
      IdentityOutcome::NeedsConfirmation { candidates }
    }
    Err(error) => resolve_failure(error),
  }
}

fn resolve_failure(error: ResolveError) -> IdentityOutcome {
  match error {
    ResolveError::Tmdb(tmdb) if tmdb.is_transient() => {
      IdentityOutcome::Deferred {
        error: tmdb.to_string(),
      }
    }
    other => IdentityOutcome::Failed {
      error: other.to_string(),
    },
  }
}

/// Log the outcome and, unless a commit already wrote the resolved row
/// under its new path, record it on the disc.
async fn record(index: &Index, disc: &DiscRecord, outcome: &IdentityOutcome) {
  let already_recorded = match outcome {
    IdentityOutcome::Resolved { identity, source } => {
      info!(
        library = %disc.library,
        path = %disc.rel_path,
        title = %identity.title,
        source = source.as_str(),
        "Disc identified"
      );
      true
    }
    IdentityOutcome::NeedsConfirmation { candidates } => {
      info!(
        library = %disc.library,
        path = %disc.rel_path,
        candidates = candidates.len(),
        "Disc needs an operator to confirm its identity"
      );
      false
    }
    IdentityOutcome::Deferred { error } => {
      warn!(
        library = %disc.library,
        path = %disc.rel_path,
        %error,
        "Disc identification deferred; will retry"
      );
      false
    }
    IdentityOutcome::Failed { error } => {
      warn!(
        library = %disc.library,
        path = %disc.rel_path,
        %error,
        "Disc identification failed"
      );
      false
    }
  };
  if !already_recorded {
    persist(index, disc, outcome).await;
  }
}

async fn persist(index: &Index, disc: &DiscRecord, outcome: &IdentityOutcome) {
  if let Err(error) = index
    .set_disc_identity(&disc.library, &disc.rel_path, outcome)
    .await
  {
    warn!(path = %disc.rel_path, %error, "Failed to record the identity outcome");
  }
}
