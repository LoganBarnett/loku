use axum::{
  body::Body,
  http::{Request, StatusCode},
};
use loku_web::config::{CliRaw, Config, ConfigError, ConfigFileRaw};
use loku_web::web_base::{base_router, AppState};
use std::fs;
use tower::ServiceExt;

/// Issue a GET request against an app and return the response.
async fn get(app: axum::Router, uri: &str) -> axum::http::Response<Body> {
  app
    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
    .await
    .unwrap()
}

/// Issue a GET and deserialize the body as JSON.
async fn get_json(app: axum::Router, uri: &str) -> serde_json::Value {
  let response = get(app, uri).await;
  assert_eq!(response.status(), StatusCode::OK);
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  serde_json::from_slice(&body).unwrap()
}

/// Build a test app rooted at the given library directory.
fn test_app(library: &std::path::Path) -> axum::Router {
  base_router(AppState::new(
    library.to_path_buf(),
    std::path::PathBuf::from("."),
  ))
}

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
async fn test_browse_compound_extension_sidecars() {
  // Files like "clip.mov.webm" have stem "clip.mov"; sidecars must be found
  // as "clip.mov.webp" / "clip.mov.info.json", not "clip.webp" / "clip.info.json".
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mov.webm"), b"").unwrap();
  fs::write(dir.path().join("clip.mov.webp"), b"").unwrap();
  fs::write(
    dir.path().join("clip.mov.info.json"),
    r#"{"title":"Compound","duration":42.0,"upload_date":"20240601"}"#,
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
  assert_eq!(v["name"], "clip.mov.webm");
  assert_eq!(v["title"], "Compound");
  assert_eq!(v["duration_secs"], 42.0);
  assert!(
    v["thumb_path"].as_str().unwrap().ends_with("clip.mov.webp"),
    "thumb_path should point to clip.mov.webp"
  );
}

#[tokio::test]
async fn test_browse_compat_copy_hidden() {
  // Companion .compat.mp4 files must not appear as separate entries.
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.webm"), b"").unwrap();
  fs::write(dir.path().join("clip.compat.mp4"), b"").unwrap();
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
  assert_eq!(
    entries.len(),
    1,
    "compat copy should not appear as a separate entry"
  );
  assert_eq!(entries[0]["name"], "clip.webm");
  assert!(
    entries[0]["compat_path"]
      .as_str()
      .unwrap()
      .ends_with("clip.compat.mp4"),
    "compat_path should point to clip.compat.mp4"
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
async fn test_browse_percent_encoded_path_traversal_rejected() {
  let dir = tempfile::tempdir().unwrap();
  let app = base_router(AppState::new(
    dir.path().to_path_buf(),
    std::path::PathBuf::from("."),
  ));

  // %2e%2e is percent-encoded "..".
  let response = app
    .oneshot(
      Request::builder()
        .uri("/api/browse?path=%2e%2e")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert!(
    response.status() == StatusCode::BAD_REQUEST
      || response.status() == StatusCode::NOT_FOUND,
    "percent-encoded traversal must not succeed: got {}",
    response.status()
  );
}

#[tokio::test]
async fn test_browse_nested_path_traversal_rejected() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("sub")).unwrap();
  let app = base_router(AppState::new(
    dir.path().to_path_buf(),
    std::path::PathBuf::from("."),
  ));

  let response = app
    .oneshot(
      Request::builder()
        .uri("/api/browse?path=sub/../../..")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[cfg(unix)]
#[tokio::test]
async fn test_browse_symlink_outside_library_rejected() {
  let dir = tempfile::tempdir().unwrap();
  let outside = tempfile::tempdir().unwrap();
  fs::write(outside.path().join("secret.mp4"), b"").unwrap();

  std::os::unix::fs::symlink(outside.path(), dir.path().join("escape"))
    .unwrap();
  let app = base_router(AppState::new(
    dir.path().to_path_buf(),
    std::path::PathBuf::from("."),
  ));

  // Browsing the symlink target resolves outside the canonical root.
  let response = app
    .oneshot(
      Request::builder()
        .uri("/api/browse?path=escape")
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

// ---------------------------------------------------------------------------
// Browse endpoint — video format and filtering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_browse_lists_multiple_video_formats() {
  let dir = tempfile::tempdir().unwrap();
  for ext in ["mp4", "mkv", "webm", "avi", "mov"] {
    fs::write(dir.path().join(format!("clip.{ext}")), b"").unwrap();
  }
  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 5);
  assert!(
    entries.iter().all(|e| e["type"] == "video"),
    "all entries should be videos"
  );
}

#[tokio::test]
async fn test_browse_ignores_non_video_files() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"").unwrap();
  fs::write(dir.path().join("notes.txt"), b"").unwrap();
  fs::write(dir.path().join("doc.pdf"), b"").unwrap();
  fs::write(dir.path().join("clip.info.json"), b"{}").unwrap();
  fs::write(dir.path().join("thumb.jpg"), b"").unwrap();
  fs::write(dir.path().join("subs.srt"), b"").unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1);
  assert_eq!(entries[0]["name"], "clip.mp4");
}

#[tokio::test]
async fn test_browse_directories_sorted_before_videos() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("alpha.mp4"), b"").unwrap();
  fs::create_dir(dir.path().join("Zebra")).unwrap();
  fs::write(dir.path().join("beta.mkv"), b"").unwrap();
  fs::create_dir(dir.path().join("Aardvark")).unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 4);
  assert_eq!(entries[0]["type"], "directory");
  assert_eq!(entries[0]["name"], "Aardvark");
  assert_eq!(entries[1]["type"], "directory");
  assert_eq!(entries[1]["name"], "Zebra");
  assert_eq!(entries[2]["type"], "video");
  assert_eq!(entries[2]["name"], "alpha.mp4");
  assert_eq!(entries[3]["type"], "video");
  assert_eq!(entries[3]["name"], "beta.mkv");
}

