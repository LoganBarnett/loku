// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! End-to-end exercises of the derivation worker against a scripted ffmpeg
//! fake: plan selection, `.part` staging, failure bookkeeping, and
//! thumbnail-only jobs.

mod common;

use common::{library, FakeFfmpeg, FakeOutcome};
use loku_server::index::scan;
use loku_server::index::store::Index;
use loku_server::library::{Library, LibraryKind};
use loku_server::media::compat::{AudioAction, VideoAction};
use loku_server::media::ffmpeg::ProbeResult;
use loku_server::media::probe::probe_pending;
use loku_server::media::worker::{process_next, ActiveDerivation};
use std::fs;
use std::path::Path;
use tokio::sync::watch;

fn dvd_mpeg2() -> ProbeResult {
  ProbeResult {
    vcodec: Some("mpeg2video".to_string()),
    acodec: Some("ac3".to_string()),
    vprofile: None,
    field_order: Some("tt".to_string()),
    duration_secs: Some(3600.0),
    width: Some(720),
    height: Some(576),
  }
}

fn bluray_h264() -> ProbeResult {
  ProbeResult {
    vcodec: Some("h264".to_string()),
    acodec: Some("ac3".to_string()),
    vprofile: Some("High".to_string()),
    field_order: Some("progressive".to_string()),
    duration_secs: Some(7200.0),
    width: Some(1920),
    height: Some(1080),
  }
}

/// Scan and probe a library so the worker has fully described candidates.
async fn indexed(
  dir: &Path,
  kind: LibraryKind,
  fake: &FakeFfmpeg,
) -> (Index, Vec<Library>) {
  let index = Index::open_in_memory().await.unwrap();
  let libraries = vec![library("media", dir, kind)];
  scan::scan_all(&index, &libraries).await.unwrap();
  probe_pending(&index, &libraries, fake).await;
  (index, libraries)
}

fn status_channel() -> watch::Sender<Option<ActiveDerivation>> {
  watch::channel(None).0
}

#[tokio::test]
async fn dvd_rip_gets_transcoded_compat_and_thumbnail() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("Movie_title_t00.mkv"), b"master").unwrap();
  let fake = FakeFfmpeg::new(vec![(
    "Movie_title_t00.mkv",
    FakeOutcome::Give(dvd_mpeg2()),
  )]);
  let (index, libraries) = indexed(dir.path(), LibraryKind::Discs, &fake).await;
  let status = status_channel();

  assert!(process_next(&index, &libraries, &fake, &status).await);

  assert_eq!(
    fs::read(dir.path().join("Movie_title_t00.compat.mp4")).unwrap(),
    b"compat bytes"
  );
  assert_eq!(
    fs::read(dir.path().join("Movie_title_t00.jpg")).unwrap(),
    b"jpeg bytes"
  );
  // Scoped so the guard provably drops before the next await (clippy's
  // await_holding_lock does not recognize an explicit drop call).
  {
    let plans = fake.derive_plans.lock().unwrap();
    assert_eq!(plans.len(), 1);
    assert_eq!(
      plans[0].1.video,
      VideoAction::Transcode { deinterlace: true },
      "interlaced DVD MPEG-2 must transcode with deinterlace"
    );
    assert_eq!(plans[0].1.audio, AudioAction::Transcode);
  }

  // The row is updated and nothing remains to do.
  assert!(!process_next(&index, &libraries, &fake, &status).await);
  let hits = index
    .search("\"movie\"*".to_string(), None, 10, 0)
    .await
    .unwrap()
    .items;
  assert_eq!(
    hits[0].compat_rel_path.as_deref(),
    Some("Movie_title_t00.compat.mp4")
  );
  assert_eq!(hits[0].thumb_rel_path.as_deref(), Some("Movie_title_t00.jpg"));
}

