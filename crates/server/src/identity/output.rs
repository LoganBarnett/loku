//! What identity resolution writes into a disc directory: the Kodi NFO and
//! the folder artwork.  Everything goes through a `.part` sibling and a
//! rename, so the watcher never sees a half-written sidecar, and the NFO is
//! the authority — artwork failures are reported, never fatal.

use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::info;

use crate::identity::tmdb::{
  image_url, ImageSize, MovieDetails, TmdbClient, TmdbError, TvDetails,
};
use crate::identity::TmdbKind;
use loku_lib::nfo::{
  self, Actor, MovieNfo, NfoWriteError, TvShowNfo, UniqueId,
};

/// Kodi's folder-art names for a movie or show directory.
pub const POSTER_FILE: &str = "poster.jpg";
pub const FANART_FILE: &str = "fanart.jpg";
/// Enough cast for a sidecar; TMDB lists hundreds for big productions.
const MAX_ACTORS: usize = 30;

#[derive(Debug, Error)]
pub enum OutputError {
  #[error("Failed to serialize the {kind} NFO: {source}")]
  NfoSerialize {
    kind: &'static str,
    source: NfoWriteError,
  },

  #[error("Failed to stage the NFO at '{path}': {source}")]
  NfoStage {
    path: String,
    source: std::io::Error,
  },

  #[error("Failed to move the NFO into place at '{path}': {source}")]
  NfoFinalize {
    path: String,
    source: std::io::Error,
  },

  #[error(
    "Failed to remove the stale '{path}' after the disc changed kind: {source}"
  )]
  StaleNfoRemove {
    path: String,
    source: std::io::Error,
  },

  #[error("Failed to download the {role} artwork: {source}")]
  ArtworkFetch {
    role: &'static str,
    source: TmdbError,
  },

  #[error("Failed to stage the {role} artwork at '{path}': {source}")]
  ArtworkStage {
    role: &'static str,
    path: String,
    source: std::io::Error,
  },

  #[error(
    "Failed to move the {role} artwork into place at '{path}': {source}"
  )]
  ArtworkFinalize {
    role: &'static str,
    path: String,
    source: std::io::Error,
  },
}

/// The Kodi `<movie>` document for a TMDB movie.
pub fn movie_nfo(details: &MovieDetails) -> MovieNfo {
  MovieNfo {
    title: Some(details.title.clone()),
    originaltitle: details.original_title.clone(),
    sorttitle: None,
    year: year_of(details.release_date.as_deref()),
    premiered: details.release_date.clone(),
    runtime: details.runtime,
    plot: details.overview.clone(),
    outline: None,
    tagline: details.tagline.clone(),
    mpaa: details.certification.clone(),
    genres: details.genres.clone(),
    studios: details.studios.clone(),
    countries: details.countries.clone(),
    directors: details.directors.clone(),
    actors: actors(&details.cast),
    unique_ids: unique_ids(details.id, details.imdb_id.as_deref(), None),
  }
}

/// The Kodi `<tvshow>` document for a TMDB series.
pub fn tvshow_nfo(details: &TvDetails) -> TvShowNfo {
  TvShowNfo {
    title: Some(details.name.clone()),
    originaltitle: details.original_name.clone(),
    year: year_of(details.first_air_date.as_deref()),
    premiered: details.first_air_date.clone(),
    plot: details.overview.clone(),
    mpaa: details.rating.clone(),
    genres: details.genres.clone(),
    studios: details.networks.clone(),
    actors: actors(&details.cast),
    unique_ids: unique_ids(
      details.id,
      details.imdb_id.as_deref(),
      details.tvdb_id,
    ),
  }
}

/// The year in a `YYYY-MM-DD` date.
pub fn year_of(date: Option<&str>) -> Option<i32> {
  // The .ok() discard is deliberate: a date TMDB could not format is the
  // same as no date.
  date.and_then(|d| d.get(..4)).and_then(|y| y.parse().ok())
}

fn actors(cast: &[crate::identity::tmdb::CastMember]) -> Vec<Actor> {
  cast
    .iter()
    .take(MAX_ACTORS)
    .map(|member| Actor {
      name: Some(member.name.clone()),
      role: member.character.clone(),
      thumb: member
        .profile_path
        .as_deref()
        .map(|path| image_url(ImageSize::W185, path)),
      order: Some(member.order),
    })
    .collect()
}

fn unique_ids(
  tmdb_id: u64,
  imdb_id: Option<&str>,
  tvdb_id: Option<u64>,
) -> Vec<UniqueId> {
  std::iter::once(UniqueId {
    id_type: Some("tmdb".to_string()),
    default: Some("true".to_string()),
    value: Some(tmdb_id.to_string()),
  })
  .chain(imdb_id.map(|id| UniqueId {
    id_type: Some("imdb".to_string()),
    default: None,
    value: Some(id.to_string()),
  }))
  .chain(tvdb_id.map(|id| UniqueId {
    id_type: Some("tvdb".to_string()),
    default: None,
    value: Some(id.to_string()),
  }))
  .collect()
}

/// The document a disc gets, which also fixes its kind.
#[derive(Debug, Clone, Copy)]
pub enum NfoDoc<'a> {
  Movie(&'a MovieNfo),
  TvShow(&'a TvShowNfo),
}

impl NfoDoc<'_> {
  pub fn kind(self) -> TmdbKind {
    match self {
      NfoDoc::Movie(_) => TmdbKind::Movie,
      NfoDoc::TvShow(_) => TmdbKind::Tv,
    }
  }
}

