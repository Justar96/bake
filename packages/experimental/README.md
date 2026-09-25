---
description: "The experimental group map: publicly installable pre-stable prototypes."
kind: "package-group"
---

# packages/experimental

English | [中文](README.zh.md)

## Summary

The experimental group currently contains the CPython backend for the PTC runtime. Its contract can change without a stability promise; released products outside this group do not depend on it.

## Table of Contents

- [Packages](#packages)
- [Related documentation](#related-documentation)
- [Dev Note](#dev-note)

-----

<a id="packages"></a>
## Packages

| Package | Role | ctx key |
|---|---|---|
| [`ptc-runtime-python`](ptc-runtime-python/README.md) | CPython subprocess backend for the PTC execution seam | `ctx.ptcRuntime` |

-----

<a id="related-documentation"></a>
## Related documentation

- [Experimental publication decision](../../.agents/notes/implemented/process/2026-09-12-experimental-publication-denylist.md) — public defaults and private exceptions.
- [Experimental subtree rules](AGENTS.md) — what experimental status does and does not relax.

-----

<a id="dev-note"></a>
## Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
