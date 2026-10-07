# NixOS (Linux/systemd) module for the loku-server service.
# Thin wrapper around the foundation's mkNixosService helper.  See
# mkDarwinService for the macOS/launchd equivalent.  Cross-platform
# declarations live in `./common.nix`, which is imported below so
# anything added there merges into both platform modules via the
# module-merge system.
#
# This file is the seam where NixOS-only declarations belong.
#
# Minimal usage (defaults to Unix domain socket activation):
#
#   inputs.loku.nixosModules.server
#
#   services.loku-server = {
#     enable = true;
#     libraries = {
#       downloads = { path = "/media/videos"; kind = "downloads"; };
#       discs = { path = "/media/rips"; kind = "discs"; };
#     };
#   };
#
# To use TCP instead:
#
#   services.loku-server = {
#     enable = true;
#     socket = null;
#     port = 8080;
#     libraries.downloads = { path = "/media/videos"; kind = "downloads"; };
#   };
#
# To reference the socket from a reverse proxy (e.g. nginx):
#
#   locations."/".proxyPass =
#     "http://unix:${config.services.loku-server.socket}";
#
# Note: when using socket mode the reverse proxy user must be a member of the
# service group (cfg.group) so it can connect to the socket.
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
  configFile = import ./config-file.nix {
    inherit lib pkgs cfg;
    listen =
      if cfg.socket != null
      then "sd-listen"
      else "${cfg.host}:${toString cfg.port}";
    extra = lib.optionalAttrs cfg.rip.enable {
      rip = {
        device = cfg.rip.device;
        library = cfg.rip.library;
        policy = cfg.rip.policy;
        eject = cfg.rip.eject;
        poll_interval_secs = cfg.rip.pollIntervalSecs;
        replaced_retention_days = cfg.rip.replacedRetentionDays;
      };
    };
  };
in {
  imports = [
    ./common.nix
    (foundation.lib.mkNixosService {
      name = "loku-server";
      inherit self;
    })
  ];

  # Ripping drives the optical drive through udev and the SCSI generic node
  # and needs MakeMKV, which nixpkgs builds for x86_64-linux only, so the
  # option is declared for NixOS alone.
  options.services.loku-server.rip = lib.mkOption {
    type = lib.types.submodule {
      options = {
        enable = lib.mkEnableOption "disc ripping from an optical drive";

        device = lib.mkOption {
          type = lib.types.str;
          default = "/dev/sr0";
          description = "The optical drive the server polls for discs.";
        };

        library = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            The name of the discs-kind library (a key of `libraries`)
            rips land in.  Required when ripping is enabled.
          '';
        };

        policy = lib.mkOption {
          type = lib.types.enum ["skip" "rescan" "rerip"];
          default = "skip";
          description = ''
            What to do when an inserted disc's directory already exists:
            leave it alone (skip), refresh only its sidecars (rescan), or
            rip again and move the previous tree aside (rerip).  A manual
            POST to /api/rips can override this per disc.
          '';
        };

        eject = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Eject the disc once a rip completes.";
        };

        pollIntervalSecs = lib.mkOption {
          type = lib.types.ints.positive;
          default = 5;
          description = "How often the drive is polled for a disc, in seconds.";
        };

        replacedRetentionDays = lib.mkOption {
          type = lib.types.ints.unsigned;
          default = 14;
          description = ''
            How long replaced and failed rip trees stay under the
            library's hidden .rip/ directory before the daily sweep
            deletes them.
          '';
        };

        makemkvPackage = lib.mkOption {
          type = lib.types.package;
          default = pkgs.makemkv;
          defaultText = lib.literalExpression "pkgs.makemkv";
          description = ''
            Provides makemkvcon.  MakeMKV is unfree, so the host must
            allow it (for example through nixpkgs.config.allowUnfreePredicate).
          '';
        };
      };
    };
    default = {};
    description = ''
      Disc ripping.  When enabled the server polls the drive, rips every
      title of an inserted disc into a per-disc directory of the named
      library, and keeps the MakeMKV disc analysis beside the MKVs.
      makemkvcon reads its licence key (settings.conf) and the AACS
      KEYDB.cfg from $HOME/.MakeMKV, which the unit points at the state
      directory (/var/lib/loku-server); provisioning those files is host
      configuration.
    '';
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion =
          cfg.rip.enable
          -> (cfg.rip.library
            != null
            && cfg.libraries ? ${cfg.rip.library}
            && cfg.libraries.${cfg.rip.library}.kind == "discs");
        message = "services.loku-server.rip.library must name a configured discs-kind library.";
      }
    ];

    # MakeMKV drives the drive through its SCSI generic node.
    boot.kernelModules = lib.mkIf cfg.rip.enable ["sg"];

    systemd.services.loku-server = {
      # ffprobe/ffmpeg power the codec probe pass and the compat-derivation
      # worker; without them the server runs degraded (no probing, no
      # derived copies).  Ripping adds makemkvcon, udevadm (drive polling
      # via the udev database), and eject; without them ripping is disabled
      # and the rip health component reports degraded.
      path =
        [cfg.ffmpegPackage]
        ++ lib.optionals cfg.rip.enable [
          cfg.rip.makemkvPackage
          pkgs.util-linux
          pkgs.systemd
        ];

      serviceConfig =
        {
          # The library list cannot be expressed on the command line, so the
          # listener, base URL, and libraries travel in a config file; this
          # plain assignment replaces the helper's mkDefault ExecStart.
          ExecStart = "${cfg.package}/bin/loku-server --config ${configFile}";

          # The media index database lands in /var/lib/loku-server; the server
          # discovers it via the STATE_DIRECTORY environment variable.
          StateDirectory = "loku-server";

          # The helper contributes the OIDC credential at mkDefault priority,
          # and a plain list definition here would discard it rather than
          # merge with it, so both credentials are listed here.  systemd
          # copies each into a unit-private credential directory; the binary
          # finds them there by name.
          LoadCredential =
            lib.optional (cfg.oidcClientSecretFile != null)
            "oidc-client-secret:${cfg.oidcClientSecretFile}"
            ++ lib.optional (cfg.tmdbApiKeyFile != null)
            "tmdb-api-key:${cfg.tmdbApiKeyFile}";

          # Identity resolution talks to TMDB over HTTPS; rustls finds the
          # system trust store through SSL_CERT_FILE under ProtectSystem=strict.
          # makemkvcon reads $HOME/.MakeMKV for its licence key and KEYDB.cfg
          # and may write there; the state directory is the writable home.
          Environment =
            ["SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt"]
            ++ lib.optional cfg.rip.enable "HOME=/var/lib/loku-server";

          # ProtectSystem = "strict" makes the whole filesystem read-only by
          # default.  Libraries are read-write because derived compat copies
          # and thumbnails are written beside the masters (and, when ripping,
          # the hidden .rip/ staging directory lives inside the discs library).
          ReadWritePaths = lib.mapAttrsToList (_: value: value.path) cfg.libraries;
        }
        // lib.optionalAttrs cfg.rip.enable {
          # makemkvcon needs the drive (block-sr) and its SCSI generic node
          # (char-sg); both are group cdrom under the default udev rules.  If a
          # drive never reports media changes to udev, add
          # boot.kernelParams = ["block.events_dfl_poll_msecs=2000"] so the
          # kernel polls it.
          SupplementaryGroups = ["cdrom"];
          DevicePolicy = "closed";
          DeviceAllow = ["block-sr rw" "char-sg rw"];
        };
    };
  };
}
