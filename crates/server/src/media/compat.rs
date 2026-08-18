//! The compat-derivation planner: given a video's indexed codec facts,
//! decide whether a browser-compatible representation is needed and, if so,
//! what the cheapest honest way to produce it is.
//!
//! The universal target is H.264 + AAC in MP4 with faststart — the one
//! combination every supported browser plays.  Codecs the browser world
//! partially supports (VP9, AV1, Opus) still get a compat copy: the player
//! offers the native file first via its `native_type` hint and capable
//! browsers never touch the compat, while Safari falls back without a
//! wasted download.
//!
//! Pure decision logic, no I/O — the worker and ffmpeg boundary act on the
//! result.

/// What to do with the video stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoAction {
  /// The stream is already browser-safe H.264 — remux it untouched.  This
  /// is the BluRay fast path: full quality, no CPU-hours.
  Copy,
  /// Re-encode to browser-safe H.264, deinterlacing first when the source
  /// is interlaced (DVD MPEG-2 in particular combs badly otherwise).
  Transcode { deinterlace: bool },
}

/// What to do with the audio stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioAction {
  Copy,
  /// Re-encode to AAC — the common case for disc audio (AC-3, DTS,
  /// TrueHD), none of which browsers decode.
  Transcode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatPlan {
  pub video: VideoAction,
  pub audio: AudioAction,
}

/// The planner's verdict for one video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivationDecision {
  /// Nothing to produce: a compat copy already exists, or the native file
  /// is itself universally playable.
  NotNeeded,
  /// Produce a compat copy this way.
  Produce(CompatPlan),
  /// The codec facts are unknown (probe failed definitively), so no
  /// sensible ffmpeg invocation exists.  The item serves compat-or-bust.
  Unplannable,
}

/// The codec facts the decision reads, as stored in the index.
#[derive(Debug, Clone, Default)]
pub struct CompatInputs<'a> {
  pub container: &'a str,
  pub vcodec: Option<&'a str>,
  pub vprofile: Option<&'a str>,
  pub acodec: Option<&'a str>,
  pub field_order: Option<&'a str>,
  pub has_compat: bool,
}

/// Decide what (if anything) to derive for a video.
///
/// Codec names arrive in two vocabularies — ffprobe's bare names (`h264`,
/// `aac`) for probed files and RFC 6381 strings (`avc1.640028`,
/// `mp4a.40.2`) from yt-dlp's `info.json` — and both are recognized.
pub fn decide(inputs: &CompatInputs<'_>) -> DerivationDecision {
  if inputs.has_compat {
    return DerivationDecision::NotNeeded;
  }
  let Some(vcodec) = inputs.vcodec else {
    return DerivationDecision::Unplannable;
  };

  let video_safe = is_h264(vcodec)
    && !rfc_profile_is_unsafe(vcodec)
    && profile_is_browser_safe(inputs.vprofile);
  let audio_safe = inputs.acodec.is_none_or(is_aac);

  if inputs.container == "mp4" && video_safe && audio_safe {
    return DerivationDecision::NotNeeded;
  }

  DerivationDecision::Produce(CompatPlan {
    video: if video_safe {
      VideoAction::Copy
    } else {
      VideoAction::Transcode {
        deinterlace: is_interlaced(inputs.field_order),
      }
    },
    audio: if audio_safe {
      AudioAction::Copy
    } else {
      AudioAction::Transcode
    },
  })
}

fn is_h264(vcodec: &str) -> bool {
  vcodec == "h264"
    || vcodec == "avc"
    || vcodec.starts_with("avc1")
    || vcodec.starts_with("avc3")
}

fn is_aac(acodec: &str) -> bool {
  acodec == "aac" || acodec.starts_with("mp4a")
}

/// An RFC 6381 `avc1.PPCCLL` string names its profile_idc in the first hex
/// byte; 0x6E (High 10), 0x7A (High 4:2:2), and 0xF4 (High 4:4:4) are the
/// hardware-unfriendly ones.  Bare names have no dot and pass through.
fn rfc_profile_is_unsafe(vcodec: &str) -> bool {
  vcodec
    .split('.')
    .nth(1)
    .and_then(|tail| tail.get(0..2))
    .is_some_and(|profile_hex| {
      matches!(profile_hex.to_ascii_lowercase().as_str(), "6e" | "7a" | "f4")
    })
}

/// H.264 profiles browsers decode in hardware everywhere.  10-bit and 4:2:2
/// variants are the ones that trip real devices; an unreported profile is
/// assumed safe because the overwhelmingly common unmarked case is plain
/// 8-bit 4:2:0.
fn profile_is_browser_safe(profile: Option<&str>) -> bool {
  profile.is_none_or(|p| {
    let lowered = p.to_ascii_lowercase();
    !lowered.contains("10") && !lowered.contains("4:2:2")
  })
}

