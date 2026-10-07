//! The TMDB client: search, details, and artwork, behind a dyn-safe trait
//! so tests inject a fake.  Only the fields Loku uses are deserialized;
//! everything else TMDB returns is ignored.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tap::Tap;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::identity::TmdbKind;

const API_BASE: &str = "https://api.themoviedb.org/3";
const IMAGE_BASE: &str = "https://image.tmdb.org/t/p";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Minimum spacing between requests; TMDB tolerates far more, but a disc
/// resolution is a handful of calls and politeness costs nothing.
const REQUEST_SPACING: Duration = Duration::from_millis(250);
/// How much of an unexpected response body an error carries.
const BODY_TAIL_CHARS: usize = 300;

/// One search hit, as stored for the operator's confirmation and as the API
/// reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Candidate {
  #[serde(rename = "tmdb_kind")]
  pub kind: TmdbKind,
  #[serde(rename = "tmdb_id")]
  pub id: u64,
  pub title: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub original_title: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub year: Option<i32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub overview: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub poster_path: Option<String>,
  #[serde(default)]
  pub popularity: f64,
  #[serde(default)]
  pub vote_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastMember {
  pub name: String,
  pub character: Option<String>,
  pub order: u32,
  pub profile_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MovieDetails {
  pub id: u64,
  pub title: String,
  pub original_title: Option<String>,
  pub release_date: Option<String>,
  pub overview: Option<String>,
  pub tagline: Option<String>,
  pub runtime: Option<u32>,
  pub genres: Vec<String>,
  pub studios: Vec<String>,
  pub countries: Vec<String>,
  pub poster_path: Option<String>,
  pub backdrop_path: Option<String>,
  pub imdb_id: Option<String>,
  pub cast: Vec<CastMember>,
  pub directors: Vec<String>,
  pub certification: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TvDetails {
  pub id: u64,
  pub name: String,
  pub original_name: Option<String>,
  pub first_air_date: Option<String>,
  pub overview: Option<String>,
  pub genres: Vec<String>,
  pub networks: Vec<String>,
  pub poster_path: Option<String>,
  pub backdrop_path: Option<String>,
  pub imdb_id: Option<String>,
  pub tvdb_id: Option<u64>,
  pub cast: Vec<CastMember>,
  pub creators: Vec<String>,
  pub rating: Option<String>,
}

/// TMDB's image size buckets that Loku uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageSize {
  Original,
  W1280,
  W185,
}

impl ImageSize {
  pub fn as_str(self) -> &'static str {
    match self {
      ImageSize::Original => "original",
      ImageSize::W1280 => "w1280",
      ImageSize::W185 => "w185",
    }
  }
}

/// The URL of an image at a size, for anything that shows TMDB art without
/// downloading it.
pub fn image_url(size: ImageSize, path: &str) -> String {
  format!("{IMAGE_BASE}/{}{path}", size.as_str())
}

#[derive(Debug, Error)]
pub enum TmdbError {
  #[error("Failed to build the TMDB HTTP client: {source}")]
  ClientBuild { source: reqwest::Error },

  #[error("TMDB request to {endpoint} failed: {source}")]
  Request {
    endpoint: String,
    source: reqwest::Error,
  },

  #[error("TMDB rejected the API key on {endpoint} (HTTP {status})")]
  Unauthorized { endpoint: String, status: u16 },

  #[error("TMDB rate-limited {endpoint}; retry after {retry_after_secs}s")]
  RateLimited {
    endpoint: String,
    retry_after_secs: u64,
  },

  #[error("TMDB has no {kind} with id {id}")]
  NotFound { kind: &'static str, id: u64 },

  #[error("TMDB returned HTTP {status} for {endpoint}: {body_tail}")]
  Status {
    endpoint: String,
    status: u16,
    body_tail: String,
  },

  #[error("Failed to decode the TMDB response from {endpoint}: {source}")]
  Decode {
    endpoint: String,
    source: reqwest::Error,
  },
}

impl TmdbError {
  /// Whether a retry later could succeed (network, rate limit, server
  /// trouble) as opposed to a definitive answer (bad key, no such id).
  pub fn is_transient(&self) -> bool {
    match self {
      TmdbError::Request { .. } | TmdbError::RateLimited { .. } => true,
      TmdbError::Status { status, .. } => *status >= 500,
      TmdbError::ClientBuild { .. }
      | TmdbError::Unauthorized { .. }
      | TmdbError::NotFound { .. }
      | TmdbError::Decode { .. } => false,
    }
  }
}

#[async_trait]
pub trait TmdbClient: Send + Sync {
  /// Movies and series matching the query, most popular first.
  async fn search(
    &self,
    query: &str,
    year: Option<i32>,
  ) -> Result<Vec<Candidate>, TmdbError>;

