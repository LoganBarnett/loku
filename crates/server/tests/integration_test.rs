// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use axum::{
  body::Body,
  http::{Request, StatusCode},
};
use loku_server::config::{CliRaw, Config, ExtraCliFields};
use loku_server::frontend::Frontend;
use loku_server::web_base::{app_routes, AppState, LibraryPathCheck};
use rust_template_foundation::server::runner::{
  BaseServerState, Server, ServerRunConfig,
};
use std::fs;
use std::path::Path;
use tower::ServiceExt;

/// Build a test router rooted at the given library directory, assembled the
/// same way `main` assembles the production server but without binding a
/// listener.
async fn test_router(library: &Path) -> axum::Router {
  let run_config = ServerRunConfig {
    app_name: "loku".to_string(),
    listen_address: "127.0.0.1:0".parse().unwrap(),
    base_url: "http://localhost".to_string(),
    oidc: None,
  };
  let base = BaseServerState::init(&run_config).await.unwrap();
  base
    .health_registry
    .register(
      "library",
      LibraryPathCheck {
        library_path: library.to_path_buf(),
      },
    )
    .await;
  let library_path = library.to_path_buf();
  Server::new(base, run_config)
    .with_state(|base| AppState { base, library_path })
    .merge(app_routes(library))
    .spa::<Frontend>()
    .into_test_router()
}

/// Issue a GET request against a router and return the response.
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

/// Construct a CliRaw with all fields defaulted; tests override what they need.
fn cli_with_library(library: &Path) -> CliRaw {
  CliRaw {
    log_level: None,
    log_format: None,
    config: None,
    listen: None,
    base_url: None,
    extra: ExtraCliFields {
      library: Some(library.to_path_buf()),
      oidc_issuer: None,
      oidc_client_id: None,
      oidc_client_secret_file: None,
    },
  }
}

// ── infrastructure routes (foundation-owned) ────────────────────────────────

#[tokio::test]
async fn test_openapi_json_endpoint() {
  let body_str = {
    let response =
      get(test_router(Path::new(".")).await, "/api-docs/openapi.json").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
      .await
      .unwrap();
    String::from_utf8(body.to_vec()).unwrap()
  };

  assert!(body_str.contains("openapi"), "Response should be an OpenAPI spec");
  assert!(body_str.contains("/healthz"), "Spec should document /healthz");
  assert!(body_str.contains("/api/browse"), "Spec should document /api/browse");
}

#[tokio::test]
async fn test_scalar_ui_endpoint() {
  let response = get(test_router(Path::new(".")).await, "/scalar").await;
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
  let response = get(test_router(Path::new(".")).await, "/healthz").await;
  assert_eq!(response.status(), StatusCode::OK);
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  let body_str = String::from_utf8(body.to_vec()).unwrap();
  assert!(body_str.contains("healthy"));
}

#[tokio::test]
async fn test_healthz_response_structure() {
  let dir = tempfile::tempdir().unwrap();
  let json = get_json(test_router(dir.path()).await, "/healthz").await;
  // Foundation wraps component checks; the library check reports healthy for
  // an existing directory.
  assert_eq!(json["status"], "healthy");
  assert_eq!(json["components"]["library"]["status"], "healthy");
}