/// ffprobe's `field_order` for interlaced material: any of the
/// top/bottom-first variants.  `progressive`, unknown, and absent all mean
/// no deinterlace pass.
fn is_interlaced(field_order: Option<&str>) -> bool {
  matches!(field_order, Some("tt" | "bb" | "tb" | "bt"))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn inputs<'a>(
    container: &'a str,
    vcodec: Option<&'a str>,
    vprofile: Option<&'a str>,
    acodec: Option<&'a str>,
    field_order: Option<&'a str>,
  ) -> CompatInputs<'a> {
    CompatInputs {
      container,
      vcodec,
      vprofile,
      acodec,
      field_order,
      has_compat: false,
    }
  }

  #[test]
  fn existing_compat_means_not_needed() {
    let mut i = inputs("mkv", Some("mpeg2video"), None, Some("ac3"), None);
    i.has_compat = true;
    assert_eq!(decide(&i), DerivationDecision::NotNeeded);
  }

  #[test]
  fn universal_mp4_needs_nothing() {
    assert_eq!(
      decide(&inputs("mp4", Some("h264"), Some("High"), Some("aac"), None)),
      DerivationDecision::NotNeeded
    );
    // Video-only is fine too.
    assert_eq!(
      decide(&inputs("mp4", Some("h264"), None, None, None)),
      DerivationDecision::NotNeeded
    );
    // RFC 6381 names from info.json count the same as bare probe names.
    assert_eq!(
      decide(&inputs(
        "mp4",
        Some("avc1.640028"),
        None,
        Some("mp4a.40.2"),
        None,
      )),
      DerivationDecision::NotNeeded
    );
  }

  #[test]
  fn rfc_high10_profile_is_not_browser_safe() {
    // avc1.6E**** is profile_idc 110 (High 10).
    assert_eq!(
      decide(&inputs(
        "mp4",
        Some("avc1.6E0028"),
        None,
        Some("mp4a.40.2"),
        None,
      )),
      DerivationDecision::Produce(CompatPlan {
        video: VideoAction::Transcode { deinterlace: false },
        audio: AudioAction::Copy,
      })
    );
  }

  #[test]
  fn unknown_codecs_are_unplannable() {
    assert_eq!(
      decide(&inputs("mkv", None, None, None, None)),
      DerivationDecision::Unplannable
    );
  }

  #[test]
  fn bluray_h264_mkv_remuxes_with_audio_transcode() {
    // The BluRay fast path: stream-copy the video, re-encode AC-3 audio.
    assert_eq!(
      decide(&inputs(
        "mkv",
        Some("h264"),
        Some("High"),
        Some("ac3"),
        Some("progressive"),
      )),
      DerivationDecision::Produce(CompatPlan {
        video: VideoAction::Copy,
        audio: AudioAction::Transcode,
      })
    );
  }

  #[test]
  fn dvd_mpeg2_interlaced_transcodes_with_deinterlace() {
    assert_eq!(
      decide(
        &inputs("mkv", Some("mpeg2video"), None, Some("ac3"), Some("tt"),)
      ),
      DerivationDecision::Produce(CompatPlan {
        video: VideoAction::Transcode { deinterlace: true },
        audio: AudioAction::Transcode,
      })
    );
  }

  #[test]
  fn progressive_vc1_transcodes_without_deinterlace() {
    assert_eq!(
      decide(&inputs(
        "mkv",
        Some("vc1"),
        None,
        Some("dts"),
        Some("progressive"),
      )),
      DerivationDecision::Produce(CompatPlan {
        video: VideoAction::Transcode { deinterlace: false },
        audio: AudioAction::Transcode,
      })
    );
  }

  #[test]
  fn h264_high10_is_not_browser_safe() {
    assert_eq!(
      decide(&inputs("mp4", Some("h264"), Some("High 10"), Some("aac"), None,)),
      DerivationDecision::Produce(CompatPlan {
        video: VideoAction::Transcode { deinterlace: false },
        audio: AudioAction::Copy,
      })
    );
  }

  #[test]
  fn vp9_webm_gets_a_compat_for_safari() {
    // Capable browsers keep playing the native via its type hint; the
    // compat exists for the rest.
    assert_eq!(
      decide(&inputs("webm", Some("vp9"), None, Some("opus"), None)),
      DerivationDecision::Produce(CompatPlan {
        video: VideoAction::Transcode { deinterlace: false },
        audio: AudioAction::Transcode,
      })
    );
  }

  #[test]
  fn av1_in_mp4_still_derives() {
    assert_eq!(
      decide(&inputs("mp4", Some("av1"), None, Some("opus"), None)),
      DerivationDecision::Produce(CompatPlan {
        video: VideoAction::Transcode { deinterlace: false },
        audio: AudioAction::Transcode,
      })
    );
  }
}
