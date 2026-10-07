// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! The real TMDB client, ignored by default: it needs network access and a
//! key in the file named by `loku_tmdb_api_key_file`.

use loku_server::identity::tmdb::{ImageSize, RealTmdb, TmdbClient};

#[tokio::test]
#[ignore = "requires network and a TMDB key in loku_tmdb_api_key_file; run with `just test-tmdb`"]
async fn real_tmdb_answers_search_details_and_images() {
  let key_file = std::env::var("loku_tmdb_api_key_file")
    .expect("loku_tmdb_api_key_file names the key file");
  let key = std::fs::read_to_string(key_file).unwrap();
  let client = RealTmdb::new(key.trim(), "en-US").unwrap();

  let hits = client.search("The Matrix", Some(1999)).await.unwrap();
  let matrix = hits.iter().find(|c| c.id == 603).expect("The Matrix (603)");
  assert_eq!(matrix.year, Some(1999));

  let movie = client.movie(603).await.unwrap();
  assert_eq!(movie.imdb_id.as_deref(), Some("tt0133093"));
  assert!(movie.runtime.is_some());
  assert!(!movie.directors.is_empty());

  let show = client.tv(1399).await.unwrap();
  assert!(!show.name.is_empty());
  assert!(show.tvdb_id.is_some());

  let poster = client
    .image(ImageSize::W185, movie.poster_path.as_deref().unwrap())
    .await
    .unwrap();
  assert_eq!(&poster[..2], &[0xFF, 0xD8], "a JPEG");
}