  async fn movie(&self, id: u64) -> Result<MovieDetails, TmdbError>;

  async fn tv(&self, id: u64) -> Result<TvDetails, TmdbError>;

  /// The bytes of an image at the given size.
  async fn image(
    &self,
    size: ImageSize,
    path: &str,
  ) -> Result<Vec<u8>, TmdbError>;
}

/// How the key authenticates: a v4 read-access token is a JWT (dots in it)
/// sent as a bearer; a v3 key rides the query string.
enum Auth {
  BearerV4(String),
  ApiKeyV3(String),
}

impl Auth {
  fn from_key(key: &str) -> Self {
    if key.contains('.') {
      Auth::BearerV4(key.to_string())
    } else {
      Auth::ApiKeyV3(key.to_string())
    }
  }
}

/// The real client over `api.themoviedb.org`.
pub struct RealTmdb {
  http: reqwest::Client,
  auth: Auth,
  language: String,
  region: String,
  last_request: Mutex<Option<Instant>>,
}

impl RealTmdb {
  pub fn new(api_key: &str, language: &str) -> Result<Self, TmdbError> {
    let http = reqwest::Client::builder()
      .timeout(REQUEST_TIMEOUT)
      .connect_timeout(CONNECT_TIMEOUT)
      .user_agent(format!("loku/{}", env!("CARGO_PKG_VERSION")))
      .build()
      .map_err(|source| TmdbError::ClientBuild { source })?;
    Ok(Self {
      http,
      auth: Auth::from_key(api_key.trim()),
      language: language.to_string(),
      region: region_of(language),
      last_request: Mutex::new(None),
    })
  }

  /// Wait out the spacing since the previous request.
  async fn pace(&self) {
    let mut last = self.last_request.lock().await;
    if let Some(previous) = *last {
      let elapsed = previous.elapsed();
      if elapsed < REQUEST_SPACING {
        tokio::time::sleep(REQUEST_SPACING - elapsed).await;
      }
    }
    *last = Some(Instant::now());
  }

  async fn send(
    &self,
    url: &str,
    query: &[(&str, String)],
  ) -> Result<reqwest::Response, TmdbError> {
    self.pace().await;
    let request = self.http.get(url).query(query);
    let request = match &self.auth {
      Auth::BearerV4(token) => request.bearer_auth(token),
      Auth::ApiKeyV3(key) => request.query(&[("api_key", key.as_str())]),
    };
    // A wrapped reqwest error would leak the API key: reqwest's message
    // includes the request URL, and a v3 key travels in the URL.  The
    // endpoint field already names the call, so the URL is dropped.
    let response =
      request.send().await.map_err(|source| TmdbError::Request {
        endpoint: url.to_string(),
        source: source.without_url(),
      })?;
    let status = response.status();
    if status.is_success() {
      Ok(response)
    } else if status == reqwest::StatusCode::UNAUTHORIZED {
      Err(TmdbError::Unauthorized {
        endpoint: url.to_string(),
        status: status.as_u16(),
      })
    } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
      Err(TmdbError::RateLimited {
        endpoint: url.to_string(),
        retry_after_secs: response
          .headers()
          .get(reqwest::header::RETRY_AFTER)
          // The .ok() discards are deliberate: a header that is not a plain
          // second count (an HTTP date, say) falls back to a short wait.
          .and_then(|value| value.to_str().ok())
          .and_then(|value| value.parse().ok())
          .unwrap_or(1),
      })
    } else {
      // The body is best-effort context for the status error; failing to
      // read it must not hide the status itself.
      let body = response.text().await.unwrap_or_default();
      Err(TmdbError::Status {
        endpoint: url.to_string(),
        status: status.as_u16(),
        body_tail: body.chars().take(BODY_TAIL_CHARS).collect(),
      })
    }
  }

