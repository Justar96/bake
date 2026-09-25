---
description: "The subagent package group: the delegation seam, its in-process and out-of-process backends, and the model-facing delegation tools."
kind: "package-group"
---

# subagent/ — subagent capability family

English | [中文](README.zh.md)

## Summary

The subagent family lets an agent delegate work to an in-process child, continue that child, and discover its status. A child can start fresh or inherit completed parent history. Model-facing tools support delegation, follow-up messages, interruption, and listing.

## Table of Contents

- [Packages](#packages)
- [Related documentation](#related-documentation)
- [Dev Note](#dev-note)

-----

<a id="packages"></a>
## Packages

| Package | Role | ctx key |
|---|---|---|
| [`subagent/`](subagent/README.md) | Defines the delegation service: provider registry, one-shot runs, continuable children, and discovery | `ctx.subagents` |
| [`subagent-in-process-driver/`](subagent-in-process-driver/README.md) | Provides the shared in-process run driver | — |
| [`subagent-spawn-in-process/`](subagent-spawn-in-process/README.md) | Runs a fresh in-process child | registers on `ctx.subagents` |
| [`subagent-fork-in-process/`](subagent-fork-in-process/README.md) | Runs an in-process child seeded from the parent's completed history | registers on `ctx.subagents` |
| [`tool-subagent/`](tool-subagent/README.md) | Exposes delegation to the model | registers on `ctx.tools` |
| [`tool-subagent-control/`](tool-subagent-control/README.md) | Exposes adjacent-agent messaging, interrupt, and listing to the model | registers on `ctx.tools` |

-----

<a id="related-documentation"></a>
## Related documentation

- [Subagent subsystem](../../docs/subsystems/subagent.md) — the service contract, provider contract, and terminal result semantics.
- [Subagent capability seam](../../.agents/notes/implemented/feature/2026-06-21-subagent-capability-seam.md) — the design record for the delegation capability family.
- [Continuable subagents](../../.agents/notes/implemented/feature/2026-07-28-continuable-subagent-conversations.md) — durable children that accept follow-up turns.
- [tool-subagent-control README](tool-subagent-control/README.md) — the follow-up, interrupt, and listing surface.

<a id="dev-note"></a>
## Dev Note

None.
