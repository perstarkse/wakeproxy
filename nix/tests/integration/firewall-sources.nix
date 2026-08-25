{ self, pkgs }:
let
  secrets = import ../lib/secrets.nix { inherit pkgs; };
  network = import ../lib/network.nix;
  targetSim = import ../lib/target-sim.nix { inherit pkgs; };
  nodeNames = [
    "clientAllowed"
    "clientDenied"
    "proxy"
    "target"
  ];
  proxyIp = network.nodeIp "proxy" nodeNames;
  clientAllowedIp = network.nodeIp "clientAllowed" nodeNames;
  targetIp = network.nodeIp "target" nodeNames;
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-vm-integration-firewall-sources";

  nodes = {
    proxy = network.mkEth1Node {
      extraConfig = {
        imports = [ self.nixosModules.wake-proxy ];
        networking.firewall.enable = true;
        environment.systemPackages = with pkgs; [
          curl
          python3
        ];

        services.wakeproxy = import ../lib/proxy-module.nix {
          passwordHashFile = secrets.passwordHashFile;
          wolBroadcastIp = targetIp;
          extra = {
            allowedFirewallSources = [ clientAllowedIp ];
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

    clientAllowed = network.mkEth1Node {
      extraConfig = {
        networking.firewall.enable = false;
        environment.systemPackages = [ pkgs.curl ];
      };
    };

    clientDenied = network.mkEth1Node {
      extraConfig = {
        networking.firewall.enable = false;
        environment.systemPackages = [ pkgs.curl ];
      };
    };
  };

  testScript = ''
    start_all()

    proxy.wait_for_unit("multi-user.target")
    proxy.wait_for_unit("wake-proxy.service")
    proxy.wait_for_open_port(8091)
    target.wait_for_unit("multi-user.target")
    clientAllowed.wait_for_unit("multi-user.target")
    clientDenied.wait_for_unit("multi-user.target")

    target.succeed("${targetSim.initStateDir}")
    target.succeed("${targetSim.startAlwaysUpUpstream}")
    target.wait_for_open_port(3000)

    proxy.wait_until_succeeds("curl -fsS http://target:3000/health | grep -F 'ok'")
    proxy.succeed("curl -fsS http://127.0.0.1:8091/_health >/dev/null")
    clientAllowed.succeed("ping -c1 ${proxyIp} >/dev/null")

    clientAllowed.wait_until_succeeds(
      "curl --connect-timeout 2 -fsS http://${proxyIp}:8091/_health >/dev/null",
      timeout=30,
    )
    clientDenied.fail(
      "curl --connect-timeout 2 -fsS http://${proxyIp}:8091/_health >/dev/null",
    )
  '';
}
