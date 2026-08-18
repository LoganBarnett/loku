use aide::axum::{
  routing::{get_with, post_with},
  ApiRouter,
};
use rust_template_foundation::server::health::{ComponentHealth, HealthCheck};
use rust_template_foundation::{
  impl_server_state, server::runner::BaseServerState,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tower_http::services::ServeDir;

use crate::index::store::Index;
use crate::library::Library;
use crate::media::worker::ActiveDerivation;
use crate::routes;

/// Application state: the foundation's shared base state plus the configured
/// library roots, the media index the API routes read from, and the
/// derivation worker's live status.
#[derive(Clone)]
pub struct AppState {
  pub base: BaseServerState,
  pub libraries: Arc<Vec<Library>>,
  pub index: Index,
  /// What the worker is deriving right now; a never-updated `None` channel
  /// when the worker is not running (no media tools).
  pub derivation_active: watch::Receiver<Option<ActiveDerivation>>,
}

impl AppState {
  /// Look up a configured library by name.
  pub fn library(&self, name: &str) -> Option<&Library> {
    self.libraries.iter().find(|l| l.name == name)
  }
}

impl_server_state!(AppState, base);

/// The loku-specific routes merged onto the foundation server: the browse and
/// library APIs plus static media serving, one mount per library under
/// `/files/{name}`.  Foundation owns the infrastructure routes (`/healthz`,
/// `/metrics`, `/scalar`, `/api-docs/openapi.json`, `/auth/*`, `/me`) and the
/// SPA fallback.
pub fn app_routes(libraries: &[Library]) -> ApiRouter<AppState> {
  libraries.iter().fold(
    ApiRouter::new()
      .api_route(
        "/api/browse",
        get_with(routes::browse::handler, routes::browse::browse_docs),
      )
      .api_route(
        "/api/libraries",
        get_with(routes::libraries::handler, routes::libraries::libraries_docs),
      )
      .api_route(
        "/api/search",
        get_with(routes::search::handler, routes::search::search_docs),
      )
      .api_route(
        "/api/item",
        get_with(routes::item::handler, routes::item::item_docs),
      )
      .api_route(
        "/api/disc-sets/main",
        post_with(
          routes::disc_sets::set_main_handler,
          routes::disc_sets::set_main_docs,
        ),
      ),
    |router, library| {
      router.nest_service(
        &format!("/files/{}", library.name),
        ServeDir::new(&library.path),
      )
    },
  )
}

/// Reports whether ffmpeg/ffprobe were found at startup.  Without them the
/// server still browses and serves, but codec probing (and, later, compat
/// derivation) is disabled — degraded, not broken.
pub struct MediaToolsCheck {
  /// The startup detection failure, when there was one.
  pub unavailable_reason: Option<String>,
}

impl HealthCheck for MediaToolsCheck {
  fn check(&self) -> ComponentHealth {
    self.unavailable_reason.as_ref().map_or(
      ComponentHealth::Healthy,
      |reason| {
        ComponentHealth::Degraded(format!(
          "media tools unavailable, codec probing disabled: {reason}"
        ))
      },
    )
  }
}

/// Reports whether the media index has completed its initial library scan.
/// Reads a cached timestamp (set by the scan task) rather than querying the
/// database, per the health-check guidance to avoid I/O.
pub struct IndexCheck {
  /// Unix seconds of the last completed scan; zero until the first finishes.
  pub last_scan_completed: Arc<AtomicI64>,
}

impl HealthCheck for IndexCheck {
  fn check(&self) -> ComponentHealth {
    if self.last_scan_completed.load(Ordering::Relaxed) == 0 {
      ComponentHealth::Degraded(
        "initial library scan has not completed yet".to_string(),
      )
    } else {
      ComponentHealth::Healthy
    }
  }
}

/// Reports whether a media library directory is accessible.  Registered into
/// the foundation health registry once per library, so `/healthz` reflects
/// each root's availability.
pub struct LibraryPathCheck {
  pub library_path: PathBuf,
}

impl HealthCheck for LibraryPathCheck {
  fn check(&self) -> ComponentHealth {
    // A single stat is cheap and matches loku's prior healthz behaviour;
    // the library path is local, so this does not perform remote I/O.
    if self.library_path.exists() {
      ComponentHealth::Healthy
    } else {
      ComponentHealth::Unhealthy(format!(
        "library path is not accessible: {}",
        self.library_path.display()
      ))
    }
  }
}
