//! The rip worker: one job at a time per drive, from "disc detected" to a
//! per-disc directory swapped into the library.
//!
//! Serial by construction — a bounded channel of one request feeds a single
//! task, so two rips can never contend for the drive.  Idempotency comes from
//! on-disk truth: output is ripped into a hidden staging directory the index
//! never sees, renamed into place only once the title count matches the disc
//! scan, and any existing tree is moved aside rather than overwritten.  A
//! crash mid-job leaves only hidden staging (deleted by the next attempt)
//! and an open job row (closed as failed at the next startup); the disc is
//! still in the drive, so the poller simply starts over.
//!
//! The state machine of one job:
//!
//! ```text
//! detected ─scan─▶ scanning ─decide─┬─▶ skipped
//!                                   ├─▶ finalizing ─▶ done   (rescan)
//!                                   └─▶ ripping ─▶ finalizing ─▶ done
//! any ─error─▶ failed  (staging quarantined, tray unlocked)
//! ```

use futures_util::stream::{self, TryStreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tracing::{info, warn};

use crate::identity::fingerprint;
use crate::index::scan::{self, ScanError};
use crate::index::store::{now_secs, Index, IndexError, NewRip, RipRecord};
use crate::index::wakes::Wakes;
use crate::rip::config::{RipConfig, RipPolicy};
use crate::rip::makemkv::{
  self, DiscFacts, DiscRipError, DiscScanError, DiscTools, InfoDump, RipEvent,
  RipOutput,
};
use crate::rip::manifest::{
  self, DiscManifest, ManifestError, MANIFEST_SCHEMA,
};
use crate::rip::naming::{
  self, DiscIdentity, FAILED_DIR, REPLACED_DIR, RIP_DIR, STAGING_DIR,
};
use crate::rip::policy::{self, PolicyError, RipAction};

/// The live status channel updates on every progress event; the job row is
/// written at most this often, so a rip does not hammer the database.
const PROGRESS_WRITE_INTERVAL: Duration = Duration::from_secs(2);

/// What started a rip job.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RipTrigger {
  /// The drive poller saw a disc appear.
  Auto,
  /// An operator asked through the API.
  Manual,
}

impl RipTrigger {
  pub fn as_str(self) -> &'static str {
    match self {
      RipTrigger::Auto => "auto",
      RipTrigger::Manual => "manual",
    }
  }

  pub fn parse(value: &str) -> Option<Self> {
    match value {
      "auto" => Some(RipTrigger::Auto),
      "manual" => Some(RipTrigger::Manual),
      _ => None,
    }
  }
}

/// Where a rip job stands.
#[derive(
  Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RipState {
  Detected,
  Scanning,
  Ripping,
  Finalizing,
  Done,
  Skipped,
  Failed,
}

impl RipState {
  pub fn as_str(self) -> &'static str {
    match self {
      RipState::Detected => "detected",
      RipState::Scanning => "scanning",
      RipState::Ripping => "ripping",
      RipState::Finalizing => "finalizing",
      RipState::Done => "done",
      RipState::Skipped => "skipped",
      RipState::Failed => "failed",
    }
  }

  pub fn parse(value: &str) -> Option<Self> {
    match value {
      "detected" => Some(RipState::Detected),
      "scanning" => Some(RipState::Scanning),
      "ripping" => Some(RipState::Ripping),
      "finalizing" => Some(RipState::Finalizing),
      "done" => Some(RipState::Done),
      "skipped" => Some(RipState::Skipped),
      "failed" => Some(RipState::Failed),
      _ => None,
    }
  }

  pub fn is_terminal(self) -> bool {
    matches!(self, RipState::Done | RipState::Skipped | RipState::Failed)
  }
}

/// A queued rip: the disc as udev described it plus the policy to apply.
#[derive(Debug, Clone)]
pub struct RipRequest {
  pub job_id: i64,
  pub device: PathBuf,
  pub facts: DiscFacts,
  pub policy: RipPolicy,
  pub trigger: RipTrigger,
}

