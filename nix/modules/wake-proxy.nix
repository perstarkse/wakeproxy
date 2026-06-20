{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.wakeproxy;
  firewallSourceRules = lib.concatMapStringsSep "\n" (source:
    if builtins.match ".*:.*" source != null
    then "ip6 saddr ${source} tcp dport ${toString cfg.port} accept"
    else "ip saddr ${source} tcp dport ${toString cfg.port} accept")
  cfg.allowedFirewallSources;
  mkFirewallExtraCommands = port: sources: let
    allowRules =
      map (
        source:
          if builtins.match ".*:.*" source != null
          then "${pkgs.iptables}/bin/ip6tables -A nixos-fw -p tcp -s ${source} --dport ${toString port} -j ACCEPT"
          else "${pkgs.iptables}/bin/iptables -A nixos-fw -p tcp -s ${source} --dport ${toString port} -j ACCEPT"
      )
      sources;
  in
    lib.concatStringsSep "\n" (
      allowRules
      ++ [
        "${pkgs.iptables}/bin/iptables -A nixos-fw -p tcp --dport ${toString port} -j DROP"
        "${pkgs.iptables}/bin/ip6tables -A nixos-fw -p tcp --dport ${toString port} -j DROP"
      ]
    );
in {
  imports = [
    (import ./keep-awake-target.nix {inherit self;})
  ];

  options.services.wakeproxy = {
    enable = lib.mkEnableOption "Wake-on-LAN authenticated reverse proxy";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "self.packages.${pkgs.stdenv.hostPlatform.system}.default";
      description = "Package containing the wakeproxy binary.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "wake-proxy";
      description = "System user to run wakeproxy.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "wake-proxy";
      description = "System group to run wakeproxy.";
    };

    listenAddress = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      description = "Address to bind the wakeproxy service to.";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 8091;
      description = "Port for wakeproxy.";
    };

    upstreamHost = lib.mkOption {
      type = lib.types.str;
      description = "Upstream host to proxy after wake.";
    };

    upstreamPort = lib.mkOption {
      type = lib.types.port;
      description = "Upstream port to proxy after wake.";
    };

    healthPath = lib.mkOption {
      type = lib.types.str;
      default = "/health";
      description = "HTTP health path on upstream.";
    };

    wolMac = lib.mkOption {
      type = lib.types.str;
      description = "Wake-on-LAN MAC address in aa:bb:cc:dd:ee:ff format.";
    };

    wolBroadcastIp = lib.mkOption {
      type = lib.types.str;
      default = "255.255.255.255";
      description = "Broadcast IP for WoL packets.";
    };

    wolBroadcastPort = lib.mkOption {
      type = lib.types.port;
      default = 9;
      description = "Broadcast port for WoL packets.";
    };

    wakeTimeout = lib.mkOption {
      type = lib.types.int;
      default = 180;
      description = "Maximum seconds to wait for wake readiness.";
    };

    pollInterval = lib.mkOption {
      type = lib.types.int;
      default = 2;
      description = "Seconds between readiness polls.";
    };

    readyCacheTtl = lib.mkOption {
      type = lib.types.int;
      default = 5;
      description = "Seconds to cache ready state to avoid redundant probes.";
    };

    wakePollIntervalMs = lib.mkOption {
      type = lib.types.int;
      default = 2000;
      description = "Milliseconds between client-side wake status polls.";
    };

    sessionTtl = lib.mkOption {
      type = lib.types.int;
      default = 43200;
      description = "Session cookie TTL in seconds.";
    };

    cookieSecure = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Set secure flag on auth cookie.";
    };

    loginMaxFailures = lib.mkOption {
      type = lib.types.int;
      default = 5;
      description = "Max login failures before lockout.";
    };

    loginLockoutSecs = lib.mkOption {
      type = lib.types.int;
      default = 60;
      description = "Lockout duration in seconds.";
    };

    loginWindowSecs = lib.mkOption {
      type = lib.types.int;
      default = 300;
      description = "Rolling window for counting failed attempts.";
    };

    loginMaxTrackedIps = lib.mkOption {
      type = lib.types.int;
      default = 10000;
      description = "Maximum IP attempt records to retain for brute-force tracking.";
    };

    trustProxyHeaders = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Trust forwarded headers when peer address is explicitly trusted.";
    };

    trustedProxyIps = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Peer IPs allowed to supply trusted X-Forwarded-* headers.";
    };

    keepAwakeCommand = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Optional command to run when keep-awake is requested (receives seconds as argument).";
    };

    keepAwakeMaxSecs = lib.mkOption {
      type = lib.types.int;
      default = 14400;
      description = "Maximum keep-awake duration in seconds.";
    };

    wakeMaxStarts = lib.mkOption {
      type = lib.types.int;
      default = 3;
      description = "Maximum wake starts per IP within the wake window.";
    };

    wakeWindowSecs = lib.mkOption {
      type = lib.types.int;
      default = 300;
      description = "Rate-limit window for wake starts in seconds.";
    };

    externalOrigin = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "https://wake.example.com";
      description = "Canonical external origin used for CSRF and websocket origin validation.";
    };

    passwordHashFile = lib.mkOption {
      type = lib.types.path;
      description = "File containing WAKEPROXY_PASSWORD_HASH and WAKEPROXY_SESSION_SECRET.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open wakeproxy port in firewall.";
    };

    allowedFirewallSources = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      example = ["10.0.0.1"];
      description = "Source IPs/CIDRs allowed to access wakeproxy. When non-empty, only these sources are accepted.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.${cfg.user} = {
      isSystemUser = true;
      inherit (cfg) group;
    };

    users.groups.${cfg.group} = {};

    systemd.services.wake-proxy = {
      description = "Wake-on-LAN web proxy";
      wantedBy = ["multi-user.target"];
      after = ["network-online.target"];
      wants = ["network-online.target"];
      path = [pkgs.iputils];

      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = lib.getExe cfg.package;
        Restart = "always";
        RestartSec = "5s";

        Environment = [
          "WAKEPROXY_UPSTREAM_HOST=${cfg.upstreamHost}"
          "WAKEPROXY_UPSTREAM_PORT=${toString cfg.upstreamPort}"
          "WAKEPROXY_HEALTH_PATH=${cfg.healthPath}"
          "WAKEPROXY_WOL_MAC=${cfg.wolMac}"
          "WAKEPROXY_WOL_BROADCAST_IP=${cfg.wolBroadcastIp}"
          "WAKEPROXY_WOL_BROADCAST_PORT=${toString cfg.wolBroadcastPort}"
          "WAKEPROXY_WAKE_TIMEOUT=${toString cfg.wakeTimeout}"
          "WAKEPROXY_POLL_INTERVAL=${toString cfg.pollInterval}"
          "WAKEPROXY_READY_CACHE_TTL=${toString cfg.readyCacheTtl}"
          "WAKEPROXY_WAKE_POLL_INTERVAL_MS=${toString cfg.wakePollIntervalMs}"
          "WAKEPROXY_BIND_ADDR=${cfg.listenAddress}:${toString cfg.port}"
          "WAKEPROXY_SESSION_TTL_SECS=${toString cfg.sessionTtl}"
          "WAKEPROXY_COOKIE_SECURE=${lib.boolToString cfg.cookieSecure}"
          "WAKEPROXY_LOGIN_MAX_FAILURES=${toString cfg.loginMaxFailures}"
          "WAKEPROXY_LOGIN_LOCKOUT_SECS=${toString cfg.loginLockoutSecs}"
          "WAKEPROXY_LOGIN_WINDOW_SECS=${toString cfg.loginWindowSecs}"
          "WAKEPROXY_LOGIN_MAX_TRACKED_IPS=${toString cfg.loginMaxTrackedIps}"
          "WAKEPROXY_TRUST_PROXY_HEADERS=${lib.boolToString cfg.trustProxyHeaders}"
          "WAKEPROXY_TRUSTED_PROXY_IPS=${lib.concatStringsSep "," cfg.trustedProxyIps}"
          "WAKEPROXY_WAKE_MAX_STARTS=${toString cfg.wakeMaxStarts}"
          "WAKEPROXY_WAKE_WINDOW_SECS=${toString cfg.wakeWindowSecs}"
        ] ++ lib.optional (cfg.externalOrigin != null) "WAKEPROXY_EXTERNAL_ORIGIN=${cfg.externalOrigin}"
          ++ lib.optional (cfg.keepAwakeCommand != null) "WAKEPROXY_KEEP_AWAKE_COMMAND=${cfg.keepAwakeCommand}"
          ++ lib.optional (cfg.keepAwakeCommand != null) "WAKEPROXY_KEEP_AWAKE_MAX_SECS=${toString cfg.keepAwakeMaxSecs}";
        EnvironmentFile = [cfg.passwordHashFile];

        NoNewPrivileges = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectControlGroups = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectClock = true;
        ProtectHostname = true;
        RestrictSUIDSGID = true;
        RestrictRealtime = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RemoveIPC = true;
        CapabilityBoundingSet = "";
        AmbientCapabilities = "";
        SystemCallArchitectures = "native";
      };
    };

    networking.firewall = {
      allowedTCPPorts = lib.mkIf (cfg.openFirewall && cfg.allowedFirewallSources == []) [cfg.port];
      extraInputRules = lib.mkIf (cfg.allowedFirewallSources != []) (lib.mkAfter ''
        ${firewallSourceRules}
        tcp dport ${toString cfg.port} drop
      '');
      extraCommands = lib.mkIf (!config.networking.nftables.enable && cfg.allowedFirewallSources != []) (lib.mkAfter ''
        ${mkFirewallExtraCommands cfg.port cfg.allowedFirewallSources}
      '');
    };
  };
}
