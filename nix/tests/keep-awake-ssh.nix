{ self, pkgs }:
let
  privateKey = pkgs.writeText "wakeproxy-keep-awake-test-key" ''
    -----BEGIN OPENSSH PRIVATE KEY-----
    b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
    QyNTUxOQAAACClbrLeIng+fxAnDxFMQeyzajxLU0OFKv2oDiCAP02x6gAAAKCYkOzvmJDs
    7wAAAAtzc2gtZWQyNTUxOQAAACClbrLeIng+fxAnDxFMQeyzajxLU0OFKv2oDiCAP02x6g
    AAAEBbiHT2aCwQG1c3JfcQNrMAQZTFnQ+79u6LpwrXgPw49KVust4ieD5/ECcPEUxB7LNq
    PEtTQ4Uq/agOIIA/TbHqAAAAGXdha2Vwcm94eS1rZWVwLWF3YWtlLXRlc3QBAgME
    -----END OPENSSH PRIVATE KEY-----
  '';
  publicKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKVust4ieD5/ECcPEUxB7LNqPEtTQ4Uq/agOIIA/TbHq wakeproxy-keep-awake-test";
  passwordSecrets = pkgs.writeText "wakeproxy-secrets" ''
    WAKEPROXY_PASSWORD_HASH=$argon2id$v=19$m=19456,t=2,p=1$yDDaCqWkOPAHJMrn5KI97w$+sEaPfPEi0Ic2N7Op7r/nOJkdFmZ22/QRdb2QTs/0Vc
    WAKEPROXY_SESSION_SECRET=very-secret-session-key
  '';
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-keep-awake-ssh";

  nodes = {
    proxy = { ... }: {
      imports = [ self.nixosModules.wake-proxy ];

      networking.useDHCP = false;
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = "192.168.1.3";
          prefixLength = 24;
        }
      ];
      networking.firewall.enable = false;
      environment.systemPackages = [ pkgs.curl ];

      system.activationScripts.wakeproxySshKey = ''
        mkdir -p /var/lib/wake-proxy
        cp ${privateKey} /var/lib/wake-proxy/keep-awake-ssh-key
        chmod 600 /var/lib/wake-proxy/keep-awake-ssh-key
        chown wake-proxy /var/lib/wake-proxy/keep-awake-ssh-key
      '';

      services.wakeproxy = {
        enable = true;
        listenAddress = "127.0.0.1";
        port = 8091;
        upstreamHost = "target";
        upstreamPort = 3000;
        wolMac = "aa:bb:cc:dd:ee:ff";
        wolBroadcastIp = "192.168.1.2";
        wolBroadcastPort = 4009;
        wakeTimeout = 20;
        pollInterval = 1;
        wakePollIntervalMs = 500;
        readyCacheTtl = 1;
        passwordHashFile = passwordSecrets;
        cookieSecure = false;
        keepAwake = {
          maxDurationSeconds = 30;
          remoteSsh = {
            host = "target";
            identityFile = "/var/lib/wake-proxy/keep-awake-ssh-key";
            hostPublicKey = null;
            connectTimeoutSeconds = 5;
          };
        };
      };
    };

    target = { ... }: {
      imports = [ self.nixosModules.wake-proxy ];

      networking.useDHCP = false;
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = "192.168.1.2";
          prefixLength = 24;
        }
      ];
      networking.firewall.enable = false;
      services.openssh.enable = true;
      environment.systemPackages = [ pkgs.python3 ];

      services.wakeproxy.keepAwake = {
        maxDurationSeconds = 30;
        sshTarget = {
          enable = true;
          authorizedKeys = [ publicKey ];
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

target.succeed("""cat > /tmp/upstream.py <<'PY'
import http.server
import socketserver

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        return
    def do_GET(self):
        self.send_response(200 if self.path == '/health' else 404)
        self.send_header('Content-Length', '2')
        self.end_headers()
        self.wfile.write(b'ok')

with socketserver.TCPServer(('0.0.0.0', 3000), Handler) as httpd:
    httpd.serve_forever()
PY""")

target.succeed("python3 /tmp/upstream.py >/tmp/upstream.log 2>&1 &")
target.wait_for_open_port(3000)
proxy.wait_until_succeeds("curl -fsS http://target:3000/health | grep -F 'ok'")

proxy.succeed("curl -sS -o /dev/null -D /tmp/login-headers -c /tmp/wake.cookies -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_auth/login?next=%2Fdemo' -d 'password=test-password&next=%2Fdemo' http://127.0.0.1:8091/_auth/login")
proxy.succeed("grep -F '303 See Other' /tmp/login-headers")
proxy.succeed("curl -sS -b /tmp/wake.cookies -o /tmp/wake-start-body -D /tmp/wake-start-headers -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_wake?next=%2Fdemo' -d 'next=%2Fdemo&keep_awake_for_secs=15' http://127.0.0.1:8091/_wake/start")
proxy.succeed("grep -F '303 See Other' /tmp/wake-start-headers")

target.wait_until_succeeds("systemd-inhibit --list | grep -F 'wakeproxy remote session'", timeout=30)
target.succeed("test -s /var/lib/wakeproxy-keep-awake/wakeproxy-keep-awake.pid")
target.succeed("test -s /var/lib/wakeproxy-keep-awake/wakeproxy-keep-awake.until")
proxy.succeed("! systemd-inhibit --list | grep -F 'wakeproxy remote session'")
  '';
}