/// The job in progress, published for status display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ActiveRip {
  pub id: i64,
  pub device: String,
  pub state: RipState,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub label: Option<String>,
  /// The `<label>__<id>` directory name, once the scan has named it.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub path: Option<String>,
  pub progress_permille: u32,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub current_op: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub total_op: Option<String>,
  pub started_at: i64,
}

/// Everything the worker needs.
pub struct WorkerDeps {
  pub index: Index,
  pub config: Arc<RipConfig>,
  pub tools: Arc<dyn DiscTools>,
  pub wakes: Wakes,
}

/// The handle the poller and the API submit through.
#[derive(Clone)]
pub struct RipQueue {
  sender: mpsc::Sender<RipRequest>,
  pub active: watch::Receiver<Option<ActiveRip>>,
  index: Index,
  library: String,
}

#[derive(Debug, Error)]
pub enum SubmitError {
  #[error("A rip is already running on '{device}'")]
  RipInProgress { device: String },

  #[error(transparent)]
  Index(#[from] IndexError),
}

impl RipQueue {
  /// Queue a rip.  The channel slot is reserved before the job row is
  /// inserted, so a refused submission never leaves a dangling row.
  pub async fn submit(
    &self,
    device: &Path,
    facts: DiscFacts,
    policy: RipPolicy,
    trigger: RipTrigger,
  ) -> Result<RipRecord, SubmitError> {
    let display = device.display().to_string();
    if self.active.borrow().is_some() {
      return Err(SubmitError::RipInProgress { device: display });
    }
    let permit = self.sender.try_reserve().map_err(|_full| {
      SubmitError::RipInProgress {
        device: display.clone(),
      }
    })?;
    let job = self
      .index
      .insert_rip(&NewRip {
        device: display,
        library: self.library.clone(),
        policy,
        trigger,
      })
      .await?;
    permit.send(RipRequest {
      job_id: job.id,
      device: device.to_path_buf(),
      facts,
      policy,
      trigger,
    });
    Ok(job)
  }
}

/// Spawn the worker task and hand back its queue.
pub fn spawn(deps: WorkerDeps) -> RipQueue {
  let (sender, mut receiver) = mpsc::channel::<RipRequest>(1);
  let (status, active) = watch::channel(None);
  let queue = RipQueue {
    sender,
    active,
    index: deps.index.clone(),
    library: deps.config.library.name.clone(),
  };
  tokio::spawn(async move {
    while let Some(request) = receiver.recv().await {
      run_job(&deps, request, &status).await;
    }
  });
  queue
}

#[derive(Debug, Error)]
enum JobError {
  #[error(transparent)]
  Scan(#[from] DiscScanError),

  #[error(
    "makemkvcon reported no title count for '{device}'; the disc scan \
     output is unusable"
  )]
  TitleCountMissing { device: String },

  #[error(transparent)]
  Policy(#[from] PolicyError),

  #[error("Failed to clear rip staging '{path}': {source}")]
  StagingClear {
    path: String,
    source: std::io::Error,
  },

  #[error("Failed to create rip staging '{path}': {source}")]
  StagingCreate {
    path: String,
    source: std::io::Error,
  },

  #[error("Failed to list rip staging '{path}': {source}")]
  StagingList {
    path: String,
    source: std::io::Error,
  },

  #[error(transparent)]
  Rip(#[from] DiscRipError),

  #[error(
    "makemkvcon reported success but staging '{staging}' holds {found} MKV \
     files where the disc scan promised {expected}"
  )]
  TitleCountMismatch {
    expected: u32,
    found: usize,
    staging: String,
  },

