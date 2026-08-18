//! Kodi `.nfo` sidecar parsing.
//!
//! `.nfo` files are XML despite the extension, and there is no formal schema:
//! Kodi, tinyMediaManager, FileBot, and Sonarr/Radarr each emit different
//! subsets, and real-world files carry BOMs, stray HTML entities, and the
//! occasional file that is just a bare URL or database id.  The parser here
//! is therefore deliberately liberal: nearly every field is optional or
//! repeated, unparseable numeric fields degrade to absent, and unknown
//! elements are ignored.  Loku only needs this tolerance for NFOs written by
//! *other* tools; the ones Loku will write follow the canonical subset these
//! types model.
//!
//! Callers are expected to read the file as lossy UTF-8 before handing it
//! here, which is how mixed or broken encodings are tolerated.

use serde::{Deserialize, Deserializer};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

/// A parsed `.nfo` sidecar, branched on the document's root element.
#[derive(Debug, Clone, PartialEq)]
pub enum Nfo {
  Movie(MovieNfo),
  Episode(EpisodeNfo),
  TvShow(TvShowNfo),
  /// The degenerate foreign case: the whole "NFO" is a bare URL or an
  /// IMDb/TMDB id rather than XML.  Some scrapers emit these as breadcrumbs
  /// for other scrapers to follow.
  Reference(String),
}

impl Nfo {
  /// The best display title the sidecar carries, if any.
  pub fn title(&self) -> Option<&str> {
    match self {
      Nfo::Movie(m) => m.title.as_deref(),
      Nfo::Episode(e) => e.title.as_deref(),
      Nfo::TvShow(t) => t.title.as_deref(),
      Nfo::Reference(_) => None,
    }
  }
}

/// `<movie>` document contents.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct MovieNfo {
  pub title: Option<String>,
  pub originaltitle: Option<String>,
  pub sorttitle: Option<String>,
  #[serde(deserialize_with = "forgiving_number")]
  pub year: Option<i32>,
  pub premiered: Option<String>,
  /// Runtime in minutes, per Kodi convention.
  #[serde(deserialize_with = "forgiving_number")]
  pub runtime: Option<u32>,
  pub plot: Option<String>,
  pub outline: Option<String>,
  pub tagline: Option<String>,
  pub mpaa: Option<String>,
  #[serde(rename = "genre")]
  pub genres: Vec<String>,
  #[serde(rename = "studio")]
  pub studios: Vec<String>,
  #[serde(rename = "country")]
  pub countries: Vec<String>,
  #[serde(rename = "director")]
  pub directors: Vec<String>,
  #[serde(rename = "actor")]
  pub actors: Vec<Actor>,
  #[serde(rename = "uniqueid")]
  pub unique_ids: Vec<UniqueId>,
}

/// `<episodedetails>` document contents.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct EpisodeNfo {
  pub title: Option<String>,
  pub showtitle: Option<String>,
  #[serde(deserialize_with = "forgiving_number")]
  pub season: Option<i32>,
  #[serde(deserialize_with = "forgiving_number")]
  pub episode: Option<i32>,
  pub aired: Option<String>,
  pub plot: Option<String>,
  /// Runtime in minutes, per Kodi convention.
  #[serde(deserialize_with = "forgiving_number")]
  pub runtime: Option<u32>,
  #[serde(rename = "actor")]
  pub actors: Vec<Actor>,
  #[serde(rename = "uniqueid")]
  pub unique_ids: Vec<UniqueId>,
}

/// `<tvshow>` document contents (the show-level `tvshow.nfo`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct TvShowNfo {
  pub title: Option<String>,
  pub originaltitle: Option<String>,
  #[serde(deserialize_with = "forgiving_number")]
  pub year: Option<i32>,
  pub premiered: Option<String>,
  pub plot: Option<String>,
  pub mpaa: Option<String>,
  #[serde(rename = "genre")]
  pub genres: Vec<String>,
  #[serde(rename = "studio")]
  pub studios: Vec<String>,
  #[serde(rename = "actor")]
  pub actors: Vec<Actor>,
  #[serde(rename = "uniqueid")]
  pub unique_ids: Vec<UniqueId>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Actor {
  pub name: Option<String>,
  pub role: Option<String>,
  pub thumb: Option<String>,
  #[serde(deserialize_with = "forgiving_number")]
  pub order: Option<u32>,
}

