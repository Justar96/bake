---
description: "Package map for the application's Remote layer: typed Client-to-Host capability calls, results, and forwarded events, for users and maintainers navigating the group."
kind: "package-group"
---

# api/ — Remote API layers

English | [中文](README.zh.md)

## Summary

The `api/` group contains the Typert Gateway and settings controller. The Gateway carries typed calls and events between a Client and Host; the settings controller owns configuration reads and writes. Each package README describes its current contract.

## Table of Contents

- [Packages](#packages)
- [Related documentation](#related-documentation)
- [Dev Note](#dev-note)

-----

<a id="packages"></a>
## Packages

The packages below provide the Remote layer; the package READMEs own the exhaustive contracts.

| Package | Role | ctx key |
|---|---|---|
| [`gateway/`](gateway/README.md) | Carries typed unary calls, multiplexed streams, and forwarded Host events. | `ctx.typertGateway` / `ctx.remote` |
| [`settings-controller/`](settings-controller/README.md) | Owns the configuration-surface reads and writes over the settings-domain seams. | `ctx.settingsController`, `ctx.credentialsController` / `ctx.remote.settings`, `ctx.remote.credentials` |

Remote calls run Client → Host over the application's shared Connection. API Gateway owns Remote transport, while the controller packages own Session, configuration-surface, and Workspace behavior. Feature packages register exact Connection Fetch routes for responses that do not fit Remote invocation, such as streamed downloads.

-----

<a id="related-documentation"></a>
## Related documentation

Start with the API Gateway reference to see the Remote model end to end, then the Typert subsystem page for the shared definitions and Connection for the physical carrier.

- [API Gateway reference](../../docs/api-gateway.md) — the current-state reference for the Typert API Gateway: programming model, generation pipeline, and runtime invocation.
- [Typert subsystem reference](../../docs/subsystems/typert.md) — the public contracts shared by protocol, Gateway, and consumer assemblies.
- [Connection](../client/connection/README.md) — the RPC carrier, `/api` trust fence, and response envelopes behind every Remote call.

<a id="dev-note"></a>
## Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
