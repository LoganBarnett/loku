# Darwin (macOS/launchd) module for the loku-server service.
# Thin wrapper around the foundation's mkDarwinService helper.  See
# mkNixosService for the Linux/systemd equivalent.  Cross-platform
# declarations live in `./common.nix`, which is imported below so
# anything added there merges into both platform modules via the
# module-merge system.
#
# This file is the seam where nix-darwin-only declarations belong.  Disc
# ripping is not available here: it needs udev and an x86_64-linux-only
# MakeMKV build, so the rip options exist only in the NixOS module.
#
# Minimal usage (defaults to a Unix domain socket the server binds itself):
#
#   inputs.loku.darwinModules.server
#
#   services.loku-server = {
#     enable = true;
#     libraries.downloads = {
#       path = "/Users/Shared/videos";
#       kind = "downloads";
#     };
#   };
#
# To use TCP instead:
#
#   services.loku-server = {
#     enable = true;
#     socket = null;
#     port = 8080;
#     libraries.downloads = {
#       path = "/Users/Shared/videos";
#       kind = "downloads";
#     };
#   };
#
# To enable health checking (the agent probes over TCP, so this needs the TCP
# form above):
#
#   services.loku-server = {
#     enable = true;
#     socket = null;
#     healthCheck.enable = true;
#     libraries.downloads = {
#       path = "/Users/Shared/videos";
#       kind = "downloads";
#     };
#   };
{
  self,
  foundation,
}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.loku-server;
  stateDir = "/var/lib/loku-server";
  configFile = import ./config-file.nix {
    inherit lib pkgs cfg;
    # launchd has no socket activation: the server binds the socket itself.
    listen =
      if cfg.socket != null
      then toString cfg.socket
      else "${cfg.host}:${toString cfg.port}";
    # launchd has neither LoadCredential nor StateDirectory, so the secret
    # files and the state directory reach the binary as plain paths the
    # service user must be able to read.
    extra =
      {
        state_dir = stateDir;
      }
      // lib.optionalAttrs (cfg.tmdbApiKeyFile != null) {
        tmdb_api_key_file = toString cfg.tmdbApiKeyFile;
      }
      // lib.optionalAttrs (cfg.oidcClientSecretFile != null) {
        oidc_client_secret_file = toString cfg.oidcClientSecretFile;
      };
  };
in {
  imports = [
    ./common.nix
    (foundation.lib.mkDarwinService {
      name = "loku-server";
      inherit self;
    })
  ];

  config = lib.mkIf cfg.enable {
    # The foundation serves its health endpoint at /healthz, while the
    # helper's default URL names /health; with that default an enabled agent
    # would 404 on every probe and try to kill a healthy server.
    services.loku-server.healthCheck.url =
      lib.mkDefault "http://127.0.0.1:${toString cfg.port}/healthz";

    # The helper's ProgramArguments own the socket directory setup and the
    # drop to the service user, so the config file is handed over through
    # the binary's `--config` environment variable instead of the command
    # line.  ffprobe/ffmpeg must be on PATH for codec probing and
    # compat-copy derivation.
    launchd.servers.loku-server.serviceConfig.EnvironmentVariables = {
      loku_config = "${configFile}";
      PATH = "${cfg.ffmpegPackage}/bin:/usr/bin:/bin";
    };

    # Nothing else on darwin creates the state directory the media index
    # database lives in.  The BSD tools lack long option names, so coreutils
    # supplies mkdir, chown, and chmod.
    system.activationScripts.postActivation.text = ''
      ${pkgs.coreutils}/bin/mkdir --parents ${stateDir}
      ${pkgs.coreutils}/bin/chown ${cfg.user}:${cfg.group} ${stateDir}
      ${pkgs.coreutils}/bin/chmod 0750 ${stateDir}
    '';
  };
}
