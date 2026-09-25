---
description: "Package map for the shared Client-to-Host Connection."
kind: "package-group"
---

# client/ — shared Connection

English | [中文](README.zh.md)

## Summary

The `client/` group currently contains the shared Connection package. It carries typed Client-to-Host requests and event delivery for consumers that use the Client transport.

## Table of Contents

- [Packages](#packages)
- [Related documentation](#related-documentation)
- [Dev Note](#dev-note)

-----

<a id="packages"></a>
## Packages

The package README owns the transport contract and configuration.

| Package | Role | ctx key |
|---|---|---|
| [`connection/`](connection/README.md) | Maintains browser-host RPC communication and event delivery | `ctx.connection` |

-----

<a id="related-documentation"></a>
## Related documentation

See the [Connection package](connection/README.md) for its transport contract and the [Host group map](../host/README.md) for server-side packages.

<a id="dev-note"></a>
## Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
