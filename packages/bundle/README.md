---
description: "Ready-made Bake profile bundles for the shared core, the Bake Desktop app, and one-shot headless tasks; the terminal app's bundle lives in `apps/tui`."
kind: "package-group"
---

# bundle/ — profile plugin bundles

## Summary

This group contains the `base`, `desktop`, and `headless` profile bundles. The launcher stacks their patches to assemble the shipped terminal, Bake Desktop, and one-task profiles.

## Table of Contents

- [Packages](#packages)
- [Related documentation](#related-documentation)
- [Dev Note](#dev-note)

<a id="packages"></a>
## Packages

| Package | Role | ctx key |
|---|---|---|
| [`base`](base/README.md) | Shared core for base-backed profiles | — (patch only) |
| [`desktop`](desktop/README.md) | Long-lived bridge the Bake Desktop app drives over base | `desktop-bridge` |
| [`headless`](headless/README.md) | One-shot command-line task application over base | `headless-runner` |

In-box bundles resolve from the Bake installation; out-of-tree bundles install into a profile through `bake plugin --profile <name> add <package>`.

<a id="related-documentation"></a>
## Related documentation

- [Bake app](../../apps/cli/README.md) — the `bake` command that starts a profile.
- [app-boot](../boot/app-boot/README.md) — how profiles are resolved, layered, and customized.
- [Profile plugin bundles note](../../.agents/notes/implemented/architecture/2026-08-05-profile-plugin-bundles.md) — the profile and bundle composition design.
- [Generated composition graph](../../apps/cli/composition.md) — the exact composition each shipped profile uses.

<a id="dev-note"></a>
## Dev Note

None.
