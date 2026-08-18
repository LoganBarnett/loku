//! loku-server — entry point.
//!
//! The `#[foundation_main]` macro handles CLI parsing, config resolution,
//! logging init, OIDC discovery, listener binding, systemd integration, and
//! graceful shutdown.  This file only wires the loku-specific state, routes,
//! background scan, and health checks onto the foundation-provided `Server`.

use futures_util::stream::{self, StreamExt};
use loku_server::config::Config;
use loku_server::frontend::Frontend;
use loku_server::index::store::Index;
use loku_server::index::watch;
use loku_server::media::ffmpeg::{FfmpegRunner, RealFfmpeg};
use loku_server::media::worker;
use loku_server::web_base::{
  app_routes, AppState, IndexCheck, LibraryPathCheck, MediaToolsCheck,
};
use rust_template_foundation::main as foundation_main;
use rust_template_foundation::{Server, ServerError};
use std::process::ExitCode;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use tracing::{error, warn};

#[foundation_main]
pub async fn main(
  config: Config,
  server: Server,
) -> Result<ExitCode, ServerError> {
  // Opening the index is a startup prerequisite: without it the search API
  // cannot function, so a failure is reported and the process exits rather
  // than limping along half-alive.
  let index = match Index::open(&config.state_dir).await {
    Ok(index) => index,
    Err(error) => {
      error!(%error, "Failed to open the media index");
      return Ok(ExitCode::FAILURE);
    }
  };

  // Media tools are optional: without ffmpeg/ffprobe the server still
  // browses and serves, with codec probing disabled and health degraded.
  let (ffmpeg, tools_unavailable): (Option<Arc<dyn FfmpegRunner>>, _) =
    match RealFfmpeg::detect().await {
      Ok(runner) => (Some(Arc::new(runner) as Arc<dyn FfmpegRunner>), None),
      Err(detect_error) => {
        warn!(
          error = %detect_error,
          "ffmpeg/ffprobe unavailable; codec probing is disabled"
        );
        (None, Some(detect_error.to_string()))
      }
    };

  // Register health checks on the base state before swapping in the app
  // state — `base_state()` is only available on the un-parameterised
  // `Server<BaseServerState>`.
  let last_scan_completed = Arc::new(AtomicI64::new(0));
  server
    .base_state()
    .health_registry
    .register(
      "media-tools",
      MediaToolsCheck {
        unavailable_reason: tools_unavailable,
      },
    )
    .await;
  server
    .base_state()
    .health_registry
    .register(
      "index",
      IndexCheck {
        last_scan_completed: last_scan_completed.clone(),
      },
    )
    .await;
  stream::iter(&config.libraries)
    .for_each(|library| {
      server.base_state().health_registry.register(
        format!("library-{}", library.name),
        LibraryPathCheck {
          library_path: library.path.clone(),
        },
      )
    })
    .await;

  // Index maintenance runs in the background so the listener binds (and
  // systemd readiness fires) immediately: watchers register first, then the
  // initial scan runs, then filesystem events keep the index current.  The
  // index health component reports degraded until the first scan lands.  A
  // watcher failure (e.g. inotify limits) degrades to a one-shot scan with
  // no live updates rather than killing the server.
  // The derivation worker produces compat copies and thumbnails, woken by
  // the maintenance task whenever the index gains new facts.  Its status
  // receiver feeds the API's per-item derivation reporting; without media
  // tools a never-updated channel stands in.
  let worker_wake = Arc::new(tokio::sync::Notify::new());
  let derivation_active = ffmpeg.as_ref().map_or_else(
    || tokio::sync::watch::channel(None).1,
    |runner| {
      worker::spawn(
        index.clone(),
        config.libraries.clone(),
        runner.clone(),
        worker_wake.clone(),
      )
    },
  );

  if let Err(watch_error) = watch::spawn_maintenance(
    index.clone(),
    config.libraries.clone(),
    last_scan_completed.clone(),
    ffmpeg.clone(),
    worker_wake.clone(),
  ) {
    warn!(
      error = %watch_error,
      "Filesystem watching unavailable; the index will only update on restart"
    );
    watch::spawn_scan_only(
      index.clone(),
      config.libraries.clone(),
      last_scan_completed.clone(),
      ffmpeg,
      worker_wake,
    );
  }

  let libraries = Arc::new(config.libraries.clone());
  let server = server
    .with_state(|base| AppState {
      base,
      libraries,
      index,
      derivation_active,
    })
    .merge(app_routes(&config.libraries))
    // Serve the embedded Elm frontend as an SPA, falling back to index.html
    // for client-side routes.
    .spa::<Frontend>();

  server.listen().await?;
  Ok(ExitCode::SUCCESS)
}
