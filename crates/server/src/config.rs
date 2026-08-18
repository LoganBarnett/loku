use loku_lib::{LogFormat, LogLevel};
use rust_template_foundation::auth::OidcConfig;
use rust_template_foundation::config::{
  credential_secret_path, ConfigFileError,
};
use rust_template_foundation::server::runner::{ServerApp, ServerRunConfig};
use rust_template_foundation::{CliApp, MergeConfig};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;
use tokio_listener::ListenerAddress;

use crate::library::{
  valid_library_name, Library, LibraryFileEntry, LibraryKind,
};

/// Loku-specific and OIDC CLI arguments, flattened into the generated
/// `CliRaw`.
///
/// Env-var names are written out long-hand here because this struct is raw
/// clap (flattened in via `extra_cli`), not a `MergeConfig` field — the
/// macro's bare-`env` derivation does not reach inside `extra_cli` types.
/// Names follow the same `<app>_<flag>` convention the macro uses elsewhere.
#[derive(Debug, clap::Args)]
pub struct ExtraCliFields {
  /// Root directory of a single downloads library.  Developer sugar for a
  /// one-library configuration; multi-library setups use `[[library]]`
  /// entries in the config file, and this flag overrides them entirely.
  #[arg(long = "library", env = "loku_library")]
  pub library: Option<PathBuf>,

  /// Directory for persistent server state (the media index database).
  #[arg(long = "state-dir", env = "loku_state_dir")]
  pub state_dir: Option<PathBuf>,

  /// OIDC issuer URL
  /// (e.g. https://sso.example.com/application/o/loku).
  #[arg(long, env = "loku_oidc_issuer")]
  pub oidc_issuer: Option<String>,

  /// OIDC client ID.
  #[arg(long, env = "loku_oidc_client_id")]
  pub oidc_client_id: Option<String>,

  /// Path to a file containing the OIDC client secret.
  #[arg(long, env = "loku_oidc_client_secret_file")]
  pub oidc_client_secret_file: Option<PathBuf>,
}

/// Loku-specific and OIDC config-file fields, flattened into the generated
/// `ConfigFileRaw`.
#[derive(Debug, Deserialize, Default)]
pub struct ExtraFileFields {
  /// `[[library]]` entries: named roots with a dataset kind.
  pub library: Option<Vec<LibraryFileEntry>>,
  pub state_dir: Option<PathBuf>,
  pub oidc_issuer: Option<String>,
  pub oidc_client_id: Option<String>,
  pub oidc_client_secret_file: Option<PathBuf>,
}

#[derive(Debug, Clone, MergeConfig)]
#[merge_config(
  app_name = "loku",
  extra_cli = "ExtraCliFields",
  extra_file = "ExtraFileFields"
)]
pub struct Config {
  #[merge_config(common)]
  pub log_level: LogLevel,
  #[merge_config(common)]
  pub log_format: LogFormat,
  /// Address to listen on: host:port for TCP, /path/to.sock for Unix socket,
  /// or sd-listen to inherit from systemd.
  #[merge_config(
    name = "listen",
    env,
    default = "\"127.0.0.1:3000\".to_string()",
    parse
  )]
  pub listen_address: ListenerAddress,
  /// Base URL of the service (e.g. https://loku.example.com), used to
  /// construct the OIDC redirect URI.  Defaults to a local address so
  /// unauthenticated local use needs no configuration; only OIDC deployments
  /// must set it.
  #[merge_config(env, default = "\"http://localhost:3000\".to_string()")]
  pub base_url: String,
  #[merge_config(skip)]
  pub oidc: Option<OidcConfig>,
  /// The configured library roots, at least one.  Resolved (and checked for
  /// existence) in `resolve_libraries` — the `MergeConfig` macro has no
  /// per-field validation hook, so required-and-must-exist fields ride the
  /// `skip` escape hatch.  See tasks.org "Cross-repo".
  #[merge_config(skip)]
  pub libraries: Vec<Library>,
  /// Directory for persistent server state (the media index database).  Not
  /// required to exist yet; it is created when the index opens.
  #[merge_config(skip)]
  pub state_dir: PathBuf,
}

impl std::fmt::Display for Config {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "Config(listen={}, libraries=[{}], state_dir={})",
      self.listen_address,
      self
        .libraries
        .iter()
        .map(|l| format!("{}:{}", l.name, l.path.display()))
        .collect::<Vec<_>>()
        .join(", "),
      self.state_dir.display()
    )
  }
}

impl ServerApp for Config {
  fn server_run_configs(&self) -> Vec<ServerRunConfig> {
    vec![ServerRunConfig {
      app_name: Self::app_name().to_string(),
      listen_address: self.listen_address.clone(),
      base_url: self.base_url.clone(),
      oidc: self.oidc.clone(),
    }]
  }
}