  async fn json<T: serde::de::DeserializeOwned>(
    &self,
    endpoint: &str,
    query: &[(&str, String)],
  ) -> Result<T, TmdbError> {
    let url = format!("{API_BASE}{endpoint}");
    let query: Vec<(&str, String)> = query
      .iter()
      .cloned()
      .chain(std::iter::once(("language", self.language.clone())))
      .collect();
    self
      .send(&url, &query)
      .await?
      .json::<T>()
      .await
      .map_err(|source| TmdbError::Decode {
        endpoint: url,
        source: source.without_url(),
      })
  }
}

/// The certification region for a language tag: `en-US` → `US`; a bare
/// language falls back to the US, TMDB's most complete region.
fn region_of(language: &str) -> String {
  language
    .split_once('-')
    .map_or("US", |(_, region)| region)
    .to_ascii_uppercase()
}

#[async_trait]
impl TmdbClient for RealTmdb {
  async fn search(
    &self,
    query: &str,
    year: Option<i32>,
  ) -> Result<Vec<Candidate>, TmdbError> {
    // /search/multi returns people and accepts no year filter, so the movie
    // and TV searches are made separately.
    let base = [
      ("query", query.to_string()),
      ("include_adult", "false".to_string()),
      ("page", "1".to_string()),
    ];
    let movie_query: Vec<(&str, String)> = base
      .iter()
      .cloned()
      .chain(year.map(|year| ("year", year.to_string())))
      .collect();
    let tv_query: Vec<(&str, String)> = base
      .iter()
      .cloned()
      .chain(year.map(|year| ("first_air_date_year", year.to_string())))
      .collect();
    let movies: SearchPage<MovieResult> =
      self.json("/search/movie", &movie_query).await?;
    let shows: SearchPage<TvResult> =
      self.json("/search/tv", &tv_query).await?;
    Ok(merge_candidates(movies.results, shows.results))
  }

  async fn movie(&self, id: u64) -> Result<MovieDetails, TmdbError> {
    let json: MovieJson = self
      .json(
        &format!("/movie/{id}"),
        &[("append_to_response", "credits,release_dates".to_string())],
      )
      .await
      .map_err(|error| not_found_as(error, "movie", id))?;
    Ok(movie_details(json, &self.region))
  }

  async fn tv(&self, id: u64) -> Result<TvDetails, TmdbError> {
    let json: TvJson = self
      .json(
        &format!("/tv/{id}"),
        &[(
          "append_to_response",
          "credits,content_ratings,external_ids".to_string(),
        )],
      )
      .await
      .map_err(|error| not_found_as(error, "tv", id))?;
    Ok(tv_details(json, &self.region))
  }

  async fn image(
    &self,
    size: ImageSize,
    path: &str,
  ) -> Result<Vec<u8>, TmdbError> {
    let url = image_url(size, path);
    self
      .send(&url, &[])
      .await?
      .bytes()
      .await
      .map(|bytes| bytes.to_vec())
      .map_err(|source| TmdbError::Decode {
        endpoint: url,
        source: source.without_url(),
      })
  }
}

/// A 404 on a details endpoint means the id does not exist.
fn not_found_as(error: TmdbError, kind: &'static str, id: u64) -> TmdbError {
  match error {
    TmdbError::Status { status: 404, .. } => TmdbError::NotFound { kind, id },
    other => other,
  }
}

// ── JSON shapes (only the fields Loku reads) ────────────────────────────────