#[tokio::test]
async fn test_metrics_endpoint() {
  let dir = tempfile::tempdir().unwrap();
  let app = test_router(dir.path()).await;
  // Warm up one labeled sample so the IntCounterVec emits a series.
  let _ = get(app.clone(), "/healthz").await;
  let response = get(app, "/metrics").await;
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
async fn test_metrics_content_type_is_text_plain() {
  let dir = tempfile::tempdir().unwrap();
  let response = get(test_router(dir.path()).await, "/metrics").await;
  assert_eq!(response.status(), StatusCode::OK);
  let ct = response
    .headers()
    .get("content-type")
    .unwrap()
    .to_str()
    .unwrap();
  assert!(ct.starts_with("text/plain"), "expected text/plain, got {ct}");
}

// ── browse endpoint ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_browse_empty_root() {
  let dir = tempfile::tempdir().unwrap();
  let json = get_json(test_router(dir.path()).await, "/api/browse").await;
  assert_eq!(json["entries"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_browse_lists_directory_entry() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("My Channel")).unwrap();
  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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
  // Files like "clip.mov.webm" have stem "clip.mov"; sidecars must be found as
  // "clip.mov.webp" / "clip.mov.info.json", not "clip.webp" / "clip.info.json".
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mov.webm"), b"").unwrap();
  fs::write(dir.path().join("clip.mov.webp"), b"").unwrap();
  fs::write(
    dir.path().join("clip.mov.info.json"),
    r#"{"title":"Compound","duration":42.0,"upload_date":"20240601"}"#,
  )
  .unwrap();

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1, "compat copy should not appear separately");
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
  let response =
    get(test_router(dir.path()).await, "/api/browse?path=..").await;
  assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_browse_percent_encoded_path_traversal_rejected() {
  let dir = tempfile::tempdir().unwrap();
  let response =
    get(test_router(dir.path()).await, "/api/browse?path=%2e%2e").await;
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
  let response =
    get(test_router(dir.path()).await, "/api/browse?path=sub/../../..").await;
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

  let response =
    get(test_router(dir.path()).await, "/api/browse?path=escape").await;
  assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_browse_missing_directory() {
  let dir = tempfile::tempdir().unwrap();
  let response =
    get(test_router(dir.path()).await, "/api/browse?path=nonexistent").await;
  assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_browse_lists_multiple_video_formats() {
  let dir = tempfile::tempdir().unwrap();
  for ext in ["mp4", "mkv", "webm", "avi", "mov"] {
    fs::write(dir.path().join(format!("clip.{ext}")), b"").unwrap();
  }
  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let app = test_router(dir.path()).await;
  let json = get_json(app.clone(), "/api/browse?path=a").await;
  let entries = json["entries"].as_array().unwrap();
  assert_eq!(entries.len(), 1);
  assert_eq!(entries[0]["type"], "directory");
  assert_eq!(entries[0]["name"], "b");

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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
  let v = &json["entries"][0];
  assert_eq!(v["type"], "video");
  assert_eq!(v["name"], "bare.mp4");
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
  let thumb = json["entries"][0]["thumb_path"].as_str().unwrap();
  assert!(thumb.ends_with("clip.jpg"), "expected jpg first, got {thumb}");

  fs::remove_file(dir.path().join("clip.jpg")).unwrap();
  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let app = test_router(dir.path()).await;
  let with_slash = get_json(app.clone(), "/api/browse?path=/subdir").await;
  let without_slash = get_json(app, "/api/browse?path=subdir").await;
  assert_eq!(with_slash["entries"], without_slash["entries"]);
}

#[tokio::test]
async fn test_browse_case_insensitive_video_extensions() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.MP4"), b"").unwrap();
  fs::write(dir.path().join("clip2.MKV"), b"").unwrap();

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
  let v = &json["entries"][0];
  assert_eq!(v["title"], "X");
  assert!(v.get("foo").is_none());
  assert!(v.get("baz").is_none());
}

#[tokio::test]
async fn test_browse_special_characters_in_directory_names() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("My Channel (2024) [HD]")).unwrap();

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
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

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
  let v = &json["entries"][0];
  assert_eq!(v["name"], "My Video.mp4");
  assert_eq!(v["title"], "Spaced");
  assert!(v["thumb_path"].as_str().unwrap().ends_with("My Video.jpg"));
}

#[tokio::test]
async fn test_browse_empty_subdirectory_returns_empty_entries() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("empty-dir")).unwrap();

  let json =
    get_json(test_router(dir.path()).await, "/api/browse?path=empty-dir").await;
  assert_eq!(json["entries"].as_array().unwrap().len(), 0);
}