#[tokio::test]
async fn test_browse_nested_subdirectory_navigation() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir_all(dir.path().join("a/b")).unwrap();
  fs::write(dir.path().join("a/b/clip.mp4"), b"").unwrap();

  let app = test_app(dir.path());

  // First level should show directory "b".
  let json = get_json(app.clone(), "/api/browse?path=a").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1);
  assert_eq!(entries[0]["type"], "directory");
  assert_eq!(entries[0]["name"], "b");

  // Second level should show the video.
  let json = get_json(app, "/api/browse?path=a/b").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1);
  assert_eq!(entries[0]["type"], "video");
  assert_eq!(entries[0]["name"], "clip.mp4");
}

#[tokio::test]
async fn test_browse_video_without_info_json_returns_null_metadata() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("bare.mp4"), b"").unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let v = &json["entries"][0];
  assert_eq!(v["type"], "video");
  assert_eq!(v["name"], "bare.mp4");
  // All optional metadata fields should be absent (skip_serializing_if).
  assert!(v.get("title").is_none());
  assert!(v.get("duration_secs").is_none());
  assert!(v.get("upload_date").is_none());
  assert!(v.get("description").is_none());
  assert!(v.get("channel").is_none());
  assert!(v.get("channel_url").is_none());
  assert!(v.get("webpage_url").is_none());
  assert!(v.get("view_count").is_none());
}

#[tokio::test]
async fn test_browse_malformed_info_json_falls_back_gracefully() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"").unwrap();
  fs::write(dir.path().join("clip.info.json"), b"not json").unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1);
  assert_eq!(entries[0]["name"], "clip.mp4");
  assert!(entries[0].get("title").is_none());
}

#[tokio::test]
async fn test_browse_info_json_with_partial_fields() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"").unwrap();
  fs::write(dir.path().join("clip.info.json"), r#"{"title":"Partial"}"#)
    .unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let v = &json["entries"][0];
  assert_eq!(v["title"], "Partial");
  assert!(v.get("duration_secs").is_none());
  assert!(v.get("upload_date").is_none());
}

#[tokio::test]
async fn test_browse_thumbnail_precedence_order() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"").unwrap();
  fs::write(dir.path().join("clip.jpg"), b"").unwrap();
  fs::write(dir.path().join("clip.webp"), b"").unwrap();
  fs::write(dir.path().join("clip.png"), b"").unwrap();

  // With all three present, jpg should win (first in THUMB_EXTENSIONS).
  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let thumb = json["entries"][0]["thumb_path"].as_str().unwrap();
  assert!(thumb.ends_with("clip.jpg"), "expected jpg first, got {thumb}");

  // Remove jpg — webp should be next.
  fs::remove_file(dir.path().join("clip.jpg")).unwrap();
  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let thumb = json["entries"][0]["thumb_path"].as_str().unwrap();
  assert!(
    thumb.ends_with("clip.webp"),
    "expected webp after removing jpg, got {thumb}"
  );
}

