//! Disc ripping: Loku owns the rip so the per-disc directory, its manifest,
//! and the structural facts identity resolution needs are produced once and
//! kept.  See `tasks.org` ("Produce NFO sidecars") for the design rationale.

pub mod config;
pub mod drive;
pub mod makemkv;
pub mod manifest;
pub mod naming;
pub mod policy;
pub mod retention;
pub mod worker;

use rust_template_foundation::Server;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::warn;

use crate::config::Config;
use crate::index::store::Index;
use crate::index::wakes::Wakes;
use crate::web_base::RipToolsCheck;
use config::RipConfig;
use makemkv::{DiscTools, RealDiscTools};

/// Everything the API needs from a running rip subsystem.
#[derive(Clone)]
pub struct RipHandles {
  pub queue: worker::RipQueue,
  pub drives: Vec<watch::Receiver<drive::DriveStatus>>,
  pub config: Arc<RipConfig>,
}

pub struct RipDeps {
  pub index: Index,
  pub config: RipConfig,
  pub tools: Arc<dyn DiscTools>,
  pub wakes: Wakes,
}

/// Bring up the rip subsystem.
pub fn spawn(deps: RipDeps) -> RipHandles {
  let config = Arc::new(deps.config);
  let queue = worker::spawn(worker::WorkerDeps {
    index: deps.index,
    config: config.clone(),
    tools: deps.tools.clone(),
    wakes: deps.wakes,
  });
  let drive = drive::spawn(
    config.device.clone(),
    config.poll_interval,
    config.policy,
    deps.tools,
    queue.clone(),
  );
  retention::spawn(config.library.path.clone(), config.replaced_retention);
  RipHandles {
    queue,
    drives: vec![drive],
    config,
  }
}

/// Bring up ripping when it is configured and its tools are present.
pub async fn subsystem(
  config: &Config,
  server: &Server,
  index: &Index,
  wakes: &Wakes,
) -> Option<RipHandles> {
  let rip_config = config.rip.as_ref()?;
  match RealDiscTools::detect().await {
    Err(error) => {
      warn!(%error, "Rip tools unavailable; disc ripping is disabled");
      server
        .base_state()
        .health_registry
        .register(
          "rip",
          RipToolsCheck {
            unavailable_reason: Some(error.to_string()),
          },
        )
        .await;
      None
    }
    Ok(tools) => {
      server
        .base_state()
        .health_registry
        .register(
          "rip",
          RipToolsCheck {
            unavailable_reason: None,
          },
        )
        .await;
      match index.fail_interrupted_rips().await {
        Ok(0) => {}
        Ok(interrupted) => {
          warn!(
            interrupted,
            "Closed rip jobs the previous process left running"
          )
        }
        Err(error) => warn!(%error, "Failed to close interrupted rip jobs"),
      }
      Some(spawn(RipDeps {
        index: index.clone(),
        config: rip_config.clone(),
        tools: Arc::new(tools),
        wakes: wakes.clone(),
      }))
    }
  }
}
