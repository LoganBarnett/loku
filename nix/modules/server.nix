# NixOS module for the loku-server service.
# Exported from the flake as nixosModules.server.
#
# Minimal usage (defaults to Unix domain socket activation):
#
#   inputs.loku.nixosModules.server
#
#   services.loku-server = {
#     enable = true;
#     libraryPath = "/media/videos";
#   };
#
# To use TCP instead:
#
#   services.loku-server = {
#     enable = true;
#     socket = null;
#     port = 8080;
#     libraryPath = "/media/videos";
#   };
#
# To reference the socket from a reverse proxy (e.g. nginx):
#
#   locations."/".proxyPass =
#     "http://unix:${config.services.loku-server.socket}";
#
# Note: when using socket mode the reverse proxy user must be a member of the
# service group (cfg.group) so it can connect to the socket.
{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.loku-server;
in {
  options.services.loku-server = {
    enable = lib.mkEnableOption "loku-server video service";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.server;
      defaultText = lib.literalExpression "self.packages.\${system}.server";
      description = "Package providing the service binary.";
    };

    socket = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = "/run/loku-server/loku-server.sock";
      description = ''
        Path for the Unix domain socket used by the service.  When set,
        systemd socket activation is used and the host/port options are
        ignored.  Set to null to use TCP instead.

        Other services (e.g. nginx) that proxy to this socket must be
        members of the service group to connect.
      '';
    };

    host = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      description = "IP address to bind to.  Ignored when socket is set.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 3000;
      description = "TCP port to listen on.  Ignored when socket is set.";
    };

    logLevel = lib.mkOption {
      type = lib.types.enum ["trace" "debug" "info" "warn" "error"];
      default = "info";
      description = "Tracing log verbosity level.";
    };

    logFormat = lib.mkOption {
      type = lib.types.enum ["text" "json"];
      default = "json";
      description = ''
        Log output format.  Use "text" for human-readable local logs and
        "json" for structured logs consumed by a log aggregator.
      '';
    };

    libraryPath = lib.mkOption {
      type = lib.types.path;
      description = "Root directory of the video library to serve.";
      example = "/media/videos";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "loku-server";
      description = "System user account the service runs as.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "loku-server";
      description = "System group the service runs as.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      description = "loku-server service user";
    };

    users.groups.${cfg.group} = {};

    # Create the socket directory before the socket unit tries to bind.
    systemd.tmpfiles.rules = lib.mkIf (cfg.socket != null) [
      "d ${dirOf cfg.socket} 0750 ${cfg.user} ${cfg.group} -"
    ];

    # Socket unit: systemd creates and holds the Unix domain socket, then
    # passes the open file descriptor to the service on first activation.
    systemd.sockets.loku-server = lib.mkIf (cfg.socket != null) {
      description = "loku-server Unix domain socket";
      wantedBy = ["sockets.target"];
      socketConfig = {
        ListenStream = cfg.socket;
        SocketUser = cfg.user;
        SocketGroup = cfg.group;
        # 0660: accessible to the service user and group only.  Add the
        # reverse proxy user to cfg.group to grant it access.
        SocketMode = "0660";
        Accept = false;
      };
    };

    systemd.services.loku-server = {
      description = "loku-server video service";
      wantedBy = ["multi-user.target"];
      after =
        ["network.target"]
        ++ lib.optional (cfg.socket != null) "loku-server.socket";
      requires =
        lib.optional (cfg.socket != null) "loku-server.socket";

      serviceConfig = {
        # Type = notify causes systemd to wait for the binary to call
        # sd_notify(READY=1) before marking the unit active.  Foundation's
        # server runner does this immediately after the listener is bound.
        # NotifyAccess = main restricts who may send notifications to the main
        # process only.
        Type = "notify";
        NotifyAccess = "main";

        # Restart if no WATCHDOG=1 heartbeat arrives within 30 s.  The binary
        # reads WATCHDOG_USEC and pings at half this interval (15 s).  Override
        # via systemd.services.loku-server.serviceConfig.WatchdogSec.
        WatchdogSec = lib.mkDefault "30s";

        ExecStart =
          "${cfg.package}/bin/loku-server"
          + (
            if cfg.socket != null
            then " --listen sd-listen"
            else " --listen ${cfg.host}:${toString cfg.port}"
          )
          + " --library ${cfg.libraryPath}"
          + " --log-level ${cfg.logLevel}"
          + " --log-format ${cfg.logFormat}";

        User = cfg.user;
        Group = cfg.group;
        Restart = "on-failure";
        RestartSec = "5s";

        # Harden the service environment.
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;

        # Grant read access to the video library.  ProtectSystem = "strict"
        # makes the whole filesystem read-only by default, so the library
        # directory must be explicitly allowed.
        BindReadOnlyPaths = [cfg.libraryPath];
      };
    };
  };
}
