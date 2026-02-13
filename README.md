# wol-web-proxy

Axum-based gateway that protects an upstream app with password login, wakes it via Wake-on-LAN, waits for readiness (`ping` + HTTP health), and then proxies full requests.

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
- `WOL_PROXY_EDGE_WAIT_TIMEOUT` (default: `20`)
- `WOL_PROXY_TRUST_PROXY_HEADERS` (default: `false`)
- `WOL_PROXY_TRUSTED_PROXY_IPS` (default: empty, comma-separated IPs)

## Local run

```bash
cargo run
```

## Tests

```bash
cargo test
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