#[tokio::test]
async fn test_browse_video_with_all_metadata_fields() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"").unwrap();
  fs::write(
    dir.path().join("clip.info.json"),
    r#"{
      "title": "Full",
      "duration": 120.5,
      "upload_date": "20250101",
      "description": "A test video.",
      "channel": "TestChannel",
      "channel_url": "https://example.com/channel",
      "webpage_url": "https://example.com/watch",
      "view_count": 42
    }"#,
  )
  .unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let v = &json["entries"][0];
  assert_eq!(v["title"], "Full");
  assert_eq!(v["duration_secs"], 120.5);
  assert_eq!(v["upload_date"], "20250101");
  assert_eq!(v["description"], "A test video.");
  assert_eq!(v["channel"], "TestChannel");
  assert_eq!(v["channel_url"], "https://example.com/channel");
  assert_eq!(v["webpage_url"], "https://example.com/watch");
  assert_eq!(v["view_count"], 42);
}

#[tokio::test]
async fn test_browse_with_leading_slash_path() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("subdir")).unwrap();
  fs::write(dir.path().join("subdir/clip.mp4"), b"").unwrap();

  let app = test_app(dir.path());

  let with_slash = get_json(app.clone(), "/api/browse?path=/subdir").await;
  let without_slash = get_json(app, "/api/browse?path=subdir").await;

  assert_eq!(with_slash["entries"], without_slash["entries"]);
}

#[tokio::test]
async fn test_browse_case_insensitive_video_extensions() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.MP4"), b"").unwrap();
  fs::write(dir.path().join("clip2.MKV"), b"").unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 2);
  assert!(
    entries.iter().all(|e| e["type"] == "video"),
    "uppercase extensions should be recognized as videos"
  );
}

#[tokio::test]
async fn test_browse_info_json_with_extra_unknown_fields() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"").unwrap();
  fs::write(
    dir.path().join("clip.info.json"),
    r#"{"title":"X","foo":"bar","baz":123}"#,
  )
  .unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let v = &json["entries"][0];
  assert_eq!(v["title"], "X");
  // Unknown fields should not appear in the response.
  assert!(v.get("foo").is_none());
  assert!(v.get("baz").is_none());
}

// ---------------------------------------------------------------------------
// File serving
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_files_serves_video_with_correct_content_type() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"fake video").unwrap();

  let response = get(test_app(dir.path()), "/files/clip.mp4").await;
  assert_eq!(response.status(), StatusCode::OK);
  let ct = response
    .headers()
    .get("content-type")
    .unwrap()
    .to_str()
    .unwrap();
  assert!(ct.contains("video/mp4"), "expected video/mp4, got {ct}");
}

#[tokio::test]
async fn test_files_serves_thumbnail_with_correct_content_type() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("thumb.jpg"), b"fake jpeg").unwrap();

  let response = get(test_app(dir.path()), "/files/thumb.jpg").await;
  assert_eq!(response.status(), StatusCode::OK);
  let ct = response
    .headers()
    .get("content-type")
    .unwrap()
    .to_str()
    .unwrap();
  assert!(ct.contains("image/jpeg"), "expected image/jpeg, got {ct}");
}

#[tokio::test]
async fn test_files_returns_404_for_nonexistent_file() {
  let dir = tempfile::tempdir().unwrap();
  let response = get(test_app(dir.path()), "/files/does-not-exist.mp4").await;
  assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_files_serves_files_from_nested_directories() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("sub")).unwrap();
  fs::write(dir.path().join("sub/clip.mp4"), b"nested video").unwrap();

  let response = get(test_app(dir.path()), "/files/sub/clip.mp4").await;
  assert_eq!(response.status(), StatusCode::OK);
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  assert_eq!(&body[..], b"nested video");
}

// ---------------------------------------------------------------------------
// SPA fallback
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_spa_fallback_cache_control_header() {
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

  let response = get(app, "/player/clip.mp4").await;
  assert_eq!(response.status(), StatusCode::OK);
  let cc = response
    .headers()
    .get("cache-control")
    .unwrap()
    .to_str()
    .unwrap();
  assert_eq!(cc, "no-store");
}

#[tokio::test]
async fn test_spa_fallback_does_not_override_api_routes() {
  let frontend_dir = tempfile::tempdir().unwrap();
  fs::write(
    frontend_dir.path().join("index.html"),
    b"<!doctype html><title>Loku</title>",
  )
  .unwrap();
  let library_dir = tempfile::tempdir().unwrap();
  let app = base_router(AppState::new(
    library_dir.path().to_path_buf(),
    frontend_dir.path().to_path_buf(),
  ));

  for uri in [
    "/healthz",
    "/metrics",
    "/api/browse",
    "/api-docs/openapi.json",
  ] {
    let response = get(app.clone(), uri).await;
    assert_eq!(
      response.status(),
      StatusCode::OK,
      "API route {uri} should return 200"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
      .await
      .unwrap();
    let body_str = String::from_utf8_lossy(&body);
    assert!(
      !body_str.contains("<title>Loku</title>"),
      "{uri} should not return the SPA index.html"
    );
  }
}

// ---------------------------------------------------------------------------
// Health and metrics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_healthz_response_structure() {
  let dir = tempfile::tempdir().unwrap();
  let json = get_json(test_app(dir.path()), "/healthz").await;
  assert_eq!(json, serde_json::json!({"status": "healthy"}));
}

