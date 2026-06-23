# Contributing

Thanks for considering a contribution.

## Project constraints

This project aims to stay small and operationally simple.

- Prefer tightening the existing core over adding new surface area
- Avoid introducing databases, external control planes, or account systems
- Keep the deployment story straightforward for a single self-hosted upstream

## Local workflow

Run the CI gate before opening a change:

```bash
nix flake check
```

This runs Rust checks and all CI NixOS VM tests (unit + integration). See `nix/tests/README.md` for the matrix and how to run individual checks.

## Change style

- Keep changes focused and easy to review
- Add tests for behavior changes where practical
- Preserve the current security posture unless a change explicitly hardens it
- Update `README.md` when configuration, runtime behavior, or deployment expectations change
