//! From disc labels to search queries, and from resolved titles to
//! directory names.
//!
//! A volume label such as `VOLTRON_S1_D1` carries the title plus packaging
//! structure (season, disc number, format markers).  `title_hint` strips the
//! structure into a clean query and keeps what it learned — a season marker
//! is also a strong hint that this is a series, and the disc number is what
//! keeps two discs of one box set apart once they share a resolved title.

use crate::identity::TmdbKind;

/// What a disc's own naming says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleHint {
  /// The cleaned search query.
  pub query: String,
  /// The source string, untouched, for display.
  pub raw: String,
  pub year: Option<i32>,
  pub kind_hint: Option<TmdbKind>,
  pub season: Option<u32>,
  pub disc_number: Option<u32>,
}

/// Byte cap for a resolved directory name's title half; keeps
/// `<Title (Year)>__<id>` inside every filesystem's name limit.
const DIR_TITLE_MAX_BYTES: usize = 120;

/// The first usable hint in precedence order: MakeMKV's disc name (Blu-rays
/// often carry the real title there), then its volume name, then the udev
/// label.  A source whose cleaned query is empty (`LOGICAL_VOLUME_ID`) is
/// skipped in favour of the next.
pub fn title_hint(
  disc_name: Option<&str>,
  volume_name: Option<&str>,
  label: Option<&str>,
) -> Option<TitleHint> {
  [disc_name, volume_name, label]
    .into_iter()
    .flatten()
    .map(clean_query)
    .find(|hint| !hint.query.is_empty())
}

/// Markers whose following bare number belongs to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
  Season,
  Disc,
  Volume,
}

#[derive(Debug, Default)]
struct Cleaning {
  kept: Vec<String>,
  season: Option<u32>,
  disc_number: Option<u32>,
  tv: bool,
  pending: Option<Marker>,
}

/// Strip packaging structure from a label into a search query.  Structural
/// tokens (disc and season markers, format tags, udev filler) are dropped;
/// `S1`/`D2`-style tokens and a bare number after a marker are read as the
/// season or disc number; a trailing four-digit year leaves the query and
/// becomes a hint, unless it is the only token (`2001`, the film).
pub fn clean_query(raw: &str) -> TitleHint {
  let cleaned = raw
    .replace(['_', '.', '-'], " ")
    .split_whitespace()
    .fold(Cleaning::default(), fold_token);
  let (kept, year) = split_trailing_year(cleaned.kept);
  TitleHint {
    query: kept.join(" "),
    raw: raw.to_string(),
    year,
    kind_hint: cleaned.tv.then_some(TmdbKind::Tv),
    season: cleaned.season,
    disc_number: cleaned.disc_number,
  }
}

fn fold_token(mut state: Cleaning, token: &str) -> Cleaning {
  let lower = token.to_ascii_lowercase();
  // A pending marker claims a following bare number; whether or not one
  // follows, the marker is spent.
  if let Some((marker, number)) = state.pending.take().zip(small_number(&lower))
  {
    state.assign(marker, number);
  } else {
    match lower.as_str() {
      "disc" | "disk" => state.pending = Some(Marker::Disc),
      "season" | "series" => {
        state.tv = true;
        state.pending = Some(Marker::Season);
      }
      "vol" | "volume" => state.pending = Some(Marker::Volume),
      "dvd" | "bd" | "bluray" | "blu" | "ray" | "ntsc" | "pal" | "ws"
      | "fs" | "uhd" | "4k" | "1080p" | "720p" | "logical" | "id" => {}
      _ => match marker_with_number(&lower) {
        Some((Marker::Season, season, disc)) => {
          state.tv = true;
          state.season = Some(season);
          state.disc_number = disc.or(state.disc_number);
        }
        Some((marker, number, _)) => state.assign(marker, number),
        None => state.kept.push(token.to_string()),
      },
    }
  }
  state
}

impl Cleaning {
  fn assign(&mut self, marker: Marker, number: u32) {
    match marker {
      Marker::Season => self.season = Some(number),
      Marker::Disc | Marker::Volume => self.disc_number = Some(number),
    }
  }
}