// ── file serving ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_files_serves_video_with_correct_content_type() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"fake video").unwrap();

  let response = get(test_router(dir.path()).await, "/files/clip.mp4").await;
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

  let response = get(test_router(dir.path()).await, "/files/thumb.jpg").await;
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
  let response =
    get(test_router(dir.path()).await, "/files/does-not-exist.mp4").await;
  assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_files_advertises_byte_ranges() {
  // Safari (desktop and iOS) refuses to play a video whose server does not
  // advertise range support, so a full GET must carry Accept-Ranges: bytes.
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"0123456789").unwrap();

  let response = get(test_router(dir.path()).await, "/files/clip.mp4").await;
  assert_eq!(response.status(), StatusCode::OK);
  assert_eq!(
    response
      .headers()
      .get("accept-ranges")
      .and_then(|v| v.to_str().ok()),
    Some("bytes"),
    "video responses must advertise byte-range support for Safari"
  );
}

#[tokio::test]
async fn test_files_serves_range_request() {
  // A Range request must yield 206 with a Content-Range and only the requested
  // bytes; this is the seek/streaming contract Safari relies on.
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"0123456789").unwrap();

  let response = test_router(dir.path())
    .await
    .oneshot(
      Request::builder()
        .uri("/files/clip.mp4")
        .header("range", "bytes=0-3")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();

  assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
  assert_eq!(
    response
      .headers()
      .get("content-range")
      .and_then(|v| v.to_str().ok()),
    Some("bytes 0-3/10"),
  );
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  assert_eq!(&body[..], b"0123", "should return only the requested bytes");
}

#[tokio::test]
async fn test_browse_native_type_from_codecs() {
  // The player's <source type> hint is derived server-side from info.json's
  // vcodec/acodec so canPlayType can reject an undecodable native up front.
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"").unwrap();
  fs::write(
    dir.path().join("clip.info.json"),
    r#"{"vcodec":"av01.0.05M.08","acodec":"opus"}"#,
  )
  .unwrap();

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
  let v = &json["entries"].as_array().unwrap()[0];
  assert_eq!(v["native_type"], r#"video/mp4; codecs="av01.0.05M.08, opus""#);
}

#[tokio::test]
async fn test_browse_native_type_absent_for_non_standard_container() {
  // An MKV native has no MIME canPlayType accepts, so no native_type is sent
  // and the player will serve only the compat copy.
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mkv"), b"").unwrap();
  fs::write(
    dir.path().join("clip.info.json"),
    r#"{"vcodec":"avc1.640028","acodec":"mp4a.40.2"}"#,
  )
  .unwrap();

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
  let v = &json["entries"].as_array().unwrap()[0];
  assert!(
    v.get("native_type").is_none(),
    "native_type must be omitted for non-standard containers, got {:?}",
    v.get("native_type"),
  );
}

#[tokio::test]
async fn test_browse_native_type_absent_without_info_json() {
  // With no info.json the codecs are unknown, so the native is not offered as
  // a typed source.
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"").unwrap();

  let json = get_json(test_router(dir.path()).await, "/api/browse?path=").await;
  let v = &json["entries"].as_array().unwrap()[0];
  assert!(
    v.get("native_type").is_none(),
    "native_type must be omitted when codecs are unknown"
  );
}

#[tokio::test]
async fn test_files_serves_files_from_nested_directories() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("sub")).unwrap();
  fs::write(dir.path().join("sub/clip.mp4"), b"nested video").unwrap();

  let response =
    get(test_router(dir.path()).await, "/files/sub/clip.mp4").await;
  assert_eq!(response.status(), StatusCode::OK);
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  assert_eq!(&body[..], b"nested video");
}

// ── SPA fallback (embedded frontend) ────────────────────────────────────────

#[tokio::test]
async fn test_spa_fallback_serves_index_html() {
  // Any path not matched by a registered route must return 200 with the
  // embedded SPA index.html, not 404 — this covers direct navigation and page
  // refresh at /player/<path> and /browse/<path> URLs.
  let dir = tempfile::tempdir().unwrap();
  let app = test_router(dir.path()).await;
  for path in [
    "/player/some-video.webm",
    "/browse/My%20Channel",
    "/unknown",
  ] {
    let response = get(app.clone(), path).await;
    assert_eq!(
      response.status(),
      StatusCode::OK,
      "expected 200 for SPA path {path}"
    );
  }
}

