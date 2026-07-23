use aide::axum::{routing::get_with, ApiRouter};
use rust_template_foundation::server::health::{ComponentHealth, HealthCheck};
use rust_template_foundation::{
  impl_server_state, server::runner::BaseServerState,
};
use std::path::{Path, PathBuf};
use tower_http::services::ServeDir;

use crate::routes;

/// Application state: the foundation's shared base state plus the video
/// library root the browse and file-serving routes read from.
#[derive(Clone)]
pub struct AppState {
  pub base: BaseServerState,
  pub library_path: PathBuf,
}

impl_server_state!(AppState, base);

/// The loku-specific routes merged onto the foundation server: the browse API
/// and static media serving from the library root.  Foundation owns the
/// infrastructure routes (`/healthz`, `/metrics`, `/scalar`,
/// `/api-docs/openapi.json`, `/auth/*`, `/me`) and the SPA fallback.
pub fn app_routes(library_path: &Path) -> ApiRouter<AppState> {
  ApiRouter::new()
    .api_route(
      "/api/browse",
      get_with(routes::browse::handler, routes::browse::browse_docs),
    )
    .nest_service("/files", ServeDir::new(library_path))
}

/// Reports whether the media library directory is accessible.  Registered into
/// the foundation health registry so `/healthz` reflects library availability.
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
