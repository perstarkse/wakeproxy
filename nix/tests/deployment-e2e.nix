{ self, pkgs }:
let
  passwordSecrets = pkgs.writeText "wakeproxy-secrets" ''
    WAKEPROXY_PASSWORD_HASH=$argon2id$v=19$m=19456,t=2,p=1$yDDaCqWkOPAHJMrn5KI97w$+sEaPfPEi0Ic2N7Op7r/nOJkdFmZ22/QRdb2QTs/0Vc
    WAKEPROXY_SESSION_SECRET=very-secret-session-key
  '';
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-deployment-e2e";

  nodes = {
    client =
      { ... }:
      {
        imports = [ self.nixosModules.wake-proxy ];

        networking.useDHCP = false;
        networking.interfaces.eth1.ipv4.addresses = [
          {
            address = "192.168.1.3";
            prefixLength = 24;
          }
        ];
        networking.firewall.enable = false;
        environment.systemPackages = with pkgs; [ curl jq python3 ];

        services.wakeproxy = {
          enable = true;
          listenAddress = "0.0.0.0";
          port = 8091;
          upstreamHost = "server";
          upstreamPort = 3000;
          wolMac = "aa:bb:cc:dd:ee:ff";
          wolBroadcastIp = "192.168.1.2";
          wolBroadcastPort = 4009;
          wakeTimeout = 20;
          pollInterval = 1;
          readyCacheTtl = 1;
          wakePollIntervalMs = 500;
          passwordHashFile = passwordSecrets;
          cookieSecure = false;
          loginMaxFailures = 1000;


        };
      };

    server =
      { ... }:
      {
        networking.useDHCP = false;
        networking.interfaces.eth1.ipv4.addresses = [
          {
            address = "192.168.1.2";
            prefixLength = 24;
          }
        ];
        networking.firewall.enable = false;
        environment.systemPackages = [ pkgs.python3 ];
      };
  };

  testScript = ''
start_all()

client.wait_for_unit("multi-user.target")
client.wait_for_unit("wake-proxy.service")
client.wait_for_open_port(8091)

server.wait_for_unit("multi-user.target")
server.succeed("mkdir -p /tmp/wakeproxy-e2e && : > /tmp/wakeproxy-e2e/wol.log && : > /tmp/wakeproxy-e2e/upstream.log")
server.succeed("""cat > /tmp/wakeproxy-e2e/upstream.py <<'PY'
import http.server
import pathlib
import socketserver

state_dir = pathlib.Path('/tmp/wakeproxy-e2e')
log_path = state_dir / 'upstream.log'

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        return

    def _write(self, status, body):
        body_bytes = body.encode()
        self.send_response(status)
        self.send_header('Content-Type', 'text/plain; charset=utf-8')
        self.send_header('Content-Length', str(len(body_bytes)))
        self.end_headers()
        self.wfile.write(body_bytes)

    def do_GET(self):
        with log_path.open('a', encoding='utf-8') as log:
            log.write(f'GET {self.path}\\n')
        if self.path == '/health':
            self._write(200, 'ok')
        elif self.path == '/demo':
            self._write(200, 'demo-ok')
        else:
            self._write(404, 'not-found')

with socketserver.TCPServer(('0.0.0.0', 3000), Handler) as httpd:
    httpd.serve_forever()
PY""")

server.succeed("""cat > /tmp/wakeproxy-e2e/wol-listener.py <<'PY'
import pathlib
import socket

state_dir = pathlib.Path('/tmp/wakeproxy-e2e')
log_path = state_dir / 'wol.log'
magic_prefix = b'\\xff' * 6
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
sock.bind(('0.0.0.0', 4009))
packet, addr = sock.recvfrom(4096)
with log_path.open('a', encoding='utf-8') as log:
    log.write(f'WOL from {addr[0]}:{addr[1]} bytes={len(packet)}\\n')
    log.write(packet.hex() + '\\n')
if not packet.startswith(magic_prefix):
    raise SystemExit('missing WoL magic prefix')
(state_dir / 'wol.received').write_text('1', encoding='utf-8')
PY""")

server.succeed("""cat > /tmp/wakeproxy-e2e/bring-upstream.sh <<'SH'
#!/bin/sh
set -eu
while [ ! -f /tmp/wakeproxy-e2e/wol.received ]; do
  sleep 0.2
done
echo upstream-start-after-wol >> /tmp/wakeproxy-e2e/wol.log
exec python3 /tmp/wakeproxy-e2e/upstream.py
SH""")
server.succeed("chmod +x /tmp/wakeproxy-e2e/bring-upstream.sh")

