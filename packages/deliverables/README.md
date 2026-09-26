---
description: "Package map for the explicit file delivery tool."
kind: "package-group"
---

# packages/deliverables

English | [中文](README.zh.md)

## Summary

The `deliverables/` group contains `tool-present`, which lets an agent identify files for delivery in a durable Session event. Its README owns the tool configuration and event contract.

## Table of Contents

- [Packages](#packages)
- [Related documentation](#related-documentation)
- [Dev Note](#dev-note)

-----

<a id="packages"></a>
## Packages

| Package | Role | ctx key |
|---|---|---|
| [`tool-present`](tool-present/README.md) | Declares existing files as final deliverables through the `present` tool | registers on `ctx.tools` |

-----

<a id="related-documentation"></a>
## Related documentation

- [Deliverables subsystem](../../docs/subsystems/deliverables.md) — the `PresentedFile` and `WorkspaceChangesSummary` vocabulary, the two durable events, and the summary service.
- [Present declares workspace source files](../../.agents/notes/implemented/feature/2026-09-08-present-workspace-source-files.md) — the delivery decision.
- [Turn changed-files card](../../.agents/notes/implemented/feature/2026-09-11-turn-changed-files-card.md) — the snapshot design and coverage rules.

-----

<a id="dev-note"></a>
## Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
