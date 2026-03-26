use axum::{
  http::StatusCode,
  response::{IntoResponse, Response},
  routing::get,
  Json, Router,
};
use prometheus::{Encoder, IntCounter, Registry, TextEncoder};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
use tower_http::services::{ServeDir, ServeFile};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::routes;

#[derive(Clone)]
pub struct AppState {
  pub registry: Arc<Registry>,
  pub request_counter: IntCounter,
  pub library_path: PathBuf,
}

impl AppState {
  pub fn new(library_path: PathBuf) -> Self {
    let registry = Registry::new();
    let request_counter =
      IntCounter::new("http_requests_total", "Total HTTP requests")
        .expect("Failed to create counter");

    registry
      .register(Box::new(request_counter.clone()))
      .expect("Failed to register counter");

    Self {
      registry: Arc::new(registry),
      request_counter,
      library_path,
    }
  }
}

#[derive(OpenApi)]
#[openapi(
    paths(healthz, metrics_endpoint),
    tags(
        (name = "health", description = "Health check endpoints"),
        (name = "metrics", description = "Metrics endpoints")
    )
)]
pub struct ApiDoc;

pub fn base_router(state: AppState) -> Router {
  let openapi = ApiDoc::openapi();
  let library_path = state.library_path.clone();

  Router::new()
    .route("/healthz", get(healthz))
    .route("/metrics", get(metrics_endpoint))
    .route("/api/browse", get(routes::browse::handler))
    .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", openapi))
    .nest_service("/files", ServeDir::new(&library_path))
    .with_state(state)
    .fallback_service(
      ServeDir::new("frontend/dist")
        .not_found_service(ServeFile::new("frontend/dist/index.html")),
    )
}

#[utoipa::path(
    get,
    path = "/healthz",
    tag = "health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse)
    )
)]
async fn healthz() -> Json<HealthResponse> {
  Json(HealthResponse {
    status: "healthy".to_string(),
  })
}

#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct HealthResponse {
  status: String,
}

#[utoipa::path(
    get,
    path = "/metrics",
    tag = "metrics",
    responses(
        (status = 200, description = "Prometheus metrics", content_type = "text/plain")
    )
)]
async fn metrics_endpoint(
  axum::extract::State(state): axum::extract::State<AppState>,
) -> Response {
  let encoder = TextEncoder::new();
  let metric_families = state.registry.gather();
  let mut buffer = Vec::new();

  match encoder.encode(&metric_families, &mut buffer) {
    Ok(_) => {
      (StatusCode::OK, [("content-type", encoder.format_type())], buffer)
        .into_response()
    }
    Err(e) => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(json!({
          "error": format!("Failed to encode metrics: {}", e)
      })),
    )
      .into_response(),
  }
}
