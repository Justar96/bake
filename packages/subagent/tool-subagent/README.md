---
description: "Model-facing subagent delegation tool for users and maintainers configuring, composing, or debugging delegation over a subagent provider."
kind: "package-reference"
---

# bake-tool-subagent

## Summary

Use this package to give an agent a named tool that delegates work to a configured child-agent backend. In `one-shot` mode, calls wait for the child by default; in `continuable` mode, they start a persistent child in the background and return an id for later messages. Supported backends can also expose approved child LLM providers, models, and reasoning effort for selection. Each instance can set child persona, tool access, and depth limits, while failed runs return errors instead of partial success.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Further Exploration](#further-exploration)
- [Model Experience](#model-experience)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

Mount one instance per delegation target, each with a distinct `toolName`. The tool exists exactly while its provider does, so sibling load order and provider reloads never strand it.

### Minimal configuration

Load the subagent service, an in-process or remote backend, and this tool; then name the provider. This composition exposes a `subagent` tool that delegates to the `spawn` backend:

```yaml
- name: 'bake-subagent'
- name: 'bake-subagent-spawn-in-process'
- name: 'bake-tool-subagent'
  config:
    provider: spawn
    toolName: subagent
```

| Field | Default | Meaning |
|---|---|---|
| `provider` | required | Provider name on `ctx.subagents` (e.g. `spawn`) |
| `toolName` | `subagent` | Model-facing tool name; distinct for every loaded instance |
| `modelSelectionSettings` | `false` | Sample the Host's exact-route authorization preference for each top-level Session; a standing composition observes its Agents — a preset's own, or every Agent when mounted unscoped — while direct Agent setup passes its Session explicitly; requires provider `agentOptions` support |
| `enableRunInBackground` | `true` | Expose `run_in_background`; disabling also rejects forced background calls |
| `backgroundMode` | `one-shot` | Background policy: `one-shot` defaults calls to foreground; `continuable` defaults them to background and requires the provider's `prepareContinuable` capability |
| `agentOptions` | — | Configured child `provider`, `model`, adapter-owned `reasoningEffort`, and positive `maxTokens` defaults; requires provider `agentOptions` support and overlays any provider-owned route defaults |
| `persona` | — | Per-child persona; requires the provider's `persona` capability |
| `toolFilter` | — | Per-child global-tool restriction; requires the `toolFilter` capability |
| `maxDepth` | Host setting (`1`) | Absolute delegation-depth cap (`0` forbids delegation); `'provider-managed'` sends no cap to an out-of-process provider |

The generated [configuration catalog](../../../docs/config-catalog.md#bake-tool-subagent) is the exhaustive source for every accepted field and its JSDoc.

### Foreground and background modes

Under `one-shot` policy, an omitted `run_in_background` waits in the foreground and returns the child's final text; `run_in_background: true` starts a plain parent-owned background job and returns `started background subagent job <id>`, collected with `job_output` and stopped with `job_kill`.

Under `continuable` policy, an omitted or `true` `run_in_background` starts a durable child and returns `started subagent <childId>` without waiting for a result; the runtime delivers one settlement notice when the child's Activation ends, and the optional `send_message` tool sends it more work. Set `run_in_background: false` to wait for the result in the foreground.

`maxDepth` caps recursion (`0` forbids delegation); omission reads the current Host `subagent.maxDepth` setting, initially `1`, at each delegation. A numeric depth requires a provider with the `depthLimit` capability; `'provider-managed'` leaves the budget to an out-of-process provider. `persona` and `toolFilter` configure every child when the provider supports them, and the tool stays visible at the cap — each attempted start checks the calling agent's current depth and rejects with an errored result.

### Selecting a child LLM

Set `modelSelectionSettings: true` to sample the Host's `subagent-model-selection` preference when each fresh top-level Session is composed. A restored Session without a recorded policy remains disabled, including an explicitly empty restore. When enabled, the non-empty exact provider/model route list is recorded in the Session, inherited by child Sessions, and unchanged by later settings edits. The tool then exposes optional `provider`, `model`, and `reasoning_effort` fields and registers the shared `list_subagent_models` tool. This mode requires a backend that advertises `agentOptions`; both in-process backends and DSH SDK support it, while ACP, Codex, and Claude Code reject it rather than ignore it.

A call supplies `provider` and `model` together, or supplies only an effort when configured, parent, or provider-owned defaults provide the route. Static `provider.agentRouteDefaults`, when present, form the provider/model baseline; tool configuration and model fields overlay it before route-aware effort merging and exact-route preflight. Providers without these defaults use compatible values from the parent's latest logged request, then the parent's creation options before its first request, while retaining the configured `maxTokens`. Changing the route without an explicit effort clears the inherited route-owned effort, so the selected model resolves its default. The live LLM adapter validates the effective route before child creation. Catalog membership remains advisory, so a model can use an unlisted id when its adapter accepts it.

The setting's `router` is a task router that chooses for calls that name no route. It is a beta, off until `router.enabled` is set. While it is on, such a call first posts its description and prompt and the Session's allowed routes to `<url>/v1/bake/select`, the [ing](https://github.com/Justar96/ing) router protocol; `url` defaults to the hosted ing router, `https://ing.gissx.org`. A task longer than 20,000 characters is sent as its opening three eighths and its end, joined by `[…]`, the excerpt the router itself judges, since a delegation that pastes a log usually puts the ask last. The router only chooses; every route still runs on the user's own provider. Each route carries its model info, so a router that recognizes no model name can still constrain and rank it: its reasoning efforts (none when the model exposes no effort control), default effort, context window, and input types. It also carries the user's `router.hints` entry for that route, if any: `sameAs` names a model the router knows that the route serves, such as `claude-opus-4.5` behind a gateway alias, `quality` (`low`, `medium`, `high`, `frontier`) stands in for missing benchmarks, and `cost` (`free`, `low`, `medium`, `high`) replaces the router's price. `router.priority` (`quality`, `cost`, `speed`, `balanced`), when set, replaces the trade-off the router would infer from the task text. The request carries `Authorization: Bearer` with the credential that `router.tokenEnv` names (default `ING_API_TOKEN`), resolved through `ctx.credentials` for each request, so an exported variable of that name wins over the stored one; without a credential store only the environment is read. The answer is used only when it names one of the allowed routes and is not marked `fallback`, which a router sets when nothing told the routes apart. Its reasoning effort is used when the route's model info lists it. Otherwise the listed effort nearest in strength is used; a tie goes to less thinking for `low` and more for anything harder. An effort id of unknown strength is dropped, so the model's default applies. A failed request, a non-2xx status, a malformed or out-of-policy answer, or a timeout after `router.timeoutMs` (default 5000) logs a warning, and the call then uses its default route. Aborting the delegation also aborts the router request. Turning routing on with an empty `url` is refused. The router is read at each delegation, so editing it changes later calls in running Sessions without changing their recorded route list. `describeRoutes` posts the same route descriptions, without a task, to `<url>/v1/bake/routes`, even while routing is off. It returns, for each allowed model, the benchmarked model the router matched, whether it is ranked (has quality evidence), and the quality and blended price it scores the model with. `/settings` shows this as Model calibration. A router that answers `401` to a request sent without a token fails with the name of the variable to set. The service also signs in to an ing router by email: `requestSignInCode` has the router email a one-time code to an address, `signIn` trades the code at `<url>/auth/email/verify` for a token labelled `bake` and stores it under `router.tokenEnv` through `ctx.credentials.set`, and `signOut` revokes the token at `<url>/auth/logout` and removes it, even when the router cannot be reached. A first sign-in with an address registers it. `routerTokenStatus` says whether a token is stored and from which layer, never the token, and `routerAccount` asks `<url>/auth/me` which address it belongs to. Signing in or out is refused before anything is sent while the environment supplies the token or no credential store is mounted. A refused sign-in fails with the router's own reason, such as `wrong or expired code`.

ing may attach an optional `routing` assessment with a policy version, `normal`, `cautious`, `needs_context`, or `fallback` status, normalized difficulty, and reasons. Missing or malformed assessment metadata does not invalidate an otherwise valid allowed route; a `fallback` answer still retains the existing default. Explanations are stripped of terminal controls and bounded before recording: 500 code points for the reason, 64 for the policy, and eight reasons of 240 code points each. Model calibration also preserves `quality_source: inherited`, distinguishing predecessor evidence from measured benchmarks and user hints.

After child admission succeeds, the tool records an ignorable, log-only `subagent/routing-decision` event on the direct parent. It names the child and tool call, the selection source (`explicit`, `default`, `auto`, or `fallback`), the effective route when known, and optional router evidence. Unknown effort is omitted; a rejected router suggestion is never presented as the effective route. The `subagentRoutingDecisions` projection exposes a wire record keyed by child id; `SubagentRoutingDecision` and `SubagentRoutingAssessment` are exported types for consumers. Replay preserves the explanation, while forked children exclude inherited parent decisions. Older Sessions without a decision remain valid. These records add no model messages, tool arguments, results, or token usage. A recording failure logs a warning and leaves the admitted child's execution and ownership unchanged.

```yaml
subagent-model-selection:
  enabled: true
  allowedModels:
    - { provider: cliproxyapi, model: claude-opus-5-5 }
    - { provider: cliproxyapi, model: glm-5.3-flash }
    - { provider: ollama, model: "llama3.1:70b" }
  router:
    enabled: true
    url: https://ing.gissx.org
    tokenEnv: ING_API_TOKEN
    timeoutMs: 5000
    hints:
      - { provider: ollama, model: "llama3.1:70b", quality: medium, cost: free }
```

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

This section explains how the tool mirrors provider lifecycle and settles runs; the observable behavior is covered in [Use this package](#use-this-package).

### Design concept

One instance is one provider plus one tool name. The plugin mirrors provider lifecycle: it registers the tool when the named provider appears and disposes it when the provider leaves, so sibling load order and HMR replacement cannot strand a dangling tool. Direct Agent setup passes its unpublished Session explicitly and awaits installation before publication. A settings-backed standing composition receives each of its Agents through `agent/created`, selects policy from its Session, and awaits installation through its Context; installation failure rejects creation. A preset's composition covers the Agents composed under its scope; an unscoped one, such as the one-shot profile's, covers every Agent in the process. A numeric `maxDepth` or configured LLM selection the provider cannot enforce fails the mount instead of the first delegation. At most one instance in a tool scope may own model selection because `list_subagent_models` has a global name.

### Foreground settlement

A foreground call awaits `run.result`, maps every non-completed stop reason to an error headline, appends the provider diagnostic and any preserved partial assistant text, and always awaits `run.dispose()` before returning; when result collection and disposal both reject, the errored result preserves both failures.

### Background routes

One-shot background registers a plain parent-owned Task whose done channel settles the start and keeps the stop reason and optional provider diagnostic in its detail. Continuable background calls `ctx.subagents.startContinuable()`, which resolves at inbox acceptance: the child owns its own turns from there, so the call neither waits for nor collects a result.

### Delegation wording

The tool's description says the child does not see this conversation, because every child starts with a fresh one, so the model writes a standalone prompt. The description is the only home for delegation guidance: the plugin registers no system-prompt section, so a tool restriction that hides the schema also hides the guidance.

### UI presentation

Each delegation tool declares a pure `presentCall`: a UI titles the call by its short `description`, or by the prompt's first line when the description is blank, cut at 80 characters. The prompt, which can run to many paragraphs, stays out of the card; it is the child session's first message. `list_subagent_models` is titled by what it looks up: `List subagent providers`, `List <provider> models`, or `Show <provider>/<model>`. Neither tool declares `presentResult`, so a completed call keeps the UI's generic rendering of the result text, and obsolete logged arguments keep generic rendering for the call as well.

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | Tool registration, lifecycle mirroring, mode resolution, result settlement |
| [`src/presentation.ts`](src/presentation.ts) | Pure call titles for the delegation and discovery tools |
| [`src/model-selection.ts`](src/model-selection.ts) | Request/config merge and live LLM route preflight |
| [`src/model-selection-settings.ts`](src/model-selection-settings.ts) | Host-owned opt-in setting sampled for new Sessions |
| [`src/model-selection-state.ts`](src/model-selection-state.ts) | Session event that records and inherits the sampled decision |
| [`src/auto-route.ts`](src/auto-route.ts) | Task-router request that picks an allowed route for an unrouted call |
| [`src/routing-state.ts`](src/routing-state.ts) | Parent-owned route decisions, projection, and replay filtering |
| [`src/types.ts`](src/types.ts) | Display-only routing decision and assessment types |
| [`src/list-models.ts`](src/list-models.ts) | `list_subagent_models` runtime discovery tool |

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read these pages when the package-level contract is not enough; they move from the tool's runtime behavior to the seam it delegates over and the adjacent child tools.

- [Subagent subsystem](../../../docs/subsystems/subagent.md) — providers, one-shot start requests, continuable children and activations.
- [bake-tool-subagent-control](../tool-subagent-control/README.md) — messaging, interrupt, and listing tools for continuable children.
- [Generated tool catalog](../../../docs/tool-catalog.md#bake-tool-subagent) — the default schema and per-mode wording.
- [Generated configuration catalog](../../../docs/config-catalog.md#bake-tool-subagent) — every accepted config field.
- [Background-first continuable delegation](../../../.agents/notes/archived/feature/2026-08-11-background-first-continuable-delegation.md) — why continuable work defaults to background.
- [Model-selected subagent routes](../../../.agents/notes/implemented/feature/2026-08-18-model-selected-subagent-routes.md) — selection policy, inheritance, discovery, and the fork restriction.

-----

<a id="model-experience"></a>
## Model Experience

### Tool schema

#### What the model sees

The generated default [`subagent` schema](../../../docs/tool-catalog.md#bake-tool-subagent) under this instance's configured name while its provider exists. An enabled Session policy adds `provider`, `model`, and `reasoning_effort` plus inheritance and selection guidance; the provider must support `agentOptions`. Provider context inheritance changes the tool and prompt descriptions. Enabled background mode adds `run_in_background`: continuable mode documents its background default, the returned agent id, the completion notice, `send_message` follow-ups, starting independent subagents together, and when to pass `false`, while one-shot mode documents that the call waits unless `run_in_background` returns a job id for `job_output` or `job_kill`. The wording is kept terse because every parent request resends it. The package adds no system-prompt section, so all of this guidance travels with the schema. With the default tool name `subagent`, the continuable description is:

##### Continuable description

```markdown
Delegate a self-contained task to a subagent with its own context; it does not see this conversation and returns only its result. It runs in the background by default, returning its agent id: start independent ones together and keep working; you are notified when one finishes. `send_message` continues it later or steers it while running.
```

#### Token effect

Fixed schema cost per parent request; model selection adds three parameters. Each provider instance adds one schema and nothing to the system prompt.

#### KV Cache effect

Prefix-stable while provider instances and their configuration are unchanged. Adapter catalog changes do not alter the definition.

### Model selection and discovery

#### What the model sees

A settings-controlled instance whose Session carries a policy exposes the child LLM selection fields and `list_subagent_models`. Calls reject while the optional `ctx.llm` service is unavailable. Discovery returns only registered providers and advertised models in the exact route policy; an unauthorized provider is rejected before its adapter catalog is called, and an exact lookup must be allowed before it resolves the model's reasoning efforts and default. Execution independently enforces the same policy. Both tools read a blank `provider`, `model`, or `reasoning_effort` as omitted, because GPT models send every optional field and leave the unused ones empty; a call with all three blank delegates on the default route or asks the router. A refused provider or route names the Session's allowed routes, so the model can correct the call without another lookup.

#### Token effect

One fixed discovery schema is present in enabled compositions. Directory contents enter the transcript only when the model calls the tool.

#### KV Cache effect

The schema is prefix-stable across adapter registration and catalog changes. Each discovery result is appended after the reusable prefix.

### Foreground result

#### What the model sees

The call retains the description and prompt. Success contains only the child's final text; other outcomes become `Error: <stop reason>`, followed by a safe provider diagnostic when present and then any partial assistant text. Intermediate child steps stay out of the parent.

#### Token effect

The prompt and result remain in parent history until compaction; child working context remains in the child.

#### KV Cache effect

Append-only; newly visible content follows the reusable request prefix and does not invalidate existing KV-cache entries.

### Background result

#### What the model sees

Start returns exactly `started subagent <childId>` in configured continuable mode, or `started background subagent job <id>` in configured one-shot mode. In one-shot mode the generic task surface provides later status, final output, cancellation responses, and notices; failed status detail includes the provider diagnostic when the result supplied one. In continuable mode this tool returns no result of its own: the child's settlement reaches the parent as a service-owned notice, an independently loaded `send_message` tool delivers follow-ups, and the child's transcript by its id is the source of its detailed output.

#### Token effect

The acknowledgement is retained; a one-shot final output enters parent history only when collected or injected, while a continuable child's output never returns through this tool — its settlement notice arrives independently of any tool result.

#### KV Cache effect

Append-only; newly visible content follows the reusable request prefix and does not invalidate existing KV-cache entries.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>


These limits define what this tool does not return or enforce; they are current package constraints.

- **Background runs expose no result through this tool** — a one-shot task's final output is collected through the generic task surface, and a continuable child's output stays in its own session, read by its agent id. The settlement notice states how that child ended and carries nonempty text from its final assistant output, but it is not this call's return value and cannot be awaited here.
- **Duplicate names across waiting instances are detected late** (`TODO(subagent-dup-toolname)`) — two instances with the same `toolName` collide only when their provider appears, and the duplicate-name failure rolls back that provider registration; failing earlier requires a registry of intended names.
- **Non-routing child policy is fixed per instance** — another persona, tool filter, or depth cap requires another distinctly named tool. LLM selection requires an enabled per-Session preference and a provider that advertises `agentOptions`; the in-process provider and DSH SDK advertise it, while ACP, Codex, and Claude Code reject it rather than ignore it.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
