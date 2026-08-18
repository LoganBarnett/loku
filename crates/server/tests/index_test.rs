// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! Direct exercises of the media index: scanning, reconciliation, title
//! precedence, and the drop-and-rescan schema policy.

mod common;

use common::{library, FakeFfmpeg, FakeOutcome};
use loku_server::index::scan;
use loku_server::index::store::{fts_query, Index, TitleSource};
use loku_server::library::LibraryKind;
use std::fs;

async fn search_one(
  index: &Index,
  query: &str,
) -> Vec<loku_server::index::store::VideoRecord> {
  index
    .search(fts_query(query).unwrap(), None, 50, 0)
    .await
    .unwrap()
    .items
}

#[tokio::test]
async fn scan_indexes_videos_across_libraries() {
  let downloads = tempfile::tempdir().unwrap();
  let discs = tempfile::tempdir().unwrap();
  fs::write(downloads.path().join("clip.mp4"), b"x").unwrap();
  fs::write(
    downloads.path().join("clip.info.json"),
    r#"{"title":"Matrix Lobby Scene"}"#,
  )
  .unwrap();
  fs::write(discs.path().join("The_Matrix_title_t00.mkv"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![
    library("downloads", downloads.path(), LibraryKind::Downloads),
    library("discs", discs.path(), LibraryKind::Discs),
  ];
  let stats = scan::scan_all(&index, &libraries).await.unwrap();
  assert_eq!(stats.videos, 2);

  let hits = search_one(&index, "matrix").await;
  assert_eq!(hits.len(), 2, "both libraries should match");
  assert!(hits.iter().any(|r| r.library == "downloads"));
  assert!(hits.iter().any(|r| r.library == "discs"));
}

#[tokio::test]
async fn rip_names_get_cleaned_titles_in_discs_libraries_only() {
  let downloads = tempfile::tempdir().unwrap();
  let discs = tempfile::tempdir().unwrap();
  // The same filename in both kinds: only the discs library should invent a
  // rip-derived title.
  fs::write(downloads.path().join("Some_Movie_title_t00.mkv"), b"x").unwrap();
  fs::write(discs.path().join("Some_Movie_title_t00.mkv"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![
    library("downloads", downloads.path(), LibraryKind::Downloads),
    library("discs", discs.path(), LibraryKind::Discs),
  ];
  scan::scan_all(&index, &libraries).await.unwrap();

  let hits = search_one(&index, "Some Movie").await;
  let disc_hit = hits.iter().find(|r| r.library == "discs").unwrap();
  assert_eq!(disc_hit.title.as_deref(), Some("Some Movie"));
  assert_eq!(disc_hit.title_source, Some(TitleSource::RipName));
  assert_eq!(disc_hit.disc_set.as_deref(), Some("Some_Movie"));
  assert_eq!(disc_hit.disc_title_index, Some(0));

  let download_hit = hits.iter().find(|r| r.library == "downloads").unwrap();
  assert_eq!(download_hit.title, None);
  assert_eq!(download_hit.disc_set, None);
}

#[tokio::test]
async fn nfo_title_beats_info_json_title() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("movie.mkv"), b"x").unwrap();
  fs::write(
    dir.path().join("movie.info.json"),
    r#"{"title":"From Info Json"}"#,
  )
  .unwrap();
  fs::write(
    dir.path().join("movie.nfo"),
    "<movie><title>From The Nfo</title><year>1999</year>\
     <genre>Action</genre></movie>",
  )
  .unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![library("discs", dir.path(), LibraryKind::Discs)];
  scan::scan_all(&index, &libraries).await.unwrap();

  let hits = search_one(&index, "nfo").await;
  assert_eq!(hits.len(), 1);
  assert_eq!(hits[0].title.as_deref(), Some("From The Nfo"));
  assert_eq!(hits[0].title_source, Some(TitleSource::Nfo));
  assert_eq!(hits[0].year, Some(1999));
  assert_eq!(hits[0].genres, vec!["Action"]);
}

#[tokio::test]
async fn folder_nfo_applies_only_to_a_lone_video() {
  let lone = tempfile::tempdir().unwrap();
  fs::write(lone.path().join("anything.mkv"), b"x").unwrap();
  fs::write(
    lone.path().join("movie.nfo"),
    "<movie><title>Folder Level</title></movie>",
  )
  .unwrap();

  let crowded = tempfile::tempdir().unwrap();
  fs::write(crowded.path().join("first.mkv"), b"x").unwrap();
  fs::write(crowded.path().join("second.mkv"), b"x").unwrap();
  fs::write(
    crowded.path().join("movie.nfo"),
    "<movie><title>Folder Level</title></movie>",
  )
  .unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![
    library("lone", lone.path(), LibraryKind::Discs),
    library("crowded", crowded.path(), LibraryKind::Discs),
  ];
  scan::scan_all(&index, &libraries).await.unwrap();

  let hits = search_one(&index, "Folder Level").await;
  assert_eq!(
    hits.len(),
    1,
    "only the lone video may claim the folder-level movie.nfo"
  );
  assert_eq!(hits[0].library, "lone");
}

#[tokio::test]
async fn malformed_sidecars_degrade_to_untitled_entries() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("broken.mp4"), b"x").unwrap();
  fs::write(dir.path().join("broken.info.json"), b"not json").unwrap();
  fs::write(dir.path().join("broken.nfo"), b"<movie><title>Un").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries =
    vec![library("downloads", dir.path(), LibraryKind::Downloads)];
  scan::scan_all(&index, &libraries).await.unwrap();

  // The file name is still searchable even with both sidecars broken.
  let hits = search_one(&index, "broken").await;
  assert_eq!(hits.len(), 1);
  assert_eq!(hits[0].title, None);
}

