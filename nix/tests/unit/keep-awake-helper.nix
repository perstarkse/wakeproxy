{ self, pkgs }:
let
  secrets = import ../lib/secrets.nix { inherit pkgs; };
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-vm-unit-keep-awake-helper";

  nodes.proxy =
    { ... }:
    {
      imports = [ self.nixosModules.wake-proxy ];

      services.wakeproxy = {
        enable = true;
        upstreamHost = "127.0.0.1";
        upstreamPort = 3000;
        wolMac = "aa:bb:cc:dd:ee:ff";
        passwordHashFile = secrets.passwordHashFile;
        cookieSecure = false;
        keepAwake.sshTarget.enable = true;
      };
    };

  testScript = ''
    start_all()
    proxy.wait_for_unit("multi-user.target")
    proxy.wait_for_unit("polkit.service")
    proxy.wait_for_unit("wake-proxy.service")

    proxy.succeed("systemd-inhibit --what=idle:sleep --mode=block --why=manual-test sleep 15 >/tmp/manual-inhibit.log 2>&1 & echo $! > /tmp/manual-inhibit.pid")
    proxy.sleep(1)
    proxy.succeed("systemd-inhibit --list | grep -F 'manual-test'")
    proxy.succeed("kill $(cat /tmp/manual-inhibit.pid) || true")

    helper_env = "WAKEPROXY_KEEP_AWAKE_TARGET_MAX_SECS=30 WAKEPROXY_KEEP_AWAKE_TARGET_UNIT=wakeproxy-keep-awake-test WAKEPROXY_KEEP_AWAKE_STATE_DIR=/tmp/wakeproxy-keep-awake-test"

    proxy.succeed(f"su -s /bin/sh -c 'mkdir -p /tmp/wakeproxy-keep-awake-test && {helper_env} /run/current-system/sw/bin/wakeproxy-keep-awake-target 15' wakeproxy-keep-awake")
    proxy.sleep(1)
    proxy.succeed("systemd-inhibit --list | grep -F 'wakeproxy remote session'")
    proxy.succeed("test -s /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.pid")
    proxy.succeed("test -s /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.until")
    proxy.succeed("cat /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.pid > /tmp/long-lease.pid")
    proxy.succeed("cat /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.until > /tmp/long-lease.until")

    proxy.succeed(f"su -s /bin/sh -c 'mkdir -p /tmp/wakeproxy-keep-awake-test && {helper_env} /run/current-system/sw/bin/wakeproxy-keep-awake-target 5' wakeproxy-keep-awake")
    proxy.succeed("cmp -s /tmp/long-lease.pid /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.pid")
    proxy.succeed("cmp -s /tmp/long-lease.until /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.until")
    proxy.succeed("kill -0 $(cat /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.pid)")
    proxy.succeed("systemd-inhibit --list | grep -F 'wakeproxy remote session'")

    proxy.succeed(f"su -s /bin/sh -c 'mkdir -p /tmp/wakeproxy-keep-awake-test && {helper_env} /run/current-system/sw/bin/wakeproxy-keep-awake-target 20' wakeproxy-keep-awake")
    proxy.succeed("! cmp -s /tmp/long-lease.pid /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.pid")
    proxy.succeed("test $(cat /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.until) -gt $(cat /tmp/long-lease.until)")
    proxy.succeed("kill -0 $(cat /tmp/wakeproxy-keep-awake-test/wakeproxy-keep-awake-test.pid)")
  '';
}