  #[error(transparent)]
  Manifest(#[from] ManifestError),

  #[error("Failed to create the replaced-rip archive '{path}': {source}")]
  ReplacedDirCreate {
    path: String,
    source: std::io::Error,
  },

  #[error(
    "Failed to move the previous rip '{from}' aside to '{to}': {source}"
  )]
  MoveAside {
    from: String,
    to: String,
    source: std::io::Error,
  },

  #[error(
    "Failed to move the finished rip '{from}' into place at '{to}': {source}"
  )]
  SwapIn {
    from: String,
    to: String,
    source: std::io::Error,
  },

  #[error(transparent)]
  Index(#[from] IndexError),

  #[error(transparent)]
  IndexScan(#[from] ScanError),
}

/// How a job ended well.
enum Completion {
  Skipped {
    dir_name: String,
  },
  Rescanned {
    dir_name: String,
  },
  Ripped {
    dir_name: String,
    replaced: Option<PathBuf>,
  },
}

/// Run one job to completion, recording its outcome.  Public so tests can
/// drive the worker one job at a time without spawning it.
pub async fn run_job(
  deps: &WorkerDeps,
  request: RipRequest,
  status: &watch::Sender<Option<ActiveRip>>,
) {
  let mut tracker = Tracker::new(&deps.index, status, &request);
  match execute(deps, &request, &mut tracker).await {
    Ok(Completion::Skipped { dir_name }) => {
      info!(
        job = request.job_id,
        directory = %dir_name,
        "Disc already ripped; skipping (POST /api/rips with policy \
         rescan or rerip to redo it)"
      );
      finish(&deps.index, request.job_id, RipState::Skipped, None).await;
    }
    Ok(Completion::Rescanned { dir_name }) => {
      info!(job = request.job_id, directory = %dir_name, "Disc sidecars refreshed");
      finish(&deps.index, request.job_id, RipState::Done, None).await;
      eject_if_configured(deps, &request.device).await;
    }
    Ok(Completion::Ripped { dir_name, replaced }) => {
      info!(
        job = request.job_id,
        directory = %dir_name,
        replaced = replaced.as_ref().map(|p| p.display().to_string()),
        "Disc rip complete"
      );
      finish(&deps.index, request.job_id, RipState::Done, None).await;
      eject_if_configured(deps, &request.device).await;
    }
    Err(error) => {
      warn!(job = request.job_id, %error, "Disc rip failed");
      if let Some(dir_name) = tracker.dir_name() {
        quarantine_staging(deps, dir_name).await;
      }
      tracker.state(RipState::Failed).await;
      finish(
        &deps.index,
        request.job_id,
        RipState::Failed,
        Some(&error.to_string()),
      )
      .await;
      if let Err(tray_error) = deps.tools.unlock(&request.device).await {
        warn!(error = %tray_error, "Could not unlock the drive tray after a failed rip");
      }
    }
  }
  status.send_replace(None);
}

async fn execute(
  deps: &WorkerDeps,
  request: &RipRequest,
  tracker: &mut Tracker<'_>,
) -> Result<Completion, JobError> {
  let library = &deps.config.library;
  tracker.state(RipState::Scanning).await;
  let info_text = deps.tools.scan(&request.device).await?;
  let info = makemkv::parse_info(&info_text);
  let title_count =
    info
      .title_count
      .ok_or_else(|| JobError::TitleCountMissing {
        device: request.device.display().to_string(),
      })?;
  let identity = naming::disc_identity(
    request.facts.fs_uuid.as_deref(),
    &info.tinfo_lines,
    now_secs(),
  );
  let label = naming::disc_label(
    request.facts.label.as_deref(),
    info.volume_name.as_deref(),
    info.disc_name.as_deref(),
  );
  let dir_name = naming::dir_name(&label, &identity.id);
  let final_dir = library.path.join(&dir_name);
  tracker.describe(&label, &dir_name, &identity.id).await;
  let action = policy::decide(
    request.policy,
    identity.source,
    final_dir.exists(),
    &dir_name,
  )?;
  let naming = Naming {
    label,
    identity,
    dir_name,
  };
  match action {
    RipAction::Skip => Ok(Completion::Skipped {
      dir_name: naming.dir_name,
    }),
    RipAction::Rescan => {
      rescan(deps, request, tracker, &final_dir, &info_text, &info, naming)
        .await
    }
    RipAction::Rip { replace } => {
      rip(
        deps,
        request,
        tracker,
        &final_dir,
        &info_text,
        &info,
        title_count,
        naming,
        replace,
      )
      .await
    }
  }
}

/// The names the scan settled on for this disc.
struct Naming {
  label: String,
  identity: DiscIdentity,
  dir_name: String,
}

/// Refresh only the sidecars of an existing directory, keeping the rip
/// timestamp its manifest already carries.
async fn rescan(
  deps: &WorkerDeps,
  request: &RipRequest,
  tracker: &mut Tracker<'_>,
  final_dir: &Path,
  info_text: &str,
  info: &InfoDump,
  naming: Naming,
) -> Result<Completion, JobError> {
  tracker.state(RipState::Finalizing).await;
  let ripped_at = manifest::read_manifest(final_dir)
    .await
    .unwrap_or_else(|error| {
      warn!(%error, "Previous disc manifest unreadable; the rip timestamp is lost");
      None
    })
    .and_then(|previous| previous.ripped_at);
  let manifest = build_manifest(request, info, &naming, ripped_at);
  manifest::write_info_dump(final_dir, info_text).await?;
  manifest::write_manifest(final_dir, &manifest).await?;
  reconcile(deps, final_dir).await?;
  tracker.state(RipState::Done).await;
  Ok(Completion::Rescanned {
    dir_name: naming.dir_name,
  })
}

#[allow(clippy::too_many_arguments)]
// The rip step is the one place every fact of the job converges; bundling
// them further would only hide the data flow.
async fn rip(
  deps: &WorkerDeps,
  request: &RipRequest,
  tracker: &mut Tracker<'_>,
  final_dir: &Path,
  info_text: &str,
  info: &InfoDump,
  title_count: u32,
  naming: Naming,
  replace: bool,
) -> Result<Completion, JobError> {
  let library = &deps.config.library;
  let staging = library
    .path
    .join(RIP_DIR)
    .join(STAGING_DIR)
    .join(&naming.dir_name);
  clear_staging(&staging).await?;
  tokio::fs::create_dir_all(&staging)
    .await
    .map_err(|source| JobError::StagingCreate {
      path: staging.display().to_string(),
      source,
    })?;
  manifest::write_info_dump(&staging, info_text).await?;
  tracker.state(RipState::Ripping).await;
  let output = run_rip(deps, &request.device, &staging, tracker).await?;
  let found = count_mkvs(&staging).await?;
  if usize::try_from(title_count).is_ok_and(|expected| expected != found) {
    return Err(JobError::TitleCountMismatch {
      expected: title_count,
      found,
      staging: staging.display().to_string(),
    });
  }
  info!(
    titles = found,
    last_message = output.last_messages.last().map(String::as_str),
    "makemkvcon finished"
  );
  tracker.state(RipState::Finalizing).await;
  let manifest = build_manifest(request, info, &naming, Some(now_secs()));
  manifest::write_manifest(&staging, &manifest).await?;
  let replaced = if replace {
    Some(move_aside(library, final_dir, &naming.dir_name).await?)
  } else {
    None
  };
  tokio::fs::rename(&staging, final_dir)
    .await
    .map_err(|source| JobError::SwapIn {
      from: staging.display().to_string(),
      to: final_dir.display().to_string(),
      source,
    })?;
  reconcile(deps, final_dir).await?;
  tracker.state(RipState::Done).await;
  Ok(Completion::Ripped {
    dir_name: naming.dir_name,
    replaced,
  })
}

/// Delete a stale staging directory from an earlier attempt; absence is the
/// normal case.
async fn clear_staging(staging: &Path) -> Result<(), JobError> {
  match tokio::fs::remove_dir_all(staging).await {
    Ok(()) => {
      info!(path = %staging.display(), "Removed stale rip staging");
      Ok(())
    }
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(source) => Err(JobError::StagingClear {
      path: staging.display().to_string(),
      source,
    }),
  }
}

/// Drive the ripper while folding its progress into the tracker.
async fn run_rip(
  deps: &WorkerDeps,
  device: &Path,
  staging: &Path,
  tracker: &mut Tracker<'_>,
) -> Result<RipOutput, DiscRipError> {
  let (events, mut receiver) = mpsc::unbounded_channel::<RipEvent>();
  let rip = deps.tools.rip(device, staging, events);
  tokio::pin!(rip);
  let mut events_open = true;
  loop {
    tokio::select! {
      result = &mut rip => {
        // Whatever the ripper reported just before exiting is still worth
        // showing; the channel is empty or closed once this drains.
        while let Ok(event) = receiver.try_recv() {
          tracker.event(event).await;
        }
        break result;
      }
      event = receiver.recv(), if events_open => match event {
        Some(event) => tracker.event(event).await,
        None => events_open = false,
      }
    }
  }
}

/// How many MKV files the rip left in staging.
async fn count_mkvs(dir: &Path) -> Result<usize, JobError> {
  let list_error = |source| JobError::StagingList {
    path: dir.display().to_string(),
    source,
  };
  let entries = tokio::fs::read_dir(dir).await.map_err(list_error)?;
  stream::try_unfold(entries, |mut entries| async move {
    entries
      .next_entry()
      .await
      .map(|next| next.map(|entry| (entry, entries)))
  })
  .and_then(|entry| async move {
    let is_mkv = entry
      .path()
      .extension()
      .is_some_and(|ext| ext.eq_ignore_ascii_case("mkv"));
    entry.file_type().await.map(|kind| is_mkv && kind.is_file())
  })
  .try_fold(
    0,
    |count, is_title| async move { Ok(count + usize::from(is_title)) },
  )
  .await
  .map_err(list_error)
}

/// Move the existing tree into the replaced archive under a timestamped
/// name; the retention sweep deletes it later.
async fn move_aside(
  library: &crate::library::Library,
  final_dir: &Path,
  dir_name: &str,
) -> Result<PathBuf, JobError> {
  let archive = library.path.join(RIP_DIR).join(REPLACED_DIR);
  tokio::fs::create_dir_all(&archive)
    .await
    .map_err(|source| JobError::ReplacedDirCreate {
      path: archive.display().to_string(),
      source,
    })?;
  let aside = archive.join(naming::archived_name(dir_name, now_secs()));
  tokio::fs::rename(final_dir, &aside)
    .await
    .map_err(|source| JobError::MoveAside {
      from: final_dir.display().to_string(),
      to: aside.display().to_string(),
      source,
    })?;
  info!(from = %final_dir.display(), to = %aside.display(), "Moved the previous rip aside");
  Ok(aside)
}

/// Fold the finished directory into the index right away rather than
/// waiting on the watcher's debounce, then wake the workers that act on it.
async fn reconcile(deps: &WorkerDeps, dir: &Path) -> Result<(), JobError> {
  let generation = deps.index.current_generation().await?;
  scan::reconcile_directory(&deps.index, &deps.config.library, dir, generation)
    .await?;
  deps.wakes.notify_all();
  Ok(())
}

fn build_manifest(
  request: &RipRequest,
  info: &InfoDump,
  naming: &Naming,
  ripped_at: Option<i64>,
) -> DiscManifest {
  DiscManifest {
    schema: MANIFEST_SCHEMA,
    device: request.device.display().to_string(),
    dir_name: naming.dir_name.clone(),
    label: naming.label.clone(),
    id: naming.identity.id.clone(),
    id_source: naming.identity.source,
    scanned_at: now_secs(),
    ripped_at,
    policy: request.policy,
    udev: request.facts.clone(),
    makemkv_version: info.makemkv_version.clone(),
    disc_name: info.disc_name.clone(),
    volume_name: info.volume_name.clone(),
    title_count: info.title_count.unwrap_or_default(),
    titles: info.titles.clone(),
    fingerprint: Some(fingerprint::compute(&info.titles).stored()),
  }
}

/// Keep a failed attempt's staging for inspection under the failed archive;
/// the retention sweep deletes it later.  Failures here only warn — the
/// job's own error is the one worth reporting.
async fn quarantine_staging(deps: &WorkerDeps, dir_name: &str) {
  let rip_dir = deps.config.library.path.join(RIP_DIR);
  let staging = rip_dir.join(STAGING_DIR).join(dir_name);
  if !staging.exists() {
    return;
  }
  let archive = rip_dir.join(FAILED_DIR);
  let target = archive.join(naming::archived_name(dir_name, now_secs()));
  let moved = async {
    tokio::fs::create_dir_all(&archive).await?;
    tokio::fs::rename(&staging, &target).await
  }
  .await;
  match moved {
    Ok(()) => {
      info!(path = %target.display(), "Quarantined the failed rip's staging")
    }
    Err(error) => warn!(
      path = %staging.display(),
      %error,
      "Could not quarantine the failed rip's staging"
    ),
  }
}

async fn finish(
  index: &Index,
  job_id: i64,
  state: RipState,
  error: Option<&str>,
) {
  if let Err(index_error) = index.finish_rip(job_id, state, error).await {
    warn!(job = job_id, error = %index_error, "Failed to record the rip job's outcome");
  }
}

async fn eject_if_configured(deps: &WorkerDeps, device: &Path) {
  if !deps.config.eject {
    return;
  }
  match deps.tools.eject(device).await {
    Ok(()) => info!(device = %device.display(), "Ejected the disc"),
    Err(error) => {
      warn!(device = %device.display(), %error, "Could not eject the disc");
    }
  }
}

/// The job's live status: mirrored to the watch channel on every change and
/// to the job row at a throttled pace.  Row-write failures only warn — the
/// channel is the live truth and the row is history.
struct Tracker<'a> {
  index: &'a Index,
  status: &'a watch::Sender<Option<ActiveRip>>,
  active: ActiveRip,
  last_row_write: Instant,
}