server.succeed("python3 /tmp/wakeproxy-e2e/wol-listener.py >/tmp/wakeproxy-e2e/wol-listener.stdout 2>/tmp/wakeproxy-e2e/wol-listener.stderr & echo $! > /tmp/wakeproxy-e2e/wol-listener.pid")
server.succeed("/tmp/wakeproxy-e2e/bring-upstream.sh >/tmp/wakeproxy-e2e/upstream.stdout 2>/tmp/wakeproxy-e2e/upstream.stderr &")
server.wait_until_succeeds("test -s /tmp/wakeproxy-e2e/wol-listener.pid && kill -0 $(cat /tmp/wakeproxy-e2e/wol-listener.pid)")

client.wait_until_succeeds("ping -c1 192.168.1.2 >/dev/null")
client.wait_until_succeeds("curl -fsS http://127.0.0.1:8091/_health >/dev/null")
client.succeed("curl -sS -o /tmp/anon-body -D /tmp/anon-headers http://127.0.0.1:8091/demo")
client.succeed("grep -F '303 See Other' /tmp/anon-headers")
client.succeed("grep -F '/_auth/login?next=%2Fdemo' /tmp/anon-headers")

client.succeed("curl -sS -o /tmp/login-body -D /tmp/login-headers -c /tmp/wake.cookies -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_auth/login?next=%2Fdemo' -d 'password=test-password&next=%2Fdemo' http://127.0.0.1:8091/_auth/login")
client.succeed("grep -F '303 See Other' /tmp/login-headers")
client.succeed("grep -iF 'set-cookie: wakeproxy_session=' /tmp/login-headers")

client.succeed("curl -sS -b /tmp/wake.cookies -o /tmp/prewake-body -D /tmp/prewake-headers http://127.0.0.1:8091/demo")
client.succeed("grep -F '303 See Other' /tmp/prewake-headers")
client.succeed("grep -F '/_wake?next=%2Fdemo' /tmp/prewake-headers")

client.succeed("curl -sS -b /tmp/wake.cookies -o /tmp/wake-page.html http://127.0.0.1:8091/_wake?next=%2Fdemo")
client.succeed("grep -F 'Defaults to 15 minutes' /tmp/wake-page.html")
client.succeed("grep -F '<option value=\"900\" selected>15 minutes</option>' /tmp/wake-page.html")

client.succeed("curl -sS -b /tmp/wake.cookies -o /tmp/wake-start-body -D /tmp/wake-start-headers -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_wake?next=%2Fdemo' -d 'next=%2Fdemo&keep_awake_for_secs=5' http://127.0.0.1:8091/_wake/start")
client.succeed("grep -F '303 See Other' /tmp/wake-start-headers")
client.succeed("grep -F '/_wake?next=%2Fdemo' /tmp/wake-start-headers")

server.wait_until_succeeds("test -f /tmp/wakeproxy-e2e/wol.received")
server.wait_until_succeeds("grep -F 'upstream-start-after-wol' /tmp/wakeproxy-e2e/wol.log")
server.wait_for_open_port(3000)
client.wait_until_succeeds("curl -fsS http://server:3000/health | grep -F 'ok'")

client.wait_until_succeeds("curl -fsS -b /tmp/wake.cookies http://127.0.0.1:8091/_wake/poll?next=%2Fdemo > /tmp/wake-poll.json")
client.wait_until_succeeds("jq -e '.ready == true and .redirect_to == \"/demo\"' /tmp/wake-poll.json >/dev/null")
client.wait_until_succeeds("curl -fsS -b /tmp/wake.cookies http://127.0.0.1:8091/demo | grep -F 'demo-ok'")

client.succeed("journalctl -u wake-proxy --no-pager > /tmp/wake-proxy-journal.txt")
client.succeed("grep -F 'successful login' /tmp/wake-proxy-journal.txt")
client.succeed("grep -F 'upstream ready' /tmp/wake-proxy-journal.txt")

server.succeed("grep -F 'WOL from ' /tmp/wakeproxy-e2e/wol.log")
server.succeed("grep -F 'GET /health' /tmp/wakeproxy-e2e/upstream.log")
server.succeed("grep -F 'GET /demo' /tmp/wakeproxy-e2e/upstream.log")
  '';
}
