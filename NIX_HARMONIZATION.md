# Nix Harmonization Steps

`wol-web-proxy` is strongest on NixOS module testing. The release path now builds
through Nix end to end.

## Completed

1. Moved the release workflow to Nix.
   - `.github/workflows/release.yml` runs `nix flake check` and
     `nix build .#release-archive` instead of bare `cargo test` and
     `cargo build --release`.
   - The release archive is packaged from the Nix result, not `target/release`.
   - No non-Nix Rust toolchain is set up in CI.

2. Added a release archive derivation.
   - `nix/packages/release-archive.nix` stages `wakeproxy`, `README.md`,
     `LICENSE`, and the example systemd unit into the same tarball layout used
     before, with a `.sha256` sidecar.
   - Exposed as `.#release-archive`.

3. VM tests remain the deployment contract.
   - `nix/tests/default.nix` still feeds `nix flake check`, with the CI tier
     focused on unit + integration and the optional auto-suspend test kept
     outside the default tier.

4. Made formatting a single Nix entrypoint.
   - `treefmt-nix` powers `nix fmt`, the pre-commit hook, and the `format`
     check in `nix flake check` (rustfmt for `.rs`, nixfmt for `.nix`).

5. Migrated the flake to `flake-parts`.
   - `flake.nix` is the `mkFlake` entrypoint; outputs live in
     `nix/modules/{context,packages,checks,formatter,dev-shell}.nix`.
   - `context.nix` owns the shared, overlay-applied `pkgs` (via
     `_module.args.pkgs`) so every per-system module sees one consistent
     package set.

## Still open

1. Make the Rust toolchain source explicit everywhere.
   - The dev shell and `checks.rust` use `fenix`; the `packages.default` build
     follows nixpkgs `rustPlatform`. Either reuse the fenix toolchain in the
     package derivation or document why package builds intentionally follow
     nixpkgs Rust.
