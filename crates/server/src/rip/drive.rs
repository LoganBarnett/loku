//! The drive poller: notices a disc appearing in the configured drive and
//! queues a rip for it.
//!
//! Reacting to udev events would need udev rules, an unauthenticated trigger
//! route, or direct device access; polling the udev property database
//! (`udevadm info`) every few seconds needs none of those and is trivially
//! faked in tests.  The cost is negligible next to a rip that takes half an
//! hour.  Only the absent-to-present edge queues a job, so a disc left in the
//! drive after a rip never re-triggers until it is removed and reinserted.

use schemars::JsonSchema;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use crate::index::store::now_secs;
use crate::rip::config::RipPolicy;
use crate::rip::makemkv::{DiscFacts, DiscTools};
use crate::rip::worker::{RipQueue, RipTrigger};

/// What the poller last saw in the drive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DriveStatus {
  pub device: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub media: Option<DiscFacts>,
  pub polled_at: i64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub last_error: Option<String>,
}

/// Pure edge detector.  Starts "absent", so a disc already in the drive at
/// startup counts as an insertion (which is also how a rip a restart killed
/// gets retried).
#[derive(Debug, Default)]
pub struct MediaPresence {
  present: bool,
}

impl MediaPresence {
  pub fn new() -> Self {
    Self::default()
  }

  /// Record the latest observation; true exactly on absent-to-present.
  pub fn observe(&mut self, now_present: bool) -> bool {
    let inserted = now_present && !self.present;
    self.present = now_present;
    inserted
  }
}

/// Spawn the poller for one drive.
pub fn spawn(
  device: PathBuf,
  interval: Duration,
  default_policy: RipPolicy,
  tools: Arc<dyn DiscTools>,
  queue: RipQueue,
) -> watch::Receiver<DriveStatus> {
  let device_name = device.display().to_string();
  let (sender, receiver) = watch::channel(DriveStatus {
    device: device_name.clone(),
    media: None,
    polled_at: 0,
    last_error: None,
  });
  tokio::spawn(async move {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut presence = MediaPresence::new();
    let mut last_error: Option<String> = None;
    loop {
      ticker.tick().await;
      match tools.media(&device).await {
        Ok(media) => {
          last_error = None;
          let inserted = presence.observe(media.is_some());
          sender.send_replace(DriveStatus {
            device: device_name.clone(),
            media: media.clone(),
            polled_at: now_secs(),
            last_error: None,
          });
          if let Some(facts) = media.filter(|_| inserted) {
            info!(
              device = %device_name,
              label = facts.label.as_deref(),
              uuid = facts.fs_uuid.as_deref(),
              "Disc inserted"
            );
            match queue
              .submit(&device, facts, default_policy, RipTrigger::Auto)
              .await
            {
              Ok(job) => {
                info!(job = job.id, "Queued a rip for the inserted disc")
              }
              Err(error) => {
                warn!(%error, "Could not queue a rip for the inserted disc");
              }
            }
          }
        }
        Err(error) => {
          let message = error.to_string();
          // A missing drive would otherwise log every poll; once per
          // distinct failure is enough.
          if last_error.as_deref() != Some(message.as_str()) {
            warn!(device = %device_name, %error, "Drive poll failed");
          }
          last_error = Some(message.clone());
          sender.send_replace(DriveStatus {
            device: device_name.clone(),
            media: None,
            polled_at: now_secs(),
            last_error: Some(message),
          });
        }
      }
    }
  });
  receiver
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn presence_triggers_only_on_the_rising_edge() {
    let mut presence = MediaPresence::new();
    assert!(presence.observe(true), "a disc present at startup counts");
    assert!(!presence.observe(true), "the same disc left in place does not");
    assert!(!presence.observe(false));
    assert!(presence.observe(true), "reinsertion counts again");
  }
}