#[tokio::test]
async fn rescan_drops_vanished_files() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("keep.mp4"), b"x").unwrap();
  fs::write(dir.path().join("gone.mp4"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries =
    vec![library("downloads", dir.path(), LibraryKind::Downloads)];
  scan::scan_all(&index, &libraries).await.unwrap();
  assert_eq!(index.video_count(None).await.unwrap(), 2);

  fs::remove_file(dir.path().join("gone.mp4")).unwrap();
  // The removal lands during the directory reconcile (the directory still
  // exists, so the generation sweep has nothing left to do).
  scan::scan_all(&index, &libraries).await.unwrap();
  assert_eq!(index.video_count(None).await.unwrap(), 1);
  assert!(search_one(&index, "gone").await.is_empty());
}

#[tokio::test]
async fn rescan_drops_vanished_directories() {
  let dir = tempfile::tempdir().unwrap();
  fs::create_dir(dir.path().join("season1")).unwrap();
  fs::write(dir.path().join("season1/ep.mp4"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries =
    vec![library("downloads", dir.path(), LibraryKind::Downloads)];
  scan::scan_all(&index, &libraries).await.unwrap();
  assert_eq!(index.video_count(None).await.unwrap(), 1);

  fs::remove_dir_all(dir.path().join("season1")).unwrap();
  scan::scan_all(&index, &libraries).await.unwrap();
  assert_eq!(index.video_count(None).await.unwrap(), 0);
}

#[tokio::test]
async fn compat_copies_index_as_sidecars_not_entries() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.webm"), b"x").unwrap();
  fs::write(dir.path().join("clip.compat.mp4"), b"x").unwrap();
  fs::write(dir.path().join("clip.jpg"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries =
    vec![library("downloads", dir.path(), LibraryKind::Downloads)];
  scan::scan_all(&index, &libraries).await.unwrap();

  assert_eq!(index.video_count(None).await.unwrap(), 1);
  let hits = search_one(&index, "clip").await;
  assert_eq!(hits[0].compat_rel_path.as_deref(), Some("clip.compat.mp4"));
  assert_eq!(hits[0].thumb_rel_path.as_deref(), Some("clip.jpg"));
}

/// Poll until the index reaches the expected video count or the deadline
/// passes.  Filesystem-event latency varies by platform, so watcher tests
/// assert eventual convergence rather than exact timing.
async fn wait_for_count(index: &Index, expected: u64) {
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
  loop {
    if index.video_count(None).await.unwrap() == expected {
      return;
    }
    assert!(
      std::time::Instant::now() < deadline,
      "index did not reach {expected} videos in time"
    );
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
  }
}

#[tokio::test]
async fn watcher_folds_filesystem_changes_into_the_index() {
  use loku_server::index::watch;
  use std::sync::atomic::{AtomicI64, Ordering};
  use std::sync::Arc;

  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("first.mp4"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries =
    vec![library("downloads", dir.path(), LibraryKind::Downloads)];
  let last_scan = Arc::new(AtomicI64::new(0));
  watch::spawn_maintenance(
    index.clone(),
    libraries,
    last_scan.clone(),
    None,
    Arc::new(tokio::sync::Notify::new()),
  )
  .unwrap();

  // The maintenance task runs the initial scan itself; wait for it.
  wait_for_count(&index, 1).await;
  assert!(last_scan.load(Ordering::Relaxed) > 0);

  // A new video (with sidecar metadata) appears without any rescan call.
  eprintln!("stage: create second.mkv");
  fs::write(dir.path().join("second.mkv"), b"x").unwrap();
  fs::write(dir.path().join("second.info.json"), r#"{"title":"Second"}"#)
    .unwrap();
  wait_for_count(&index, 2).await;

  // An in-progress download must not become an entry.
  eprintln!("stage: part file");
  fs::write(dir.path().join("partial.mp4.part"), b"x").unwrap();
  tokio::time::sleep(std::time::Duration::from_secs(3)).await;
  assert_eq!(index.video_count(None).await.unwrap(), 2);

  // Deleting a file drops its row.
  eprintln!("stage: delete second.mkv");
  fs::remove_file(dir.path().join("second.mkv")).unwrap();
  wait_for_count(&index, 1).await;

  // Removing a whole subtree drops every row beneath it.
  eprintln!("stage: create show/season1 tree");
  fs::create_dir_all(dir.path().join("show/season1")).unwrap();
  fs::write(dir.path().join("show/season1/ep.mp4"), b"x").unwrap();
  wait_for_count(&index, 2).await;
  eprintln!("stage: remove show tree");
  fs::remove_dir_all(dir.path().join("show")).unwrap();
  wait_for_count(&index, 1).await;
}

// ── probe pass ──────────────────────────────────────────────────────────────

use loku_server::media::ffmpeg::ProbeResult;
use loku_server::media::probe::probe_pending;

fn h264_interlaced() -> ProbeResult {
  ProbeResult {
    vcodec: Some("h264".to_string()),
    acodec: Some("ac3".to_string()),
    vprofile: Some("High".to_string()),
    field_order: Some("tt".to_string()),
    duration_secs: Some(5400.0),
    width: Some(1920),
    height: Some(1080),
  }
}

#[tokio::test]
async fn probe_fills_codecs_for_rips_and_caches() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("The_Matrix_title_t00.mkv"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![library("discs", dir.path(), LibraryKind::Discs)];
  scan::scan_all(&index, &libraries).await.unwrap();

  let fake = FakeFfmpeg::new(vec![(
    "The_Matrix_title_t00.mkv",
    FakeOutcome::Give(h264_interlaced()),
  )]);
  assert_eq!(probe_pending(&index, &libraries, &fake).await, 1);
  assert_eq!(fake.probe_calls(), 1);

  let hit = &search_one(&index, "matrix").await[0];
  assert_eq!(hit.vcodec.as_deref(), Some("h264"));
  assert_eq!(hit.acodec.as_deref(), Some("ac3"));
  assert_eq!(hit.vprofile.as_deref(), Some("High"));
  assert_eq!(hit.field_order.as_deref(), Some("tt"));
  assert_eq!(hit.duration_secs, Some(5400.0));

  // Nothing left to probe; the fake is not consulted again.
  assert_eq!(probe_pending(&index, &libraries, &fake).await, 0);
  assert_eq!(fake.probe_calls(), 1);
}

#[tokio::test]
async fn rescan_of_unchanged_file_does_not_reprobe() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("Movie_title_t00.mkv"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![library("discs", dir.path(), LibraryKind::Discs)];
  scan::scan_all(&index, &libraries).await.unwrap();

  let fake = FakeFfmpeg::new(vec![(
    "Movie_title_t00.mkv",
    FakeOutcome::Give(h264_interlaced()),
  )]);
  probe_pending(&index, &libraries, &fake).await;
  assert_eq!(fake.probe_calls(), 1);

  // A rescan rebuilds the row from sidecars, but the unchanged (size,
  // mtime) preserves the probed facts, so nothing needs probing.
  scan::scan_all(&index, &libraries).await.unwrap();
  assert_eq!(probe_pending(&index, &libraries, &fake).await, 0);
  assert_eq!(fake.probe_calls(), 1);
  let hit = &search_one(&index, "movie").await[0];
  assert_eq!(hit.vcodec.as_deref(), Some("h264"));
}

#[tokio::test]
async fn changed_file_is_reprobed_via_cache_miss() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("Movie_title_t00.mkv");
  fs::write(&path, b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![library("discs", dir.path(), LibraryKind::Discs)];
  scan::scan_all(&index, &libraries).await.unwrap();

  let fake = FakeFfmpeg::new(vec![(
    "Movie_title_t00.mkv",
    FakeOutcome::Give(h264_interlaced()),
  )]);
  probe_pending(&index, &libraries, &fake).await;
  assert_eq!(fake.probe_calls(), 1);

  // Different content (size change) resets the codec facts on rescan and
  // misses the probe cache, so a real probe runs again.
  fs::write(&path, b"different content").unwrap();
  scan::scan_all(&index, &libraries).await.unwrap();
  assert_eq!(probe_pending(&index, &libraries, &fake).await, 1);
  assert_eq!(fake.probe_calls(), 2);
}

#[tokio::test]
async fn definitive_probe_failure_is_not_retried() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("broken.mkv"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![library("discs", dir.path(), LibraryKind::Discs)];
  scan::scan_all(&index, &libraries).await.unwrap();

  let fake =
    FakeFfmpeg::new(vec![("broken.mkv", FakeOutcome::FailDefinitively)]);
  assert_eq!(probe_pending(&index, &libraries, &fake).await, 0);
  assert_eq!(fake.probe_calls(), 1);

  // The empty recorded result marks it probed: served compat-or-bust, not
  // re-probed forever.
  assert_eq!(probe_pending(&index, &libraries, &fake).await, 0);
  assert_eq!(fake.probe_calls(), 1);
  let hit = &search_one(&index, "broken").await[0];
  assert_eq!(hit.vcodec, None);
}

#[tokio::test]
async fn transient_probe_failure_retries_next_pass() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("flaky.mkv"), b"x").unwrap();

  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![library("discs", dir.path(), LibraryKind::Discs)];
  scan::scan_all(&index, &libraries).await.unwrap();

  let fake = FakeFfmpeg::new(vec![("flaky.mkv", FakeOutcome::FailTransiently)]);
  assert_eq!(probe_pending(&index, &libraries, &fake).await, 0);
  assert_eq!(fake.probe_calls(), 1);

  // Still unprobed, so the next pass tries again.
  assert_eq!(probe_pending(&index, &libraries, &fake).await, 0);
  assert_eq!(fake.probe_calls(), 2);
}

#[tokio::test]
async fn file_backed_index_persists_across_reopen() {
  let library_dir = tempfile::tempdir().unwrap();
  let state_dir = tempfile::tempdir().unwrap();
  fs::write(library_dir.path().join("clip.mp4"), b"x").unwrap();
  let libraries = vec![library(
    "downloads",
    library_dir.path(),
    LibraryKind::Downloads,
  )];

  {
    let index = Index::open(state_dir.path()).await.unwrap();
    scan::scan_all(&index, &libraries).await.unwrap();
    assert_eq!(index.video_count(None).await.unwrap(), 1);
  }

  let reopened = Index::open(state_dir.path()).await.unwrap();
  assert_eq!(
    reopened.video_count(None).await.unwrap(),
    1,
    "rows must survive a reopen"
  );
}

#[tokio::test]
async fn schema_version_mismatch_rebuilds_the_index() {
  let library_dir = tempfile::tempdir().unwrap();
  let state_dir = tempfile::tempdir().unwrap();
  fs::write(library_dir.path().join("clip.mp4"), b"x").unwrap();
  let libraries = vec![library(
    "downloads",
    library_dir.path(),
    LibraryKind::Downloads,
  )];

  {
    let index = Index::open(state_dir.path()).await.unwrap();
    scan::scan_all(&index, &libraries).await.unwrap();
  }

  // Simulate an old database by rewriting user_version out from under it.
  {
    let conn =
      rusqlite::Connection::open(state_dir.path().join("index.db")).unwrap();
    conn.execute_batch("PRAGMA user_version = 999;").unwrap();
  }

  let reopened = Index::open(state_dir.path()).await.unwrap();
  assert_eq!(
    reopened.video_count(None).await.unwrap(),
    0,
    "a version mismatch must drop and rebuild the schema"
  );
}