impl<'a> Tracker<'a> {
  fn new(
    index: &'a Index,
    status: &'a watch::Sender<Option<ActiveRip>>,
    request: &RipRequest,
  ) -> Self {
    let tracker = Self {
      index,
      status,
      active: ActiveRip {
        id: request.job_id,
        device: request.device.display().to_string(),
        state: RipState::Detected,
        label: None,
        path: None,
        progress_permille: 0,
        current_op: None,
        total_op: None,
        started_at: now_secs(),
      },
      last_row_write: Instant::now(),
    };
    tracker.publish();
    tracker
  }

  fn dir_name(&self) -> Option<&str> {
    self.active.path.as_deref()
  }

  fn publish(&self) {
    self.status.send_replace(Some(self.active.clone()));
  }

  async fn state(&mut self, state: RipState) {
    self.active.state = state;
    self.publish();
    if let Err(error) = self.index.set_rip_state(self.active.id, state).await {
      warn!(job = self.active.id, %error, "Failed to record the rip job's state");
    }
  }

  async fn describe(&mut self, label: &str, dir_name: &str, disc_id: &str) {
    self.active.label = Some(label.to_string());
    self.active.path = Some(dir_name.to_string());
    self.publish();
    if let Err(error) = self
      .index
      .describe_rip(self.active.id, dir_name, label, disc_id)
      .await
    {
      warn!(job = self.active.id, %error, "Failed to record the rip job's disc");
    }
  }

