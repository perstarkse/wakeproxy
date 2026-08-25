{ self, pkgs }:
let
  secrets = import ../lib/secrets.nix { inherit pkgs; };
  targetSim = import ../lib/target-sim.nix { inherit pkgs; };
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-vm-unit-module-smoke";

  nodes.proxy =
    { ... }:
    {
      imports = [ self.nixosModules.wake-proxy ];

      networking = {
        useDHCP = false;
        firewall.enable = false;
      };

      services.wakeproxy = import ../lib/proxy-module.nix {
        passwordHashFile = secrets.passwordHashFile;
        upstreamHost = "127.0.0.1";
        listenAddress = "127.0.0.1";
        wolBroadcastIp = "127.0.0.1";
        extra = {
          upstreamPort = 3000;
        };
      };

      environment.systemPackages = [
        pkgs.curl
        pkgs.python3
      ];
    };

  testScript = ''
    start_all()
    proxy.wait_for_unit("multi-user.target")
    proxy.wait_for_unit("wake-proxy.service")
    proxy.wait_for_open_port(8091)

    proxy.succeed("${targetSim.initStateDir}")
    proxy.succeed("${targetSim.startAlwaysUpUpstream}")
    proxy.wait_for_open_port(3000)

    proxy.succeed("curl -fsS http://127.0.0.1:8091/_health >/dev/null")
    proxy.succeed("systemctl is-active --quiet wake-proxy.service")
    proxy.succeed("test \"$(systemctl show wake-proxy.service -p User --value)\" = wake-proxy")
    proxy.succeed("systemctl show wake-proxy.service -p Environment | grep -F 'WAKEPROXY_UPSTREAM_HOST=127.0.0.1'")
  '';
}