/// A `<uniqueid type="tmdb" default="true">603</uniqueid>` element.  The
/// attribute values are kept as strings — real files put arbitrary casing and
/// junk in them, and consumers only ever compare them.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct UniqueId {
  #[serde(rename = "@type")]
  pub id_type: Option<String>,
  #[serde(rename = "@default")]
  pub default: Option<String>,
  #[serde(rename = "$text")]
  pub value: Option<String>,
}

#[derive(Debug, Error)]
pub enum NfoError {
  #[error("NFO file is empty")]
  Empty,

  #[error("NFO has no XML root element")]
  MissingRoot,

  #[error(
    "NFO root element '{root}' is not a recognized Kodi document type \
     (movie, episodedetails, tvshow)"
  )]
  UnrecognizedRoot { root: String },

  #[error("Failed to parse NFO '<{root}>' document: {source}")]
  XmlParse {
    root: String,
    source: quick_xml::DeError,
  },
}

/// Parse an `.nfo` sidecar's contents.
///
/// Tolerances, in order of application: a leading BOM is stripped; a document
/// with no `<` at all is treated as a bare URL/id reference; ampersands that
/// do not begin a valid XML entity (stray `&`, HTML entities like `&nbsp;`)
/// are escaped so they cannot fail the XML parse; and the document is then
/// branched on its root element.
pub fn parse_nfo(input: &str) -> Result<Nfo, NfoError> {
  let content = input.trim_start_matches('\u{feff}').trim();
  if content.is_empty() {
    return Err(NfoError::Empty);
  }
  if !content.contains('<') {
    return Ok(Nfo::Reference(reference_line(content)));
  }
  let sanitized = sanitize_entities(content);
  let root = root_element(&sanitized).ok_or(NfoError::MissingRoot)?;
  let xml_error = {
    let root = root.clone();
    move |source| NfoError::XmlParse { root, source }
  };
  match root.to_ascii_lowercase().as_str() {
    "movie" => quick_xml::de::from_str(&sanitized)
      .map(Nfo::Movie)
      .map_err(xml_error),
    "episodedetails" => quick_xml::de::from_str(&sanitized)
      .map(Nfo::Episode)
      .map_err(xml_error),
    "tvshow" => quick_xml::de::from_str(&sanitized)
      .map(Nfo::TvShow)
      .map_err(xml_error),
    _ => Err(NfoError::UnrecognizedRoot { root }),
  }
}

/// The Kodi lookup rule for a video's own NFO, in precedence order: the
/// video's basename with `.nfo` (compound extensions keep their full stem,
/// matching Loku's sidecar convention), then a folder-level `movie.nfo`.
pub fn nfo_candidates(video_path: &Path) -> Vec<PathBuf> {
  let parent = video_path.parent().unwrap_or(Path::new(""));
  video_path
    .file_stem()
    .map(|stem| {
      let mut name = stem.to_os_string();
      name.push(".nfo");
      parent.join(name)
    })
    .into_iter()
    .chain(std::iter::once(parent.join("movie.nfo")))
    .collect()
}

/// The Kodi location for series-level metadata: `tvshow.nfo` at the
/// show-folder level.
pub fn tvshow_nfo_path(show_dir: &Path) -> PathBuf {
  show_dir.join("tvshow.nfo")
}

/// The first non-empty line of a bare-reference NFO.  The caller has already
/// established the content is non-empty, but the fallback keeps this total
/// rather than asserting that invariant at a distance.
fn reference_line(content: &str) -> String {
  content
    .lines()
    .map(str::trim)
    .find(|line| !line.is_empty())
    .unwrap_or(content)
    .to_string()
}

/// The name of the first start (or empty) element in the document, skipping
/// prolog noise.  `None` when the document ends or breaks before one appears.
fn root_element(xml: &str) -> Option<String> {
  use quick_xml::events::Event;
  let mut reader = quick_xml::Reader::from_str(xml);
  loop {
    match reader.read_event() {
      Ok(Event::Start(e) | Event::Empty(e)) => {
        return Some(String::from_utf8_lossy(e.name().as_ref()).to_string());
      }
      Ok(Event::Eof) | Err(_) => return None,
      Ok(_) => {}
    }
  }
}

