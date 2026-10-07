// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! Identity resolution end to end over the fake TMDB: automatic resolution,
//! the fingerprint cache, operator confirmation through the API, retries,
//! and the directory rename.

mod common;

use axum::{
  body::Body,
  http::{Request, StatusCode},
};
use common::{
  candidate, library, movie_details, tv_details, write_disc_dir, FakeTmdb,
  FakeTmdbFailure,
};
use loku_lib::nfo::{self, Nfo};
use loku_server::frontend::Frontend;
use loku_server::identity::cache::IdentityCache;
use loku_server::identity::fingerprint;
use loku_server::identity::resolver::Resolver;
use loku_server::identity::tmdb::TmdbClient;
use loku_server::identity::worker::{self, RETRY_BACKOFF};
use loku_server::identity::{
  Identity, IdentitySource, IdentityState, TmdbKind,
};
use loku_server::index::scan;
use loku_server::index::store::{DiscRecord, Index};
use loku_server::library::{Library, LibraryKind};
use loku_server::web_base::{app_routes, AppState};
use rust_template_foundation::server::runner::{
  BaseServerState, Server, ServerRunConfig,
};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const STRONG: &[(u64, u32)] = &[(8160, 30), (1300, 5)];
const WEAK: &[(u64, u32)] = &[(8160, 30)];

struct Harness {
  root: tempfile::TempDir,
  index: Index,
  cache: IdentityCache,
  tmdb: Arc<FakeTmdb>,
  resolver: Arc<Resolver>,
  libraries: Vec<Library>,
}

impl Harness {
  async fn new(tmdb: FakeTmdb) -> Self {
    let root = tempfile::tempdir().unwrap();
    let index = Index::open_in_memory().await.unwrap();
    let cache = IdentityCache::open_in_memory().await.unwrap();
    let tmdb = Arc::new(tmdb);
    let resolver = Arc::new(Resolver::new(
      tmdb.clone() as Arc<dyn TmdbClient>,
      cache.clone(),
    ));
    let libraries = vec![library("discs", root.path(), LibraryKind::Discs)];
    Self {
      root,
      index,
      cache,
      tmdb,
      resolver,
      libraries,
    }
  }

  async fn scan(&self) {
    scan::scan_all(&self.index, &self.libraries).await.unwrap();
  }

  async fn step_with(&self, backoff: Duration) -> bool {
    let (status, _rx) = tokio::sync::watch::channel(None);
    worker::process_next(
      &self.index,
      &self.libraries,
      &self.resolver,
      &status,
      backoff,
    )
    .await
  }

  async fn step(&self) -> bool {
    self.step_with(RETRY_BACKOFF).await
  }

  async fn disc(&self, rel_path: &str) -> Option<DiscRecord> {
    self.index.disc("discs", rel_path).await.unwrap()
  }

  fn path(&self, rel: &str) -> std::path::PathBuf {
    self.root.path().join(rel)
  }

  async fn router(&self, identity: bool) -> axum::Router {
    let run_config = ServerRunConfig {
      app_name: "loku".to_string(),
      listen_address: "127.0.0.1:0".parse().unwrap(),
      base_url: "http://localhost".to_string(),
      oidc: None,
    };
    let base = BaseServerState::init(&run_config).await.unwrap();
    let state_index = self.index.clone();
    let state_libraries = Arc::new(self.libraries.clone());
    let resolver = identity.then(|| self.resolver.clone());
    Server::new(base, run_config)
      .with_state(|base| AppState {
        base,
        libraries: state_libraries,
        index: state_index,
        derivation_active: tokio::sync::watch::channel(None).1,
        rips: None,
        identity: resolver,
      })
      .merge(app_routes(&self.libraries))
      .spa::<Frontend>()
      .into_test_router()
  }
}

fn matrix_tmdb() -> FakeTmdb {
  FakeTmdb::new()
    .with_results(
      "THE MATRIX",
      vec![
        candidate(TmdbKind::Movie, 603, "The Matrix", Some(1999), 80.0),
        candidate(
          TmdbKind::Movie,
          604,
          "The Matrix Reloaded",
          Some(2003),
          30.0,
        ),
      ],
    )
    .with_movie(movie_details(603, "The Matrix", 1999))
    .with_movie(movie_details(604, "The Matrix Reloaded", 2003))
}