/// A one- or two-digit token.
fn small_number(token: &str) -> Option<u32> {
  // The .ok() discard is deliberate: a non-numeric token is simply not a
  // number, which is the question being asked.
  (matches!(token.len(), 1 | 2) && token.bytes().all(|b| b.is_ascii_digit()))
    .then(|| token.parse().ok())
    .flatten()
}

/// `s1`, `d2`, `s1d2`, `disc3`, `season2`: a marker fused with its number.
fn marker_with_number(token: &str) -> Option<(Marker, u32, Option<u32>)> {
  let (marker, rest) = [
    ("season", Marker::Season),
    ("disc", Marker::Disc),
    ("s", Marker::Season),
    ("d", Marker::Disc),
  ]
  .into_iter()
  .find_map(|(prefix, marker)| {
    token.strip_prefix(prefix).map(|rest| (marker, rest))
  })?;
  let (number, tail) =
    rest.split_at(rest.bytes().take_while(u8::is_ascii_digit).count());
  let number = small_number(number)?;
  match (marker, tail) {
    (_, "") => Some((marker, number, None)),
    (Marker::Season, tail) => tail
      .strip_prefix('d')
      .and_then(small_number)
      .map(|disc| (Marker::Season, number, Some(disc))),
    _ => None,
  }
}

/// A trailing four-digit year leaves the query when other words remain.
fn split_trailing_year(mut kept: Vec<String>) -> (Vec<String>, Option<i32>) {
  let year = kept
    .last()
    .filter(|_| kept.len() > 1)
    // The .ok() discard is deliberate: a non-numeric last word is a title
    // word, not a year.
    .and_then(|last| last.parse::<i32>().ok())
    .filter(|year| (1900..=2099).contains(year));
  if year.is_some() {
    kept.pop();
  }
  (kept, year)
}