impl Config {
  fn resolve_libraries(
    cli: &CliRaw,
    file: &ConfigFileRaw,
  ) -> Result<Vec<Library>, ConfigError> {
    // The CLI flag is single-root developer sugar: it wins outright
    // (matching the CLI-over-file precedence used elsewhere) and implies one
    // downloads-kind library under a fixed name.
    let candidates = cli.extra.library.clone().map_or_else(
      || file.extra.library.clone().unwrap_or_default(),
      |path| {
        vec![LibraryFileEntry {
          name: "downloads".to_string(),
          path,
          kind: LibraryKind::Downloads,
        }]
      },
    );

    if candidates.is_empty() {
      return Err(ConfigError::Validation(
        "at least one library is required: pass --library or define \
         [[library]] entries in the config file"
          .to_string(),
      ));
    }

    let mut seen = HashSet::new();
    candidates
      .into_iter()
      .map(|entry| {
        if !valid_library_name(&entry.name) {
          return Err(ConfigError::Validation(format!(
            "library name '{}' is invalid: names become URL path segments \
             and must be non-empty lowercase ASCII letters, digits, '_', \
             or '-'",
            entry.name
          )));
        }
        if !seen.insert(entry.name.clone()) {
          return Err(ConfigError::Validation(format!(
            "library name '{}' is defined more than once",
            entry.name
          )));
        }
        if !entry.path.exists() {
          return Err(ConfigError::Validation(format!(
            "library path for '{}' does not exist: {}",
            entry.name,
            entry.path.display()
          )));
        }
        Ok(Library {
          name: entry.name,
          path: entry.path,
          kind: entry.kind,
        })
      })
      .collect()
  }

  fn resolve_state_dir(
    cli: &CliRaw,
    file: &ConfigFileRaw,
  ) -> Result<PathBuf, ConfigError> {
    cli
      .extra
      .state_dir
      .clone()
      .or_else(|| file.extra.state_dir.clone())
      .or_else(state_dir_from_env)
      .ok_or_else(|| {
        ConfigError::Validation(
          "state_dir could not be determined: pass --state-dir, set \
           `state_dir` in the config file, or run with STATE_DIRECTORY, \
           XDG_STATE_HOME, or HOME set"
            .to_string(),
        )
      })
  }

  fn resolve_oidc(
    cli: &CliRaw,
    file: &ConfigFileRaw,
  ) -> Result<Option<OidcConfig>, ConfigError> {
    let oidc_issuer = cli
      .extra
      .oidc_issuer
      .clone()
      .or_else(|| file.extra.oidc_issuer.clone());
    let oidc_client_id = cli
      .extra
      .oidc_client_id
      .clone()
      .or_else(|| file.extra.oidc_client_id.clone());
    let oidc_secret_file = cli
      .extra
      .oidc_client_secret_file
      .clone()
      .or_else(|| file.extra.oidc_client_secret_file.clone());

    match (&oidc_issuer, &oidc_client_id) {
      (None, None) if oidc_secret_file.is_none() => Ok(None),
      (Some(issuer), Some(client_id)) => {
        let secret_file = oidc_secret_file
          .or_else(credential_secret_path)
          .ok_or_else(|| {
            ConfigError::Validation(
              "oidc_client_secret_file is required when oidc_issuer and \
               oidc_client_id are set (set it explicitly or run under \
               systemd with LoadCredential)"
                .to_string(),
            )
          })?;

        let client_secret = std::fs::read_to_string(&secret_file)
          .map(|s| s.trim().to_string())
          .map_err(|source| ConfigFileError::FileRead {
            path: secret_file,
            source,
          })?;

        Ok(Some(OidcConfig {
          issuer: issuer.clone(),
          client_id: client_id.clone(),
          client_secret,
        }))
      }
      _ => {
        let (present, missing): (Vec<_>, Vec<_>) = [
          ("oidc_issuer", oidc_issuer.is_some()),
          ("oidc_client_id", oidc_client_id.is_some()),
          (
            "oidc_client_secret_file",
            oidc_secret_file.is_some() || credential_secret_path().is_some(),
          ),
        ]
        .into_iter()
        .partition(|(_, set)| *set);
        Err(ConfigError::Validation(format!(
          "partial OIDC configuration: set all three fields or none. \
           present: [{}], missing: [{}]",
          field_names(present),
          field_names(missing)
        )))
      }
    }
  }
}

/// The field names from a (name, set) list, comma-joined for an error
/// message.
fn field_names(fields: Vec<(&str, bool)>) -> String {
  fields
    .into_iter()
    .map(|(name, _)| name)
    .collect::<Vec<_>>()
    .join(", ")
}

/// Environment-derived state-directory fallbacks, in precedence order:
/// systemd's `STATE_DIRECTORY` (colon-separated when the unit declares
/// several; the first is ours), then the XDG state home, then the
/// conventional `~/.local/state` location.  `var_os` is used because an
/// absent variable is an expected case here, not an error to handle.
fn state_dir_from_env() -> Option<PathBuf> {
  std::env::var_os("STATE_DIRECTORY")
    .as_deref()
    .and_then(std::ffi::OsStr::to_str)
    .and_then(|v| v.split(':').next())
    .filter(|v| !v.is_empty())
    .map(PathBuf::from)
    .or_else(|| {
      std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join("loku"))
    })
    .or_else(|| {
      std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join(".local/state/loku"))
    })
}
