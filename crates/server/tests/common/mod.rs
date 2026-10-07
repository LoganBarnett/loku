// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.  dead_code is allowed because each test binary compiles this
// module separately and none uses every helper.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, dead_code)]

//! Shared test doubles and fixtures for the integration suites.

use loku_server::library::{Library, LibraryKind};
use loku_server::media::compat::CompatPlan;
use loku_server::media::ffmpeg::{
  DeriveError, FfmpegRunner, ProbeError, ProbeResult,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

pub fn library(name: &str, path: &Path, kind: LibraryKind) -> Library {
  Library {
    name: name.to_string(),
    path: path.to_path_buf(),
    kind,
  }
}

/// What the fake should do when asked to probe a given file name.
pub enum FakeOutcome {
  Give(ProbeResult),
  FailDefinitively,
  FailTransiently,
}

/// A scripted FfmpegRunner: canned probe outcomes keyed by file name, plus
/// call/plan records so tests can assert cache behavior and the exact plans
/// the worker chose.  Derivations write marker bytes to the destination the
/// way the real encoder would produce a file.
pub struct FakeFfmpeg {
  outcomes: HashMap<String, FakeOutcome>,
  probe_calls: AtomicUsize,
  pub derive_plans: Mutex<Vec<(String, CompatPlan)>>,
  pub fail_derive: bool,
}

impl FakeFfmpeg {
  pub fn new(outcomes: Vec<(&str, FakeOutcome)>) -> Self {
    Self {
      outcomes: outcomes
        .into_iter()
        .map(|(name, outcome)| (name.to_string(), outcome))
        .collect(),
      probe_calls: AtomicUsize::new(0),
      derive_plans: Mutex::new(Vec::new()),
      fail_derive: false,
    }
  }

  pub fn failing_derive(outcomes: Vec<(&str, FakeOutcome)>) -> Self {
    Self {
      fail_derive: true,
      ..Self::new(outcomes)
    }
  }

  pub fn probe_calls(&self) -> usize {
    self.probe_calls.load(Ordering::Relaxed)
  }
}

#[async_trait::async_trait]
impl FfmpegRunner for FakeFfmpeg {
  async fn probe(&self, path: &Path) -> Result<ProbeResult, ProbeError> {
    self.probe_calls.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    match self.outcomes.get(&name) {
      Some(FakeOutcome::Give(result)) => Ok(result.clone()),
      Some(FakeOutcome::FailDefinitively) => Err(ProbeError::FfprobeExit {
        path: name,
        status: "exit status: 1".to_string(),
        stderr_tail: "moov atom not found".to_string(),
      }),
      Some(FakeOutcome::FailTransiently) => Err(ProbeError::FfprobeTimeout {
        path: name,
        timeout_secs: 30,
      }),
      None => Ok(ProbeResult::default()),
    }
  }

  async fn derive_compat(
    &self,
    source: &Path,
    dest: &Path,
    plan: &CompatPlan,
  ) -> Result<(), DeriveError> {
    let name = source.file_name().unwrap().to_string_lossy().to_string();
    self
      .derive_plans
      .lock()
      .unwrap()
      .push((name.clone(), plan.clone()));
    if self.fail_derive {
      return Err(DeriveError::FfmpegExit {
        path: name,
        status: "exit status: 1".to_string(),
        stderr_tail: "scripted failure".to_string(),
      });
    }
    std::fs::write(dest, b"compat bytes").unwrap();
    Ok(())
  }

  async fn extract_thumbnail(
    &self,
    _source: &Path,
    dest: &Path,
    _at_secs: f64,
  ) -> Result<(), DeriveError> {
    std::fs::write(dest, b"jpeg bytes").unwrap();
    Ok(())
  }
}

use loku_server::rip::config::RipPolicy;
use loku_server::rip::makemkv::{DiscFacts, TitleInfo};
use loku_server::rip::manifest::{DiscManifest, MANIFEST_SCHEMA};
use loku_server::rip::naming::IdSource;

/// A manifest the way a finished rip would leave it, for a disc whose
/// titles have the given (duration, chapters) pairs.
pub fn disc_manifest(
  label: &str,
  id: &str,
  titles: &[(u64, u32)],
) -> DiscManifest {
  DiscManifest {
    schema: MANIFEST_SCHEMA,
    device: "/dev/sr0".to_string(),
    dir_name: format!("{label}__{id}"),
    label: label.to_string(),
    id: id.to_string(),
    id_source: IdSource::Uuid,
    scanned_at: 1_757_400_000,
    ripped_at: Some(1_757_403_600),
    policy: RipPolicy::Skip,
    udev: DiscFacts {
      label: Some(label.to_string()),
      fs_uuid: Some(id.to_string()),
      fs_type: Some("udf".to_string()),
      media_dvd: true,
      ..DiscFacts::default()
    },
    makemkv_version: Some("1.18.2".to_string()),
    disc_name: Some(label.replace('_', " ")),
    volume_name: Some(label.to_string()),
    title_count: u32::try_from(titles.len()).unwrap(),
    titles: titles
      .iter()
      .enumerate()
      .map(|(index, (duration, chapters))| TitleInfo {
        index: u32::try_from(index).unwrap(),
        output_file: Some(format!("title_t{index:02}.mkv")),
        duration_secs: Some(*duration),
        chapters: Some(*chapters),
        size_bytes: Some(duration * 1_000_000),
        segments_map: Some(format!("{}", index + 1)),
        original_title_id: None,
      })
      .collect(),
    fingerprint: None,
  }
}

/// Write a per-disc rip directory under `root`: the manifest plus one
/// marker `.mkv` per title, sized so a later title is the largest (the
/// main-title heuristic picks it).  Returns the directory path.
pub fn write_disc_dir(
  root: &Path,
  label: &str,
  id: &str,
  titles: &[(u64, u32)],
) -> std::path::PathBuf {
  let dir = root.join(format!("{label}__{id}"));
  std::fs::create_dir_all(&dir).unwrap();
  let manifest = disc_manifest(label, id, titles);
  std::fs::write(
    dir.join("disc.json"),
    serde_json::to_vec_pretty(&manifest).unwrap(),
  )
  .unwrap();
  manifest.titles.iter().for_each(|title| {
    std::fs::write(
      dir.join(title.output_file.as_deref().unwrap()),
      vec![b'x'; usize::try_from(title.index).unwrap() + 1],
    )
    .unwrap();
  });
  dir
}

use loku_server::rip::makemkv::{
  DiscRipError, DiscScanError, DiscTools, MediaQueryError, RipEvent, RipOutput,
  TrayError,
};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Which `DiscTools` operations a test exercised, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeCall {
  Media,
  Scan,
  Rip,
  Eject,
  Unlock,
}

/// A scripted `DiscTools`: the drive's contents are set by the test, the
/// scan returns canned robot-mode output, and a rip writes marker `.mkv`
/// files into staging the way makemkvcon would, emitting a few progress
/// events on the way.
pub struct FakeDiscTools {
  pub media: Mutex<Option<DiscFacts>>,
  pub info_text: String,
  pub mkv_names: Vec<String>,
  pub fail_scan: bool,
  pub fail_rip: bool,
  /// When set, a rip parks until the gate is notified, so tests can observe
  /// the busy state.
  pub rip_gate: Option<Arc<tokio::sync::Notify>>,
  pub calls: Mutex<Vec<FakeCall>>,
}

impl FakeDiscTools {
  pub fn new(info_text: &str, mkv_names: &[&str]) -> Self {
    Self {
      media: Mutex::new(None),
      info_text: info_text.to_string(),
      mkv_names: mkv_names.iter().map(|name| name.to_string()).collect(),
      fail_scan: false,
      fail_rip: false,
      rip_gate: None,
      calls: Mutex::new(Vec::new()),
    }
  }

  pub fn insert(&self, facts: DiscFacts) {
    *self.media.lock().unwrap() = Some(facts);
  }

  pub fn remove(&self) {
    *self.media.lock().unwrap() = None;
  }

  pub fn calls(&self) -> Vec<FakeCall> {
    self.calls.lock().unwrap().clone()
  }

  pub fn count(&self, call: FakeCall) -> usize {
    self.calls().iter().filter(|c| **c == call).count()
  }

  fn record(&self, call: FakeCall) {
    self.calls.lock().unwrap().push(call);
  }
}

#[async_trait::async_trait]
impl DiscTools for FakeDiscTools {
  async fn media(
    &self,
    _device: &Path,
  ) -> Result<Option<DiscFacts>, MediaQueryError> {
    self.record(FakeCall::Media);
    Ok(self.media.lock().unwrap().clone())
  }

  async fn scan(&self, device: &Path) -> Result<String, DiscScanError> {
    self.record(FakeCall::Scan);
    if self.fail_scan {
      return Err(DiscScanError::MakemkvScanExit {
        device: device.display().to_string(),
        status: "exit status: 1".to_string(),
        output_tail: "scripted scan failure".to_string(),
      });
    }
    Ok(self.info_text.clone())
  }

  async fn rip(
    &self,
    device: &Path,
    staging: &Path,
    events: mpsc::UnboundedSender<RipEvent>,
  ) -> Result<RipOutput, DiscRipError> {
    self.record(FakeCall::Rip);
    drop(events.send(RipEvent::TotalOp("Saving all titles".to_string())));
    drop(events.send(RipEvent::Progress {
      current: 1,
      total: 32768,
      max: 65536,
    }));
    // Park mid-rip, with progress already reported, so a test can observe
    // the busy state.
    if let Some(gate) = &self.rip_gate {
      gate.notified().await;
    }
    if self.fail_rip {
      return Err(DiscRipError::MakemkvRipExit {
        device: device.display().to_string(),
        status: "exit status: 1".to_string(),
        last_messages: "scripted rip failure".to_string(),
      });
    }
    self.mkv_names.iter().for_each(|name| {
      std::fs::write(staging.join(name), b"mkv bytes").unwrap();
    });
    drop(events.send(RipEvent::Progress {
      current: 1,
      total: 65536,
      max: 65536,
    }));
    drop(events.send(RipEvent::Message {
      code: 5003,
      text: "Copy complete.".to_string(),
    }));
    Ok(RipOutput {
      last_messages: vec!["Copy complete.".to_string()],
    })
  }

  async fn eject(&self, _device: &Path) -> Result<(), TrayError> {
    self.record(FakeCall::Eject);
    Ok(())
  }

  async fn unlock(&self, _device: &Path) -> Result<(), TrayError> {
    self.record(FakeCall::Unlock);
    Ok(())
  }
}

/// A realistic `makemkvcon --robot info` dump for a disc with `title_count`
/// titles of increasing length.
pub fn fake_info(title_count: u32) -> String {
  let header = "MSG:1005,0,1,\"MakeMKV v1.18.2 linux(x64-release) started\",\
\"%1 started\",\"MakeMKV v1.18.2 linux(x64-release)\"\n\
DRV:0,2,999,12,\"BD-RE ASUS\",\"DISC\",\"/dev/sr0\"\n";
  let titles = (0..title_count)
    .map(|index| {
      format!(
        "TINFO:{index},8,0,\"{}\"\nTINFO:{index},9,0,\"0:{}:00\"\n\
TINFO:{index},11,0,\"{}\"\nTINFO:{index},26,0,\"{}\"\n\
TINFO:{index},27,0,\"title_t{index:02}.mkv\"\nSINFO:{index},0,1,6201,\"Video\"\n",
        index + 3,
        21 + index,
        (index + 1) * 1_000_000,
        index + 1,
      )
    })
    .collect::<String>();
  format!(
    "{header}TCOUNT:{title_count}\nCINFO:1,6209,\"DVD disc\"\n\
CINFO:2,0,\"Fake Disc Name\"\nCINFO:32,0,\"FAKE_VOLUME\"\n{titles}"
  )
}

/// The udev facts for a UDF disc with the given label and (optional) UUID.
pub fn facts_udf(label: &str, uuid: Option<&str>) -> DiscFacts {
  DiscFacts {
    label: Some(label.to_string()),
    fs_uuid: uuid.map(str::to_string),
    fs_type: Some("udf".to_string()),
    media_dvd: true,
    ..DiscFacts::default()
  }
}

use loku_server::identity::tmdb::{
  Candidate, CastMember, ImageSize, MovieDetails, TmdbClient, TmdbError,
  TvDetails,
};
use loku_server::identity::TmdbKind;

/// How a scripted TMDB should fail every request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeTmdbFailure {
  Transient,
  Unauthorized,
}