/// Escape every `&` that does not begin one of XML's five named entities or a
/// numeric character reference.  This turns stray ampersands and HTML-only
/// entities (`&nbsp;` and friends) into literal text instead of parse
/// failures; valid documents pass through unchanged.
fn sanitize_entities(input: &str) -> String {
  let mut out = String::with_capacity(input.len());
  let mut rest = input;
  while let Some(pos) = rest.find('&') {
    out.push_str(&rest[..pos]);
    let after = &rest[pos + 1..];
    if valid_entity_prefix(after) {
      out.push('&');
    } else {
      out.push_str("&amp;");
    }
    rest = after;
  }
  out.push_str(rest);
  out
}

/// Whether the text immediately following an `&` begins a valid XML entity:
/// one of the five named entities, `&#...;` decimal, or `&#x...;` hex.  The
/// numeric-body length cap only bounds the search; real references are short.
fn valid_entity_prefix(rest: &str) -> bool {
  ["amp;", "lt;", "gt;", "apos;", "quot;"]
    .iter()
    .any(|e| rest.starts_with(e))
    || rest
      .find(';')
      .filter(|end| *end >= 2 && *end <= 10)
      .map(|end| &rest[..end])
      .is_some_and(|body| {
        body
          .strip_prefix("#x")
          .or_else(|| body.strip_prefix("#X"))
          .map_or_else(
            || {
              body.strip_prefix('#').is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
              })
            },
            |hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()),
          )
      })
}