#[tokio::test]
async fn test_spa_fallback_cache_control_header() {
  let dir = tempfile::tempdir().unwrap();
  let response = get(test_router(dir.path()).await, "/player/clip.mp4").await;
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
  let dir = tempfile::tempdir().unwrap();
  let app = test_router(dir.path()).await;
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
      !body_str.trim_start().starts_with('<'),
      "{uri} should not return the SPA index.html"
    );
  }
}

// ── configuration ───────────────────────────────────────────────────────────

#[test]
fn test_config_cli_args_override_config_file_values() {
  let dir = tempfile::tempdir().unwrap();
  let config_path = dir.path().join("config.toml");
  fs::write(&config_path, "log_level = \"error\"\n").unwrap();

  let mut cli = cli_with_library(dir.path());
  cli.log_level = Some("debug".to_string());
  cli.config = Some(config_path);

  let config = Config::from_cli_and_file(cli).unwrap();
  assert_eq!(config.log_level.to_string(), "debug");
}

#[test]
fn test_config_missing_library_returns_error() {
  let cli = CliRaw {
    log_level: None,
    log_format: None,
    config: None,
    listen: None,
    base_url: None,
    extra: ExtraCliFields {
      library: None,
      oidc_issuer: None,
      oidc_client_id: None,
      oidc_client_secret_file: None,
    },
  };

  let err = Config::from_cli_and_file(cli).unwrap_err();
  assert!(
    err.to_string().contains("library is required"),
    "expected a missing-library error, got: {err}"
  );
}

#[test]
fn test_config_nonexistent_library_returns_error() {
  let cli = cli_with_library(Path::new("/nonexistent/path/for/test"));
  let err = Config::from_cli_and_file(cli).unwrap_err();
  assert!(
    err.to_string().contains("does not exist"),
    "expected a nonexistent-library error, got: {err}"
  );
}

#[test]
fn test_config_invalid_log_level_returns_error() {
  let dir = tempfile::tempdir().unwrap();
  let mut cli = cli_with_library(dir.path());
  cli.log_level = Some("bogus".to_string());

  assert!(
    Config::from_cli_and_file(cli).is_err(),
    "an invalid log level should be rejected"
  );
}

#[test]
fn test_config_defaults_when_no_file_and_no_cli_args() {
  let dir = tempfile::tempdir().unwrap();
  let config = Config::from_cli_and_file(cli_with_library(dir.path())).unwrap();
  assert_eq!(config.log_level.to_string(), "info");
  assert_eq!(config.log_format.to_string(), "text");
  assert_eq!(config.listen_address.to_string(), "127.0.0.1:3000");
  assert_eq!(config.base_url, "http://localhost:3000");
  assert_eq!(config.library_path, dir.path());
  assert!(config.oidc.is_none());
}

#[test]
fn test_config_full_oidc() {
  let dir = tempfile::tempdir().unwrap();
  let secret_file = dir.path().join("oidc-secret");
  fs::write(&secret_file, "test-secret\n").unwrap();

  let mut cli = cli_with_library(dir.path());
  cli.extra.oidc_issuer = Some("https://sso.example.com".to_string());
  cli.extra.oidc_client_id = Some("loku-client".to_string());
  cli.extra.oidc_client_secret_file = Some(secret_file);

  let config = Config::from_cli_and_file(cli).unwrap();
  let oidc = config.oidc.expect("OIDC config should be Some");
  assert_eq!(oidc.issuer, "https://sso.example.com");
  assert_eq!(oidc.client_id, "loku-client");
  assert_eq!(oidc.client_secret, "test-secret");
}

#[test]
fn test_config_partial_oidc_errors() {
  let dir = tempfile::tempdir().unwrap();
  let mut cli = cli_with_library(dir.path());
  cli.extra.oidc_issuer = Some("https://sso.example.com".to_string());

  let err = Config::from_cli_and_file(cli).unwrap_err();
  assert!(
    err.to_string().contains("partial OIDC"),
    "error should describe partial OIDC config, got: {err}"
  );
}
