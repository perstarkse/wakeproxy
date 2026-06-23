# VM tests

NixOS VM tests prove cross-host behavior and NixOS module integration that Rust tests cannot cover.

## Tiers

| Tier | Directory | CI | Purpose |
|---|---|---|---|
| Unit | `unit/` | yes | Single-node module wiring, helper binary, local keep-awake |
| Integration | `integration/` | yes | Multi-node WoL, readiness, SSH keep-awake, firewall |
| Optional | `optional/` | no | Slow infra-specific scenarios (auto-suspend) |

## Check names

All checks are exposed as `checks.<system>.vm-<tier>-<subject>`.

```bash
nix flake check

# Single test
nix build .#checks.x86_64-linux.vm-integration-proxy-wake-ssh
```

## When to add a VM test

Add one when behavior requires:

- UDP WoL across hosts
- Hostname-based readiness (`ping` + HTTP health)
- SSH keep-awake across machines
- NixOS firewall source restrictions
- systemd / polkit / logind integration

Do **not** add VM tests for HTTP edge cases, auth logic, CSRF, lockout, or websocket framing — those belong in `cargo test`.

## Shared fixtures

`lib/` holds secrets, SSH keys, network topology helpers (`nodeIp`), proxy config fragments, and target-side upstream/WoL simulation scripts.

NixOS VM tests auto-assign `eth1` as `192.168.1.<n>` where `n` is the node's index in alphabetical order of `nodes` keys. Use `network.nodeIp "proxy" nodeNames` when a test needs literal IPs (e.g. firewall rules).
