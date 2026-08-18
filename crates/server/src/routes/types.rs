//! Shared API payload types for index-backed routes.

use schemars::JsonSchema;
use serde::Serialize;

use crate::index::store::{TitleSource, VideoRecord};
use crate::media::codecs::native_source_type;
use crate::media::compat::{self, CompatInputs, DerivationDecision};
use crate::media::worker::ActiveDerivation;

/// Where a video stands on having a browser-playable representation.
/// Computed at response time from on-disk truth (does a compat exist), the
/// failure table, and the worker's live status — never stored, so it can
/// never go stale or stick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DerivationState {
  /// The native file is itself universally playable.
  NotNeeded,
  /// A compat copy exists (produced by Loku or already present).
  Done,
  /// The worker is deriving this item right now.
  Processing,
  /// Waiting its turn: for a probe, or for the derivation worker.
  Pending,
  /// The last attempt failed for this exact file content; see `error`.
  Failed,
  /// The file's codecs could not be determined, so nothing can be derived;
  /// playback is compat-or-bust and no compat exists.
  Unknown,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DerivationStatus {
  pub state: DerivationState,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<String>,
}

/// One video as exposed by the search, browse, and item APIs.  Paths are
/// relative to the owning library and are served under
/// `/files/{library}/{path}`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct VideoItem {
  pub library: String,
  pub path: String,
  pub name: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub title: Option<String>,
  /// Where the title came from — lets the UI style guessed titles (e.g. a
  /// cleaned rip filename) differently from authoritative ones.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub title_source: Option<TitleSource>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub duration_secs: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub upload_date: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub year: Option<i32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub channel: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub channel_url: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub webpage_url: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub view_count: Option<i64>,
  #[serde(skip_serializing_if = "Vec::is_empty", default)]
  pub genres: Vec<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub thumb_path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub compat_path: Option<String>,
  /// The `<source type>` hint for the native file (see the browse route's
  /// documentation); derived at response time from the indexed container and
  /// codec facts, never stored.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub native_type: Option<String>,
  pub derivation: DerivationStatus,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub disc_set: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub disc_title_index: Option<i64>,
}

impl VideoItem {
  /// Build the API item, folding in the worker's live status for the
  /// `processing` state.
  pub fn from_record(
    record: VideoRecord,
    active: Option<&ActiveDerivation>,
  ) -> Self {
    let native_type = native_source_type(
      &record.container,
      record.vcodec.as_deref(),
      record.acodec.as_deref(),
    );
    let derivation = derivation_status(&record, active);
    VideoItem {
      library: record.library,
      path: record.rel_path,
      name: record.file_name,
      title: record.title,
      title_source: record.title_source,
      duration_secs: record.duration_secs,
      upload_date: record.upload_date,
      year: record.year,
      description: record.description,
      channel: record.channel,
      channel_url: record.channel_url,
      webpage_url: record.webpage_url,
      view_count: record.view_count,
      genres: record.genres,
      thumb_path: record.thumb_rel_path,
      compat_path: record.compat_rel_path,
      native_type,
      derivation,
      disc_set: record.disc_set,
      disc_title_index: record.disc_title_index,
    }
  }
}

fn derivation_status(
  record: &VideoRecord,
  active: Option<&ActiveDerivation>,
) -> DerivationStatus {
  let is_active = active.is_some_and(|a| {
    a.library == record.library && a.rel_path == record.rel_path
  });
  let state = if is_active {
    DerivationState::Processing
  } else if record.compat_rel_path.is_some() {
    DerivationState::Done
  } else if record.codec_source.is_none() {
    // Not yet probed; the probe pass will decide what happens next.
    DerivationState::Pending
  } else {
    match compat::decide(&CompatInputs {
      container: &record.container,
      vcodec: record.vcodec.as_deref(),
      vprofile: record.vprofile.as_deref(),
      acodec: record.acodec.as_deref(),
      field_order: record.field_order.as_deref(),
      has_compat: false,
    }) {
      DerivationDecision::NotNeeded => DerivationState::NotNeeded,
      DerivationDecision::Unplannable => DerivationState::Unknown,
      DerivationDecision::Produce(_) => {
        if record.derivation_error.is_some() {
          DerivationState::Failed
        } else {
          DerivationState::Pending
        }
      }
    }
  };
  DerivationStatus {
    state,
    error: (state == DerivationState::Failed)
      .then(|| record.derivation_error.clone())
      .flatten(),
  }
}
