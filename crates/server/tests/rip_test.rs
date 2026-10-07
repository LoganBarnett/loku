// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! The rip pipeline end to end over the fake disc tools: directory layout,
//! sidecars, the re-insert policies, failure quarantine, and the API.

mod common;

use axum::{
  body::Body,
  http::{Request, StatusCode},
};
use common::{facts_udf, fake_info, library, FakeCall, FakeDiscTools};
use loku_server::index::store::{Index, RipRecord};
use loku_server::index::wakes::Wakes;
use loku_server::library::LibraryKind;
use loku_server::rip::config::{RipConfig, RipPolicy};
use loku_server::rip::makemkv::DiscTools;
use loku_server::rip::manifest::read_manifest_sync;
use loku_server::rip::naming::IdSource;
use loku_server::rip::worker::{self, RipState, RipTrigger, WorkerDeps};
use loku_server::rip::{self, RipDeps};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn rip_config(root: &Path, policy: RipPolicy) -> RipConfig {
  RipConfig {
    device: PathBuf::from("/dev/sr0"),
    library: library("discs", root, LibraryKind::Discs),
    policy,
    eject: true,
    poll_interval: Duration::from_secs(3600),
    replaced_retention: Duration::from_secs(14 * 24 * 60 * 60),
  }
}

/// A worker wired to an in-memory index and a temp library, driven one job
/// at a time through `run_job`.
struct Harness {
  root: tempfile::TempDir,
  index: Index,
  tools: Arc<FakeDiscTools>,
  deps: WorkerDeps,
}

impl Harness {
  async fn new(tools: FakeDiscTools) -> Self {
    let root = tempfile::tempdir().unwrap();
    let index = Index::open_in_memory().await.unwrap();
    let tools = Arc::new(tools);
    let deps = WorkerDeps {
      index: index.clone(),
      config: Arc::new(rip_config(root.path(), RipPolicy::Skip)),
      tools: tools.clone() as Arc<dyn DiscTools>,
      wakes: Wakes::new(),
    };
    Self {
      root,
      index,
      tools,
      deps,
    }
  }

  async fn run(
    &self,
    label: &str,
    uuid: Option<&str>,
    policy: RipPolicy,
  ) -> RipRecord {
    let facts = facts_udf(label, uuid);
    let job = self
      .index
      .insert_rip(&loku_server::index::store::NewRip {
        device: "/dev/sr0".to_string(),
        library: "discs".to_string(),
        policy,
        trigger: RipTrigger::Auto,
      })
      .await
      .unwrap();
    let (status, _rx) = tokio::sync::watch::channel(None);
    worker::run_job(
      &self.deps,
      worker::RipRequest {
        job_id: job.id,
        device: PathBuf::from("/dev/sr0"),
        facts,
        policy,
        trigger: RipTrigger::Auto,
      },
      &status,
    )
    .await;
    self.index.rip(job.id).await.unwrap().unwrap()
  }

  fn dir(&self, name: &str) -> PathBuf {
    self.root.path().join(name)
  }
}

#[tokio::test]
async fn first_rip_lands_in_a_per_disc_directory() {
  let harness = Harness::new(FakeDiscTools::new(
    &fake_info(2),
    &["title_t00.mkv", "title_t01.mkv"],
  ))
  .await;
  let job = harness
    .run("VOLTRON_S1_D1", Some("2016-05-12-10-15-33-00"), RipPolicy::Skip)
    .await;
  assert_eq!(job.state, RipState::Done, "{job:?}");
  assert_eq!(job.progress_permille, 1000);
  assert_eq!(
    job.dir_rel_path.as_deref(),
    Some("VOLTRON_S1_D1__2016-05-12-10-15-33-00")
  );
  assert_eq!(job.label.as_deref(), Some("VOLTRON_S1_D1"));
  assert!(job.finished_at.is_some());

  let dir = harness.dir("VOLTRON_S1_D1__2016-05-12-10-15-33-00");
  assert!(dir.join("title_t00.mkv").is_file());
  assert!(dir.join("title_t01.mkv").is_file());
  assert!(dir.join("disc-info.txt").is_file());
  let manifest = read_manifest_sync(&dir).unwrap().expect("manifest");
  assert_eq!(manifest.label, "VOLTRON_S1_D1");
  assert_eq!(manifest.id, "2016-05-12-10-15-33-00");
  assert_eq!(manifest.id_source, IdSource::Uuid);
  assert_eq!(manifest.title_count, 2);
  assert_eq!(manifest.titles.len(), 2);
  assert_eq!(manifest.disc_name.as_deref(), Some("Fake Disc Name"));
  assert_eq!(manifest.makemkv_version.as_deref(), Some("1.18.2"));
  assert!(manifest.ripped_at.is_some());
  assert!(manifest.fingerprint.is_some());
  assert!(
    !harness
      .root
      .path()
      .join(".rip/staging")
      .join(&manifest.dir_name)
      .exists(),
    "staging is gone after the swap-in"
  );

  let disc = harness
    .index
    .disc("discs", &manifest.dir_name)
    .await
    .unwrap()
    .expect("the finished rip is indexed immediately");
  assert_eq!(disc.label, "VOLTRON_S1_D1");
  assert_eq!(harness.index.video_count(None).await.unwrap(), 2);
  assert_eq!(harness.tools.count(FakeCall::Eject), 1);
  assert_eq!(harness.tools.count(FakeCall::Unlock), 0);
}

