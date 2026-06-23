{ self, pkgs }:
let
  secrets = import ../lib/secrets.nix { inherit pkgs; };
  targetSim = import ../lib/target-sim.nix { inherit pkgs; };
  helper = self.packages.${pkgs.stdenv.hostPlatform.system}.wakeproxy-keep-awake-target;
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-vm-unit-keep-awake-local";

  nodes.proxy =
    { ... }:
    {
      imports = [ self.nixosModules.wake-proxy ];

      networking.firewall.enable = false;

      security.polkit.enable = true;
      security.polkit.extraConfig = ''
        polkit.addRule(function(action, subject) {
          if (
            subject.user == "wake-proxy" &&
            (
              action.id == "org.freedesktop.login1.inhibit-block-sleep" ||
              action.id == "org.freedesktop.login1.inhibit-block-idle"
            )
          ) {
            return polkit.Result.YES;
          }
        });
      '';

      services.wakeproxy = import ../lib/proxy-module.nix {
        passwordHashFile = secrets.passwordHashFile;
        upstreamHost = "127.0.0.1";
        listenAddress = "127.0.0.1";
        extra = {
          upstreamPort = 3000;
          keepAwakeCommand = "${helper}/bin/wakeproxy-keep-awake-target";
          keepAwakeMaxSecs = 30;
        };
      };

      environment.systemPackages = with pkgs; [ curl jq python3 ];
    };

  testScript = ''
    start_all()
    proxy.wait_for_unit("multi-user.target")
    proxy.wait_for_unit("polkit.service")
    proxy.wait_for_unit("wake-proxy.service")
    proxy.wait_for_open_port(8091)

    proxy.succeed("${targetSim.initStateDir}")
    proxy.succeed("${targetSim.startAlwaysUpUpstream}")
    proxy.wait_for_open_port(3000)

    proxy.succeed("curl -sS -o /dev/null -D /tmp/login-headers -c /tmp/wake.cookies -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_auth/login?next=%2Fdemo' -d 'password=${secrets.testPassword}&next=%2Fdemo' http://127.0.0.1:8091/_auth/login")
    proxy.succeed("grep -F '303 See Other' /tmp/login-headers")

    proxy.succeed("curl -sS -o /dev/null -D /tmp/wake-start-headers -b /tmp/wake.cookies -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_wake?next=%2Fdemo' -d 'next=%2Fdemo&keep_awake_for_secs=10' http://127.0.0.1:8091/_wake/start")
    proxy.succeed("grep -F '303 See Other' /tmp/wake-start-headers")

    proxy.wait_until_succeeds("systemd-inhibit --list | grep -F 'wakeproxy remote session'")
    proxy.succeed("ps aux | grep -F 'systemd-inhibit' | grep -F 'wakeproxy remote session' | grep -v grep")
  '';
}
