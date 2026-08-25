# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project follows Semantic Versioning.

## [Unreleased]

### Added

- GitHub Actions Nix caching via [Magic Nix Cache](https://github.com/DeterminateSystems/magic-nix-cache-action): `cache-warmup` job primes shared packages, then parallel jobs restore from the Actions cache
- `checks.<system>.rust` flake check: fmt, clippy, tests, and 90% line coverage (same gates as git hooks)
- Overhauled NixOS VM tests under `nix/tests/` with `vm-unit-*` and `vm-integration-*` naming
- `vm-integration-proxy-wake-ssh`: combined multi-node wake flow and remote SSH keep-awake
- `vm-integration-firewall-sources`: `allowedFirewallSources` firewall rules
- `vm-integration-ssh-known-hosts`: strict SSH host key checking for remote keep-awake
- `vm-unit-module-smoke`: fast NixOS module and systemd wiring smoke test
- Shared VM test fixtures in `nix/tests/lib/`

### Changed

- Drop the `test` CI job and remove `scripts/` entirely;
- Remove `e2e-local` pre-commit hook (deprecated in favour of NixOS VM tests in `nix/tests/`)
- Replace devenv dev shell with `pkgs.mkShell` + git-hooks.nix; `nix develop` no longer requires `--impure`
- Share fenix Rust toolchain config between dev shell and `checks.rust` via `nix/rust-env.nix`
- Run GitHub Actions as parallel `rust`, `vm-unit`, and `vm-integration` jobs (replacing a single `nix flake check` step)
- Rename `deployment-e2e` to `vm-integration-proxy-wake-flow`; merge `keep-awake-ssh` into `vm-integration-proxy-wake-ssh`
- Quiet `.envrc` loading after git hooks are installed

### Fixed

- Wake status and poll responses no longer report a synthetic cycle id `0` when the upstream was found ready by direct probing; `cycle_id` is now absent in that case, and the ready cache still suppresses redundant WoL packets
- Wake rate limiter prunes expired entries on every request so tracked IPs cannot linger after a burst (mirrors the login-attempt tracker)
- Removed a duplicate login-attempt prune pass on every lockout check
- Per-request readiness-gate log demoted from `info` to `debug` to avoid duplicating trace-layer request logs
- Cookie parsing and gateway-cookie stripping no longer accept a `__Host-`-prefixed alias of the session cookie that the gateway never issues
- README: documented the enforced 16-character session-secret minimum alongside the 32-byte recommendation
- Split wake coordinator unit tests into `src/wake/tests.rs`, bringing `src/wake.rs` under 650 lines
- VM test networking: use NixOS test auto-assigned `192.168.1.<nodeNumber>` IPs via `network.nodeIp` instead of hardcoded addresses that the test harness overwrites
- `vm-integration-proxy-wake-ssh`: wait for proxy→target connectivity before WoL; derive `wolBroadcastIp` from `nodeIp`
- WoL test listener ignores non-magic UDP packets instead of exiting on the first datagram

### Removed

- devenv flake input and `.devenv` state directory
- `deployment-e2e`, `keep-awake-ssh`, `keep-awake-debug` VM tests (replaced by the overhauled suite)

## [0.1.0] - 2026-03-10

Initial open source release.

### Added

- Password-gated reverse proxy for a single upstream app
- Explicit Wake-on-LAN flow with readiness polling before traffic release
- In-memory session handling and login rate limiting
- Proxy support for normal HTTP traffic and websocket upgrades
- Login page, wake status page, and lightweight PWA assets
- CI workflow, release workflow, MIT license, and systemd deployment example

### Security

- Same-origin protection for login, logout, and wake-start POST routes
- Trusted proxy allowlist for forwarded header handling
- Redirect sanitization and gateway-session cookie stripping before upstream proxying

### Notes

- Runtime state is intentionally memory-backed; restarting the service clears sessions and lockout history
- Project scope is intentionally narrow: one upstream, one shared password, one wake-aware gateway