#[tokio::test]
async fn reinsert_under_skip_policy_touches_nothing() {
  let harness =
    Harness::new(FakeDiscTools::new(&fake_info(1), &["title_t00.mkv"])).await;
  harness.run("LBL", Some("uuid"), RipPolicy::Skip).await;
  let manifest_path = harness.dir("LBL__uuid/disc.json");
  let before = std::fs::metadata(&manifest_path)
    .unwrap()
    .modified()
    .unwrap();

  let job = harness.run("LBL", Some("uuid"), RipPolicy::Skip).await;
  assert_eq!(job.state, RipState::Skipped);
  assert_eq!(
    std::fs::metadata(&manifest_path)
      .unwrap()
      .modified()
      .unwrap(),
    before,
    "skip must not rewrite sidecars"
  );
  assert_eq!(harness.tools.count(FakeCall::Rip), 1);
  assert_eq!(harness.tools.count(FakeCall::Eject), 1, "skip never ejects");
}

#[tokio::test]
async fn rescan_refreshes_sidecars_and_keeps_the_rip_timestamp() {
  let harness =
    Harness::new(FakeDiscTools::new(&fake_info(1), &["title_t00.mkv"])).await;
  harness.run("LBL", Some("uuid"), RipPolicy::Skip).await;
  let dir = harness.dir("LBL__uuid");
  let first = read_manifest_sync(&dir).unwrap().unwrap();
  std::fs::write(dir.join("disc-info.txt"), b"stale").unwrap();

  let job = harness.run("LBL", Some("uuid"), RipPolicy::Rescan).await;
  assert_eq!(job.state, RipState::Done);
  let second = read_manifest_sync(&dir).unwrap().unwrap();
  assert_eq!(
    second.ripped_at, first.ripped_at,
    "the rip time survives a rescan"
  );
  assert!(second.scanned_at >= first.scanned_at);
  assert_ne!(std::fs::read(dir.join("disc-info.txt")).unwrap(), b"stale");
  assert_eq!(std::fs::read(dir.join("title_t00.mkv")).unwrap(), b"mkv bytes");
  assert_eq!(harness.tools.count(FakeCall::Rip), 1, "rescan does not rip");
  assert_eq!(harness.tools.count(FakeCall::Eject), 2);
}

#[tokio::test]
async fn rerip_moves_the_previous_tree_aside() {
  let harness =
    Harness::new(FakeDiscTools::new(&fake_info(1), &["title_t00.mkv"])).await;
  harness.run("LBL", Some("uuid"), RipPolicy::Skip).await;
  std::fs::write(harness.dir("LBL__uuid/title_t00.mkv"), b"old bytes").unwrap();

  let job = harness.run("LBL", Some("uuid"), RipPolicy::Rerip).await;
  assert_eq!(job.state, RipState::Done, "{job:?}");
  assert_eq!(
    std::fs::read(harness.dir("LBL__uuid/title_t00.mkv")).unwrap(),
    b"mkv bytes"
  );
  let replaced: Vec<_> = std::fs::read_dir(harness.dir(".rip/replaced"))
    .unwrap()
    .map(|entry| entry.unwrap().path())
    .collect();
  assert_eq!(replaced.len(), 1);
  assert!(replaced[0]
    .file_name()
    .unwrap()
    .to_string_lossy()
    .starts_with("LBL__uuid__"));
  assert_eq!(
    std::fs::read(replaced[0].join("title_t00.mkv")).unwrap(),
    b"old bytes"
  );
  assert!(harness
    .index
    .disc("discs", "LBL__uuid")
    .await
    .unwrap()
    .is_some());
  assert_eq!(harness.index.video_count(None).await.unwrap(), 1);
}

