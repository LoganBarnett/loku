// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! Smoke test over the real rip tools, ignored by default: it needs
//! `udevadm`, `makemkvcon`, and `eject` on `PATH` and an optical drive at
//! `/dev/sr0`, which only a Linux host with a drive has.

use loku_server::rip::makemkv::{DiscTools, RealDiscTools};
use std::path::Path;

#[tokio::test]
#[ignore = "requires udevadm/makemkvcon/eject on PATH and /dev/sr0; run with `just test-rip-tools`"]
async fn real_tools_detect_and_query_the_drive() {
  let tools = RealDiscTools::detect().await.expect("rip tools on PATH");
  let media = tools
    .media(Path::new("/dev/sr0"))
    .await
    .expect("udevadm answers for /dev/sr0");
  eprintln!("drive contents: {media:?}");
}
