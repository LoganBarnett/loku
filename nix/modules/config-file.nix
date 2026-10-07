# Renders the TOML config file both service modules hand the server.  The
# platforms differ only in how the listener is expressed (systemd socket
# activation versus a path the server binds itself) and in the keys only one
# platform supports, so both arrive as parameters instead of branches.
{
  lib,
  pkgs,
  cfg,
  # The `listen` value for this platform: "sd-listen" under systemd socket
  # activation, otherwise a socket path or host:port.
  listen,
  # Platform-only keys, merged in last: the NixOS module's `rip` table, the
  # darwin module's state directory and secret paths.
  extra ? {},
}:
(pkgs.formats.toml {}).generate "loku-config.toml" ({
    inherit listen;
    log_level = cfg.logLevel;
    log_format = cfg.logFormat;
    base_url = cfg.baseUrl;
    library =
      lib.mapAttrsToList (name: value: {
        inherit name;
        path = toString value.path;
        kind = value.kind;
      })
      cfg.libraries;
  }
  // lib.optionalAttrs (cfg.oidcIssuer != null) {
    oidc_issuer = cfg.oidcIssuer;
    oidc_client_id = cfg.oidcClientId;
  }
  // lib.optionalAttrs (cfg.tmdbApiKeyFile != null) {
    tmdb_language = cfg.tmdbLanguage;
  }
  // extra)
