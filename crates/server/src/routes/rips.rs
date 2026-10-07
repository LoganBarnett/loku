//! The rips API: what each configured drive holds, the job in progress with
//! live progress, recent history, and a manual trigger that reruns the
//! inserted disc under a chosen policy.

use aide::transform::TransformOperation;
use axum::{
  extract::State,
  http::StatusCode,
  response::{IntoResponse, Response},
  Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::index::store::{IndexError, RipRecord};
use crate::rip::config::RipPolicy;
use crate::rip::drive::DriveStatus;
use crate::rip::makemkv::DiscFacts;
use crate::rip::naming;
use crate::rip::worker::{ActiveRip, RipState, RipTrigger, SubmitError};
use crate::rip::RipHandles;
use crate::web_base::AppState;

/// How much history the status endpoint returns.
const RECENT_LIMIT: u32 = 20;

#[derive(Debug, Serialize, JsonSchema)]
pub struct RipsStatus {
  /// Whether a `[rip]` table is configured and the tools were found.
  pub enabled: bool,
  pub drives: Vec<DriveItem>,
  /// The most recent jobs, newest first.
  pub recent: Vec<RipJobItem>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DriveItem {
  pub device: String,
  pub library: String,
  pub policy: RipPolicy,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub last_error: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub media: Option<MediaItem>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub active: Option<ActiveRip>,
}

/// The disc in a drive as udev describes it, before any scan.
#[derive(Debug, Serialize, JsonSchema)]
pub struct MediaItem {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub label: Option<String>,
  /// The disc id when udev reported a filesystem UUID; the fallback ids
  /// need the scan.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub disc_id: Option<String>,
  pub kind: MediaKind,
  /// The directory a rip of this disc would land in, when it already
  /// exists (so the policy will apply).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub existing_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
  Dvd,
  Bd,
  Unknown,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RipJobItem {
  pub id: i64,
  pub device: String,
  pub library: String,
  /// The library-relative directory the rip landed in (or will).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub label: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub disc_id: Option<String>,
  pub policy: RipPolicy,
  pub trigger: RipTrigger,
  pub state: RipState,
  pub progress_permille: u32,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub current_op: Option<String>,
  pub started_at: i64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub finished_at: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
}

impl RipJobItem {
  /// The row, with the worker's live progress overlaid when this is the
  /// job in progress (the row only refreshes every couple of seconds).
  fn from_record(record: RipRecord, active: Option<&ActiveRip>) -> Self {
    let live = active.filter(|active| active.id == record.id);
    RipJobItem {
      id: record.id,
      device: record.device,
      library: record.library,
      path: live.and_then(|a| a.path.clone()).or(record.dir_rel_path),
      label: live.and_then(|a| a.label.clone()).or(record.label),
      disc_id: record.disc_id,
      policy: record.policy,
      trigger: record.trigger,
      state: live.map_or(record.state, |a| a.state),
      progress_permille: live
        .map_or(record.progress_permille, |a| a.progress_permille),
      current_op: live
        .and_then(|a| a.current_op.clone())
        .or(record.current_op),
      started_at: record.started_at,
      finished_at: record.finished_at,
      error: record.error,
    }
  }
}

/// A manual rip of whatever is in the drive, under the given policy: the
/// way an operator asks for a rescan or a rerip of a disc the default
/// policy skipped.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TriggerRipRequest {
  /// Defaults to the configured drive.
  pub device: Option<String>,
  pub policy: RipPolicy,
}

#[derive(Debug, Error)]
pub(crate) enum RipsError {
  #[error("Disc ripping is not configured on this server")]
  RipDisabled,

  #[error("Device '{device}' is not a configured rip drive")]
  UnknownDevice { device: String },

  #[error("No media detected in '{device}'")]
  NoMedia { device: String },

  #[error(transparent)]
  Submit(#[from] SubmitError),

  #[error(transparent)]
  Index(#[from] IndexError),
}

impl aide::operation::OperationOutput for RipsError {
  type Inner = Self;
}

impl IntoResponse for RipsError {
  fn into_response(self) -> Response {
    let status = match &self {
      RipsError::RipDisabled
      | RipsError::NoMedia { .. }
      | RipsError::Submit(SubmitError::RipInProgress { .. }) => {
        StatusCode::CONFLICT
      }
      RipsError::UnknownDevice { .. } => StatusCode::NOT_FOUND,
      RipsError::Submit(SubmitError::Index(_)) | RipsError::Index(_) => {
        StatusCode::INTERNAL_SERVER_ERROR
      }
    };
    (status, self.to_string()).into_response()
  }
}

pub(crate) fn status_docs(op: TransformOperation) -> TransformOperation {
  op.description("Drive contents, the rip in progress, and recent rip history.")
    .response::<200, Json<RipsStatus>>()
    .response_with::<500, (), _>(|r| r.description("Index query failed."))
}

pub(crate) async fn status_handler(
  State(state): State<AppState>,
) -> Result<Json<RipsStatus>, RipsError> {
  match state.rips.as_ref() {
    None => Ok(Json(RipsStatus {
      enabled: false,
      drives: Vec::new(),
      recent: Vec::new(),
    })),
    Some(handles) => {
      let active = handles.queue.active.borrow().clone();
      let drives = handles
        .drives
        .iter()
        .map(|status| drive_item(&status.borrow(), handles, active.as_ref()))
        .collect();
      let recent = state
        .index
        .recent_rips(RECENT_LIMIT)
        .await?
        .into_iter()
        .map(|record| RipJobItem::from_record(record, active.as_ref()))
        .collect();
      Ok(Json(RipsStatus {
        enabled: true,
        drives,
        recent,
      }))
    }
  }
}

fn drive_item(
  status: &DriveStatus,
  handles: &RipHandles,
  active: Option<&ActiveRip>,
) -> DriveItem {
  DriveItem {
    device: status.device.clone(),
    library: handles.config.library.name.clone(),
    policy: handles.config.policy,
    last_error: status.last_error.clone(),
    media: status
      .media
      .as_ref()
      .map(|facts| media_item(facts, &handles.config.library.path)),
    active: active
      .filter(|active| active.device == status.device)
      .cloned(),
  }
}

fn media_item(facts: &DiscFacts, library_path: &Path) -> MediaItem {
  let disc_id = facts
    .fs_uuid
    .as_deref()
    .and_then(naming::sanitize_component);
  // Only the udev label is known before the scan, so the existing-directory
  // check is exact only for discs udev could name; the policy itself runs
  // on the scan's fuller picture.
  let existing_path = facts
    .label
    .as_deref()
    .and_then(naming::sanitize_component)
    .zip(disc_id.as_deref())
    .map(|(label, id)| naming::dir_name(&label, id))
    .filter(|name| library_path.join(name).exists());
  MediaItem {
    label: facts.label.clone(),
    disc_id,
    kind: if facts.media_bd {
      MediaKind::Bd
    } else if facts.media_dvd {
      MediaKind::Dvd
    } else {
      MediaKind::Unknown
    },
    existing_path,
  }
}

pub(crate) fn trigger_docs(op: TransformOperation) -> TransformOperation {
  op.description(
    "Rip the disc in the drive now under the given policy (rescan or rerip \
     redo a disc the default policy would skip).",
  )
  .response::<202, Json<RipJobItem>>()
  .response_with::<404, (), _>(|r| r.description("Unknown device."))
  .response_with::<409, (), _>(|r| {
    r.description(
      "Ripping is not configured, no media is loaded, or a rip is already \
       running on that device.",
    )
  })
  .response_with::<500, (), _>(|r| r.description("Index query failed."))
}

pub(crate) async fn trigger_handler(
  State(state): State<AppState>,
  Json(request): Json<TriggerRipRequest>,
) -> Result<(StatusCode, Json<RipJobItem>), RipsError> {
  let handles = state.rips.as_ref().ok_or(RipsError::RipDisabled)?;
  let device = request
    .device
    .map_or_else(|| handles.config.device.clone(), PathBuf::from);
  let display = device.display().to_string();
  let drive = handles
    .drives
    .iter()
    .find(|status| status.borrow().device == display)
    .ok_or_else(|| RipsError::UnknownDevice {
      device: display.clone(),
    })?;
  let facts = drive
    .borrow()
    .media
    .clone()
    .ok_or_else(|| RipsError::NoMedia { device: display })?;
  let job = handles
    .queue
    .submit(&device, facts, request.policy, RipTrigger::Manual)
    .await?;
  Ok((StatusCode::ACCEPTED, Json(RipJobItem::from_record(job, None))))
}