#[tokio::test]
async fn bluray_h264_is_stream_copied() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("Film_title_t00.mkv"), b"master").unwrap();
  let fake = FakeFfmpeg::new(vec![(
    "Film_title_t00.mkv",
    FakeOutcome::Give(bluray_h264()),
  )]);
  let (index, libraries) = indexed(dir.path(), LibraryKind::Discs, &fake).await;

  assert!(process_next(&index, &libraries, &fake, &status_channel()).await);

  let plans = fake.derive_plans.lock().unwrap();
  assert_eq!(
    plans[0].1.video,
    VideoAction::Copy,
    "browser-safe H.264 must be remuxed, not re-encoded"
  );
  assert_eq!(plans[0].1.audio, AudioAction::Transcode);
}

#[tokio::test]
async fn universal_mp4_gets_thumbnail_only() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("clip.mp4"), b"master").unwrap();
  fs::write(
    dir.path().join("clip.info.json"),
    r#"{"vcodec":"avc1.640028","acodec":"mp4a.40.2"}"#,
  )
  .unwrap();
  let fake = FakeFfmpeg::new(vec![]);
  let (index, libraries) =
    indexed(dir.path(), LibraryKind::Downloads, &fake).await;
  let status = status_channel();

  // The thumbnail is produced; no compat copy is.
  assert!(process_next(&index, &libraries, &fake, &status).await);
  assert!(dir.path().join("clip.jpg").exists());
  assert!(!dir.path().join("clip.compat.mp4").exists());
  assert!(fake.derive_plans.lock().unwrap().is_empty());

  assert!(!process_next(&index, &libraries, &fake, &status).await);
}

#[tokio::test]
async fn existing_thumbnail_and_compat_mean_no_work() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("Movie_title_t00.mkv"), b"master").unwrap();
  fs::write(dir.path().join("Movie_title_t00.compat.mp4"), b"c").unwrap();
  fs::write(dir.path().join("Movie_title_t00.jpg"), b"t").unwrap();
  let fake = FakeFfmpeg::new(vec![(
    "Movie_title_t00.mkv",
    FakeOutcome::Give(dvd_mpeg2()),
  )]);
  let (index, libraries) = indexed(dir.path(), LibraryKind::Discs, &fake).await;

  assert!(!process_next(&index, &libraries, &fake, &status_channel()).await);
}

#[tokio::test]
async fn failed_derivation_recorded_and_not_retried_until_change() {
  let dir = tempfile::tempdir().unwrap();
  let path = dir.path().join("Movie_title_t00.mkv");
  fs::write(&path, b"master").unwrap();
  let fake = FakeFfmpeg::failing_derive(vec![(
    "Movie_title_t00.mkv",
    FakeOutcome::Give(dvd_mpeg2()),
  )]);
  let (index, libraries) = indexed(dir.path(), LibraryKind::Discs, &fake).await;
  let status = status_channel();

  // The attempt happens and fails; the failure is recorded.
  assert!(process_next(&index, &libraries, &fake, &status).await);
  assert!(!dir.path().join("Movie_title_t00.compat.mp4").exists());
  assert!(!process_next(&index, &libraries, &fake, &status).await);
  assert_eq!(fake.derive_plans.lock().unwrap().len(), 1);

  // Changing the file's content clears the way for a retry.
  fs::write(&path, b"new master content").unwrap();
  scan::scan_all(&index, &libraries).await.unwrap();
  probe_pending(&index, &libraries, &fake).await;
  assert!(process_next(&index, &libraries, &fake, &status).await);
  assert_eq!(fake.derive_plans.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn stale_part_file_is_replaced() {
  let dir = tempfile::tempdir().unwrap();
  fs::write(dir.path().join("Movie_title_t00.mkv"), b"master").unwrap();
  fs::write(
    dir.path().join("Movie_title_t00.compat.mp4.part"),
    b"stale interrupted output",
  )
  .unwrap();
  let fake = FakeFfmpeg::new(vec![(
    "Movie_title_t00.mkv",
    FakeOutcome::Give(dvd_mpeg2()),
  )]);
  let (index, libraries) = indexed(dir.path(), LibraryKind::Discs, &fake).await;

  assert!(process_next(&index, &libraries, &fake, &status_channel()).await);
  assert!(!dir.path().join("Movie_title_t00.compat.mp4.part").exists());
  assert_eq!(
    fs::read(dir.path().join("Movie_title_t00.compat.mp4")).unwrap(),
    b"compat bytes"
  );
}