#[tokio::test]
async fn test_metrics_content_type_is_text_plain() {
  let dir = tempfile::tempdir().unwrap();
  let response = get(test_app(dir.path()), "/metrics").await;
  assert_eq!(response.status(), StatusCode::OK);
  let ct = response
    .headers()
    .get("content-type")
    .unwrap()
    .to_str()
    .unwrap();
  assert!(ct.starts_with("text/plain"), "expected text/plain, got {ct}");
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_browse_special_characters_in_directory_names() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("My Channel (2024) [HD]")).unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1);
  assert_eq!(entries[0]["name"], "My Channel (2024) [HD]");
}

#[tokio::test]
async fn test_browse_spaces_in_video_filenames() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("My Video.mp4"), b"").unwrap();
  fs::write(dir.path().join("My Video.jpg"), b"").unwrap();
  fs::write(dir.path().join("My Video.info.json"), r#"{"title":"Spaced"}"#)
    .unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=").await;
  let v = &json["entries"][0];
  assert_eq!(v["name"], "My Video.mp4");
  assert_eq!(v["title"], "Spaced");
  assert!(v["thumb_path"].as_str().unwrap().ends_with("My Video.jpg"));
}

#[tokio::test]
async fn test_browse_empty_subdirectory_returns_empty_entries() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("empty-dir")).unwrap();

  let json = get_json(test_app(dir.path()), "/api/browse?path=empty-dir").await;
  assert_eq!(json["entries"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[test]
fn test_config_cli_args_override_config_file_values() {
  let dir = tempfile::tempdir().unwrap();
  let config_path = dir.path().join("config.toml");
  fs::write(&config_path, "log_level = \"error\"\n").unwrap();

  let cli = CliRaw {
    log_level: Some("debug".to_string()),
    log_format: None,
    config: Some(config_path),
    listen: None,
    library_path: Some(dir.path().to_path_buf()),
    frontend_path: None,
  };

  let config = Config::from_cli_and_file(cli).unwrap();
  assert_eq!(config.log_level.to_string(), "debug");
}

#[test]
fn test_config_missing_library_path_returns_error() {
  let cli = CliRaw {
    log_level: None,
    log_format: None,
    config: None,
    listen: None,
    library_path: Some(std::path::PathBuf::from("/nonexistent/path/for/test")),
    frontend_path: None,
  };

  let err = Config::from_cli_and_file(cli).unwrap_err();
  assert!(
    matches!(err, ConfigError::LibraryPathNotFound { .. }),
    "expected LibraryPathNotFound, got {err:?}"
  );
}

#[test]
fn test_config_invalid_log_level_returns_error() {
  let dir = tempfile::tempdir().unwrap();
  let cli = CliRaw {
    log_level: Some("bogus".to_string()),
    log_format: None,
    config: None,
    listen: None,
    library_path: Some(dir.path().to_path_buf()),
    frontend_path: None,
  };

  let err = Config::from_cli_and_file(cli).unwrap_err();
  assert!(
    matches!(err, ConfigError::Validation(..)),
    "expected Validation, got {err:?}"
  );
}

#[test]
fn test_config_malformed_toml_returns_error() {
  let dir = tempfile::tempdir().unwrap();
  let config_path = dir.path().join("bad.toml");
  fs::write(&config_path, "not = [valid toml {{{").unwrap();

  let err = ConfigFileRaw::from_file(&config_path).unwrap_err();
  assert!(
    matches!(err, ConfigError::Parse { .. }),
    "expected Parse, got {err:?}"
  );
}

#[test]
fn test_config_defaults_when_no_file_and_no_cli_args() {
  // library_path defaults to "." which must exist.
  let cli = CliRaw {
    log_level: None,
    log_format: None,
    config: None,
    listen: None,
    library_path: None,
    frontend_path: None,
  };

  let config = Config::from_cli_and_file(cli).unwrap();
  assert_eq!(config.log_level.to_string(), "info");
  assert_eq!(config.log_format.to_string(), "text");
  assert_eq!(config.listen_address.to_string(), "127.0.0.1:3000");
  assert_eq!(config.library_path, std::path::PathBuf::from("."));
  assert_eq!(config.frontend_path, std::path::PathBuf::from("frontend/dist"));
}
