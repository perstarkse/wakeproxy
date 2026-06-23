{ self, pkgs }:
let
  secrets = import ../lib/secrets.nix { inherit pkgs; };
  network = import ../lib/network.nix;
  sshKeys = import ../lib/ssh-keys.nix { inherit pkgs; };
  targetSim = import ../lib/target-sim.nix { inherit pkgs; };
  nodeNames = [ "proxy" "target" ];
  targetIp = network.nodeIp "target" nodeNames;
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-vm-integration-ssh-known-hosts";

  nodes = {
    proxy =
      network.mkEth1Node {
        extraConfig = {
          imports = [ self.nixosModules.wake-proxy ];
          networking.firewall.enable = false;
          environment.systemPackages = [ pkgs.curl pkgs.jq ];

          system.activationScripts.wakeproxySshKey = sshKeys.proxyActivationScript;

          services.wakeproxy = import ../lib/proxy-module.nix {
            passwordHashFile = secrets.passwordHashFile;
            listenAddress = "127.0.0.1";
            wolBroadcastIp = targetIp;
            keepAwake = {
              maxDurationSeconds = 30;
              remoteSsh = {
                host = "target";
                identityFile = "/var/lib/wake-proxy/keep-awake-ssh-key";
                hostPublicKey = "ssh-ed25519 INVALID_HOST_KEY_FOR_TEST";
                connectTimeoutSeconds = 5;
              };
            };
          };
        };
      };

    target =
      network.mkEth1Node {
        extraConfig = {
          imports = [ self.nixosModules.wake-proxy ];
          networking.firewall.enable = false;
          environment.systemPackages = [ pkgs.python3 ];

          services.openssh.enable = true;

          services.wakeproxy.keepAwake = {
            maxDurationSeconds = 30;
            sshTarget = {
              enable = true;
              authorizedKeys = [ sshKeys.publicKey ];
            };
          };
        };
      };
  };

  testScript = ''
    start_all()

    proxy.wait_for_unit("multi-user.target")
    proxy.wait_for_unit("wake-proxy.service")
    proxy.wait_for_open_port(8091)
    target.wait_for_unit("multi-user.target")
    target.wait_for_unit("sshd.service")

    target.succeed("${targetSim.initStateDir}")
    target.succeed("${targetSim.startAlwaysUpUpstream}")
    target.wait_for_open_port(3000)

    proxy.succeed("curl -sS -o /dev/null -D /tmp/login-headers -c /tmp/wake.cookies -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_auth/login?next=%2Fdemo' -d 'password=${secrets.testPassword}&next=%2Fdemo' http://127.0.0.1:8091/_auth/login")
    proxy.succeed("grep -F '303 See Other' /tmp/login-headers")

    proxy.succeed("curl -sS -b /tmp/wake.cookies -o /tmp/wake-start-body -D /tmp/wake-start-headers -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_wake?next=%2Fdemo' -d 'next=%2Fdemo&keep_awake_for_secs=15' http://127.0.0.1:8091/_wake/start")
    proxy.succeed("grep -F '303 See Other' /tmp/wake-start-headers")

    proxy.wait_until_succeeds("curl -fsS -b /tmp/wake.cookies http://127.0.0.1:8091/demo | grep -F 'demo-ok'")

    proxy.sleep(3)
    proxy.succeed("! systemd-inhibit --list | grep -F 'wakeproxy remote session'")
    target.succeed("! systemd-inhibit --list | grep -F 'wakeproxy remote session'")
    proxy.succeed("journalctl -u wake-proxy --no-pager | grep -Ei 'keep.?awake|ssh'")
  '';
}
