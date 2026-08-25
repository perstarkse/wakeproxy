{ self, pkgs }:
let
  secrets = import ../lib/secrets.nix { inherit pkgs; };
  network = import ../lib/network.nix;
  targetSim = import ../lib/target-sim.nix { inherit pkgs; };
  nodeNames = [
    "proxy"
    "target"
  ];
  targetIp = network.nodeIp "target" nodeNames;
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-vm-integration-proxy-wake-flow";

  nodes = {
    proxy = network.mkEth1Node {
      extraConfig = {
        imports = [ self.nixosModules.wake-proxy ];
        networking.firewall.enable = false;
        environment.systemPackages = with pkgs; [
          curl
          jq
          python3
        ];
        services.wakeproxy = import ../lib/proxy-module.nix {
          passwordHashFile = secrets.passwordHashFile;
          wolBroadcastIp = targetIp;
          extra = {
            keepAwakeCommand = "/bin/true";
            keepAwakeMaxSecs = 3600;
          };
        };
      };
    };

    target = network.mkEth1Node {
      extraConfig = {
        networking.firewall.enable = false;
        environment.systemPackages = [ pkgs.python3 ];
      };
    };
  };

  testScript = ''
    start_all()

    proxy.wait_for_unit("multi-user.target")
    proxy.wait_for_unit("wake-proxy.service")
    proxy.wait_for_open_port(8091)

    target.wait_for_unit("multi-user.target")
    target.succeed("${targetSim.initStateDir}")
    target.succeed("${targetSim.startDelayedUpstream}")
    target.wait_until_succeeds("${targetSim.waitForWolListener}")

    proxy.wait_until_succeeds("ping -c1 target >/dev/null")
    proxy.wait_until_succeeds("curl -fsS http://127.0.0.1:8091/_health >/dev/null")

    proxy.succeed("curl -sS -o /tmp/anon-body -D /tmp/anon-headers http://127.0.0.1:8091/demo")
    proxy.succeed("grep -F '303 See Other' /tmp/anon-headers")
    proxy.succeed("grep -F '/_auth/login?next=%2Fdemo' /tmp/anon-headers")

    proxy.succeed("curl -sS -o /tmp/login-body -D /tmp/login-headers -c /tmp/wake.cookies -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_auth/login?next=%2Fdemo' -d 'password=${secrets.testPassword}&next=%2Fdemo' http://127.0.0.1:8091/_auth/login")
    proxy.succeed("grep -F '303 See Other' /tmp/login-headers")
    proxy.succeed("grep -iF 'set-cookie: wakeproxy_session=' /tmp/login-headers")

    proxy.succeed("curl -sS -b /tmp/wake.cookies -o /tmp/prewake-body -D /tmp/prewake-headers http://127.0.0.1:8091/demo")
    proxy.succeed("grep -F '303 See Other' /tmp/prewake-headers")
    proxy.succeed("grep -F '/_wake?next=%2Fdemo' /tmp/prewake-headers")

    proxy.succeed("curl -sS -b /tmp/wake.cookies -o /tmp/wake-page.html http://127.0.0.1:8091/_wake?next=%2Fdemo")
    proxy.succeed("grep -F 'Defaults to 15 minutes' /tmp/wake-page.html")
    proxy.succeed("grep -F '<option value=\"900\" selected>15 minutes</option>' /tmp/wake-page.html")

    proxy.succeed("curl -sS -b /tmp/wake.cookies -o /tmp/wake-start-body -D /tmp/wake-start-headers -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_wake?next=%2Fdemo' -d 'next=%2Fdemo&keep_awake_for_secs=5' http://127.0.0.1:8091/_wake/start")
    proxy.succeed("grep -F '303 See Other' /tmp/wake-start-headers")
    proxy.succeed("grep -F '/_wake?next=%2Fdemo' /tmp/wake-start-headers")

    target.wait_until_succeeds("test -f /tmp/wakeproxy-e2e/wol.received")
    target.wait_until_succeeds("grep -F 'upstream-start-after-wol' /tmp/wakeproxy-e2e/wol.log")
    target.wait_for_open_port(3000)
    proxy.wait_until_succeeds("curl -fsS http://target:3000/health | grep -F 'ok'")

    proxy.wait_until_succeeds("curl -fsS -b /tmp/wake.cookies http://127.0.0.1:8091/_wake/poll?next=%2Fdemo > /tmp/wake-poll.json")
    proxy.wait_until_succeeds("jq -e '.ready == true and .redirect_to == \"/demo\"' /tmp/wake-poll.json >/dev/null")
    proxy.wait_until_succeeds("curl -fsS -b /tmp/wake.cookies http://127.0.0.1:8091/demo | grep -F 'demo-ok'")

    proxy.succeed("journalctl -u wake-proxy --no-pager > /tmp/wake-proxy-journal.txt")
    proxy.succeed("grep -F 'successful login' /tmp/wake-proxy-journal.txt")
    proxy.succeed("grep -F 'upstream ready' /tmp/wake-proxy-journal.txt")

    target.succeed("grep -F 'WOL from ' /tmp/wakeproxy-e2e/wol.log")
    target.succeed("grep -F 'GET /health' /tmp/wakeproxy-e2e/upstream.log")
    target.succeed("grep -F 'GET /demo' /tmp/wakeproxy-e2e/upstream.log")
  '';
}