/// A scripted `TmdbClient`: search results keyed by (lowercased) query,
/// details by id, images as a few JPEG-looking bytes, and a switch to make
/// every call fail one way or another.
#[derive(Default)]
pub struct FakeTmdb {
  results: Mutex<HashMap<String, Vec<Candidate>>>,
  movies: Mutex<HashMap<u64, MovieDetails>>,
  tvs: Mutex<HashMap<u64, TvDetails>>,
  pub failure: Mutex<Option<FakeTmdbFailure>>,
  pub searches: Mutex<Vec<(String, Option<i32>)>>,
}

impl FakeTmdb {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_results(self, query: &str, candidates: Vec<Candidate>) -> Self {
    self
      .results
      .lock()
      .unwrap()
      .insert(query.to_lowercase(), candidates);
    self
  }

  pub fn with_movie(self, details: MovieDetails) -> Self {
    self.movies.lock().unwrap().insert(details.id, details);
    self
  }

  pub fn with_tv(self, details: TvDetails) -> Self {
    self.tvs.lock().unwrap().insert(details.id, details);
    self
  }

  pub fn fail(&self, failure: Option<FakeTmdbFailure>) {
    *self.failure.lock().unwrap() = failure;
  }

  pub fn search_count(&self) -> usize {
    self.searches.lock().unwrap().len()
  }

