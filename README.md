# wakeproxy

Axum-based gateway that protects an upstream app with password login. If the upstream is asleep, authenticated users land on a wake status page and can explicitly start Wake-on-LAN; the gateway then polls readiness (`ping` + HTTP health) and redirects to the requested path when ready.

## Project shape

- Single shared password for a small trusted group
- Single upstream app behind one gateway process
- In-memory sessions and login-attempt tracking only
- Explicit wake flow instead of background auto-wake on every unauthenticated hit

This is intentionally a small-scope project. It is not trying to be a full identity system, a generic reverse proxy platform, or a multi-service control plane.

## Non-goals

- No database or external state store
- No OAuth, SSO, or multi-user administration
- No multi-upstream routing or per-app policy engine
- No heavy frontend shell beyond the login and wake flow

## Quickstart

Generate a password hash using the built-in helper:

```bash
cargo run --quiet --example password-hash -- "replace-me"
```

Generate a strong session secret:

```bash
openssl rand -base64 32 | tr -d '\n'
```

Copy `.env.example` to `.env`, fill in the real values, then load it into your shell and run the gateway:

```bash
set -a
. ./.env
set +a
cargo run
```

## Required environment variables

- `WAKEPROXY_UPSTREAM_HOST`
- `WAKEPROXY_UPSTREAM_PORT`
- `WAKEPROXY_WOL_MAC` (format: `aa:bb:cc:dd:ee:ff`)
- `WAKEPROXY_WOL_BROADCAST_IP`
- `WAKEPROXY_PASSWORD_HASH` (argon2 hash)
- `WAKEPROXY_SESSION_SECRET`

## Optional environment variables

- `WAKEPROXY_HEALTH_PATH` (default: `/health`)
- `WAKEPROXY_WOL_BROADCAST_PORT` (default: `9`)
- `WAKEPROXY_WAKE_TIMEOUT` (default: `180` seconds)
- `WAKEPROXY_POLL_INTERVAL` (default: `2` seconds)
- `WAKEPROXY_BIND_ADDR` (default: `127.0.0.1:8080`)
- `WAKEPROXY_READY_CACHE_TTL` (default: `5` seconds)
- `WAKEPROXY_SESSION_TTL_SECS` (default: `43200`)
- `WAKEPROXY_COOKIE_SECURE` (default: `true`)
- `WAKEPROXY_LOGIN_MAX_FAILURES` (default: `5`)
- `WAKEPROXY_LOGIN_LOCKOUT_SECS` (default: `60`)
- `WAKEPROXY_LOGIN_WINDOW_SECS` (default: `300`)
- `WAKEPROXY_LOGIN_MAX_TRACKED_IPS` (default: `10000`)
- `WAKEPROXY_WAKE_POLL_INTERVAL_MS` (default: `2000`)
- `WAKEPROXY_KEEP_AWAKE_COMMAND` (optional helper command receiving requested seconds as argv[1])
- `WAKEPROXY_KEEP_AWAKE_MAX_SECS` (default: `14400`)
- `WAKEPROXY_TRUST_PROXY_HEADERS` (default: `false`)
- `WAKEPROXY_TRUSTED_PROXY_IPS` (default: empty, comma-separated IPs)
- `WAKEPROXY_EXTERNAL_ORIGIN` (recommended behind reverse proxies; exact external `http://` or `https://` origin, no path)

## Security assumptions and hardening

- Keep `WAKEPROXY_TRUST_PROXY_HEADERS=false` unless the gateway is behind a known reverse proxy.
- If proxy header trust is enabled, set `WAKEPROXY_TRUSTED_PROXY_IPS` to only your proxy hops and ensure your edge overwrites/sanitizes incoming `X-Forwarded-*` headers from clients.
- Set `WAKEPROXY_EXTERNAL_ORIGIN` behind reverse proxies so CSRF and websocket origin checks do not depend on incoming `Host`.
- The gateway strips its own `wakeproxy_session` cookie before forwarding upstream requests, so upstream services do not receive gateway auth sessions.
- iOS soft redirects only run for safe targets (relative paths or `http/https` URLs).
- Session state is memory-backed by design; restarting the process clears active sessions and login-attempt tracking.
- Use a randomly generated session secret of at least 32 bytes.
- Websocket upgrades with an `Origin` header are accepted only when the origin matches the configured external origin or request host.

