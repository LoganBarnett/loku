//! loku-server — entry point.
//!
//! The `#[foundation_main]` macro handles CLI parsing, config resolution,
//! logging init, OIDC discovery, listener binding, systemd integration, and
//! graceful shutdown.  This file only wires the loku-specific state, routes,
//! and health check onto the foundation-provided `Server`.

use loku_server::config::Config;
use loku_server::frontend::Frontend;
use loku_server::web_base::{app_routes, AppState, LibraryPathCheck};
use rust_template_foundation::main as foundation_main;
use rust_template_foundation::{Server, ServerError};
use std::process::ExitCode;

#[foundation_main]
pub async fn main(
  config: Config,
  server: Server,
) -> Result<ExitCode, ServerError> {
  // Register the library-directory health check on the base state before
  // swapping in the app state — `base_state()` is only available on the
  // un-parameterised `Server<BaseServerState>`.
  server
    .base_state()
    .health_registry
    .register(
      "library",
      LibraryPathCheck {
        library_path: config.library_path.clone(),
      },
    )
    .await;

  let library_path = config.library_path.clone();
  let server = server
    .with_state(|base| AppState { base, library_path })
    .merge(app_routes(&config.library_path))
    // Serve the embedded Elm frontend as an SPA, falling back to index.html
    // for client-side routes.
    .spa::<Frontend>();

  server.listen().await?;
  Ok(ExitCode::SUCCESS)
}
