# Contributing

Thanks for considering a contribution.

## Project constraints

This project aims to stay small and operationally simple.

- Prefer tightening the existing core over adding new surface area
- Avoid introducing databases, external control planes, or account systems
- Keep the deployment story straightforward for a single self-hosted upstream

## Local workflow

Run the main checks before opening a change:

```bash
nix develop --impure -c devenv test
```

For the local end-to-end smoke flow:

```bash
./scripts/e2e-local.sh
```

## Change style

- Keep changes focused and easy to review
- Add tests for behavior changes where practical
- Preserve the current security posture unless a change explicitly hardens it
- Update `README.md` when configuration, runtime behavior, or deployment expectations change
