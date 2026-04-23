{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.wakeproxy.keepAwake;
  helperPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.wakeproxy-keep-awake-target;
in {
  options.services.wakeproxy.keepAwake = {
    enable = lib.mkEnableOption "wakeproxy keep-awake target helper";

    package = lib.mkOption {
      type = lib.types.package;
      default = helperPackage;
      defaultText = lib.literalExpression "self.packages.${pkgs.stdenv.hostPlatform.system}.wakeproxy-keep-awake-target";
      description = "Package providing the wakeproxy keep-awake helper command.";
    };

    maxDurationSeconds = lib.mkOption {
      type = lib.types.ints.positive;
      default = 14400;
      description = "Maximum keep-awake duration accepted by the helper in seconds.";
    };

    unitName = lib.mkOption {
      type = lib.types.str;
      default = "wakeproxy-keep-awake";
      description = "Transient systemd unit name used for the active keep-awake lease.";
    };

    allowInhibitViaPolkit = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Install a polkit rule that allows the wakeproxy service user to create logind inhibit locks without interactive authentication.";
    };
  };

  config = lib.mkIf (cfg.enable && config.services.wakeproxy.enable) {
    environment.systemPackages = [cfg.package];

    systemd.services.wake-proxy.serviceConfig = {
      Environment = [
        "WAKEPROXY_KEEP_AWAKE_COMMAND=${lib.getExe cfg.package}"
        "WAKEPROXY_KEEP_AWAKE_MAX_SECS=${toString cfg.maxDurationSeconds}"
        "WAKEPROXY_KEEP_AWAKE_TARGET_MAX_SECS=${toString cfg.maxDurationSeconds}"
        "WAKEPROXY_KEEP_AWAKE_TARGET_UNIT=${cfg.unitName}"
        "WAKEPROXY_KEEP_AWAKE_STATE_DIR=/var/lib/${config.services.wakeproxy.user}"
      ];
      StateDirectory = config.services.wakeproxy.user;
      PrivateDevices = lib.mkForce false;
      ProtectControlGroups = lib.mkForce false;
      ProtectSystem = lib.mkForce "full";
    };

    security.polkit.enable = lib.mkIf cfg.allowInhibitViaPolkit true;

    security.polkit.extraConfig = lib.mkIf cfg.allowInhibitViaPolkit ''
      polkit.addRule(function(action, subject) {
        if (
          subject.user == "${config.services.wakeproxy.user}" &&
          (
            action.id == "org.freedesktop.login1.inhibit-block-sleep" ||
            action.id == "org.freedesktop.login1.inhibit-block-idle"
          )
        ) {
          return polkit.Result.YES;
        }
      });
    '';
  };
}