  fn failing(&self, endpoint: &str) -> Result<(), TmdbError> {
    match *self.failure.lock().unwrap() {
      Some(FakeTmdbFailure::Transient) => Err(TmdbError::Status {
        endpoint: endpoint.to_string(),
        status: 503,
        body_tail: "scripted outage".to_string(),
      }),
      Some(FakeTmdbFailure::Unauthorized) => Err(TmdbError::Unauthorized {
        endpoint: endpoint.to_string(),
        status: 401,
      }),
      None => Ok(()),
    }
  }
}

#[async_trait::async_trait]
impl TmdbClient for FakeTmdb {
  async fn search(
    &self,
    query: &str,
    year: Option<i32>,
  ) -> Result<Vec<Candidate>, TmdbError> {
    self
      .searches
      .lock()
      .unwrap()
      .push((query.to_string(), year));
    self.failing("/search")?;
    Ok(
      self
        .results
        .lock()
        .unwrap()
        .get(&query.to_lowercase())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|candidate| year.is_none() || candidate.year == year)
        .collect(),
    )
  }

  async fn movie(&self, id: u64) -> Result<MovieDetails, TmdbError> {
    self.failing("/movie")?;
    self
      .movies
      .lock()
      .unwrap()
      .get(&id)
      .cloned()
      .ok_or(TmdbError::NotFound { kind: "movie", id })
  }

  async fn tv(&self, id: u64) -> Result<TvDetails, TmdbError> {
    self.failing("/tv")?;
    self
      .tvs
      .lock()
      .unwrap()
      .get(&id)
      .cloned()
      .ok_or(TmdbError::NotFound { kind: "tv", id })
  }

  async fn image(
    &self,
    _size: ImageSize,
    path: &str,
  ) -> Result<Vec<u8>, TmdbError> {
    self.failing("/image")?;
    Ok(format!("\u{FFFD}\u{FFD8}fake image {path}").into_bytes())
  }
}