/// Serialize the NFO document.
pub fn nfo_xml(doc: NfoDoc<'_>) -> Result<String, OutputError> {
  match doc {
    NfoDoc::Movie(movie) => {
      nfo::movie_to_xml(movie).map_err(|source| OutputError::NfoSerialize {
        kind: "movie",
        source,
      })
    }
    NfoDoc::TvShow(show) => {
      nfo::tvshow_to_xml(show).map_err(|source| OutputError::NfoSerialize {
        kind: "tvshow",
        source,
      })
    }
  }
}

/// The NFO file name for a kind.
pub fn nfo_file(kind: TmdbKind) -> &'static str {
  match kind {
    TmdbKind::Movie => "movie.nfo",
    TmdbKind::Tv => "tvshow.nfo",
  }
}

/// Write the NFO for the disc's kind and remove the other kind's file, so
/// an operator correction from movie to series (or back) leaves one
/// authority behind.
pub async fn write_nfo(
  dir: &Path,
  kind: TmdbKind,
  xml: &str,
) -> Result<PathBuf, OutputError> {
  let path = dir.join(nfo_file(kind));
  let part = part_path(&path);
  tokio::fs::write(&part, xml.as_bytes())
    .await
    .map_err(|source| OutputError::NfoStage {
      path: part.display().to_string(),
      source,
    })?;
  tokio::fs::rename(&part, &path).await.map_err(|source| {
    OutputError::NfoFinalize {
      path: path.display().to_string(),
      source,
    }
  })?;
  let stale = dir.join(nfo_file(match kind {
    TmdbKind::Movie => TmdbKind::Tv,
    TmdbKind::Tv => TmdbKind::Movie,
  }));
  match tokio::fs::remove_file(&stale).await {
    Ok(()) => {
      info!(path = %stale.display(), "Removed the previous kind's NFO");
      Ok(())
    }
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(source) => Err(OutputError::StaleNfoRemove {
      path: stale.display().to_string(),
      source,
    }),
  }?;
  Ok(path)
}

/// Download the poster and fanart into the directory.  Each image fails
/// independently; the failures come back for reporting.
pub async fn write_artwork(
  dir: &Path,
  client: &dyn TmdbClient,
  poster: Option<&str>,
  fanart: Option<&str>,
) -> Vec<OutputError> {
  let poster = write_image(
    dir,
    client,
    "poster",
    POSTER_FILE,
    ImageSize::Original,
    poster,
  )
  .await;
  let fanart =
    write_image(dir, client, "fanart", FANART_FILE, ImageSize::W1280, fanart)
      .await;
  poster.err().into_iter().chain(fanart.err()).collect()
}

async fn write_image(
  dir: &Path,
  client: &dyn TmdbClient,
  role: &'static str,
  file: &str,
  size: ImageSize,
  tmdb_path: Option<&str>,
) -> Result<(), OutputError> {
  let Some(tmdb_path) = tmdb_path else {
    return Ok(());
  };
  let bytes = client
    .image(size, tmdb_path)
    .await
    .map_err(|source| OutputError::ArtworkFetch { role, source })?;
  let path = dir.join(file);
  let part = part_path(&path);
  tokio::fs::write(&part, &bytes).await.map_err(|source| {
    OutputError::ArtworkStage {
      role,
      path: part.display().to_string(),
      source,
    }
  })?;
  tokio::fs::rename(&part, &path).await.map_err(|source| {
    OutputError::ArtworkFinalize {
      role,
      path: path.display().to_string(),
      source,
    }
  })
}

fn part_path(path: &Path) -> PathBuf {
  let mut name = path.as_os_str().to_os_string();
  name.push(".part");
  PathBuf::from(name)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::identity::tmdb::CastMember;

  #[test]
  fn movie_nfo_carries_ids_and_capped_cast() {
    let details = MovieDetails {
      id: 603,
      title: "The Matrix".to_string(),
      original_title: Some("The Matrix".to_string()),
      release_date: Some("1999-03-30".to_string()),
      overview: Some("Whoa.".to_string()),
      tagline: None,
      runtime: Some(136),
      genres: vec!["Action".to_string()],
      studios: vec![],
      countries: vec![],
      poster_path: None,
      backdrop_path: None,
      imdb_id: Some("tt0133093".to_string()),
      cast: (0..40)
        .map(|order| CastMember {
          name: format!("Actor {order}"),
          character: None,
          order,
          profile_path: Some("/p.jpg".to_string()),
        })
        .collect(),
      directors: vec!["Lana Wachowski".to_string()],
      certification: Some("R".to_string()),
    };
    let nfo = movie_nfo(&details);
    assert_eq!(nfo.year, Some(1999));
    assert_eq!(nfo.actors.len(), MAX_ACTORS);
    assert_eq!(
      nfo.actors[0].thumb.as_deref(),
      Some("https://image.tmdb.org/t/p/w185/p.jpg")
    );
    assert_eq!(nfo.unique_ids.len(), 2);
    assert_eq!(nfo.unique_ids[0].value.as_deref(), Some("603"));
    assert_eq!(nfo.unique_ids[0].default.as_deref(), Some("true"));
    assert_eq!(nfo.unique_ids[1].id_type.as_deref(), Some("imdb"));
    assert_eq!(nfo.mpaa.as_deref(), Some("R"));
  }

  #[tokio::test]
  async fn write_nfo_replaces_the_other_kind() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("tvshow.nfo"), b"<tvshow/>").unwrap();
    let path = write_nfo(dir.path(), TmdbKind::Movie, "<movie/>")
      .await
      .unwrap();
    assert_eq!(path, dir.path().join("movie.nfo"));
    assert!(!dir.path().join("tvshow.nfo").exists());
    assert!(!dir.path().join("movie.nfo.part").exists());
    assert_eq!(std::fs::read(path).unwrap(), b"<movie/>");
  }
}
