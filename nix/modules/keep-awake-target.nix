{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.wakeproxy.keepAwake;
  helperPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.wakeproxy-keep-awake-target;

  sshKnownHostsFile = pkgs.writeText "wakeproxy-keep-awake-known-hosts" ''
    [${cfg.remoteSsh.host}]:${toString cfg.remoteSsh.port} ${cfg.remoteSsh.hostPublicKey}
  '';

  sshKnownHostsArgs =
    if cfg.remoteSsh.hostPublicKey != null
    then ["-o" "UserKnownHostsFile=${sshKnownHostsFile}"]
    else ["-o" "UserKnownHostsFile=/dev/null"];

  sshCommand = pkgs.writeShellApplication {
    name = "wakeproxy-keep-awake-ssh";
    runtimeInputs = [pkgs.openssh];
    text = ''
      set -eu

      seconds="''${1:?missing duration seconds}"
      exec ssh ${lib.escapeShellArgs (
        [
          "-i"
          cfg.remoteSsh.identityFile
          "-o"
          "BatchMode=yes"
          "-o"
          "IdentitiesOnly=yes"
          "-o"
          "StrictHostKeyChecking=${
            if cfg.remoteSsh.hostPublicKey == null
            then "accept-new"
            else "yes"
          }"
        ]
        ++ sshKnownHostsArgs
        ++ [
          "-o"
          "ConnectTimeout=${toString cfg.remoteSsh.connectTimeoutSeconds}"
          "-p"
          (toString cfg.remoteSsh.port)
          "${cfg.remoteSsh.user}@${cfg.remoteSsh.host}"
        ]
      )} "$seconds"
    '';
  };

  forcedCommand = pkgs.writeShellScript "wakeproxy-keep-awake-forced-command" ''
    set -eu

    case "''${SSH_ORIGINAL_COMMAND:-}" in
      "")
        echo "missing duration seconds" >&2
        exit 1
        ;;
      *[!0123456789]*)
        echo "duration must be a positive integer" >&2
        exit 1
        ;;
    esac

    export WAKEPROXY_KEEP_AWAKE_TARGET_MAX_SECS=${lib.escapeShellArg (toString cfg.maxDurationSeconds)}
    export WAKEPROXY_KEEP_AWAKE_TARGET_UNIT=${lib.escapeShellArg cfg.unitName}
    export WAKEPROXY_KEEP_AWAKE_STATE_DIR=${lib.escapeShellArg "/var/lib/${cfg.sshTarget.user}"}
    exec ${lib.getExe cfg.package} "$SSH_ORIGINAL_COMMAND"
  '';

  forcedKeyOptions = ''command="${forcedCommand}",restrict,no-agent-forwarding,no-X11-forwarding,no-port-forwarding,no-pty'';

  restrictAuthorizedKeys = pkgs.writeShellApplication {
    name = "wakeproxy-restrict-authorized-keys";
    runtimeInputs = [pkgs.coreutils];
    text = ''
      set -eu

      source_file="''${1:?missing source public key file}"
      target_file="''${2:?missing target authorized_keys file}"
      mkdir -p "$(dirname "$target_file")"
      : > "$target_file"
      while IFS= read -r key || [ -n "$key" ]; do
        [ -n "$key" ] || continue
        printf '%s %s\n' '${forcedKeyOptions}' "$key" >> "$target_file"
      done < "$source_file"
    '';
  };

  isWakeproxyHost = config.services.wakeproxy.enable && cfg.remoteSsh.host != "";
  isTargetHost = cfg.sshTarget.enable;
