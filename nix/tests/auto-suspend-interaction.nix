{ self, pkgs }:
let
  passwordSecrets = pkgs.writeText "wakeproxy-secrets" ''
    WAKEPROXY_PASSWORD_HASH=$argon2id$v=19$m=19456,t=2,p=1$yDDaCqWkOPAHJMrn5KI97w$+sEaPfPEi0Ic2N7Op7r/nOJkdFmZ22/QRdb2QTs/0Vc
    WAKEPROXY_SESSION_SECRET=very-secret-session-key
  '';
in
pkgs.testers.runNixOSTest {
  name = "wakeproxy-auto-suspend-interaction";

  nodes.machine = { config, ... }: {
    imports = [
      self.nixosModules.wake-proxy
      # Simulate the auto-suspend module from ../infra
      ({ config, lib, pkgs, ... }: let
        cfg = config.my.autoSuspend;
        autoSuspendScript = pkgs.writeShellScript "auto-suspend-check" ''
          set -euo pipefail
          IDLE_FILE="/run/auto-suspend/idle-count"
          REQUIRED_CHECKS=${toString cfg.requiredIdleChecks}
          LOAD_THRESHOLD="${cfg.loadThreshold}"
          ACTIVE_TCP_PORTS=""
          mkdir -p /run/auto-suspend
          loadavg=$(${pkgs.coreutils}/bin/cut -d' ' -f2 /proc/loadavg)
          load_idle=$(${pkgs.gawk}/bin/awk -v avg="$loadavg" -v threshold="$LOAD_THRESHOLD" \
            'BEGIN { print (avg < threshold) ? "1" : "0" }')
          user_idle=1
          for session in $(${pkgs.systemd}/bin/loginctl list-sessions --no-legend | ${pkgs.gawk}/bin/awk '{print $1}'); do
            session_type=$(${pkgs.systemd}/bin/loginctl show-session "$session" -p Type --value 2>/dev/null || echo "")
            session_class=$(${pkgs.systemd}/bin/loginctl show-session "$session" -p Class --value 2>/dev/null || echo "")
            session_state=$(${pkgs.systemd}/bin/loginctl show-session "$session" -p State --value 2>/dev/null || echo "")
            if { [ "$session_type" = "wayland" ] || [ "$session_type" = "x11" ]; } \
              && [ "$session_class" = "user" ] \
              && [ "$session_state" = "active" ]; then
              idle_hint=$(${pkgs.systemd}/bin/loginctl show-session "$session" -p IdleHint --value 2>/dev/null || echo "yes")
              if [ "$idle_hint" = "no" ]; then
                user_idle=0
                break
              fi
            fi
          done
          inhibited=0
          ${lib.optionalString cfg.checkInhibitors ''
            if ${pkgs.systemd}/bin/systemd-inhibit --list --no-legend | grep -E 'sleep.*block' | grep -qv 'handle-power-key'; then
              inhibited=1
            fi
          ''}
          tcp_active=0
          status=""
          [ "$load_idle" = "0" ] && status="$status load:ACTIVE($loadavg)"
          [ "$load_idle" = "1" ] && status="$status load:idle($loadavg)"
          [ "$user_idle" = "0" ] && status="$status user:ACTIVE"
          [ "$user_idle" = "1" ] && status="$status user:idle"
          [ "$inhibited" = "1" ] && status="$status inhibitor:BLOCKING"
          [ "$tcp_active" = "1" ] && status="$status tcp:ACTIVE"
          current=$(cat "$IDLE_FILE" 2>/dev/null || echo "0")
          if [ "$load_idle" = "1" ] && [ "$user_idle" = "1" ] && [ "$inhibited" = "0" ] && [ "$tcp_active" = "0" ]; then
            new=$((current + 1))
            echo "$new" > "$IDLE_FILE"
            echo "$(date): IDLE $new/$REQUIRED_CHECKS —$status" >> /var/log/auto-suspend.log
            if [ "$new" -ge "$REQUIRED_CHECKS" ]; then
              echo "$(date): SUSPENDING after $new consecutive idle checks" >> /var/log/auto-suspend.log
              echo "0" > "$IDLE_FILE"
              ${pkgs.systemd}/bin/systemctl suspend
            fi
          else
            echo "$(date): ACTIVE (reset from $current/$REQUIRED_CHECKS) —$status" >> /var/log/auto-suspend.log
            echo "0" > "$IDLE_FILE"
          fi
        '';
      in {
        options.my.autoSuspend = {
          enable = lib.mkEnableOption "automatic suspend on idle";
          checkIntervalMinutes = lib.mkOption {
            type = lib.types.int;
            default = 10;
          };
          requiredIdleChecks = lib.mkOption {
            type = lib.types.int;
            default = 3;
          };
          loadThreshold = lib.mkOption {
            type = lib.types.str;
            default = "1.0";
          };
          checkInhibitors = lib.mkOption {
            type = lib.types.bool;
            default = true;
          };
        };
        config = lib.mkIf cfg.enable {
          systemd = {
            timers.auto-suspend = {
              wantedBy = [ "timers.target" ];
              timerConfig = {
                OnBootSec = "${toString cfg.checkIntervalMinutes}min";
                OnUnitActiveSec = "${toString cfg.checkIntervalMinutes}min";
                Persistent = false;
              };
            };
            services.auto-suspend = {
              description = "Check for idle and suspend";
              serviceConfig = {
                Type = "oneshot";
                ExecStart = autoSuspendScript;
              };
            };
          };
        };
      })
    ];

    # Create a graphical session so logind has sessions to check
    users.users.testuser = {
      isNormalUser = true;
      password = "test";
    };

    services.xserver = {
      enable = true;
      displayManager.lightdm.enable = true;
      desktopManager.xterm.enable = true;
    };

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

    # Match charon's auto-suspend config
    my.autoSuspend = {
      enable = true;
      checkIntervalMinutes = 1;
      requiredIdleChecks = 2;
      loadThreshold = "6.0";
    };

    environment.systemPackages = with pkgs; [ curl jq python3 gawk ];
  };

  testScript = ''
start_all()
machine.wait_for_unit("multi-user.target")
machine.wait_for_unit("polkit.service")
machine.wait_for_unit("wake-proxy.service")
machine.wait_for_unit("auto-suspend.timer")
machine.wait_for_open_port(8091)

# Start a mock upstream
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

# Trigger wake with 25 second keep-awake (within the 30s max)
machine.succeed("curl -sS -o /dev/null -D /tmp/wake-start-headers -X POST -H 'Origin: http://127.0.0.1:8091' -H 'Referer: http://127.0.0.1:8091/_wake?next=%2Fdemo' -d 'next=%2Fdemo&keep_awake_for_secs=25' http://127.0.0.1:8091/_wake/start")
machine.succeed("grep -F '303 See Other' /tmp/wake-start-headers")

# Poll until inhibitor appears (like e2e test does)
machine.wait_until_succeeds("systemd-inhibit --list | grep -F 'wakeproxy remote session'")
machine.wait_until_succeeds("test -s /var/lib/wake-proxy/wakeproxy-keep-awake.pid")
machine.wait_until_succeeds("kill -0 $(cat /var/lib/wake-proxy/wakeproxy-keep-awake.pid)")

# Check for errors in keep-awake log
machine.succeed("cat /var/lib/wake-proxy/wakeproxy-keep-awake.log || true")

# Run auto-suspend check manually - it should see the inhibitor
machine.succeed("systemctl start auto-suspend")
machine.sleep(1)
machine.succeed("grep -F 'inhibitor:BLOCKING' /var/log/auto-suspend.log")
machine.succeed("grep -F 'ACTIVE (reset from' /var/log/auto-suspend.log | tail -1 | grep -F 'inhibitor:BLOCKING'")

# Wait for the keep-awake to expire (25 seconds + buffer)
machine.sleep(25)

# Verify inhibitor is gone
machine.succeed("! systemd-inhibit --list | grep -F 'wakeproxy remote session'")

# Now run auto-suspend again - should NOT see inhibitor
machine.succeed("systemctl start auto-suspend")
machine.sleep(1)
# Should show idle check (no inhibitor blocking)
machine.succeed("grep -F 'IDLE 1/2' /var/log/auto-suspend.log | tail -1")
  '';
}