async fn get_json(app: axum::Router, uri: &str) -> serde_json::Value {
  let response = app
    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK, "{uri}");
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  serde_json::from_slice(&body).unwrap()
}

async fn post(
  app: axum::Router,
  uri: &str,
  body: serde_json::Value,
) -> (StatusCode, String) {
  let response = app
    .oneshot(
      Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap(),
    )
    .await
    .unwrap();
  let status = response.status();
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  (status, String::from_utf8_lossy(&body).to_string())
}

#[tokio::test]
async fn a_confident_search_resolves_and_writes_sidecars() {
  let harness = Harness::new(matrix_tmdb()).await;
  write_disc_dir(harness.root.path(), "THE_MATRIX", "uuid1", STRONG);
  harness.scan().await;

  assert!(harness.step().await, "one disc to resolve");
  assert!(!harness.step().await, "nothing left");

  let new_rel = "The Matrix (1999)__uuid1";
  assert!(!harness.path("THE_MATRIX__uuid1").exists(), "renamed away");
  let dir = harness.path(new_rel);
  let nfo =
    nfo::parse_nfo(&std::fs::read_to_string(dir.join("movie.nfo")).unwrap())
      .unwrap();
  let Nfo::Movie(movie) = nfo else {
    panic!("movie nfo")
  };
  assert_eq!(movie.title.as_deref(), Some("The Matrix"));
  assert_eq!(movie.year, Some(1999));
  assert_eq!(movie.unique_ids[0].value.as_deref(), Some("603"));
  assert!(dir.join("poster.jpg").is_file());
  assert!(dir.join("fanart.jpg").is_file());
  assert!(dir.join("title_t00.mkv").is_file());

  let disc = harness.disc(new_rel).await.expect("row moved");
  assert_eq!(disc.identity_state, IdentityState::Resolved);
  assert_eq!(disc.identity_source, Some(IdentitySource::Search));
  assert_eq!(disc.tmdb_id, Some(603));
  assert_eq!(disc.title, "The Matrix");
  assert_eq!(disc.year, Some(1999));
  assert_eq!(
    disc.poster_rel_path.as_deref(),
    Some("The Matrix (1999)__uuid1/poster.jpg")
  );
  assert!(harness.disc("THE_MATRIX__uuid1").await.is_none());
  let video = harness
    .index
    .video("discs", "The Matrix (1999)__uuid1/title_t00.mkv")
    .await
    .unwrap()
    .expect("video row moved");
  assert_eq!(video.title.as_deref(), Some("The Matrix"));

  let fingerprint = disc.fingerprint.expect("fingerprint stored");
  let cached = harness
    .cache
    .lookup(&fingerprint)
    .await
    .unwrap()
    .expect("cached");
  assert_eq!(cached.identity.id, 603);
  assert_eq!(cached.source, IdentitySource::Search);
  assert_eq!(harness.tmdb.search_count(), 1);

  // A rescan keeps the resolution: the NFO on disk is the authority.
  harness.scan().await;
  let disc = harness.disc(new_rel).await.unwrap();
  assert_eq!(disc.identity_state, IdentityState::Resolved);
  assert_eq!(disc.identity_source, Some(IdentitySource::Search));
}

#[tokio::test]
async fn a_second_pressing_resolves_from_the_cache_without_searching() {
  let harness = Harness::new(matrix_tmdb()).await;
  write_disc_dir(harness.root.path(), "THE_MATRIX", "uuid1", STRONG);
  harness.scan().await;
  assert!(harness.step().await);
  assert_eq!(harness.tmdb.search_count(), 1);

  write_disc_dir(harness.root.path(), "MATRIX_UK", "uuid2", STRONG);
  harness.scan().await;
  assert!(harness.step().await);
  let disc = harness
    .disc("The Matrix (1999)__uuid2")
    .await
    .expect("resolved from cache");
  assert_eq!(disc.identity_source, Some(IdentitySource::Cache));
  assert_eq!(disc.tmdb_id, Some(603));
  assert_eq!(harness.tmdb.search_count(), 1, "no second search");
}

