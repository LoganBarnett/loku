use axum::{
  body::Body,
  http::{Request, StatusCode},
};
use loku_web::web_base::{base_router, AppState};
use std::fs;
use tower::ServiceExt;

#[tokio::test]
async fn test_openapi_json_endpoint() {
  let app = base_router(AppState::new(
    std::path::PathBuf::from("."),
    std::path::PathBuf::from("."),
  ));

  let response = app
    .oneshot(
      Request::builder()
        .uri("/api-docs/openapi.json")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let body_str = String::from_utf8(body.to_vec()).unwrap();

  assert!(body_str.contains("openapi"), "Response should be an OpenAPI spec");
  assert!(body_str.contains("/healthz"), "Spec should document /healthz");
  assert!(body_str.contains("/api/browse"), "Spec should document /api/browse");
}

#[tokio::test]
async fn test_scalar_ui_endpoint() {
  let app = base_router(AppState::new(
    std::path::PathBuf::from("."),
    std::path::PathBuf::from("."),
  ));

  let response = app
    .oneshot(
      Request::builder()
        .uri("/scalar")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();

  assert!(
    body.starts_with(b"<!doctype html>")
      || body.starts_with(b"<!DOCTYPE html>"),
    "Scalar endpoint should return HTML"
  );
}

#[tokio::test]
async fn test_healthz_endpoint() {
  let state =
    AppState::new(std::path::PathBuf::from("."), std::path::PathBuf::from("."));
  let app = base_router(state);

  let response = app
    .oneshot(
      Request::builder()
        .uri("/healthz")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let body_str = String::from_utf8(body.to_vec()).unwrap();

  assert!(body_str.contains("healthy"));
}

#[tokio::test]
async fn test_metrics_endpoint() {
  let state =
    AppState::new(std::path::PathBuf::from("."), std::path::PathBuf::from("."));
  let app = base_router(state);

  let response = app
    .oneshot(
      Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);

  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let body_str = String::from_utf8(body.to_vec()).unwrap();

  assert!(
    body_str.contains("http_requests_total"),
    "Metrics should contain http_requests_total counter"
  );
}

#[tokio::test]
async fn test_browse_empty_root() {
  let dir = tempfile::tempdir().unwrap();
  let app = base_router(AppState::new(
    dir.path().to_path_buf(),
    std::path::PathBuf::from("."),
  ));

  let response = app
    .oneshot(
      Request::builder()
        .uri("/api/browse")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
  assert_eq!(json["entries"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_browse_lists_directory_entry() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("My Channel")).unwrap();
  let app = base_router(AppState::new(
    dir.path().to_path_buf(),
    std::path::PathBuf::from("."),
  ));

  let response = app
    .oneshot(
      Request::builder()
        .uri("/api/browse?path=")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1);
  assert_eq!(entries[0]["type"], "directory");
  assert_eq!(entries[0]["name"], "My Channel");
}

#[tokio::test]
async fn test_browse_video_with_metadata() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"").unwrap();
  fs::write(dir.path().join("clip.jpg"), b"").unwrap();
  fs::write(
    dir.path().join("clip.info.json"),
    r#"{"title":"My Clip","duration":90.0,"upload_date":"20240301"}"#,
  )
  .unwrap();
  let app = base_router(AppState::new(
    dir.path().to_path_buf(),
    std::path::PathBuf::from("."),
  ));

  let response = app
    .oneshot(
      Request::builder()
        .uri("/api/browse?path=")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::OK);
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1);
  let v = &entries[0];
  assert_eq!(v["type"], "video");
  assert_eq!(v["name"], "clip.mp4");
  assert_eq!(v["title"], "My Clip");
  assert_eq!(v["duration_secs"], 90.0);
  assert_eq!(v["upload_date"], "20240301");
  assert!(
    v["thumb_path"].as_str().unwrap().ends_with("clip.jpg"),
    "thumb_path should point to clip.jpg"
  );
}

#[tokio::test]
async fn test_browse_path_traversal_rejected() {
  let dir = tempfile::tempdir().unwrap();
  let app = base_router(AppState::new(
    dir.path().to_path_buf(),
    std::path::PathBuf::from("."),
  ));

  // ".." resolves to the parent of the library root, which is outside it.
  let response = app
    .oneshot(
      Request::builder()
        .uri("/api/browse?path=..")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_browse_missing_directory() {
  let dir = tempfile::tempdir().unwrap();
  let app = base_router(AppState::new(
    dir.path().to_path_buf(),
    std::path::PathBuf::from("."),
  ));

  let response = app
    .oneshot(
      Request::builder()
        .uri("/api/browse?path=nonexistent")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_spa_fallback_serves_index_html() {
  // Any path not matched by a registered route must return 200 with the SPA
  // index.html, not 404.  This covers direct navigation and page refresh at
  // /player/<path> and /browse/<path> URLs.
  let frontend_dir = tempfile::tempdir().unwrap();
  fs::write(
    frontend_dir.path().join("index.html"),
    b"<!doctype html><title>Loku</title>",
  )
  .unwrap();
  let app = base_router(AppState::new(
    tempfile::tempdir().unwrap().path().to_path_buf(),
    frontend_dir.path().to_path_buf(),
  ));

  for path in [
    "/player/some-video.webm",
    "/browse/My%20Channel",
    "/unknown",
  ] {
    let response = app
      .clone()
      .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(
      response.status(),
      StatusCode::OK,
      "expected 200 for SPA path {path}"
    );
  }
}
