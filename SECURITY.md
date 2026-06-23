# Security Policy

## Reporting a vulnerability

Please do not open a public issue for security-sensitive bugs.

Instead, report the problem privately via [GitHub Security Advisories](https://github.com/perstarkse/wakeproxy/security/advisories/new) with:

- a short description of the issue
- the affected version or commit
- reproduction steps or a proof of concept
- any suggested mitigation if you have one

I will acknowledge receipt and work on a fix as quickly as practical.

## Scope

The most security-sensitive areas in this project are:

- authentication and session handling
- forwarded-header trust behavior
- CSRF protections on state-changing routes
- redirect handling
- proxying behavior that could leak gateway-only data upstream