#[tokio::test]
async fn scan_failure_fails_the_job_and_unlocks_the_tray() {
  let mut tools = FakeDiscTools::new(&fake_info(1), &["title_t00.mkv"]);
  tools.fail_scan = true;
  let harness = Harness::new(tools).await;
  let job = harness.run("LBL", Some("uuid"), RipPolicy::Skip).await;
  assert_eq!(job.state, RipState::Failed);
  assert!(
    job
      .error
      .as_deref()
      .unwrap()
      .contains("scripted scan failure"),
    "{job:?}"
  );
  assert!(!harness.dir("LBL__uuid").exists());
  assert_eq!(harness.tools.count(FakeCall::Unlock), 1);
  assert_eq!(harness.tools.count(FakeCall::Eject), 0);
}

#[tokio::test]
async fn rip_failure_is_quarantined() {
  let mut tools = FakeDiscTools::new(&fake_info(1), &["title_t00.mkv"]);
  tools.fail_rip = true;
  let harness = Harness::new(tools).await;
  let job = harness.run("LBL", Some("uuid"), RipPolicy::Skip).await;
  assert_eq!(job.state, RipState::Failed);
  assert!(job
    .error
    .as_deref()
    .unwrap()
    .contains("scripted rip failure"));
  assert!(!harness.dir("LBL__uuid").exists());
  assert!(!harness.dir(".rip/staging/LBL__uuid").exists());
  let failed: Vec<_> = std::fs::read_dir(harness.dir(".rip/failed"))
    .unwrap()
    .map(|entry| entry.unwrap().path())
    .collect();
  assert_eq!(failed.len(), 1);
  assert!(
    failed[0].join("disc-info.txt").is_file(),
    "the scan dump is kept for inspection"
  );
  assert_eq!(harness.tools.count(FakeCall::Unlock), 1);
}

#[tokio::test]
async fn title_count_mismatch_fails_the_rip() {
  let harness =
    Harness::new(FakeDiscTools::new(&fake_info(2), &["title_t00.mkv"])).await;
  let job = harness.run("LBL", Some("uuid"), RipPolicy::Skip).await;
  assert_eq!(job.state, RipState::Failed);
  assert!(job.error.as_deref().unwrap().contains("promised 2"), "{job:?}");
  assert!(!harness.dir("LBL__uuid").exists());
}

#[tokio::test]
async fn discs_without_a_uuid_get_hash_or_timestamp_ids() {
  let harness =
    Harness::new(FakeDiscTools::new(&fake_info(1), &["title_t00.mkv"])).await;
  let job = harness.run("LBL", None, RipPolicy::Skip).await;
  assert_eq!(job.state, RipState::Done, "{job:?}");
  let dir_name = job.dir_rel_path.unwrap();
  let (label, id) = dir_name.rsplit_once("__").unwrap();
  assert_eq!(label, "LBL");
  assert_eq!(id.len(), 16, "info hash id: {id}");
  assert_eq!(
    read_manifest_sync(&harness.dir(&dir_name))
      .unwrap()
      .unwrap()
      .id_source,
    IdSource::InfoHash
  );

  // The same disc again hashes the same, so the policy applies.
  let again = harness.run("LBL", None, RipPolicy::Skip).await;
  assert_eq!(again.state, RipState::Skipped);

  let empty = Harness::new(FakeDiscTools::new(&fake_info(0), &[])).await;
  let job = empty.run("BLANK", None, RipPolicy::Skip).await;
  assert_eq!(job.state, RipState::Done, "{job:?}");
  let dir_name = job.dir_rel_path.unwrap();
  assert!(dir_name.starts_with("BLANK__ts"), "{dir_name}");
  assert_eq!(
    read_manifest_sync(&empty.dir(&dir_name))
      .unwrap()
      .unwrap()
      .id_source,
    IdSource::Timestamp
  );
}

#[tokio::test]
async fn stale_staging_is_cleared_before_a_rip() {
  let harness =
    Harness::new(FakeDiscTools::new(&fake_info(1), &["title_t00.mkv"])).await;
  let stale = harness.dir(".rip/staging/LBL__uuid");
  std::fs::create_dir_all(&stale).unwrap();
  std::fs::write(stale.join("junk.mkv"), b"junk").unwrap();

  let job = harness.run("LBL", Some("uuid"), RipPolicy::Skip).await;
  assert_eq!(job.state, RipState::Done, "{job:?}");
  assert!(!harness.dir("LBL__uuid/junk.mkv").exists());
  assert_eq!(harness.index.video_count(None).await.unwrap(), 1);
}

