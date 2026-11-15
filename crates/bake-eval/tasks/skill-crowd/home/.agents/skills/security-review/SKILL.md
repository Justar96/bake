---
name: security-review
description: "Review a change or a feature for security problems - authentication and authorization checks, input validation, injection, secrets in code, unsafe deserialization, server-side request forgery, and data exposure in logs and error messages. Use when asked for a security review, a threat model, whether something is safe, or to check a pull request that handles user input, files, payments, or permissions. Produces findings with severity, impact, and a concrete fix."
---

# Security review

1. Map where untrusted input enters and where it is used.
2. Check authorization on every new endpoint, not just authentication.
3. Report findings as `severity | location | impact | fix`.
