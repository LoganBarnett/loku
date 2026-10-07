//! Wake signals from index maintenance to the background workers that act
//! on what a scan discovered.  The scanner is the only thing that creates
//! the rows the workers query for, so every scan and flush notifies all of
//! them; a worker with nothing to do simply goes back to sleep.

use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct Wakes {
  /// The compat-derivation worker.
  pub derivation: Arc<Notify>,
  /// The identity-resolution worker.
  pub identity: Arc<Notify>,
}

impl Wakes {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn notify_all(&self) {
    self.derivation.notify_one();
    self.identity.notify_one();
  }
}
