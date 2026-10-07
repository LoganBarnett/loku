//! The re-insert decision: pure, given the configured policy and what
//! already exists on disk.  Keeping it I/O-free makes every row of the
//! policy table a one-line test.

use thiserror::Error;

use crate::rip::config::RipPolicy;
use crate::rip::naming::IdSource;

/// What the worker should do with the inserted disc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RipAction {
  /// Leave the existing directory alone.
  Skip,
  /// Refresh only the sidecars in the existing directory.
  Rescan,
  /// Rip into staging; `replace` moves an existing tree aside on success.
  Rip { replace: bool },
}

#[derive(Debug, Error)]
pub enum PolicyError {
  #[error(
    "Rip directory '{dir_name}' already exists, but its id is a timestamp \
     that carries no disc identity; refusing to treat it as the same disc"
  )]
  TimestampCollision { dir_name: String },
}

/// Decide the action.  A missing directory always rips.  An existing one
/// follows the policy — unless the id is a timestamp, in which case nothing
/// links the directory to this disc and reusing it would be a guess.
pub fn decide(
  policy: RipPolicy,
  id_source: IdSource,
  existing: bool,
  dir_name: &str,
) -> Result<RipAction, PolicyError> {
  match (existing, id_source, policy) {
    (false, _, _) => Ok(RipAction::Rip { replace: false }),
    (true, IdSource::Timestamp, _) => Err(PolicyError::TimestampCollision {
      dir_name: dir_name.to_string(),
    }),
    (true, _, RipPolicy::Skip) => Ok(RipAction::Skip),
    (true, _, RipPolicy::Rescan) => Ok(RipAction::Rescan),
    (true, _, RipPolicy::Rerip) => Ok(RipAction::Rip { replace: true }),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_new_disc_always_rips() {
    for policy in [RipPolicy::Skip, RipPolicy::Rescan, RipPolicy::Rerip] {
      assert_eq!(
        decide(policy, IdSource::Uuid, false, "d").unwrap(),
        RipAction::Rip { replace: false }
      );
    }
    assert_eq!(
      decide(RipPolicy::Skip, IdSource::Timestamp, false, "d").unwrap(),
      RipAction::Rip { replace: false }
    );
  }

  #[test]
  fn an_existing_disc_follows_the_policy() {
    assert_eq!(
      decide(RipPolicy::Skip, IdSource::Uuid, true, "d").unwrap(),
      RipAction::Skip
    );
    assert_eq!(
      decide(RipPolicy::Rescan, IdSource::InfoHash, true, "d").unwrap(),
      RipAction::Rescan
    );
    assert_eq!(
      decide(RipPolicy::Rerip, IdSource::Uuid, true, "d").unwrap(),
      RipAction::Rip { replace: true }
    );
  }

  #[test]
  fn a_timestamp_collision_is_refused() {
    assert!(matches!(
      decide(RipPolicy::Rerip, IdSource::Timestamp, true, "LBL__ts1"),
      Err(PolicyError::TimestampCollision { dir_name }) if dir_name == "LBL__ts1"
    ));
  }
}
