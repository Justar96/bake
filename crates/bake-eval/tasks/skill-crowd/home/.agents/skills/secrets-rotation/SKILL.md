---
name: secrets-rotation
description: "Rotate a secret - an API key, database password, signing key, or OAuth client secret - without downtime, by issuing the new secret, deploying it alongside the old one, switching over, and only then revoking the old one. Use when a secret leaked, an employee with access left, a key is due for scheduled rotation, or when asked to rotate, revoke, or replace credentials. Covers the secret manager paths, which services read each secret, and how to confirm nothing still uses the old one."
---

# Rotating a secret

1. Create the new version in the secret manager; keep the old one active.
2. Deploy every consumer so it accepts both.
3. Switch issuers to the new secret; watch auth errors for an hour.
4. Revoke the old version and record the rotation in `security/rotations.md`.
