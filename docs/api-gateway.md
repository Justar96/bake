# API Gateway

English | [中文](api-gateway.zh.md)

This is the current-state reference for the Typert API Gateway. It describes how business services declare unary Remote methods, how the Host build generates Host and Host-for-Client contracts, and how calls reuse the Connection RPC and `/api` route. Stream Remote methods declared with `@Remote({ mode: 'stream' })` and forwarded Host events are documented in the [API Gateway package README](../packages/api/gateway/README.md), not here.

## Programming model

Business services use `@Remote` or `@RemoteScope` to select the methods exposed to the Client. Unmarked methods do not enter the generated Client types or runtime contributions and cannot be called through `ctx.remote`.

`@Remote` denotes calling a Cordis service registered on the root Host Context. Complex Host objects cannot cross the wire directly; the business package must declare their association with a wire identity through `TypertLookupMap` and register a default resolution provider with `ctx.typert.lookups` at runtime. For example, an `Agent` parameter named `agent` in the Host signature produces an `agentId` wire field, and the Gateway resolves that id to a Host object before invoking the business method. Host composition can use `ctx.typert.lookups.configure()` to override the resolution policy for a lookup key without changing the parameter name, wire field, or canonical type symbol owned by the business package.

`@RemoteScope(key)` first resolves an identity to a scoped Context through `ctx.typert.contexts`, then obtains the service from that Context and invokes the method. It applies when the method itself depends on scoped composition and does not need to receive objects such as `Agent` explicitly.

Services normally extend `TypertRemoteService` so the constructor explicitly binds the Cordis service key and default Remote namespace. A service that already has another base class can instead declare `readonly typertRemote = bindTypertRemote(this, serviceKey)`; both forms leave an inspectable public binding and do not depend on the compiler injecting a symbol into the constructor.

```ts
import type { Agent } from '@deepseek-ai/dsh-agent'
import { TypertRemoteService, Remote, RemoteScope } from '@deepseek-ai/dsh-typert-protocol'
import type { Context } from '@deepseek-ai/cordis'

export interface CreateGoalRequest {
  objective: string
}

export interface CreateGoalResult {
  accepted: boolean
}

export class GoalService extends TypertRemoteService {
  constructor(ctx: Context) {
    super(ctx, 'goals')
  }

  @Remote('create')
  createForClient(
    agent: Agent,
    request: CreateGoalRequest,
    signal: AbortSignal,
  ): CreateGoalResult {
    signal.throwIfAborted()
    return this.create(agent, request)
  }

  @RemoteScope('agent', 'current')
  currentForClient(): CreateGoalResult {
    return { accepted: true }
  }

  private create(_agent: Agent, request: CreateGoalRequest): CreateGoalResult {
    return { accepted: request.objective.length > 0 }
  }
}
```

Remote methods may return a value synchronously or return a Promise. For cooperative cancellation, the final parameter in the Host signature must be `signal: AbortSignal` using the global type; it is recorded in the descriptor instead of entering `args`, while the generated Client method accepts an optional final `AbortSignal`.

The Client uses concrete functions on ordinary objects, not a JavaScript Proxy. Each namespace is a traced Cordis child Service registered as `remote.<namespace>`; `ctx.remote.$mount()` mounts a contribution, and the namespace unloads after its last method is withdrawn. A business package that reads `ctx.remote.<namespace>` declares both `remote` and `remote.<namespace>` in its own `inject`. A call is scoped when a Client Context adapter registered through `ctx.typert.contexts.registerClient()` reports an identity for the calling Context. When an `@Remote` method has exactly one lookup parameter and a same-named `TypertContextMap` uses the same wire identity, the generated scoped signature omits that identity parameter. `@RemoteScope` generates only the scoped invocation interface. Every generated unary method resolves to a `RemoteResult<T>`.

```ts ignore-check
import type { SessionId } from '@deepseek-ai/dsh-session/types'
import type { ClientRemote } from '@deepseek-ai/dsh-api-gateway/client'
import type { TypertRemoteScopeApi } from '@deepseek-ai/dsh-typert-protocol'
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-goal/remote'

export const inject = ['remote', 'remote.goals']

declare const ctx: Context
declare const agentCtx: Context & { readonly remote: ClientRemote & TypertRemoteScopeApi<'agent'> }
declare const agentId: SessionId

await ctx.remote.goals.create(agentId, { objective: 'ship it' })
await agentCtx.remote.goals.create({ objective: 'ship it' })
```

No application assembles the Client face today. Business packages still publish `./remote` contributions, but nothing outside `packages/api/gateway/tests` imports them or calls `ctx.remote.$mount()`, and no package registers a Client Context adapter. The Client statements on this page describe `@deepseek-ai/dsh-api-gateway/client` as its source and tests define it.

## Component responsibilities