pub fn candidate(
  kind: TmdbKind,
  id: u64,
  title: &str,
  year: Option<i32>,
  popularity: f64,
) -> Candidate {
  Candidate {
    kind,
    id,
    title: title.to_string(),
    original_title: None,
    year,
    overview: Some(format!("About {title}.")),
    poster_path: Some(format!("/poster-{id}.jpg")),
    popularity,
    vote_count: 100,
  }
}

pub fn movie_details(id: u64, title: &str, year: i32) -> MovieDetails {
  MovieDetails {
    id,
    title: title.to_string(),
    original_title: Some(title.to_string()),
    release_date: Some(format!("{year}-01-01")),
    overview: Some(format!("About {title}.")),
    tagline: None,
    runtime: Some(120),
    genres: vec!["Action".to_string()],
    studios: vec!["Studio".to_string()],
    countries: vec!["US".to_string()],
    poster_path: Some(format!("/poster-{id}.jpg")),
    backdrop_path: Some(format!("/backdrop-{id}.jpg")),
    imdb_id: Some(format!("tt{id:07}")),
    cast: vec![CastMember {
      name: "Someone".to_string(),
      character: Some("Lead".to_string()),
      order: 0,
      profile_path: None,
    }],
    directors: vec!["A Director".to_string()],
    certification: Some("PG".to_string()),
  }
}

pub fn tv_details(id: u64, name: &str, year: i32) -> TvDetails {
  TvDetails {
    id,
    name: name.to_string(),
    original_name: Some(name.to_string()),
    first_air_date: Some(format!("{year}-06-10")),
    overview: Some(format!("About {name}.")),
    genres: vec!["Animation".to_string()],
    networks: vec!["Network".to_string()],
    poster_path: Some(format!("/poster-{id}.jpg")),
    backdrop_path: None,
    imdb_id: None,
    tvdb_id: Some(1),
    cast: Vec::new(),
    creators: vec!["A Creator".to_string()],
    rating: Some("TV-Y7".to_string()),
  }
}
