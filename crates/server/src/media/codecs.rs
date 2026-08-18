//! Codec-to-MIME derivation for the player's `<source type>` hint.
//!
//! The input codec names come from either yt-dlp's `info.json`
//! (`vcodec`/`acodec`) or ffprobe's `codec_name` — both emit either full
//! RFC 6381 strings or the same bare names, so one mapping serves both.

/// Build the `<source type>` string the player should advertise for the native
/// file, or `None` when the native should not be offered as a typed source.
///
/// The value is a web-standard container MIME plus an RFC 6381 codecs
/// parameter, which is what lets a browser's `canPlayType` decide up front
/// whether it can play the file.  A container MIME alone cannot: `video/mp4`
/// says nothing about whether the bytes inside are H.264 (which Safari
/// hardware-decodes) or AV1 (which most Safari builds cannot decode at all), so
/// Safari would commit to the source and download it before failing.  Naming
/// the codec makes the rejection happen before any bytes move.
///
/// Non-standard containers (MKV, AVI, MOV) have no MIME `canPlayType` accepts,
/// and an unrecognized codec cannot be named honestly, so both return `None` —
/// the player then serves only the compat copy rather than risk a wrong hint.
pub(crate) fn native_source_type(
  ext: &str,
  vcodec: Option<&str>,
  acodec: Option<&str>,
) -> Option<String> {
  let container = match ext {
    "mp4" => "video/mp4",
    "webm" => "video/webm",
    _ => return None,
  };
  // The video codec is the discriminating signal, so a missing or unrecognized
  // one drops the native source entirely rather than emitting a bare container
  // type that reintroduces the download-then-fail behavior.
  let video = video_codec_token(vcodec?)?;
  let codecs = match acodec.and_then(audio_codec_token) {
    Some(audio) => format!("{video}, {audio}"),
    None => video,
  };
  Some(format!("{container}; codecs=\"{codecs}\""))
}

/// Map a yt-dlp `vcodec` value to an RFC 6381 codecs token.
///
/// Modern yt-dlp already emits the full form (`avc1.640028`, `av01.0.05M.08`,
/// `vp09.00.10.08`); that carries the exact profile and level, so pass it
/// through unchanged.  Bare names are the rare fallback: map them to a value
/// the browser accepts, honest about *which* codec even if not its exact
/// profile — the profile in a `type` hint only gates selection, and the browser
/// decodes whatever the file actually contains once selected.
fn video_codec_token(vcodec: &str) -> Option<String> {
  if vcodec.contains('.') {
    Some(vcodec.to_string())
  } else {
    match vcodec {
      // Bare vp8/vp9 name no profile, which is safest: the browser plays any
      // profile the file contains rather than being told a specific one.
      "vp8" => Some("vp8".to_string()),
      "vp9" => Some("vp9".to_string()),
      // A bare "av1" is not a valid token, so synthesize a representative Main
      // profile string; AV1-incapable browsers reject any av01.* form anyway.
      "av1" => Some("av01.0.05M.08".to_string()),
      "h264" | "avc" | "avc1" => Some("avc1.42E01E".to_string()),
      "h265" | "hevc" | "hvc1" | "hev1" => Some("hvc1.1.6.L93.B0".to_string()),
      _ => None,
    }
  }
}

/// Map a yt-dlp `acodec` value to an RFC 6381 codecs token, or `None` to omit
/// the audio token (a video-only codecs string is still valid).  Omitting an
/// unknown audio codec is safe: the video codec already drives the browser's
/// accept/reject decision for the container.
fn audio_codec_token(acodec: &str) -> Option<String> {
  if acodec.contains('.') {
    Some(acodec.to_string())
  } else {
    match acodec {
      "aac" => Some("mp4a.40.2".to_string()),
      "opus" => Some("opus".to_string()),
      "vorbis" => Some("vorbis".to_string()),
      _ => None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn native_type_passes_through_full_rfc6381_codecs() {
    // Modern yt-dlp emits the exact profile/level string; it must survive
    // verbatim so canPlayType matches the real file.
    assert_eq!(
      native_source_type("mp4", Some("avc1.640028"), Some("mp4a.40.2")),
      Some(r#"video/mp4; codecs="avc1.640028, mp4a.40.2""#.to_string()),
    );
    assert_eq!(
      native_source_type("mp4", Some("av01.0.05M.08"), Some("opus")),
      Some(r#"video/mp4; codecs="av01.0.05M.08, opus""#.to_string()),
    );
    assert_eq!(
      native_source_type("webm", Some("vp09.00.10.08"), Some("opus")),
      Some(r#"video/webm; codecs="vp09.00.10.08, opus""#.to_string()),
    );
  }

  #[test]
  fn native_type_maps_bare_codec_names() {
    assert_eq!(
      native_source_type("webm", Some("vp9"), Some("opus")),
      Some(r#"video/webm; codecs="vp9, opus""#.to_string()),
    );
    assert_eq!(
      native_source_type("mp4", Some("h264"), Some("aac")),
      Some(r#"video/mp4; codecs="avc1.42E01E, mp4a.40.2""#.to_string()),
    );
  }

  #[test]
  fn native_type_omits_unknown_audio_but_keeps_video() {
    // An unrecognized audio codec should not sink the whole hint; the video
    // codec alone still lets the browser decide on the container.
    assert_eq!(
      native_source_type("mp4", Some("avc1.640028"), Some("flac")),
      Some(r#"video/mp4; codecs="avc1.640028""#.to_string()),
    );
  }

  #[test]
  fn native_type_absent_for_non_standard_container() {
    // MKV/AVI/MOV have no MIME canPlayType accepts, so no native source is
    // offered regardless of the codecs inside.
    assert_eq!(
      native_source_type("mkv", Some("avc1.640028"), Some("aac")),
      None
    );
    assert_eq!(
      native_source_type("mov", Some("avc1.640028"), Some("aac")),
      None
    );
    assert_eq!(
      native_source_type("avi", Some("avc1.640028"), Some("aac")),
      None
    );
  }

  #[test]
  fn native_type_absent_when_video_codec_unknown_or_missing() {
    // Without a nameable video codec the player must fall back to the compat
    // copy rather than emit a bare container type.
    assert_eq!(native_source_type("mp4", None, Some("aac")), None);
    assert_eq!(native_source_type("mp4", Some("theora"), Some("aac")), None);
  }
}