#[tokio::test]
async fn a_weak_cache_hit_needs_the_search_to_agree() {
  let tmdb = matrix_tmdb()
    .with_results(
      "OTHER",
      vec![candidate(
        TmdbKind::Movie,
        604,
        "Other Film",
        Some(2003),
        10.0,
      )],
    )
    .with_results(
      "SAME",
      vec![candidate(
        TmdbKind::Movie,
        603,
        "Same Matrix",
        Some(1999),
        10.0,
      )],
    );
  let harness = Harness::new(tmdb).await;
  let seeded = Identity {
    kind: TmdbKind::Movie,
    id: 603,
    title: "The Matrix".to_string(),
    year: Some(1999),
  };
  let weak_fingerprint =
    fingerprint::compute(&common::disc_manifest("x", "y", WEAK).titles);
  assert!(!weak_fingerprint.is_strong());
  harness
    .cache
    .record(&weak_fingerprint.sha256, &seeded, IdentitySource::Search)
    .await
    .unwrap();

  // Disagreement: the search's lone hit is a different film.
  write_disc_dir(harness.root.path(), "OTHER", "uuid1", WEAK);
  harness.scan().await;
  assert!(harness.step().await);
  let disc = harness.disc("OTHER__uuid1").await.unwrap();
  assert_eq!(disc.identity_state, IdentityState::NeedsConfirmation);
  let candidates = disc.candidates();
  assert_eq!(candidates[0].id, 603, "the cached suggestion comes first");
  assert_eq!(candidates[1].id, 604);

  // Agreement: the same pressing shape whose search lands on the cached id.
  write_disc_dir(harness.root.path(), "SAME", "uuid2", WEAK);
  harness.scan().await;
  assert!(harness.step().await);
  let disc = harness
    .disc("The Matrix (1999)__uuid2")
    .await
    .expect("resolved");
  assert_eq!(disc.identity_source, Some(IdentitySource::Cache));
}

#[tokio::test]
async fn ambiguous_results_wait_for_the_operator_and_the_api_commits() {
  let tmdb = FakeTmdb::new()
    .with_results(
      "GHOSTBUSTERS",
      vec![
        candidate(TmdbKind::Movie, 1, "Ghostbusters", Some(2016), 50.0),
        candidate(TmdbKind::Movie, 2, "Ghostbusters", Some(1984), 45.0),
      ],
    )
    .with_movie(movie_details(1, "Ghostbusters", 2016))
    .with_movie(movie_details(2, "Ghostbusters", 1984));
  let harness = Harness::new(tmdb).await;
  write_disc_dir(harness.root.path(), "GHOSTBUSTERS", "uuid3", STRONG);
  harness.scan().await;
  assert!(harness.step().await);
  assert!(!harness.step().await, "awaiting confirmation is not retried");
  assert_eq!(
    harness
      .disc("GHOSTBUSTERS__uuid3")
      .await
      .unwrap()
      .identity_state,
    IdentityState::NeedsConfirmation
  );

  let app = harness.router(true).await;
  let json = get_json(app.clone(), "/api/discs/unresolved").await;
  assert_eq!(json["identity_enabled"], true);
  let disc = &json["discs"][0];
  assert_eq!(disc["path"], "GHOSTBUSTERS__uuid3");
  assert_eq!(disc["identity_state"], "needs_confirmation");
  assert_eq!(disc["query"], "GHOSTBUSTERS");
  assert_eq!(disc["fingerprint_strong"], true);
  let candidates = disc["candidates"].as_array().unwrap();
  assert_eq!(candidates.len(), 2);
  assert_eq!(candidates[1]["tmdb_id"], 2);
  assert_eq!(candidates[1]["tmdb_kind"], "movie");
  assert_eq!(candidates[1]["year"], 1984);
  assert_eq!(
    candidates[1]["poster_url"],
    "https://image.tmdb.org/t/p/w185/poster-2.jpg"
  );

  let (status, body) = post(
    app.clone(),
    "/api/discs/discs/identity",
    serde_json::json!({ "path": "GHOSTBUSTERS__uuid3", "tmdb_kind": "movie", "tmdb_id": 2 }),
  )
  .await;
  assert_eq!(status, StatusCode::OK, "{body}");
  let committed: serde_json::Value = serde_json::from_str(&body).unwrap();
  assert_eq!(committed["path"], "Ghostbusters (1984)__uuid3");
  assert_eq!(committed["previous_path"], "GHOSTBUSTERS__uuid3");
  assert_eq!(committed["title"], "Ghostbusters");
  assert_eq!(committed["year"], 1984);
  assert_eq!(committed["renamed"], true);

  let json = get_json(app.clone(), "/api/discs/unresolved").await;
  assert!(json["discs"].as_array().unwrap().is_empty());
  let disc = harness.disc("Ghostbusters (1984)__uuid3").await.unwrap();
  assert_eq!(disc.identity_state, IdentityState::Resolved);
  assert_eq!(disc.identity_source, Some(IdentitySource::Attended));
  assert!(disc.candidates().is_empty());
  let browse = get_json(app, "/api/browse?library=discs").await;
  assert_eq!(browse["entries"][0]["display_title"], "Ghostbusters");
  assert_eq!(browse["entries"][0]["year"], 1984);
  assert_eq!(browse["entries"][0]["identity_state"], "resolved");
}