#[tokio::test]
async fn interrupted_jobs_are_closed_at_startup() {
  let index = Index::open_in_memory().await.unwrap();
  let job = index
    .insert_rip(&loku_server::index::store::NewRip {
      device: "/dev/sr0".to_string(),
      library: "discs".to_string(),
      policy: RipPolicy::Skip,
      trigger: RipTrigger::Auto,
    })
    .await
    .unwrap();
  index
    .set_rip_state(job.id, RipState::Ripping)
    .await
    .unwrap();
  assert_eq!(index.fail_interrupted_rips().await.unwrap(), 1);
  let closed = index.rip(job.id).await.unwrap().unwrap();
  assert_eq!(closed.state, RipState::Failed);
  assert!(closed.error.as_deref().unwrap().contains("interrupted"));
  assert!(closed.finished_at.is_some());
  assert_eq!(index.fail_interrupted_rips().await.unwrap(), 0);
  assert_eq!(index.recent_rips(10).await.unwrap().len(), 1);
}

// ── the rips API ────────────────────────────────────────────────────────────

use loku_server::frontend::Frontend;
use loku_server::web_base::{app_routes, AppState};
use rust_template_foundation::server::runner::{
  BaseServerState, Server, ServerRunConfig,
};
use tower::ServiceExt;

async fn router(
  index: &Index,
  rips: Option<rip::RipHandles>,
  root: &Path,
) -> axum::Router {
  let libraries = vec![library("discs", root, LibraryKind::Discs)];
  let run_config = ServerRunConfig {
    app_name: "loku".to_string(),
    listen_address: "127.0.0.1:0".parse().unwrap(),
    base_url: "http://localhost".to_string(),
    oidc: None,
  };
  let base = BaseServerState::init(&run_config).await.unwrap();
  let state_index = index.clone();
  let state_libraries = Arc::new(libraries.clone());
  Server::new(base, run_config)
    .with_state(|base| AppState {
      base,
      libraries: state_libraries,
      index: state_index,
      derivation_active: tokio::sync::watch::channel(None).1,
      rips,
      identity: None,
    })
    .merge(app_routes(&libraries))
    .spa::<Frontend>()
    .into_test_router()
}

async fn get_json(app: axum::Router, uri: &str) -> serde_json::Value {
  let response = app
    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  let body = axum::body::to_bytes(response.into_body(), usize::MAX)
    .await
    .unwrap();
  serde_json::from_slice(&body).unwrap()
}