/// Deserialize a numeric element liberally: absent, empty, or junk values
/// all degrade to `None` rather than failing the whole document, per the
/// liberal-parse policy this module exists for.
fn forgiving_number<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
  D: Deserializer<'de>,
  T: FromStr,
{
  // The .ok() discard is the policy, not an accident: unparseable numerics
  // are the other tools' mess, not an error worth failing the document over.
  Option::<String>::deserialize(deserializer)
    .map(|opt| opt.and_then(|s| s.trim().parse().ok()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_kodi_movie_document() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<movie>
  <title>The Matrix</title>
  <originaltitle>The Matrix</originaltitle>
  <year>1999</year>
  <runtime>136</runtime>
  <plot>A computer hacker learns the truth.</plot>
  <mpaa>R</mpaa>
  <genre>Action</genre>
  <genre>Science Fiction</genre>
  <studio>Warner Bros.</studio>
  <uniqueid type="tmdb" default="true">603</uniqueid>
  <uniqueid type="imdb">tt0133093</uniqueid>
  <actor>
    <name>Keanu Reeves</name>
    <role>Neo</role>
    <order>0</order>
  </actor>
</movie>"#;
    let Nfo::Movie(movie) = parse_nfo(xml).unwrap() else {
      panic!("expected a movie");
    };
    assert_eq!(movie.title.as_deref(), Some("The Matrix"));
    assert_eq!(movie.year, Some(1999));
    assert_eq!(movie.runtime, Some(136));
    assert_eq!(movie.genres, vec!["Action", "Science Fiction"]);
    assert_eq!(movie.studios, vec!["Warner Bros."]);
    assert_eq!(movie.unique_ids.len(), 2);
    assert_eq!(movie.unique_ids[0].id_type.as_deref(), Some("tmdb"));
    assert_eq!(movie.unique_ids[0].default.as_deref(), Some("true"));
    assert_eq!(movie.unique_ids[0].value.as_deref(), Some("603"));
    assert_eq!(movie.actors.len(), 1);
    assert_eq!(movie.actors[0].name.as_deref(), Some("Keanu Reeves"));
    assert_eq!(movie.actors[0].order, Some(0));
  }

  #[test]
  fn parses_episode_document() {
    let xml = r#"<episodedetails>
  <title>Pilot</title>
  <showtitle>Some Show</showtitle>
  <season>1</season>
  <episode>1</episode>
  <aired>2010-04-17</aired>
  <uniqueid type="tvdb">1234567</uniqueid>
</episodedetails>"#;
    let Nfo::Episode(episode) = parse_nfo(xml).unwrap() else {
      panic!("expected an episode");
    };
    assert_eq!(episode.title.as_deref(), Some("Pilot"));
    assert_eq!(episode.season, Some(1));
    assert_eq!(episode.episode, Some(1));
    assert_eq!(episode.unique_ids[0].id_type.as_deref(), Some("tvdb"));
  }

  #[test]
  fn parses_tvshow_document() {
    let xml = r#"<tvshow>
  <title>Some Show</title>
  <year>2010</year>
  <genre>Drama</genre>
</tvshow>"#;
    let Nfo::TvShow(show) = parse_nfo(xml).unwrap() else {
      panic!("expected a tvshow");
    };
    assert_eq!(show.title.as_deref(), Some("Some Show"));
    assert_eq!(show.year, Some(2010));
    assert_eq!(show.genres, vec!["Drama"]);
  }

  #[test]
  fn tolerates_bom_stray_ampersands_and_html_entities() {
    let xml = "\u{feff}<movie>\n  <title>Fast & Furious</title>\n  \
               <plot>Cars&nbsp;go fast &amp; loud.</plot>\n</movie>";
    let Nfo::Movie(movie) = parse_nfo(xml).unwrap() else {
      panic!("expected a movie");
    };
    assert_eq!(movie.title.as_deref(), Some("Fast & Furious"));
    assert_eq!(movie.plot.as_deref(), Some("Cars&nbsp;go fast & loud."));
  }

  #[test]
  fn junk_numeric_fields_degrade_to_absent() {
    let xml = "<movie><title>X</title><year></year><runtime>about 90\
               </runtime></movie>";
    let Nfo::Movie(movie) = parse_nfo(xml).unwrap() else {
      panic!("expected a movie");
    };
    assert_eq!(movie.title.as_deref(), Some("X"));
    assert_eq!(movie.year, None);
    assert_eq!(movie.runtime, None);
  }

  #[test]
  fn unknown_elements_are_ignored() {
    let xml = "<movie><title>X</title><fileinfo><streamdetails><video>\
               <codec>h264</codec></video></streamdetails></fileinfo></movie>";
    let Nfo::Movie(movie) = parse_nfo(xml).unwrap() else {
      panic!("expected a movie");
    };
    assert_eq!(movie.title.as_deref(), Some("X"));
  }

  #[test]
  fn bare_url_becomes_reference() {
    let nfo =
      parse_nfo("https://www.themoviedb.org/movie/603-the-matrix\n").unwrap();
    assert_eq!(
      nfo,
      Nfo::Reference(
        "https://www.themoviedb.org/movie/603-the-matrix".to_string()
      )
    );
  }

  #[test]
  fn bare_imdb_id_becomes_reference() {
    assert_eq!(
      parse_nfo("tt0133093").unwrap(),
      Nfo::Reference("tt0133093".to_string())
    );
  }

  #[test]
  fn empty_input_is_an_error() {
    assert!(matches!(parse_nfo("   \n  "), Err(NfoError::Empty)));
  }

  #[test]
  fn unrecognized_root_is_an_error() {
    assert!(matches!(
      parse_nfo("<musicvideo><title>X</title></musicvideo>"),
      Err(NfoError::UnrecognizedRoot { .. })
    ));
  }

  #[test]
  fn nfo_candidates_follow_kodi_precedence() {
    let candidates = nfo_candidates(Path::new("/lib/Movie (1999).mkv"));
    assert_eq!(
      candidates,
      vec![
        PathBuf::from("/lib/Movie (1999).nfo"),
        PathBuf::from("/lib/movie.nfo"),
      ]
    );
  }

  #[test]
  fn nfo_candidates_keep_compound_extensions() {
    let candidates = nfo_candidates(Path::new("/lib/clip.mov.webm"));
    assert_eq!(candidates[0], PathBuf::from("/lib/clip.mov.nfo"));
  }

  #[test]
  fn tvshow_nfo_sits_at_show_level() {
    assert_eq!(
      tvshow_nfo_path(Path::new("/lib/Some Show")),
      PathBuf::from("/lib/Some Show/tvshow.nfo")
    );
  }
}
