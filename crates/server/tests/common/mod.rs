// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.  dead_code is allowed because each test binary compiles this
// module separately and none uses every helper.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, dead_code)]

//! Shared test doubles and fixtures for the integration suites.

use loku_server::library::{Library, LibraryKind};
use loku_server::media::compat::CompatPlan;
use loku_server::media::ffmpeg::{
  DeriveError, FfmpegRunner, ProbeError, ProbeResult,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

pub fn library(name: &str, path: &Path, kind: LibraryKind) -> Library {
  Library {
    name: name.to_string(),
    path: path.to_path_buf(),
    kind,
  }
}

/// What the fake should do when asked to probe a given file name.
pub enum FakeOutcome {
  Give(ProbeResult),
  FailDefinitively,
  FailTransiently,
}

/// A scripted FfmpegRunner: canned probe outcomes keyed by file name, plus
/// call/plan records so tests can assert cache behavior and the exact plans
/// the worker chose.  Derivations write marker bytes to the destination the
/// way the real encoder would produce a file.
pub struct FakeFfmpeg {
  outcomes: HashMap<String, FakeOutcome>,
  probe_calls: AtomicUsize,
  pub derive_plans: Mutex<Vec<(String, CompatPlan)>>,
  pub fail_derive: bool,
}

impl FakeFfmpeg {
  pub fn new(outcomes: Vec<(&str, FakeOutcome)>) -> Self {
    Self {
      outcomes: outcomes
        .into_iter()
        .map(|(name, outcome)| (name.to_string(), outcome))
        .collect(),
      probe_calls: AtomicUsize::new(0),
      derive_plans: Mutex::new(Vec::new()),
      fail_derive: false,
    }
  }

  pub fn failing_derive(outcomes: Vec<(&str, FakeOutcome)>) -> Self {
    Self {
      fail_derive: true,
      ..Self::new(outcomes)
    }
  }

  pub fn probe_calls(&self) -> usize {
    self.probe_calls.load(Ordering::Relaxed)
  }
}

#[async_trait::async_trait]
impl FfmpegRunner for FakeFfmpeg {
  async fn probe(&self, path: &Path) -> Result<ProbeResult, ProbeError> {
    self.probe_calls.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    match self.outcomes.get(&name) {
      Some(FakeOutcome::Give(result)) => Ok(result.clone()),
      Some(FakeOutcome::FailDefinitively) => Err(ProbeError::FfprobeExit {
        path: name,
        status: "exit status: 1".to_string(),
        stderr_tail: "moov atom not found".to_string(),
      }),
      Some(FakeOutcome::FailTransiently) => Err(ProbeError::FfprobeTimeout {
        path: name,
        timeout_secs: 30,
      }),
      None => Ok(ProbeResult::default()),
    }
  }

  async fn derive_compat(
    &self,
    source: &Path,
    dest: &Path,
    plan: &CompatPlan,
  ) -> Result<(), DeriveError> {
    let name = source.file_name().unwrap().to_string_lossy().to_string();
    self
      .derive_plans
      .lock()
      .unwrap()
      .push((name.clone(), plan.clone()));
    if self.fail_derive {
      return Err(DeriveError::FfmpegExit {
        path: name,
        status: "exit status: 1".to_string(),
        stderr_tail: "scripted failure".to_string(),
      });
    }
    std::fs::write(dest, b"compat bytes").unwrap();
    Ok(())
  }

  async fn extract_thumbnail(
    &self,
    _source: &Path,
    dest: &Path,
    _at_secs: f64,
  ) -> Result<(), DeriveError> {
    std::fs::write(dest, b"jpeg bytes").unwrap();
    Ok(())
  }
}