| Location | Package or entry | Responsibility |
|---|---|---|
| Shared | `@deepseek-ai/dsh-typert-protocol` | Declares decorators, Gateway bindings, merge-extensible protocol maps, invocation descriptors, and provider types; starts no TypeScript analysis and registers no Cordis services |
| Build | `@deepseek-ai/dsh-typert-generator` | Strictly analyzes Remote signatures, the type graph, lookups, Contexts, and source locations from the Host `ts.Program`, then generates Host and Host-for-Client artifacts |
| Host | `@deepseek-ai/dsh-typert-registry` and Loader | Places generated Host descriptors, schemas, and business-package registrations in `ctx.typert`, and holds lookup and Context providers |
| Host | `@deepseek-ai/dsh-agent` and `@deepseek-ai/dsh-session` | Register the `agent` and `session` lookups and the `agent` Host Context |
| Host | `@deepseek-ai/dsh-api-gateway` | Provides `ctx.typertGateway`, claims Remote endpoints, validates request values, resolves objects or Contexts, and invokes live Cordis services |
| Client | `@deepseek-ai/dsh-api-gateway/client` | Provides `ctx.remote` and `remote.<namespace>` child Services, mounts generated descriptors as concrete methods, and initiates and cancels calls through the Connection |
| Both | `@deepseek-ai/dsh-client-connection` | Provides the RPC carrier, request correlation, trust boundary, cancellation, response envelope, and the `/api` HTTP bridge |

The API Gateway package owns the Host dispatcher and Client Remote endpoint as peer entries compiled by separate projects, `tsconfig.host.json` and `tsconfig.client.json`, so the two never enter the same `ts.Program`. The Host entry does not import the Client Cordis `Context` merge, and the Client entry does not import the Host Gateway service.

## Strict generation pipeline

`bun run build` runs `build:lib:host` inside `build:runtime`, then builds the TUI. `build:lib:host` first runs `tsc -b tsconfig.host.json`, then `tsdown`; the root `tsdown.config.ts` loads `typertPlugin({ mode: 'workspace', faces: ['host'] })`, which the normal Host Project Reference graph compiles and which runs during this tsdown pass with the Host aggregate as its only `ts.Program` seed. No build phase compiles or bundles the Client face.

tsdown receives the workspace packages under `vendor/*`, `packages/*/*`, and `apps/cli`, and bundles only the `lib/types/{index,invariant,startup}.js` entries emitted by tsc.

`api/gateway` and `client/connection` split TypeScript faces: each has a `tsconfig.host.json` and a `tsconfig.client.json`, and the root `tsconfig.host.json` and `tsconfig.client.json` aggregates each reference the matching face.

Each contributing business package writes generated files to its own `lib/` directory, not to its source directory:

| File | Consumer | Contents |
|---|---|---|
| `typert.host.js` | Host Loader | Runtime reflection for the Host face, strict invocation descriptors, and schema registration values |
| `typert.host.d.ts` | Host type system | Generated declarations for the Host face |
| `typert.remote-client.js` | `ctx.remote.$mount()` | A mountable `TypertRemoteContribution` containing strict descriptors and runtime codecs |
| `typert.remote-client.d.ts` | Client type system | Declaration merges for `TypertRemoteNamespaceMap` and `TypertRemoteScopeMap`, plus Client-safe type references |
| `typert.remote-client.d.ts.map` | Editor | Maps generated method properties back to Remote method declarations in the Host package |

Business packages expose the Host Loader entry through `./typert` and the Host-for-Client entry through `./remote`. The generator also validates these package exports and published-file lists; it generates artifacts only for explicit contribution packages that provide the corresponding entry.

Parameter names in Remote Client declarations come from wire fields, while parameter and return types reference Client-safe types exported by the original business package. The declaration map resolves the generated property behind `ctx.remote.goals.create` back to the Host source method marked with `@Remote`, so editors that support declaration maps can navigate from a Client call to the real implementation instead of stopping at the generated `.d.ts`.

Strict analysis requires a Remote to be a public, non-static instance method with a concrete implementation. The method cannot be generic; parameters must be named simple identifiers and cannot use destructuring, default values, or rest parameters, and a lookup parameter must be required and named after its lookup key. Typert generates strict schemas for ordinary JSON-representable types; complex objects such as workspace classes must have a unique `TypertLookupMap` declaration. Lookup and Context packages are responsible for both static declaration merges and runtime provider registration; if either side is missing, the build fails or the first call that needs the provider fails.

## Runtime invocation

Remote calls use the Connection's `/api` route. The Client Remote calls `connection.rpc.call('/api', '<namespace>/<method>', { args }, signal)`; the HTTP carrier maps this to `POST /api/<namespace>/<method>`, with a payload containing only a named `args` object.

