# Target-side upstream and WoL simulation for VM integration tests.
{
  pkgs,
  stateDir ? "/tmp/wakeproxy-e2e",
  wolPort ? 4009,
  upstreamPort ? 3000,
}: let
  upstreamPy = pkgs.writeText "wakeproxy-test-upstream.py" ''
    import http.server
    import pathlib
    import socketserver

    state_dir = pathlib.Path('${stateDir}')
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
                log.write(f'GET {self.path}\n')
            if self.path == '/health':
                self._write(200, 'ok')
            elif self.path == '/demo':
                self._write(200, 'demo-ok')
            else:
                self._write(404, 'not-found')

    with socketserver.TCPServer(('0.0.0.0', ${toString upstreamPort}), Handler) as httpd:
        httpd.serve_forever()
  '';

  wolListenerPy = pkgs.writeText "wakeproxy-test-wol-listener.py" ''
    import pathlib
    import socket

    state_dir = pathlib.Path('${stateDir}')
    log_path = state_dir / 'wol.log'
    magic_prefix = b'\xff' * 6
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(('0.0.0.0', ${toString wolPort}))
    while True:
        packet, addr = sock.recvfrom(4096)
        with log_path.open('a', encoding='utf-8') as log:
            log.write(f'WOL from {addr[0]}:{addr[1]} bytes={len(packet)}\n')
            log.write(packet.hex() + '\n')
        if not packet.startswith(magic_prefix):
            continue
        (state_dir / 'wol.received').write_text('1', encoding='utf-8')
        break
  '';

  bringUpstreamSh = pkgs.writeShellScript "wakeproxy-bring-upstream" ''
    set -eu
    while [ ! -f ${stateDir}/wol.received ]; do
      sleep 0.2
    done
    echo upstream-start-after-wol >> ${stateDir}/wol.log
    exec python3 ${upstreamPy}
  '';
in {
  inherit stateDir upstreamPy wolListenerPy bringUpstreamSh;

  initStateDir = "mkdir -p ${stateDir} && : > ${stateDir}/wol.log && : > ${stateDir}/upstream.log";

  startDelayedUpstream = "python3 ${wolListenerPy} >${stateDir}/wol-listener.stdout 2>${stateDir}/wol-listener.stderr & echo $! > ${stateDir}/wol-listener.pid; ${bringUpstreamSh} >${stateDir}/upstream.stdout 2>${stateDir}/upstream.stderr &";

  waitForWolListener = "test -s ${stateDir}/wol-listener.pid && kill -0 $(cat ${stateDir}/wol-listener.pid)";

  startAlwaysUpUpstream = "python3 ${upstreamPy} >${stateDir}/upstream.stdout 2>${stateDir}/upstream.stderr &";
}
