// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! Round trips against the real ffmpeg/ffprobe binaries.  Ignored by default
//! so plain `cargo test` needs no media tooling; run via `just test-ffmpeg`
//! (the dev shell provides the binaries).

use loku_server::media::ffmpeg::{FfmpegRunner, RealFfmpeg};

#[tokio::test]
#[ignore = "requires ffmpeg/ffprobe on PATH; run with `just test-ffmpeg`"]
async fn real_ffprobe_round_trip() {
  let dir = tempfile::tempdir().unwrap();
  let clip = dir.path().join("clip.mp4");

  // Synthesize a one-second H.264 test clip.  ffmpeg has no long-form
  // aliases for these flags: -f = input format (the lavfi synthetic source),
  // -i = input specifier, -c:v = video codec, -pix_fmt = pixel format
  // (yuv420p keeps baseline-profile compatibility), -y = overwrite output.
  let status = tokio::process::Command::new("ffmpeg")
    .args([
      "-f",
      "lavfi",
      "-i",
      "testsrc=duration=1:size=320x240:rate=10",
      "-c:v",
      "libx264",
      "-pix_fmt",
      "yuv420p",
      "-y",
    ])
    .arg(&clip)
    .status()
    .await
    .expect("ffmpeg must be runnable for this ignored test");
  assert!(status.success(), "test clip synthesis failed");

  let runner = RealFfmpeg::detect().await.unwrap();
  let probe = runner.probe(&clip).await.unwrap();
  assert_eq!(probe.vcodec.as_deref(), Some("h264"));
  assert!(probe.duration_secs.unwrap() > 0.5);
  assert_eq!(probe.width, Some(320));
  assert_eq!(probe.height, Some(240));
}