#[derive(Debug, Deserialize)]
struct SearchPage<T> {
  #[serde(default = "Vec::new")]
  results: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct MovieResult {
  id: u64,
  #[serde(default)]
  title: Option<String>,
  #[serde(default)]
  original_title: Option<String>,
  #[serde(default)]
  release_date: Option<String>,
  #[serde(default)]
  overview: Option<String>,
  #[serde(default)]
  poster_path: Option<String>,
  #[serde(default)]
  popularity: f64,
  #[serde(default)]
  vote_count: u64,
}

#[derive(Debug, Deserialize)]
struct TvResult {
  id: u64,
  #[serde(default)]
  name: Option<String>,
  #[serde(default)]
  original_name: Option<String>,
  #[serde(default)]
  first_air_date: Option<String>,
  #[serde(default)]
  overview: Option<String>,
  #[serde(default)]
  poster_path: Option<String>,
  #[serde(default)]
  popularity: f64,
  #[serde(default)]
  vote_count: u64,
}

#[derive(Debug, Deserialize)]
struct Named {
  #[serde(default)]
  name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct Credits {
  #[serde(default)]
  cast: Vec<CastJson>,
  #[serde(default)]
  crew: Vec<CrewJson>,
}

#[derive(Debug, Deserialize)]
struct CastJson {
  #[serde(default)]
  name: Option<String>,
  #[serde(default)]
  character: Option<String>,
  #[serde(default)]
  order: u32,
  #[serde(default)]
  profile_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CrewJson {
  #[serde(default)]
  name: Option<String>,
  #[serde(default)]
  job: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ReleaseDates {
  #[serde(default)]
  results: Vec<ReleaseDatesEntry>,
}

#[derive(Debug, Deserialize)]
struct ReleaseDatesEntry {
  #[serde(default)]
  iso_3166_1: String,
  #[serde(default)]
  release_dates: Vec<ReleaseDate>,
}

#[derive(Debug, Deserialize)]
struct ReleaseDate {
  #[serde(default)]
  certification: String,
}

#[derive(Debug, Deserialize)]
struct MovieJson {
  id: u64,
  #[serde(default)]
  title: Option<String>,
  #[serde(default)]
  original_title: Option<String>,
  #[serde(default)]
  release_date: Option<String>,
  #[serde(default)]
  overview: Option<String>,
  #[serde(default)]
  tagline: Option<String>,
  #[serde(default)]
  runtime: Option<u32>,
  #[serde(default)]
  imdb_id: Option<String>,
  #[serde(default)]
  poster_path: Option<String>,
  #[serde(default)]
  backdrop_path: Option<String>,
  #[serde(default)]
  genres: Vec<Named>,
  #[serde(default)]
  production_companies: Vec<Named>,
  #[serde(default)]
  production_countries: Vec<Named>,
  #[serde(default)]
  credits: Credits,
  #[serde(default)]
  release_dates: ReleaseDates,
}

#[derive(Debug, Deserialize, Default)]
struct ContentRatings {
  #[serde(default)]
  results: Vec<ContentRating>,
}

#[derive(Debug, Deserialize)]
struct ContentRating {
  #[serde(default)]
  iso_3166_1: String,
  #[serde(default)]
  rating: String,
}

#[derive(Debug, Deserialize, Default)]
struct ExternalIds {
  #[serde(default)]
  imdb_id: Option<String>,
  #[serde(default)]
  tvdb_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TvJson {
  id: u64,
  #[serde(default)]
  name: Option<String>,
  #[serde(default)]
  original_name: Option<String>,
  #[serde(default)]
  first_air_date: Option<String>,
  #[serde(default)]
  overview: Option<String>,
  #[serde(default)]
  poster_path: Option<String>,
  #[serde(default)]
  backdrop_path: Option<String>,
  #[serde(default)]
  genres: Vec<Named>,
  #[serde(default)]
  networks: Vec<Named>,
  #[serde(default)]
  created_by: Vec<Named>,
  #[serde(default)]
  credits: Credits,
  #[serde(default)]
  content_ratings: ContentRatings,
  #[serde(default)]
  external_ids: ExternalIds,
}

/// The year in a `YYYY-MM-DD` date; empty strings (TMDB's "unknown") are
/// absent.
fn year_of(date: Option<&str>) -> Option<i32> {
  // The .ok() discard is deliberate: a date TMDB could not format is the
  // same as no date.
  date.and_then(|d| d.get(..4)).and_then(|y| y.parse().ok())
}

fn non_empty(value: Option<String>) -> Option<String> {
  value.filter(|v| !v.trim().is_empty())
}

fn names(named: Vec<Named>) -> Vec<String> {
  named
    .into_iter()
    .filter_map(|n| non_empty(n.name))
    .collect()
}

fn cast(credits: &Credits) -> Vec<CastMember> {
  credits
    .cast
    .iter()
    .filter_map(|member| {
      Some(CastMember {
        name: non_empty(member.name.clone())?,
        character: non_empty(member.character.clone()),
        order: member.order,
        profile_path: member.profile_path.clone(),
      })
    })
    .collect()
}

fn merge_candidates(
  movies: Vec<MovieResult>,
  shows: Vec<TvResult>,
) -> Vec<Candidate> {
  movies
    .into_iter()
    .filter_map(|m| {
      Some(Candidate {
        kind: TmdbKind::Movie,
        id: m.id,
        title: non_empty(m.title)?,
        original_title: non_empty(m.original_title),
        year: year_of(m.release_date.as_deref()),
        overview: non_empty(m.overview),
        poster_path: m.poster_path,
        popularity: m.popularity,
        vote_count: m.vote_count,
      })
    })
    .chain(shows.into_iter().filter_map(|s| {
      Some(Candidate {
        kind: TmdbKind::Tv,
        id: s.id,
        title: non_empty(s.name)?,
        original_title: non_empty(s.original_name),
        year: year_of(s.first_air_date.as_deref()),
        overview: non_empty(s.overview),
        poster_path: s.poster_path,
        popularity: s.popularity,
        vote_count: s.vote_count,
      })
    }))
    .collect::<Vec<Candidate>>()
    .tap_mut(|candidates| {
      candidates.sort_by(|a, b| {
        b.popularity
          .partial_cmp(&a.popularity)
          .unwrap_or(std::cmp::Ordering::Equal)
      })
    })
}

fn movie_details(json: MovieJson, region: &str) -> MovieDetails {
  let certification = json
    .release_dates
    .results
    .iter()
    .find(|entry| entry.iso_3166_1 == region)
    .and_then(|entry| {
      entry
        .release_dates
        .iter()
        .map(|release| release.certification.trim())
        .find(|certification| !certification.is_empty())
        .map(str::to_string)
    });
  MovieDetails {
    id: json.id,
    title: non_empty(json.title).unwrap_or_default(),
    original_title: non_empty(json.original_title),
    release_date: non_empty(json.release_date),
    overview: non_empty(json.overview),
    tagline: non_empty(json.tagline),
    runtime: json.runtime.filter(|minutes| *minutes > 0),
    genres: names(json.genres),
    studios: names(json.production_companies),
    countries: names(json.production_countries),
    poster_path: json.poster_path,
    backdrop_path: json.backdrop_path,
    imdb_id: non_empty(json.imdb_id),
    cast: cast(&json.credits),
    directors: json
      .credits
      .crew
      .iter()
      .filter(|crew| crew.job.as_deref() == Some("Director"))
      .filter_map(|crew| non_empty(crew.name.clone()))
      .collect(),
    certification,
  }
}

fn tv_details(json: TvJson, region: &str) -> TvDetails {
  let rating = json
    .content_ratings
    .results
    .iter()
    .find(|entry| entry.iso_3166_1 == region)
    .map(|entry| entry.rating.trim().to_string())
    .filter(|rating| !rating.is_empty());
  TvDetails {
    id: json.id,
    name: non_empty(json.name).unwrap_or_default(),
    original_name: non_empty(json.original_name),
    first_air_date: non_empty(json.first_air_date),
    overview: non_empty(json.overview),
    genres: names(json.genres),
    networks: names(json.networks),
    poster_path: json.poster_path,
    backdrop_path: json.backdrop_path,
    imdb_id: non_empty(json.external_ids.imdb_id),
    tvdb_id: json.external_ids.tvdb_id,
    cast: cast(&json.credits),
    creators: names(json.created_by),
    rating,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn search_results_map_to_candidates_by_popularity() {
    let movies: SearchPage<MovieResult> = serde_json::from_str(
      r#"{"results":[
        {"id":603,"title":"The Matrix","original_title":"The Matrix",
         "release_date":"1999-03-30","overview":"Whoa.","poster_path":"/m.jpg",
         "popularity":80.5,"vote_count":24000},
        {"id":1,"title":"Undated","release_date":"","popularity":1.0}
      ]}"#,
    )
    .unwrap();
    let shows: SearchPage<TvResult> = serde_json::from_str(
      r#"{"results":[{"id":66732,"name":"Voltron","first_air_date":"2016-06-10",
        "popularity":99.0,"vote_count":500}]}"#,
    )
    .unwrap();
    let candidates = merge_candidates(movies.results, shows.results);
    assert_eq!(candidates.len(), 3);
    assert_eq!(candidates[0].kind, TmdbKind::Tv);
    assert_eq!(candidates[0].year, Some(2016));
    assert_eq!(candidates[1].id, 603);
    assert_eq!(candidates[1].year, Some(1999));
    assert_eq!(candidates[2].year, None, "an empty date is no year");
    assert_eq!(candidates[2].overview, None);
  }

