# NixOS module for the loku-web service.
# Exported from the flake as nixosModules.web.
#
# Minimal usage (defaults to Unix domain socket activation):
#
#   inputs.loku.nixosModules.web
#
#   services.loku-web = {
#     enable = true;
#     libraryPath = "/media/videos";
#   };
#
# To use TCP instead:
#
#   services.loku-web = {
#     enable = true;
#     socket = null;
#     port = 8080;
#     libraryPath = "/media/videos";
#   };
#
# To reference the socket from a reverse proxy (e.g. nginx):
#
#   locations."/".proxyPass =
#     "http://unix:${config.services.loku-web.socket}";
#
# Note: when using socket mode the reverse proxy user must be a member of
# the service group (cfg.group) so it can connect to the socket.
{ self }:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.loku-web;
in
{
  options.services.loku-web = {
    enable = lib.mkEnableOption "loku-web web service";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.web;
      defaultText = lib.literalExpression "self.packages.\${system}.web";
      description = "Package providing the service binary.";
    };

    socket = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = "/run/loku-web/loku-web.sock";
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
      type = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];
      default = "info";
      description = "Tracing log verbosity level.";
    };

    logFormat = lib.mkOption {
      type = lib.types.enum [ "text" "json" ];
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
      default = "loku-web";
      description = "System user account the service runs as.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "loku-web";
      description = "System group the service runs as.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      description = "loku-web service user";
    };

    users.groups.${cfg.group} = { };

    # Create the socket directory before the socket unit tries to bind.
    systemd.tmpfiles.rules = lib.mkIf (cfg.socket != null) [
      "d ${dirOf cfg.socket} 0750 ${cfg.user} ${cfg.group} -"
    ];

    # Socket unit: systemd creates and holds the Unix domain socket, then
    # passes the open file descriptor to the service on first activation.
    systemd.sockets.loku-web = lib.mkIf (cfg.socket != null) {
      description = "loku-web Unix domain socket";
      wantedBy = [ "sockets.target" ];
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

    systemd.services.loku-web = {
      description = "loku-web web service";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ]
        ++ lib.optional (cfg.socket != null) "loku-web.socket";
      requires =
        lib.optional (cfg.socket != null) "loku-web.socket";

      environment = {
        LOG_LEVEL = cfg.logLevel;
        LOG_FORMAT = cfg.logFormat;
      };

      serviceConfig = {
        # Type = notify causes systemd to wait for the binary to call
        # sd_notify(READY=1) before marking the unit active.  The binary
        # does this via the sd-notify crate immediately after the listener
        # is bound.  NotifyAccess = main restricts who may send
        # notifications to the main process only.
        Type = "notify";
        NotifyAccess = "main";

        # Restart if no WATCHDOG=1 heartbeat arrives within 30 s.  The
        # binary reads WATCHDOG_USEC and pings at half this interval (15 s).
        # Override via systemd.services.loku-web.serviceConfig.WatchdogSec.
        WatchdogSec = lib.mkDefault "30s";

        ExecStart = "${cfg.package}/bin/loku-web"
          + (if cfg.socket != null
             then " --listen sd-listen"
             else " --listen ${cfg.host}:${toString cfg.port}")
          + " --library-path ${cfg.libraryPath}";

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
        BindReadOnlyPaths = [ cfg.libraryPath ];
      };
    };
  };
}
