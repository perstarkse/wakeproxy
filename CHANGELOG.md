# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project follows Semantic Versioning.

## [Unreleased]

### Changed

- Drop the `test` CI job and remove `scripts/` entirely;
- Remove `e2e-local` pre-commit hook (deprecated in favour of the NixOS VM e2e test in `nix/tests/deployment-e2e.nix`)
- Fold the devenv dev shell into `flake.nix`; use `nix develop --impure` instead of the standalone devenv CLI entry point
- Align the dev shell on the flake's `nixos-unstable` nixpkgs pin (replacing `devenv-nixpkgs/rolling`).
- Run GitHub Actions via `nix flake check --impure` (dropped the separate `devenv test` job)
- Drop devenv Cachix `nixConfig` substituters so `direnv`/`nix develop` do not prompt for untrusted flake settings
- Quiet `.envrc` loading and suppress direnv/devenv  enter-shell noise after hooks are installed

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
