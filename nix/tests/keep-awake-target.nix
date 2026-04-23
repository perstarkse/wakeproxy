{ self, pkgs }:
let
  passwordSecrets = pkgs.writeText "wakeproxy-secrets" ''
    WAKEPROXY_PASSWORD_HASH=$argon2id$v=19$m=19456,t=2,p=1$dGVzdHRlc3R0ZXN0dGVzdA$0xDPfypM3Y76kJumWn95v9PoW7A1WeNseyX2VINeodQ
    WAKEPROXY_SESSION_SECRET=very-secret-session-key
  '';
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-keep-awake-target";

  nodes.machine = { ... }: {
    imports = [ self.nixosModules.wake-proxy ];

    services.wakeproxy = {
      enable = true;
      upstreamHost = "127.0.0.1";
      upstreamPort = 3000;
      wolMac = "aa:bb:cc:dd:ee:ff";
      passwordHashFile = passwordSecrets;
      cookieSecure = false;

      keepAwake = {
        enable = true;
        unitName = "wakeproxy-keep-awake-test";
        maxDurationSeconds = 30;
      };
    };
  };

  testScript = ''
    start_all()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("polkit.service")
    machine.wait_for_unit("wake-proxy.service")

    machine.succeed("systemd-inhibit --what=idle:sleep --mode=block --why=manual-test sleep 15 >/tmp/manual-inhibit.log 2>&1 &")
    machine.sleep(1)
    machine.succeed("systemd-inhibit --list | grep -F 'manual-test'")
    machine.succeed("pkill -f 'manual-test sleep 15' || true")

    machine.succeed("su -s /bin/sh -c '/run/current-system/sw/bin/wakeproxy-keep-awake-target 15' wake-proxy")
    machine.sleep(1)
    machine.succeed("systemd-inhibit --list | grep -F 'wakeproxy remote session'")
    machine.succeed("test -s /var/lib/wake-proxy/wakeproxy-keep-awake-test.pid")
  '';
}
