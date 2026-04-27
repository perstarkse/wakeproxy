{ self, pkgs }:
let
  passwordSecrets = pkgs.writeText "wakeproxy-secrets" ''
    WAKEPROXY_PASSWORD_HASH=$argon2id$v=19$m=19456,t=2,p=1$yDDaCqWkOPAHJMrn5KI97w$+sEaPfPEi0Ic2N7Op7r/nOJkdFmZ22/QRdb2QTs/0Vc
    WAKEPROXY_SESSION_SECRET=very-secret-session-key
  '';
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-keep-awake-debug";

  nodes.machine = { config, ... }: {
    imports = [ self.nixosModules.wake-proxy ];

    services.wakeproxy = {
      enable = true;
      listenAddress = "127.0.0.1";
      port = 8091;
      upstreamHost = "127.0.0.1";
      upstreamPort = 3000;
      wolMac = "aa:bb:cc:dd:ee:ff";
      passwordHashFile = passwordSecrets;
      cookieSecure = false;
      keepAwake = {
        enable = true;
        maxDurationSeconds = 30;
      };
    };

    environment.systemPackages = with pkgs; [ curl jq python3 gawk ];
  };

  testScript = ''
start_all()
machine.wait_for_unit("multi-user.target")
machine.wait_for_unit("polkit.service")
machine.wait_for_unit("wake-proxy.service")
machine.wait_for_open_port(8091)

# Start a mock upstream that responds on /health
machine.succeed("""
cat > /tmp/upstream.py <<'PY'
import http.server
import socketserver
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args): return
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/plain')
        self.end_headers()
        self.wfile.write(b'ok')
with socketserver.TCPServer(('127.0.0.1', 3000), Handler) as httpd:
    httpd.serve_forever()
PY
python3 /tmp/upstream.py >/tmp/upstream.log 2>&1 &
echo $! > /tmp/upstream.pid
""")
machine.wait_until_succeeds("test -s /tmp/upstream.pid && kill -0 $(cat /tmp/upstream.pid)")

# Login
machine.succeed("curl -sS -o /dev/null -D /tmp/login-headers -c /tmp/wake.cookies -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_auth/login?next=%2Fdemo' -d 'password=test-password&next=%2Fdemo' http://127.0.0.1:8091/_auth/login")
machine.succeed("grep -F '303 See Other' /tmp/login-headers")

# Trigger wake with 10 second keep-awake
machine.succeed("curl -sS -o /dev/null -D /tmp/wake-start-headers -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_wake?next=%2Fdemo' -d 'next=%2Fdemo&keep_awake_for_secs=10' http://127.0.0.1:8091/_wake/start")
machine.succeed("grep -F '303 See Other' /tmp/wake-start-headers")

# Check wake-proxy logs to see if wake cycle completed
machine.succeed("journalctl -u wake-proxy --no-pager")

# Poll until inhibitor appears (give it up to 30 seconds)
machine.wait_until_succeeds("systemd-inhibit --list | grep -F 'wakeproxy remote session'")

# Check state directory
machine.succeed("ls -la /var/lib/wake-proxy/")

# Check if pid file exists - should fail if not present
machine.succeed("test -s /var/lib/wake-proxy/wakeproxy-keep-awake.pid")

# Check log file for errors - should fail if not present
machine.succeed("cat /var/lib/wake-proxy/wakeproxy-keep-awake.log")

# Check if process is running - should fail if not running
machine.succeed("ps aux | grep -i 'systemd-inhibit.*wakeproxy' | grep -v grep")
  '';
}
