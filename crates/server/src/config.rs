use loku_lib::{LogFormat, LogLevel};
use rust_template_foundation::auth::OidcConfig;
use rust_template_foundation::config::{
  credential_secret_path, ConfigFileError,
};
use rust_template_foundation::server::runner::{ServerApp, ServerRunConfig};
use rust_template_foundation::{CliApp, MergeConfig};
use serde::Deserialize;
use std::path::PathBuf;
use tokio_listener::ListenerAddress;

/// Loku-specific and OIDC CLI arguments, flattened into the generated
/// `CliRaw`.
///
/// Env-var names are written out long-hand here because this struct is raw
/// clap (flattened in via `extra_cli`), not a `MergeConfig` field — the
/// macro's bare-`env` derivation does not reach inside `extra_cli` types.
/// Names follow the same `<app>_<flag>` convention the macro uses elsewhere.
#[derive(Debug, clap::Args)]
pub struct ExtraCliFields {
  /// Root directory of the video library.
  #[arg(long = "library", env = "loku_library")]
  pub library: Option<PathBuf>,

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
  pub library: Option<PathBuf>,
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
  /// Root directory of the video library.  Resolved (and checked for
  /// existence) in `resolve_library_path` — the `MergeConfig` macro has no
  /// per-field validation hook, so a required-and-must-exist field rides the
  /// `skip` escape hatch.  See tasks.org "Cross-repo".
  #[merge_config(skip)]
  pub library_path: PathBuf,
}

impl std::fmt::Display for Config {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "Config(listen={}, library={})",
      self.listen_address,
      self.library_path.display()
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
  fn resolve_library_path(
    cli: &CliRaw,
    file: &ConfigFileRaw,
  ) -> Result<PathBuf, ConfigError> {
    let library_path = cli
      .extra
      .library
      .clone()
      .or_else(|| file.extra.library.clone())
      .ok_or_else(|| {
        ConfigError::Validation(
          "library is required: pass --library or set `library` in the \
           config file"
            .to_string(),
        )
      })?;

    if !library_path.exists() {
      return Err(ConfigError::Validation(format!(
        "library path does not exist: {}",
        library_path.display()
      )));
    }

    Ok(library_path)
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
        let mut present = Vec::new();
        let mut missing = Vec::new();
        for (name, val) in [
          ("oidc_issuer", oidc_issuer.is_some()),
          ("oidc_client_id", oidc_client_id.is_some()),
          (
            "oidc_client_secret_file",
            oidc_secret_file.is_some() || credential_secret_path().is_some(),
          ),
        ] {
          if val {
            present.push(name);
          } else {
            missing.push(name);
          }
        }
        Err(ConfigError::Validation(format!(
          "partial OIDC configuration: set all three fields or none. \
           present: [{}], missing: [{}]",
          present.join(", "),
          missing.join(", ")
        )))
      }
    }
  }
}
