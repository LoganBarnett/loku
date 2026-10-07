# Shared seam for the loku-server NixOS and nix-darwin modules.  Both
# platform entrypoints (`./nixos-server.nix` and `./darwin-server.nix`) merge
# this module in via their `imports` lists, so a declaration added here
# applies on both platforms.
#
# The foundation's service helpers declare the standard service options; only
# loku's own options live here.  The rip options are NixOS-only and live in
# `./nixos-server.nix`.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.loku-server;
in {
  options.services.loku-server = {
    # The foundation declares baseUrl without a default, which would make it
    # required on every host.  The binary itself defaults to a local address
    # so unauthenticated local use needs no configuration; this default-only
    # declaration merges with the foundation's (which carries the type and
    # description) and keeps that behaviour for module users.
    baseUrl = lib.mkOption {
      default = "http://localhost:3000";
    };

    libraries = lib.mkOption {
      type = lib.types.attrsOf (lib.types.submodule {
        options = {
          path = lib.mkOption {
            type = lib.types.path;
            description = "Root directory of this library.";
            example = "/media/videos";
          };
          kind = lib.mkOption {
            type = lib.types.enum ["downloads" "discs"];
            description = ''
              What the library holds: "downloads" for yt-dlp style trees
              with info.json sidecars, "discs" for MakeMKV disc rips.
            '';
          };
        };
      });
      description = ''
        The library roots to serve, keyed by name.  Names become URL path
        segments (/files/<name>/…), so keep them to lowercase ASCII
        letters, digits, '_', and '-'.  The server treats the first entry
        (in lexicographic name order) as the default browse target.

        Library paths must be writable by the service user: the server
        writes derived files (browser-compatible .compat.mp4 copies and
        .jpg thumbnails) beside the master videos.  Masters are never
        modified.  Other consumers of the same trees (e.g. Kodi) should
        exclude *.compat.mp4 from their scans.
      '';
      example = lib.literalExpression ''
        {
          downloads = { path = "/media/videos"; kind = "downloads"; };
          discs = { path = "/media/rips"; kind = "discs"; };
        }
      '';
    };

    ffmpegPackage = lib.mkOption {
      type = lib.types.package;
      default = pkgs.ffmpeg-headless;
      defaultText = lib.literalExpression "pkgs.ffmpeg-headless";
      description = ''
        Provides the ffprobe/ffmpeg binaries used for codec probing and
        compat-copy derivation; headless keeps the closure small.
      '';
    };

    tmdbApiKeyFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        A file holding a TMDB API key (a v3 key or a v4 read-access token).
        On NixOS it reaches the unit through systemd's LoadCredential so the
        service user never reads the secret directly; on darwin the service
        user reads the path itself.  Enables disc identity resolution:
        ripped discs get a Kodi NFO, artwork, and a directory named after
        their title.  Without it rips complete unidentified and /healthz
        reports the identity component degraded.
      '';
    };

    tmdbLanguage = lib.mkOption {
      type = lib.types.str;
      default = "en-US";
      description = "The TMDB language tag for titles and metadata (e.g. ja-JP).";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.libraries != {};
        message = "services.loku-server.libraries must define at least one library.";
      }
    ];
  };
}
