# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project follows Semantic Versioning.

## [Unreleased]

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
