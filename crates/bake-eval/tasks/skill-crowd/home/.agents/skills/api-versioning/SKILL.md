---
name: api-versioning
description: "Decide whether a change to a public HTTP API needs a new version, and ship it without breaking existing clients - additive versus breaking changes, the dated version header, deprecation notices, sunset dates, and the changelog entry clients read. Use when changing a request or response shape, removing or renaming a field, changing an error code, adding a required parameter, or when asked whether something is a breaking change for API consumers. Not for internal function signatures or database changes, which have their own processes."
---

# API versioning

- Adding an optional field or a new endpoint is additive: no new version.
- Removing, renaming, or retyping a field is breaking: add a dated version and keep the old shape.
- Announce deprecations in `docs/api-changelog.md` with a sunset date at least 180 days out.