in {
  options.services.wakeproxy.keepAwake = {
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
      description = "Install a polkit rule that allows the keep-awake user to create logind inhibit locks without interactive authentication.";
    };

    remoteSsh = {
      host = lib.mkOption {
        type = lib.types.str;
        default = "";
        description = "Remote host that should receive the keep-awake inhibit lock. Set to enable remote keep-awake.";
      };

      port = lib.mkOption {
        type = lib.types.port;
        default = 22;
        description = "SSH port on the remote keep-awake target.";
      };

      user = lib.mkOption {
        type = lib.types.str;
        default = "wakeproxy-keep-awake";
        description = "Remote SSH user dedicated to keep-awake requests.";
      };

      identityFile = lib.mkOption {
        type = lib.types.path;
        default = "/var/lib/wake-proxy/keep-awake-ssh-key";
        description = "Private SSH key used by wakeproxy to call the remote keep-awake target.";
      };

      hostPublicKey = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Remote SSH host public key. When set, strict host key checking is enabled.";
      };

      connectTimeoutSeconds = lib.mkOption {
        type = lib.types.ints.positive;
        default = 10;
        description = "SSH connection timeout for remote keep-awake requests.";
      };
    };

    sshTarget = {
      enable = lib.mkEnableOption "accepting remote keep-awake requests over SSH";

      user = lib.mkOption {
        type = lib.types.str;
        default = "wakeproxy-keep-awake";
        description = "Local SSH user dedicated to remote keep-awake requests.";
      };

      group = lib.mkOption {
        type = lib.types.str;
        default = "wakeproxy-keep-awake";
        description = "Local group for the keep-awake SSH user.";
      };

      authorizedKeys = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [];
        description = "Public SSH keys allowed to request keep-awake leases. Forced-command restrictions are added automatically.";
      };

      authorizedKeysFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "File containing plain public SSH keys allowed to request keep-awake leases. Forced-command restrictions are added automatically.";
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf isWakeproxyHost {
      systemd.services.wake-proxy = {
        path = [pkgs.openssh];
        serviceConfig.Environment = [
          "WAKEPROXY_KEEP_AWAKE_COMMAND=${lib.getExe sshCommand}"
          "WAKEPROXY_KEEP_AWAKE_MAX_SECS=${toString cfg.maxDurationSeconds}"
        ];
      };
    })

    (lib.mkIf isTargetHost {
      environment.systemPackages = [cfg.package];

      security.polkit.enable = lib.mkIf cfg.allowInhibitViaPolkit true;
      security.polkit.extraConfig = lib.mkIf cfg.allowInhibitViaPolkit ''
        polkit.addRule(function(action, subject) {
          if (
            subject.user == "${cfg.sshTarget.user}" &&
            (
              action.id == "org.freedesktop.login1.inhibit-block-sleep" ||
              action.id == "org.freedesktop.login1.inhibit-block-idle"
            )
          ) {
            return polkit.Result.YES;
          }
        });
      '';

      users.groups.${cfg.sshTarget.group} = {};
      users.users.${cfg.sshTarget.user} = {
        isSystemUser = true;
        group = cfg.sshTarget.group;
        shell = pkgs.bash;
        openssh.authorizedKeys.keys = let
          fileKeys =
            if cfg.sshTarget.authorizedKeysFile != null
            then lib.filter (k: k != "") (lib.splitString "\n" (builtins.readFile cfg.sshTarget.authorizedKeysFile))
            else [];
          allKeys = cfg.sshTarget.authorizedKeys ++ fileKeys;
        in
          map (key: "${forcedKeyOptions} ${key}") allKeys;
      };

      systemd.tmpfiles.rules = [
        "d /var/lib/${cfg.sshTarget.user} 0755 ${cfg.sshTarget.user} ${cfg.sshTarget.group} -"
        "d /etc/ssh/authorized_keys.d 0755 root root -"
      ];

      system.activationScripts.wakeproxyKeepAwakeAuthorizedKeys = lib.mkIf (cfg.sshTarget.authorizedKeysFile != null) ''
        ${lib.getExe restrictAuthorizedKeys} ${lib.escapeShellArg cfg.sshTarget.authorizedKeysFile} /etc/ssh/authorized_keys.d/${cfg.sshTarget.user}
      '';
    })
  ];
}