async fn post(
  app: axum::Router,
  body: serde_json::Value,
) -> (StatusCode, String) {
  let response = app
    .oneshot(
      Request::builder()
        .method("POST")
        .uri("/api/rips")
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

/// Poll the status endpoint until the job with `id` reaches a terminal
/// state; "no active job" alone is not enough, since a queued job sits
/// briefly between the previous job's end and its own pickup.
async fn wait_for_job(
  app: &axum::Router,
  id: &serde_json::Value,
) -> serde_json::Value {
  let deadline = std::time::Instant::now() + Duration::from_secs(15);
  loop {
    let json = get_json(app.clone(), "/api/rips").await;
    let terminal = json["recent"].as_array().unwrap().iter().any(|job| {
      &job["id"] == id
        && ["done", "skipped", "failed"]
          .contains(&job["state"].as_str().unwrap_or_default())
    });
    if terminal && json["drives"][0].get("active").is_none() {
      return json;
    }
    assert!(std::time::Instant::now() < deadline, "rip never finished: {json}");
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
}

#[tokio::test]
async fn rips_api_reports_disabled_without_configuration() {
  let root = tempfile::tempdir().unwrap();
  let index = Index::open_in_memory().await.unwrap();
  let app = router(&index, None, root.path()).await;
  let json = get_json(app.clone(), "/api/rips").await;
  assert_eq!(json["enabled"], false);
  assert_eq!(json["drives"].as_array().unwrap().len(), 0);
  let (status, body) =
    post(app, serde_json::json!({ "policy": "rerip" })).await;
  assert_eq!(status, StatusCode::CONFLICT);
  assert!(body.contains("not configured"), "{body}");
}

#[tokio::test]
async fn rips_api_drives_the_whole_pipeline() {
  let root = tempfile::tempdir().unwrap();
  let index = Index::open_in_memory().await.unwrap();
  let gate = Arc::new(tokio::sync::Notify::new());
  let mut tools = FakeDiscTools::new(&fake_info(1), &["title_t00.mkv"]);
  tools.rip_gate = Some(gate.clone());
  tools.insert(facts_udf("VOLTRON", Some("uuid-1")));
  let tools = Arc::new(tools);
  let handles = rip::spawn(RipDeps {
    index: index.clone(),
    config: rip_config(root.path(), RipPolicy::Skip),
    tools: tools.clone() as Arc<dyn DiscTools>,
    wakes: Wakes::new(),
  });
  let app = router(&index, Some(handles), root.path()).await;

  // The poller saw the disc at startup and queued an automatic rip, which
  // the fake is holding open: the drive reports it busy.
  let deadline = std::time::Instant::now() + Duration::from_secs(15);
  let busy = loop {
    let json = get_json(app.clone(), "/api/rips").await;
    if json["drives"][0]["active"]["state"] == "ripping" {
      break json;
    }
    assert!(std::time::Instant::now() < deadline, "rip never started: {json}");
    tokio::time::sleep(Duration::from_millis(50)).await;
  };
  assert_eq!(busy["enabled"], true);
  assert_eq!(busy["drives"][0]["device"], "/dev/sr0");
  assert_eq!(busy["drives"][0]["library"], "discs");
  assert_eq!(busy["drives"][0]["policy"], "skip");
  assert_eq!(busy["drives"][0]["media"]["label"], "VOLTRON");
  assert_eq!(busy["drives"][0]["media"]["disc_id"], "uuid-1");
  assert_eq!(busy["drives"][0]["media"]["kind"], "dvd");
  assert!(busy["drives"][0]["media"].get("existing_path").is_none());
  assert_eq!(busy["drives"][0]["active"]["path"], "VOLTRON__uuid-1");
  assert_eq!(busy["drives"][0]["active"]["progress_permille"], 500);
  assert_eq!(busy["recent"][0]["trigger"], "auto");
  assert_eq!(
    busy["recent"][0]["state"], "ripping",
    "live state overlays the row"
  );

  let (status, body) =
    post(app.clone(), serde_json::json!({ "policy": "rerip" })).await;
  assert_eq!(status, StatusCode::CONFLICT, "{body}");
  assert!(body.contains("already running"), "{body}");

  let first_id = busy["recent"][0]["id"].clone();
  gate.notify_one();
  let idle = wait_for_job(&app, &first_id).await;
  assert_eq!(idle["recent"][0]["state"], "done");
  assert_eq!(idle["recent"][0]["progress_permille"], 1000);
  assert_eq!(idle["drives"][0]["media"]["existing_path"], "VOLTRON__uuid-1");

  let (status, body) = post(
    app.clone(),
    serde_json::json!({ "device": "/dev/sr0", "policy": "rerip" }),
  )
  .await;
  assert_eq!(status, StatusCode::ACCEPTED, "{body}");
  let job: serde_json::Value = serde_json::from_str(&body).unwrap();
  assert_eq!(job["trigger"], "manual");
  assert_eq!(job["policy"], "rerip");
  assert_eq!(job["state"], "detected");
  gate.notify_one();
  let idle = wait_for_job(&app, &job["id"]).await;
  assert_eq!(idle["recent"].as_array().unwrap().len(), 2);
  assert_eq!(idle["recent"][0]["id"], job["id"]);
  assert_eq!(idle["recent"][0]["state"], "done");
  assert!(
    root
      .path()
      .join(".rip/replaced")
      .read_dir()
      .unwrap()
      .count()
      == 1
  );

  let (status, _) = post(
    app.clone(),
    serde_json::json!({ "device": "/dev/nope", "policy": "skip" }),
  )
  .await;
  assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rips_api_refuses_a_rip_without_media() {
  let root = tempfile::tempdir().unwrap();
  let index = Index::open_in_memory().await.unwrap();
  let tools = Arc::new(FakeDiscTools::new(&fake_info(1), &["title_t00.mkv"]));
  let handles = rip::spawn(RipDeps {
    index: index.clone(),
    config: rip_config(root.path(), RipPolicy::Skip),
    tools: tools.clone() as Arc<dyn DiscTools>,
    wakes: Wakes::new(),
  });
  let app = router(&index, Some(handles), root.path()).await;
  let (status, body) =
    post(app.clone(), serde_json::json!({ "policy": "rescan" })).await;
  assert_eq!(status, StatusCode::CONFLICT, "{body}");
  assert!(body.contains("No media"), "{body}");
  let json = get_json(app, "/api/rips").await;
  assert!(json["drives"][0].get("media").is_none());
}