## Readiness model

- Reachability uses system `ping` when available.
- If `ping` is unavailable or blocked, the gateway falls back to a TCP connect check against the upstream port.
- Health is still confirmed through `GET WAKEPROXY_HEALTH_PATH` and requires a `2xx` response before proxy traffic is released.

## Development-only routes

- `/_dev/wake-preview` exists only in debug builds and returns `404` in release builds.

## Local run

```bash
cargo run
```

## Tests

```bash
cargo test
```

## Coverage

Run coverage inside the devenv shell (same command and threshold used by hooks):

```bash
devenv shell -- cargo llvm-cov --workspace --all-features --summary-only --fail-under-lines 90
```

## Pre-commit checks

`devenv test` runs the configured hook suite (`fmt`, `clippy`, `test`, coverage, local e2e smoke):

```bash
devenv test
```

For verbose per-hook output (commands, logs, durations), run:

```bash
devenv shell -- prek run --all-files -v
```

## Local E2E Smoke Test

Run a full local flow with mock upstream on port `6994`, login, proxy, and logout verification:

```bash
./scripts/e2e-local.sh
```

Optional overrides:

```bash
UPSTREAM_PORT=6994 GATEWAY_PORT=18080 PASSWORD=my-test-password ./scripts/e2e-local.sh
```

## systemd deployment

`examples/wakeproxy.service` is the recommended starting point for production installs.

- Copy the binary to `/usr/local/bin/wakeproxy` or install it from your package manager.
- Put runtime configuration in `/etc/wakeproxy/wakeproxy.env`.
- Run it behind a reverse proxy that terminates TLS and sets a stable `Host` header.

### Install with systemd

Create the service account and config directory:

```bash
sudo useradd --system --home /var/lib/wakeproxy --shell /usr/sbin/nologin wakeproxy
sudo mkdir -p /etc/wakeproxy /var/lib/wakeproxy
sudo chown -R wakeproxy:wakeproxy /var/lib/wakeproxy
```

Install the binary and unit file:

```bash
sudo install -m 0755 target/release/wakeproxy /usr/local/bin/wakeproxy
sudo install -m 0644 examples/wakeproxy.service /etc/systemd/system/wakeproxy.service
```

Create `/etc/wakeproxy/wakeproxy.env` with your real values:

```bash
WAKEPROXY_UPSTREAM_HOST=127.0.0.1
WAKEPROXY_UPSTREAM_PORT=3000
WAKEPROXY_HEALTH_PATH=/health
WAKEPROXY_WOL_MAC=aa:bb:cc:dd:ee:ff
WAKEPROXY_WOL_BROADCAST_IP=192.168.1.255
WAKEPROXY_PASSWORD_HASH=replace-with-argon2-hash
WAKEPROXY_SESSION_SECRET=replace-with-random-secret
WAKEPROXY_BIND_ADDR=127.0.0.1:8080
WAKEPROXY_COOKIE_SECURE=true
```

Enable and start the service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now wakeproxy
sudo systemctl status wakeproxy
```

If you run wakeproxy behind TLS termination, keep `WAKEPROXY_COOKIE_SECURE=true` and make sure your reverse proxy forwards the original `Host` value consistently.

## Open-source readiness

- See `CONTRIBUTING.md` for local workflow and scope expectations.
- See `SECURITY.md` for reporting guidance.
- See `CHANGELOG.md` and `docs/releases/v0.1.0.md` for release history and the initial release notes.
