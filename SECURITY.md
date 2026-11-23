# Security Policy

## Reporting a vulnerability

Please report security problems privately through GitHub:
**Security → Report a vulnerability** on
<https://github.com/jgarzik/mudcroft/security/advisories/new>.
Do not open a public issue for a suspected vulnerability.

Include what you found, how to reproduce it, and what an attacker could do
with it. Areas of particular interest: escaping the Lua sandbox or its
instruction/memory/call limits, running code as another account (object code
runs as its owner), crossing universe boundaries, permission or credit bypass,
authentication and session tokens, and the cluster transport.

## Supported versions

Mudcroft is pre-1.0. Fixes go to `main` and the `ghcr.io/jgarzik/mudcroft:edge`
image; there are no maintained release branches.

## Verifying container images

Images are built by GitHub Actions from this repository and carry a signed
build provenance attestation:

```bash
gh attestation verify oci://ghcr.io/jgarzik/mudcroft:edge --repo jgarzik/mudcroft
```