/// Titles reduced to their comparable core: lowercase, `&` read as "and",
/// punctuation gone, whitespace collapsed.
pub fn normalized_title(title: &str) -> String {
  title
    .to_lowercase()
    .replace('&', " and ")
    .chars()
    .map(|c| if c.is_alphanumeric() { c } else { ' ' })
    .collect::<String>()
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

/// A resolved title as a directory-name component: path-hostile characters
/// stripped, whitespace and underscores collapsed (so the `__` separator
/// stays unambiguous), never hidden, capped at a char boundary.  Unicode
/// survives; this is a name people read.
pub fn sanitize_dir_component(title: &str) -> String {
  let cleaned = title
    .chars()
    .filter(|c| {
      !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    })
    .filter(|c| !c.is_control())
    .collect::<String>()
    .split(|c: char| c.is_whitespace() || c == '_')
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
  let trimmed = cleaned.trim_matches(['.', ' ']);
  let capped = trimmed
    .char_indices()
    .take_while(|(index, c)| index + c.len_utf8() <= DIR_TITLE_MAX_BYTES)
    .map(|(_, c)| c)
    .collect::<String>();
  let capped = capped.trim_end_matches(['.', ' ']);
  if capped.is_empty() {
    "Untitled".to_string()
  } else {
    capped.to_string()
  }
}

/// `<Title (Year)>[ - Season n][ - Disc n]__<id>`: the directory a resolved
/// disc is renamed to.  The season and disc suffixes come from the disc's
/// own label, so two discs of one box set stay tellable apart by name and
/// not only by id.
pub fn resolved_dir_name(
  title: &str,
  year: Option<i32>,
  hint: Option<&TitleHint>,
  disc_id: &str,
) -> String {
  let name: String = [
    Some(sanitize_dir_component(title)),
    year.map(|year| format!(" ({year})")),
    hint
      .and_then(|hint| hint.season)
      .map(|season| format!(" - Season {season}")),
    hint
      .and_then(|hint| hint.disc_number)
      .map(|disc| format!(" - Disc {disc}")),
  ]
  .into_iter()
  .flatten()
  .collect();
  format!("{name}__{disc_id}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn season_and_disc_markers_are_read_and_dropped() {
    let hint = clean_query("VOLTRON_S1_D1");
    assert_eq!(hint.query, "VOLTRON");
    assert_eq!(hint.kind_hint, Some(TmdbKind::Tv));
    assert_eq!(hint.season, Some(1));
    assert_eq!(hint.disc_number, Some(1));
    assert_eq!(hint.raw, "VOLTRON_S1_D1");

    let hint = clean_query("CHAMBER_OF_SECRETS_DISC_2");
    assert_eq!(hint.query, "CHAMBER OF SECRETS");
    assert_eq!(hint.disc_number, Some(2));
    assert_eq!(hint.kind_hint, None);

    let hint = clean_query("The Office Season 3 Disc 4");
    assert_eq!(hint.query, "The Office");
    assert_eq!(hint.season, Some(3));
    assert_eq!(hint.disc_number, Some(4));

    let hint = clean_query("SHOW_S2D3");
    assert_eq!((hint.season, hint.disc_number), (Some(2), Some(3)));
  }

  #[test]
  fn trailing_years_become_hints_unless_alone() {
    let hint = clean_query("THE_MATRIX_1999");
    assert_eq!(hint.query, "THE MATRIX");
    assert_eq!(hint.year, Some(1999));
    let hint = clean_query("2001_A_SPACE_ODYSSEY");
    assert_eq!(hint.query, "2001 A SPACE ODYSSEY");
    assert_eq!(hint.year, None);
    let hint = clean_query("1984");
    assert_eq!(hint.query, "1984");
    assert_eq!(hint.year, None);
  }

  #[test]
  fn filler_labels_fall_through_to_the_next_source() {
    assert_eq!(clean_query("LOGICAL_VOLUME_ID").query, "");
    let hint =
      title_hint(None, Some("LOGICAL_VOLUME_ID"), Some("REAL_TITLE")).unwrap();
    assert_eq!(hint.query, "REAL TITLE");
    assert_eq!(hint.raw, "REAL_TITLE");
    assert_eq!(title_hint(None, None, None), None);
    assert_eq!(
      title_hint(Some("Blu-ray Name"), Some("VOL"), None)
        .unwrap()
        .query,
      "Name",
      "the disc name wins and its format tag drops"
    );
  }

  #[test]
  fn normalization_ignores_punctuation_and_case() {
    assert_eq!(normalized_title("Fast & Furious"), "fast and furious");
    assert_eq!(normalized_title("WALL·E"), "wall e");
    assert_eq!(normalized_title("  The   Matrix! "), "the matrix");
  }

  #[test]
  fn directory_components_drop_hostile_characters_only() {
    assert_eq!(sanitize_dir_component("Face/Off"), "FaceOff");
    assert_eq!(sanitize_dir_component("Who? What: \"Why\""), "Who What Why");
    assert_eq!(sanitize_dir_component("Amélie"), "Amélie");
    assert_eq!(sanitize_dir_component("a__b   c"), "a b c");
    assert_eq!(sanitize_dir_component(".hidden."), "hidden");
    assert_eq!(sanitize_dir_component("///"), "Untitled");
    let long = "é".repeat(100);
    let capped = sanitize_dir_component(&long);
    assert!(capped.len() <= DIR_TITLE_MAX_BYTES);
    assert_eq!(capped.chars().count(), 60, "cut at a char boundary");
  }

  #[test]
  fn resolved_names_carry_year_season_and_disc() {
    let hint = clean_query("VOLTRON_S1_D2");
    assert_eq!(
      resolved_dir_name(
        "Voltron: Legendary Defender",
        Some(2016),
        Some(&hint),
        "uuid"
      ),
      "Voltron Legendary Defender (2016) - Season 1 - Disc 2__uuid"
    );
    assert_eq!(
      resolved_dir_name("The Matrix", None, None, "abc"),
      "The Matrix__abc"
    );
  }
}