The Connection performs the unified trust check for `/api` before the HTTP bridge, then dispatches inside the shared FetchHandler. The Typert Gateway claims only two-segment endpoints that have or had a strict descriptor, or that match an active SRC marker; feature-owned exact Fetch routes handle non-JSON responses, and other requests return 404. The Connection owns transport, RPC ids, response envelopes, and request cancellation, while the Gateway owns only the Remote data protocol and business dispatch. The Gateway installs its `/api` interceptor only when a `connection` service is present, and the base bundle loads the Typert registry, Loader, and Gateway without Connection.

For every call, the Gateway resolves the descriptor and live service from the current registries instead of caching business objects. It requires the fields in `args` to match the descriptor, allowing only optional JSON fields to be omitted, resolves the receiver, checks the service binding, decodes wire values with codecs, resolves objects through registered lookup providers, and invokes the service method targeted by the binding. It returns the business result without decoding it. A missing provider, unknown identity, binding mismatch, missing or extra argument, codec failure, or missing method fails before business code runs.

The lookup provider's `register()` supplies both the stable declaration and the default resolver; `configure()` supplies a resolver owned by Host composition that may execute asynchronously and is scoped to an effect lifetime. Configuration may precede provider mounting; without a provider, invocation still fails with `gateway/lookup-unavailable`, and unloading the configuration restores the provider's default policy. `@deepseek-ai/dsh-agent` and `@deepseek-ai/dsh-session` register the default `agent` and `session` resolvers, which return only a live Agent or Session from their stores; no package calls `configure()` today. A resolver that throws a `RemoteError` keeps its code on the wire, any other resolver throw becomes `gateway/lookup-failed`, and an unresolved identity fails with `gateway/lookup-not-found`; only an unclassified throw folds into `gateway/internal`.

Unloading a Client contribution removes its descriptors and concrete methods together, aborts its in-flight calls, and makes stale method handles retained by external code fail further calls. A strict endpoint withdrawn on the Host also does not degrade to SRC inference, preventing a hot unload from silently weakening validation.

## SRC development fallback

When a Host process runs from source, for example through `node --import tsx/esm`, it does not execute the Typert compiler plugin. Standard decorator initializers still record the method name and invocation mode in a versioned descriptor on the Service prototype, while `TypertRemoteService` or `bindTypertRemote()` supplies the explicit service binding; the Gateway can therefore construct a weaker temporary descriptor without starting a `ts.Program`. The descriptor's stable string property name lets `remoteMethods()` read markers written by another installed copy of the protocol package.

The SRC fallback parses simple parameter names from the live function. When a parameter name matches the `parameter` of a registered lookup, such as `agent` or `session`, it uses the lookup's `agentId` or `sessionId` wire field and resolves the object on the Host; other parameters are checked only for cycle-free, JSON-safe data with no special prototype. `@RemoteScope` directly uses the wire field of a registered Host Context provider. SRC does not read TypeScript types or generate Zod schemas; it cannot tell optional parameters apart, so it lets any non-lookup field be omitted, and it does not support destructuring, default values, rest parameters, or duplicate parameter names.

SRC solves only dispatch for a Host process running from source. The Client does not discover decorators from the running Host, and the Client Remote refuses to mount SRC descriptors that lack strict codecs; its types, codecs, and Remote registration values always come from the most recently generated `lib/typert.remote-client.*` artifacts.

## Development mode

Changing only a Remote method's implementation body without changing its contract does not require regenerating the Typert files. After adding or removing a decorator or changing an export name, namespace, parameter, return value, lookup, Context, or cancellation signature, rerun the Host lib build so the Typert plugin regenerates the strict contract:

```sh
bun run build:lib:host
```

`bun run build` runs the same phase before building the TUI. `bun run typecheck` runs `tsc -b tsconfig.host.json` and the TUI type check but not tsdown, so it does not regenerate Typert artifacts. `bun run start` and `bun run dsh` launch the built CLI rather than a source Host, so they use the strict descriptors from the last build.

## Boundaries

Unary Remote methods take one request and return one result. A method declared with `@Remote({ mode: 'stream' })` instead returns an `Iterable` or `AsyncIterable`, carries `mode: 'stream'` in its descriptor, and cannot be invoked through unary `/api` dispatch.

The API layers are organized as `gateway → connection → webserver`. The Typert Gateway and the settings controller live under `packages/api`; Connection and WebServer live at `packages/client/connection` and `packages/host/webserver`. A feature that needs a non-JSON or browser-native response registers an exact Connection Fetch route instead of defining a Remote method.

Lookup policy is configured per key, so every `agent` or `session` parameter shares one resolver. Selecting a different policy for one parameter or endpoint would require an explicit per-parameter or per-endpoint policy, which does not exist; the business method must not guess how its object was resolved.