  #[test]
  fn movie_details_pull_directors_and_regional_certification() {
    let json: MovieJson = serde_json::from_str(
      r#"{"id":603,"title":"The Matrix","release_date":"1999-03-30",
        "runtime":136,"imdb_id":"tt0133093","genres":[{"name":"Action"}],
        "production_companies":[{"name":"Village Roadshow"}],
        "production_countries":[{"iso_3166_1":"US","name":"United States of America"}],
        "credits":{"cast":[{"name":"Keanu Reeves","character":"Neo","order":0}],
                   "crew":[{"name":"Lana Wachowski","job":"Director"},
                           {"name":"Someone","job":"Producer"}]},
        "release_dates":{"results":[
          {"iso_3166_1":"DE","release_dates":[{"certification":"16"}]},
          {"iso_3166_1":"US","release_dates":[{"certification":""},{"certification":"R"}]}]}}"#,
    )
    .unwrap();
    let details = movie_details(json, "US");
    assert_eq!(details.directors, vec!["Lana Wachowski"]);
    assert_eq!(details.certification.as_deref(), Some("R"));
    assert_eq!(details.cast[0].character.as_deref(), Some("Neo"));
    assert_eq!(details.countries, vec!["United States of America"]);
    assert_eq!(details.runtime, Some(136));
  }