  async fn event(&mut self, event: RipEvent) {
    match event {
      RipEvent::Progress { total, max, .. } => {
        self.active.progress_permille = makemkv::progress_permille(total, max);
      }
      RipEvent::CurrentOp(op) => self.active.current_op = Some(op),
      RipEvent::TotalOp(op) => self.active.total_op = Some(op),
      RipEvent::Message { .. } => {}
    }
    self.publish();
    if self.last_row_write.elapsed() >= PROGRESS_WRITE_INTERVAL {
      self.last_row_write = Instant::now();
      if let Err(error) = self
        .index
        .rip_progress(
          self.active.id,
          self.active.progress_permille,
          self.active.current_op.as_deref(),
        )
        .await
      {
        warn!(job = self.active.id, %error, "Failed to record rip progress");
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rip_enums_round_trip() {
    for state in [
      RipState::Detected,
      RipState::Scanning,
      RipState::Ripping,
      RipState::Finalizing,
      RipState::Done,
      RipState::Skipped,
      RipState::Failed,
    ] {
      assert_eq!(RipState::parse(state.as_str()), Some(state));
    }
    assert!(RipState::Done.is_terminal());
    assert!(!RipState::Ripping.is_terminal());
    for trigger in [RipTrigger::Auto, RipTrigger::Manual] {
      assert_eq!(RipTrigger::parse(trigger.as_str()), Some(trigger));
    }
  }
}
