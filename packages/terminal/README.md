---
description: "Package map for the persistent terminal capability family: the owner-scoped ctx.terminals service, the shell backend that starts interactive bash or pwsh, and the six model-facing tools."
kind: "package-group"
---

# terminal/ — persistent PTY capability family

English | [中文](README.zh.md)

## Summary

The `terminal/` family keeps interactive shell sessions alive across calls, including their working directory, environment, and running child processes. `terminal/` manages owner-isolated sessions and `terminal-bash/` supplies a sandboxed bash or pwsh backend. Sessions remain local to one process.

## Table of Contents

- [Packages](#packages)
- [Related documentation](#related-documentation)
- [Dev Note](#dev-note)

-----

<a id="packages"></a>
## Packages

The family is one session service, one shell backend, and one set of model-facing tools. Each child README owns the full contract; the subsystem reference owns the shared vocabulary and the generated service surface.

| Package | Role | ctx key |
|---|---|---|
| [`terminal/`](terminal/README.md) | Session service: owner-scoped sessions with opaque ids, exact-owner fencing, and awaited cleanup | `ctx.terminals` |
| [`terminal-bash/`](terminal-bash/README.md) | Shell backend: interactive bash or pwsh under the shared sandbox policy, with readiness detection and bounded output | registers a backend on `ctx.terminals` |

-----

<a id="related-documentation"></a>
## Related documentation

Start with the subsystem reference for the shared types and the service surface, then the Agent Note for the design rationale and deferred boundaries.

- [Terminal subsystem reference](../../docs/subsystems/terminal.md) — ids, backend and session contracts, send readiness, bounded reads, and the generated `ctx.terminals` API.
- [Persistent PTY Agent Note](../../.agents/notes/implemented/feature/2026-07-16-persistent-pty-sessions.md) — the design decision, alternatives, and deferred work.
- [Capability seams](../../docs/capability-seams.md) — the Service Definition / Service Provider / Consumer split this family follows.

<a id="dev-note"></a>
## Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