  #[test]
  fn tv_details_pull_external_ids_and_rating() {
    let json: TvJson = serde_json::from_str(
      r#"{"id":66732,"name":"Voltron: Legendary Defender","first_air_date":"2016-06-10",
        "networks":[{"name":"Netflix"}],"created_by":[{"name":"Joaquim Dos Santos"}],
        "content_ratings":{"results":[{"iso_3166_1":"US","rating":"TV-Y7"}]},
        "external_ids":{"imdb_id":"tt5580122","tvdb_id":301716}}"#,
    )
    .unwrap();
    let details = tv_details(json, "US");
    assert_eq!(details.tvdb_id, Some(301716));
    assert_eq!(details.imdb_id.as_deref(), Some("tt5580122"));
    assert_eq!(details.rating.as_deref(), Some("TV-Y7"));
    assert_eq!(details.networks, vec!["Netflix"]);
    assert_eq!(details.creators, vec!["Joaquim Dos Santos"]);
  }

  #[test]
  fn auth_and_region_are_derived_from_their_inputs() {
    assert!(matches!(
      Auth::from_key("eyJhbGciOi.payload.sig"),
      Auth::BearerV4(_)
    ));
    assert!(matches!(Auth::from_key("0123456789abcdef"), Auth::ApiKeyV3(_)));
    assert_eq!(region_of("en-US"), "US");
    assert_eq!(region_of("ja-JP"), "JP");
    assert_eq!(region_of("ja"), "US");
    assert_eq!(
      image_url(ImageSize::W185, "/x.jpg"),
      "https://image.tmdb.org/t/p/w185/x.jpg"
    );
  }

  #[test]
  fn transience_classifies_errors() {
    assert!(TmdbError::RateLimited {
      endpoint: "e".into(),
      retry_after_secs: 1
    }
    .is_transient());
    assert!(TmdbError::Status {
      endpoint: "e".into(),
      status: 503,
      body_tail: String::new()
    }
    .is_transient());
    assert!(!TmdbError::Status {
      endpoint: "e".into(),
      status: 400,
      body_tail: String::new()
    }
    .is_transient());
    assert!(!TmdbError::Unauthorized {
      endpoint: "e".into(),
      status: 401
    }
    .is_transient());
    assert!(!TmdbError::NotFound {
      kind: "movie",
      id: 1
    }
    .is_transient());
  }

  #[test]
  fn candidates_serialize_with_tmdb_prefixed_keys() {
    let candidate = Candidate {
      kind: TmdbKind::Movie,
      id: 603,
      title: "The Matrix".to_string(),
      original_title: None,
      year: Some(1999),
      overview: None,
      poster_path: Some("/m.jpg".to_string()),
      popularity: 1.0,
      vote_count: 2,
    };
    let json = serde_json::to_value(&candidate).unwrap();
    assert_eq!(json["tmdb_kind"], "movie");
    assert_eq!(json["tmdb_id"], 603);
    assert!(json.get("overview").is_none());
    let back: Candidate = serde_json::from_value(json).unwrap();
    assert_eq!(back, candidate);
  }
}
