#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"

UPSTREAM_PORT="${UPSTREAM_PORT:-6994}"
GATEWAY_PORT="${GATEWAY_PORT:-18080}"
PASSWORD="${PASSWORD:-local-e2e-password}"

UPSTREAM_PID=""
GATEWAY_PID=""

cleanup() {
  set +e
  if [ -n "$GATEWAY_PID" ]; then
    kill "$GATEWAY_PID" >/dev/null 2>&1 || true
  fi
  if [ -n "$UPSTREAM_PID" ]; then
    kill "$UPSTREAM_PID" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

assert_eq() {
  local expected="$1"
  local actual="$2"
  local context="$3"

  if [ "$expected" != "$actual" ]; then
    echo "FAIL: $context (expected=$expected actual=$actual)"
    exit 1
  fi
}

echo "Building local binaries..."
cargo build --quiet --bin wol-web-proxy --example mock-upstream --example password-hash --manifest-path "$ROOT_DIR/Cargo.toml"

echo "Preparing test configuration..."
PASSWORD_HASH="$(cargo run --quiet --example password-hash --manifest-path "$ROOT_DIR/Cargo.toml" -- "$PASSWORD")"
SESSION_SECRET="$(head -c 32 /dev/urandom | base64 | tr -d '\n')"

echo "Starting mock upstream on port $UPSTREAM_PORT..."
"$ROOT_DIR/target/debug/examples/mock-upstream" "$UPSTREAM_PORT" >"$TMP_DIR/upstream.log" 2>&1 &
UPSTREAM_PID="$!"

export WOL_PROXY_UPSTREAM_HOST="127.0.0.1"
export WOL_PROXY_UPSTREAM_PORT="$UPSTREAM_PORT"
export WOL_PROXY_HEALTH_PATH="/health"
export WOL_PROXY_WOL_MAC="aa:bb:cc:dd:ee:ff"
export WOL_PROXY_WOL_BROADCAST_IP="127.255.255.255"
export WOL_PROXY_WOL_BROADCAST_PORT="9"
export WOL_PROXY_WAKE_TIMEOUT="10"
export WOL_PROXY_POLL_INTERVAL="1"
export WOL_PROXY_BIND_ADDR="127.0.0.1:$GATEWAY_PORT"
export WOL_PROXY_PASSWORD_HASH="$PASSWORD_HASH"
export WOL_PROXY_SESSION_SECRET="$SESSION_SECRET"
export WOL_PROXY_COOKIE_SECURE="false"
export WOL_PROXY_LOGIN_MAX_FAILURES="1000"

echo "Starting gateway on port $GATEWAY_PORT..."
"$ROOT_DIR/target/debug/wol-web-proxy" >"$TMP_DIR/gateway.log" 2>&1 &
GATEWAY_PID="$!"

echo "Waiting for gateway readiness..."
for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:$GATEWAY_PORT/_health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done

if ! curl -fsS "http://127.0.0.1:$GATEWAY_PORT/_health" >/dev/null 2>&1; then
  echo "FAIL: gateway did not become ready"
  echo "--- gateway log ---"
  cat "$TMP_DIR/gateway.log"
  exit 1
fi

COOKIE_JAR="$TMP_DIR/cookies.txt"

echo "Test 1/4: unauthenticated request redirects to login"
status="$(curl -sS -o "$TMP_DIR/unauth-body.txt" -w "%{http_code}" "http://127.0.0.1:$GATEWAY_PORT/demo?x=1")"
assert_eq "303" "$status" "unauthenticated redirect"

echo "Test 2/4: login succeeds and sets cookie"
status="$(curl -sS -o "$TMP_DIR/login-body.txt" -D "$TMP_DIR/login-headers.txt" -c "$COOKIE_JAR" -w "%{http_code}" -X POST -d "password=$PASSWORD&next=%2Fdemo" "http://127.0.0.1:$GATEWAY_PORT/_auth/login")"
assert_eq "303" "$status" "login redirect"

echo "Test 3/4: authenticated proxy request reaches upstream"
status="$(curl -sS -o "$TMP_DIR/proxy-body.txt" -w "%{http_code}" -X POST -b "$COOKIE_JAR" -d "hello-e2e" "http://127.0.0.1:$GATEWAY_PORT/demo?from=e2e")"
assert_eq "200" "$status" "proxied request status"

proxy_body="$(cat "$TMP_DIR/proxy-body.txt")"
[[ "$proxy_body" == *"method=POST"* ]] || { echo "FAIL: upstream method missing"; exit 1; }
[[ "$proxy_body" == *"path=/demo"* ]] || { echo "FAIL: upstream path missing"; exit 1; }
[[ "$proxy_body" == *"query=from=e2e"* ]] || { echo "FAIL: upstream query missing"; exit 1; }
[[ "$proxy_body" == *"body=hello-e2e"* ]] || { echo "FAIL: upstream body missing"; exit 1; }
[[ "$proxy_body" == *"xfp=http"* ]] || { echo "FAIL: x-forwarded-proto not set"; exit 1; }

echo "Test 4/4: logout invalidates session"
status="$(curl -sS -o "$TMP_DIR/logout-body.txt" -w "%{http_code}" -X POST -b "$COOKIE_JAR" "http://127.0.0.1:$GATEWAY_PORT/_auth/logout")"
assert_eq "303" "$status" "logout redirect"

status="$(curl -sS -o "$TMP_DIR/post-logout-body.txt" -w "%{http_code}" -b "$COOKIE_JAR" "http://127.0.0.1:$GATEWAY_PORT/demo")"
assert_eq "303" "$status" "post-logout redirect"

echo "E2E local test passed."