#[tokio::test]
async fn identify_endpoint_searches_without_committing() {
  let tmdb = FakeTmdb::new().with_results(
    "custom words",
    vec![candidate(TmdbKind::Tv, 9, "Custom Show", Some(2020), 1.0)],
  );
  let harness = Harness::new(tmdb).await;
  write_disc_dir(harness.root.path(), "LOGICAL_VOLUME_ID", "uuid4", STRONG);
  harness.scan().await;
  assert!(harness.step().await);
  let before = harness.disc("LOGICAL_VOLUME_ID__uuid4").await.unwrap();
  assert_eq!(
    before.identity_state,
    IdentityState::NeedsConfirmation,
    "no usable hint"
  );
  assert!(before.candidates().is_empty());

  let app = harness.router(true).await;
  let (status, body) = post(
    app.clone(),
    "/api/discs/discs/identify",
    serde_json::json!({ "path": "LOGICAL_VOLUME_ID__uuid4", "query": "Custom Words", "year": null }),
  )
  .await;
  assert_eq!(status, StatusCode::OK, "{body}");
  let json: serde_json::Value = serde_json::from_str(&body).unwrap();
  assert_eq!(json["candidates"][0]["tmdb_id"], 9);
  assert_eq!(json["candidates"][0]["tmdb_kind"], "tv");
  let after = harness.disc("LOGICAL_VOLUME_ID__uuid4").await.unwrap();
  assert_eq!(after, before, "a search commits nothing");

  let (status, _) = post(
    app.clone(),
    "/api/discs/discs/identify",
    serde_json::json!({ "path": "LOGICAL_VOLUME_ID__uuid4", "query": "  " }),
  )
  .await;
  assert_eq!(status, StatusCode::BAD_REQUEST, "an empty query with no hint");
  let (status, _) = post(
    app,
    "/api/discs/discs/identify",
    serde_json::json!({ "path": "nope__x", "query": "q" }),
  )
  .await;
  assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn transient_failures_defer_and_retry_after_the_backoff() {
  let harness = Harness::new(matrix_tmdb()).await;
  harness.tmdb.fail(Some(FakeTmdbFailure::Transient));
  write_disc_dir(harness.root.path(), "THE_MATRIX", "uuid1", STRONG);
  harness.scan().await;

  assert!(harness.step().await, "an attempt was made");
  let disc = harness.disc("THE_MATRIX__uuid1").await.unwrap();
  assert_eq!(disc.identity_state, IdentityState::Unresolved);
  assert!(disc.identity_error.as_deref().unwrap().contains("503"));
  assert!(disc.identity_attempted_at.is_some());
  assert!(!harness.step().await, "still in backoff");

  harness.tmdb.fail(None);
  assert!(
    harness.step_with(Duration::ZERO).await,
    "retried once the backoff lapses"
  );
  assert_eq!(
    harness
      .disc("The Matrix (1999)__uuid1")
      .await
      .unwrap()
      .identity_state,
    IdentityState::Resolved
  );
}

#[tokio::test]
async fn a_rejected_key_fails_definitively() {
  let harness = Harness::new(matrix_tmdb()).await;
  harness.tmdb.fail(Some(FakeTmdbFailure::Unauthorized));
  write_disc_dir(harness.root.path(), "THE_MATRIX", "uuid1", STRONG);
  harness.scan().await;
  assert!(harness.step().await);
  let disc = harness.disc("THE_MATRIX__uuid1").await.unwrap();
  assert_eq!(disc.identity_state, IdentityState::Failed);
  assert!(disc
    .identity_error
    .as_deref()
    .unwrap()
    .contains("rejected the API key"));
  assert!(
    !harness.step_with(Duration::ZERO).await,
    "failed discs are not retried"
  );
}

#[tokio::test]
async fn a_rename_collision_takes_a_numeric_suffix() {
  let harness = Harness::new(matrix_tmdb()).await;
  std::fs::create_dir(harness.path("The Matrix (1999)__uuid1")).unwrap();
  write_disc_dir(harness.root.path(), "THE_MATRIX", "uuid1", STRONG);
  harness.scan().await;
  assert!(harness.step().await);
  assert!(harness
    .path("The Matrix (1999) (2)__uuid1/movie.nfo")
    .is_file());
  assert!(harness.disc("The Matrix (1999) (2)__uuid1").await.is_some());
}

#[tokio::test]
async fn an_operator_can_reidentify_a_resolved_disc_as_a_series() {
  let tmdb = matrix_tmdb().with_tv(tv_details(
    66732,
    "Voltron: Legendary Defender",
    2016,
  ));
  let harness = Harness::new(tmdb).await;
  write_disc_dir(harness.root.path(), "THE_MATRIX", "uuid1", STRONG);
  harness.scan().await;
  assert!(harness.step().await);

  let app = harness.router(true).await;
  let (status, body) = post(
    app,
    "/api/discs/discs/identity",
    serde_json::json!({ "path": "The Matrix (1999)__uuid1", "tmdb_kind": "tv", "tmdb_id": 66732 }),
  )
  .await;
  assert_eq!(status, StatusCode::OK, "{body}");
  let json: serde_json::Value = serde_json::from_str(&body).unwrap();
  assert_eq!(json["path"], "Voltron Legendary Defender (2016)__uuid1");
  let dir = harness.path("Voltron Legendary Defender (2016)__uuid1");
  assert!(dir.join("tvshow.nfo").is_file());
  assert!(!dir.join("movie.nfo").exists(), "the movie NFO is gone");
  let disc = harness
    .disc("Voltron Legendary Defender (2016)__uuid1")
    .await
    .unwrap();
  assert_eq!(disc.tmdb_kind, Some(TmdbKind::Tv));
  assert_eq!(disc.identity_source, Some(IdentitySource::Attended));
  let cached = harness
    .cache
    .lookup(disc.fingerprint.as_deref().unwrap())
    .await
    .unwrap()
    .unwrap();
  assert_eq!(cached.identity.kind, TmdbKind::Tv);
  assert_eq!(cached.source, IdentitySource::Attended);
}

#[tokio::test]
async fn slashes_stay_in_the_nfo_but_not_the_directory() {
  let tmdb = FakeTmdb::new()
    .with_results(
      "FACE OFF",
      vec![candidate(
        TmdbKind::Movie,
        754,
        "Face/Off",
        Some(1997),
        40.0,
      )],
    )
    .with_movie(movie_details(754, "Face/Off", 1997));
  let harness = Harness::new(tmdb).await;
  write_disc_dir(harness.root.path(), "FACE_OFF", "uuid5", STRONG);
  harness.scan().await;
  assert!(harness.step().await);
  let dir = harness.path("FaceOff (1997)__uuid5");
  assert!(dir.is_dir(), "path-hostile characters leave the directory name");
  let Nfo::Movie(movie) =
    nfo::parse_nfo(&std::fs::read_to_string(dir.join("movie.nfo")).unwrap())
      .unwrap()
  else {
    panic!("movie nfo");
  };
  assert_eq!(movie.title.as_deref(), Some("Face/Off"));
}

#[tokio::test]
async fn without_a_key_the_api_reports_disabled() {
  let harness = Harness::new(FakeTmdb::new()).await;
  write_disc_dir(harness.root.path(), "DISC", "uuid6", STRONG);
  harness.scan().await;
  let app = harness.router(false).await;
  let json = get_json(app.clone(), "/api/discs/unresolved").await;
  assert_eq!(json["identity_enabled"], false);
  assert_eq!(json["discs"][0]["path"], "DISC__uuid6");
  let (status, body) = post(
    app,
    "/api/discs/discs/identify",
    serde_json::json!({ "path": "DISC__uuid6", "query": "Disc" }),
  )
  .await;
  assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
}
