# wol-web-proxy

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

- `WOL_PROXY_UPSTREAM_HOST`
- `WOL_PROXY_UPSTREAM_PORT`
- `WOL_PROXY_WOL_MAC` (format: `aa:bb:cc:dd:ee:ff`)
- `WOL_PROXY_WOL_BROADCAST_IP`
- `WOL_PROXY_PASSWORD_HASH` (argon2 hash)
- `WOL_PROXY_SESSION_SECRET`

## Optional environment variables

- `WOL_PROXY_HEALTH_PATH` (default: `/health`)
- `WOL_PROXY_WOL_BROADCAST_PORT` (default: `9`)
- `WOL_PROXY_WAKE_TIMEOUT` (default: `180` seconds)
- `WOL_PROXY_POLL_INTERVAL` (default: `2` seconds)
- `WOL_PROXY_BIND_ADDR` (default: `127.0.0.1:8080`)
- `WOL_PROXY_READY_CACHE_TTL` (default: `5` seconds)
- `WOL_PROXY_SESSION_TTL_SECS` (default: `43200`)
- `WOL_PROXY_COOKIE_SECURE` (default: `true`)
- `WOL_PROXY_LOGIN_MAX_FAILURES` (default: `5`)
- `WOL_PROXY_LOGIN_LOCKOUT_SECS` (default: `60`)
- `WOL_PROXY_LOGIN_WINDOW_SECS` (default: `300`)
- `WOL_PROXY_LOGIN_MAX_TRACKED_IPS` (default: `10000`)
- `WOL_PROXY_WAKE_POLL_INTERVAL_MS` (default: `2000`)
- `WOL_PROXY_TRUST_PROXY_HEADERS` (default: `false`)
- `WOL_PROXY_TRUSTED_PROXY_IPS` (default: empty, comma-separated IPs)

## Security assumptions and hardening

- Keep `WOL_PROXY_TRUST_PROXY_HEADERS=false` unless the gateway is behind a known reverse proxy.
- If proxy header trust is enabled, set `WOL_PROXY_TRUSTED_PROXY_IPS` to only your proxy hops and ensure your edge overwrites/sanitizes incoming `X-Forwarded-*` headers from clients.
- CSRF protections for `POST /_auth/login`, `POST /_auth/logout`, and `POST /_wake/start` rely on same-origin `Origin`/`Referer` validation against `Host`; keep `Host` handling consistent through your proxy.
- The gateway strips its own `wol_session` cookie before forwarding upstream requests, so upstream services do not receive gateway auth sessions.
- iOS soft redirects only run for safe targets (relative paths or `http/https` URLs).
- Session state is memory-backed by design; restarting the process clears active sessions and login-attempt tracking.
- Prefer a randomly generated session secret of at least 32 bytes, even though the minimum accepted length is lower.

## Readiness model

- Reachability uses system `ping` when available.
- If `ping` is unavailable or blocked, the gateway falls back to a TCP connect check against the upstream port.
- Health is still confirmed through `GET WOL_PROXY_HEALTH_PATH` and requires a `2xx` response before proxy traffic is released.

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

## Open-source readiness

- See `CONTRIBUTING.md` for local workflow and scope expectations.
- See `SECURITY.md` for reporting guidance.
