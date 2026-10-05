---
description: "Automatic conversation condensation for deployments choosing, tuning, or debugging how older history is summarized as token pressure builds."
kind: "package-reference"
---

# bake-compaction-basic

## Summary

This package keeps long agent conversations working near the model's context limit. As token pressure builds, it condenses the oldest history into a summary while preserving recent messages; after a context-overflow error, it condenses and retries. You can also request condensation with `/compact` and optionally trim oversized tool outputs first. Condensation uses one extra model request and retains only its summary text. It cannot reduce the system prompt, tools, or session prefix, or split one indivisible unit such as a single huge tool call.

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

Mount this package to get automatic conversation condensation in a composition that already provides an LLM, session storage, and token measurement. The shipped `bake` base enables it by default; mount it explicitly to control when condensation starts.

### What you get

With the default settings you get four behaviors: automatic condensation as the conversation grows toward the model's context limit; recovery after a confirmed context-overflow error, where the conversation condenses and the request retries; on-demand condensation through the `/compact` command; and — when the pruner is mounted — trimming of oversized tool outputs before condensation.

### Smallest working composition

Mount session storage, token measurement, the optional pruner, this backend, and optionally the on-demand command:

```yaml
- name: 'bake-session'
- name: 'bake-token-meter'
- name: 'bake-compaction-tool-result-pruner'
- name: 'bake-compaction-basic'
- name: 'bake-command-compact'
```

You can verify success by watching the conversation continue past the point where it would otherwise overflow, and by running `/compact` for an immediate condensation. If the composition lacks an LLM, session storage, or token measurement, the plugin fails to load. One backend can serve models with different context sizes; give each route its own threshold and retention with a per-model override:

```yaml
- name: 'bake-compaction-basic'
  config:
    thresholdRatio: 0.8
    retainRatio: 0.16
    modelPolicies:
      - provider: local
        model: small-context
        thresholdRatio: 0.7
        retainTokens: 2048
```

### Tuning when condensation starts

All settings are optional. The defaults start condensing at 80% of the routed model's context window and keep the newest 16% verbatim; the table below is the complete policy surface, and the generated [configuration catalog](../../../docs/config-catalog.md#bake-compaction-basic) is the exhaustive source.

| Field | Default | Meaning |
|---|---|---|
| `thresholdRatio` | `0.8` | Start condensing at `floor(routedContextWindow × ratio)`; mutually exclusive with `thresholdTokens`. |
| `thresholdTokens` | — | Start condensing at this absolute request size; mutually exclusive with `thresholdRatio` and must not exceed the routed context window. |
| `retainRatio` | `0.16` | Recent conversation kept verbatim as a fraction of the routed context window; mutually exclusive with `retainTokens`. |
| `retainTokens` | — | Absolute recent-conversation budget kept verbatim; mutually exclusive with `retainRatio` and must be below the resolved threshold. |
| `summarizationProvider` | `''` | Set together with `summarizationModel`; an empty pair uses the latest routed request target, then the `AgentOptions` pair. |
| `summarizationModel` | `''` | Set together with `summarizationProvider`; an empty pair uses the latest routed request target, then the `AgentOptions` pair. |
| `maxTokens` | `8192` | Output cap for the summarization request; may include reasoning tokens. |
| `compactionRetries` | `1` | Extra condensation attempts after the first when pressure remains above threshold. |
| `maxOverflowRetries` | `1` | Maximum retries after a confirmed context-window overflow; `0` disables recovery only. |
| `modelPolicies` | `[]` | Per-route `{ provider, model?, ...partialPolicy }` overrides; an entry without `model` covers every model on that provider route. |
| `auto` | `true` | Enable automatic condensation and overflow recovery; set `false` for manual-only operation. In settings, `false` switches it off and `true` defers to the composition. |

An override applies field by field: the exact `provider` + `model` entry wins over the provider-wide entry for that provider, which wins over the top-level defaults. A threshold or retention form set at a more specific level replaces the inherited form as a unit, so an exact entry's `thresholdRatio` replaces a provider-wide `thresholdTokens`.

Misconfiguration fails fast: an unknown setting, a second entry for the same provider and model, a second provider-wide entry for the same provider, both threshold forms or both retention forms together, or two ratios or two absolute budgets where retention is not below the threshold all reject the plugin at load. A pair that mixes a ratio with an absolute budget, and a `thresholdTokens` above the window, fail when that model is first used, because the comparison needs the model's context size.

Interfaces can ask where condensation starts for a route. `pressureThreshold(route, contextWindow)` returns the resolved `thresholdTokens`, or `floor(contextWindow × thresholdRatio)`, after the route's `modelPolicies` overrides, the same figure the automatic check compares with the token meter's measurement. It returns `undefined` with `auto: false` from the composition or settings, for an empty provider or model, and for a capacity, `thresholdTokens`, or `retainTokens` budget the automatic check would reject with a warning instead of condensing. The TUI shows this value beside context occupancy.

### Changing the policy from settings.yaml

With a settings provider such as `bake-settings-file` mounted (the shipped `bake` base mounts it), the `compaction-basic` section of `settings.yaml` overrides the composition config field by field. The TUI's `/settings` edits it in its Compaction section, where each threshold and retention takes a percent of the context window or a token count, and lists every raw field under Advanced. Every field above is accepted. `auto: false` switches automatic condensation and overflow recovery off; `auto: true` leaves the composition's switch in charge, so it cannot turn on an engine the composition keeps manual-only, such as the terminal's host engine beside each preset's own. A list replaces the composed list wholesale, so a `modelPolicies` section must repeat any composed entries it keeps. A threshold or retention form set in settings replaces the composed one, so `thresholdTokens` in settings over a composed `thresholdRatio` is not a conflict. For example, to compact every model on a `cliproxyapi` route by token count:

```yaml
# settings.yaml
compaction-basic:
  modelPolicies:
    # Every model on the cliproxyapi route compacts at 150k tokens.
    - provider: cliproxyapi
      thresholdTokens: 150000
      retainTokens: 30000
    # One model on that route keeps a ratio instead.
    - provider: cliproxyapi
      model: gpt-5-codex
      thresholdRatio: 0.7
```

A saved change applies at the next pressure check, without a restart. Every engine in the process follows the one section: the first to load registers the namespace, and each later one, such as an agent preset's own engine beside the host's, resolves the same user section over its own composition entry whenever the stored section changes. The engine that compacts a session therefore serves the user's policy whichever engine registered it. A section that fails the rules above keeps the previous policy serving and logs a warning that names the failed rule; at startup the composition policy serves until the section is repaired.

### What happens when condensation runs

The oldest balanced span is replaced by one summary message and the recent tail stays verbatim; the conversation continues from the summary. The operation reports how many history items were condensed and the estimated tokens freed. If nothing can be condensed safely — for example the whole conversation is one indivisible unit — nothing changes and nothing is written to the session log. If no model is available to write the summary (no configured target and no routed request yet), condensation fails with a clear error telling you to configure the summarization provider and model or route one request.

After the summary, the checkpoint lists the files the condensed span read and changed, so they survive condensation exactly. The lists come from the span's successful `read`, `read_image`, `write`, and `edit` calls and from the previous checkpoint's lists, so they carry forward across condensations. A path the span changed is listed only as modified. Each file list keeps its 50 most recently touched paths and counts the rest. A checkpoint from an older release may also end with a `<todo-list>` section; it stays in that checkpoint's text and is not carried into the next one.

If the summary request itself is too large for the summarizing model, condensation retries it once as a bounded plain-text transcript of the same span, with tool results, tool-call arguments, and reasoning cut to 2,000 characters. If that also overflows, it summarizes the older half of the span instead, up to three times. During overflow recovery and `/compact`, a summary request that fails with a transient provider error, such as a rate limit or a server error, is retried up to three times with the summarizing provider's backoff; automatic condensation under pressure does not retry, because the next step checks pressure again.

### On-demand condensation with /compact

With `bake-command-compact` mounted, type `/compact` in a chat UI to condense immediately, even below the pressure threshold. The command reports how many history items were condensed and the estimated tokens saved. While the agent is mid-turn or condensation is already running, `/compact` reports that condensation is unavailable; prompts you send while it runs are accepted and start after it finishes.

### Trimming oversized tool outputs

Mount `bake-compaction-tool-result-pruner` before this package to trim oversized tool results as part of condensation. Trimming makes no model call and can remove the need to summarize at all: when the trimmed conversation lands below the prune-only ceiling, halfway between the retained tail and the threshold, condensation skips the summary. A trim that clears the threshold by less still summarizes in the same pass, because each rewrite invalidates the provider's prompt cache and a barely-sufficient trim would trigger another rewrite a few steps later. Trimming only runs after a condensation trigger qualifies — a below-pressure conversation is never touched.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

This section explains the design decisions behind the backend; the observable behavior is fully covered in [Use this package](#use-this-package).

### Design philosophy

The backend is built on four commitments:

- **One measurement service prices every decision.** The singleton `ctx.tokenMeter` measures the latest canonical logged envelope and current surface at one consumed-log revision. When the routed adapter declares request-image pricing, the meter applies it to image history. Pressure, recent-tail retention, range selection, and shrink validation use the same route-priced node figures; logged replacement shadow prices stay on the route-independent heuristic so pure projection folds remain consistent.
- **The log-recorded bracket is the transaction.** All entry points share one bracket-first region transaction: validate the range and live lock, append `compaction/start` synchronously, prepare and await the summary, revalidate, append `compaction/summary` plus the replacement, and make exactly one closing attempt. Automatic and explicit-region calls require a numeric open-turn owner and whole-surface stability; `compactNow()` reserves idle admission, uses `turn: null`, accepts append-only context outside its selected span, flushes every closed attempt, and releases admission in `finally`.
- **Summarization can reuse the provider's warm prefix.** Replaying the system prompt held by the `system/message` at surface node 0, the last routed request's tools, and the shadowed-region messages can preserve a matching conversation prefix. The selected route, tool-history projection, and provider determine cache reuse.
- **`summarize()` is the sole subclass hook.** A template- or remote-summarizer subclass can override it while pressure, retention, cited source events, shrink validation, and shadowed-token accounting stay on the token meter.

### Automatic triggers and overflow recovery

With `auto: true`, a serial `agent/pre-step` listener checks pressure before request derivation: it prices the latest durable routed request envelope through `ctx.tokenMeter`, and when pressure crosses the routed model's threshold it prunes, then summarizes the oldest balanced span while keeping a priced recent tail. Every selected range starts at the first surface node that is not a `system/message`, so a system prompt at surface node 0 is never shadowed; a later `system/message` appended by an in-history prompt update is ordinary history that the range may shadow, and the agent loop's projection then replaces node 0 with the current prompt when their text differs ([decision rule](../../core/agent-loop/README.md#understand-the-implementation)). The `agent/request-error` listener reacts to a provider-confirmed `CONTEXT_WINDOW_EXCEEDED`: it bypasses the normal threshold and retention policy, attempts one maximal balanced head reduction, and authorizes a retry only after the surface replacement generation advances. Cancellation stays authoritative throughout.

Pressure policy resolves capacity from the adapter that owns the durable route. An adapter that returns no capacity for a valid dynamic route makes the manual pressure path throw a target-specific configuration error; the automatic listener warns once for that exact target and continues with full history.

### Summarization mechanics

A direct `ctx.llm.stream()` call uses the configured provider/model pair and cap, falling back to the latest logged request target and then the `AgentOptions` pair, without running the loop-only `agent/request` extension point. The call replays the derived `system/message` at surface node 0 as the leading entry of `messages`, followed by the shadowed-region messages (including a shadowed in-history `system/message` in its surface position), and carries the header's tools verbatim — including image references, which the selected adapter must resolve or explicitly reject — and appends the compaction instruction as the final user message, so a matching prefix remains eligible for provider caching. An empty-content system head contributes no message but remains outside the compacted range. The call sets `GenerateOptions.purpose` to `compaction`; only returned text enters the checkpoint, excluding reasoning and tool calls. Image output fails with `UNSUPPORTED_CONTENT` rather than disappearing. The replacement user message frames the summary with `<compacted-summary>` tags; the raw summary remains on the `compaction/summary` event.

The auxiliary call also carries `session.toolHistory()`. Native tool updates are reused only when the selected prefix contains every update anchor; otherwise the runtime sends complete active declarations. The next conversation request after a surface replacement starts a new declaration series. Prefix-cache reuse is provider-dependent and is not guaranteed for a shortened or differently routed summary request.

### The region transaction

A failed summary request is handled in this order. Under overflow recovery and `compactNow()`, a failure whose code the summarizing provider's `retryPolicy` lists as retryable (every code under an `always` policy, except `CONTEXT_WINDOW_EXCEEDED`) is retried after `retryDelayMs()` from `bake-llm`, at most `min(maxRetries, 3)` times; a provider-requested delay above a normal policy's ceiling ends the retries. The wait aborts with the transaction's signal or the plugin's disposal. Pressure compaction passes no retry plan. Next, failed summary requests dispatch synchronous `compaction/summary-error` after checking cancellation and selection stability. A recovery listener must record a durable input change before requesting retry. The backend re-derives the selected messages and refreshes their token prices and shrink baseline. The image-offload plugin owns image selection; its recorded omissions remain effective if the summary later fails or is cancelled.

When no listener repairs a `CONTEXT_WINDOW_EXCEEDED` summary failure, the transaction prepares a bounded attempt: the same span serialized by `boundedSummarizationInput()` as one transcript user message, without the system head or tool schemas, then up to `MAX_SUMMARY_RANGE_HALVINGS` (3) attempts over the older half of the previous span, each cut at the latest balanced boundary. A shortened span is the span the checkpoint replaces, and its shadowed seqs and prices are re-derived. Every failed attempt is logged as a warning; the `compaction/summary` record does not say which form produced the summary.

After the summary succeeds, `collectCheckpointContext()` reads the final span's derived messages, never the log beyond them, and `formatCheckpointContext()` renders the sections that `frameSummary()` appends after `</compacted-summary>`. A prior checkpoint in the span contributes the sections it carries, parsed only from text after its summary block.

The transaction validates the surface span and the durable lock, appends `compaction/start`, summarizes through the hook, revalidates stability (whole-surface for automatic calls, selected-span for manual calls), rejects a summary that does not shrink its source, appends `compaction/summary` plus the replacement `user/message`, and makes exactly one `compaction/end` attempt. A live unmatched start is the durable lock: an unmatched marker before a newer `session/end-seed` is stale evidence from a prior lifecycle and does not block; one after that boundary reports `busy`. A failed close deliberately leaves a blocking orphan. Cancellation remains authoritative after cleanup and durability.

### Config resolution

`resolveConfig` validates and detaches the defaults, `resolveTargetPolicy` layers the exact provider/model override over the provider-wide override over them, and `resolveCompactSpec` scales the merged policy into concrete token budgets using the adapter-owned context capacity. Model discovery (`listModels()`) is never consulted for policy; only the durable route's capacity matters.

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | Plugin entry: `BasicCompactionEngine`, automatic listeners, entry-point dispatch |
| [`src/region.ts`](src/region.ts) | Retention selection and the shared bracket-first compaction transaction |
| [`src/summarizer.ts`](src/summarizer.ts) | Default `ctx.llm.stream()` summarization, checkpoint framing, safe-summary projection |
| [`src/checkpoint-context.ts`](src/checkpoint-context.ts) | Read and modified file lists appended to a checkpoint |
| [`src/bounded-input.ts`](src/bounded-input.ts) | Transcript summarizer input after a summary request overflows |
| [`src/summary-retry.ts`](src/summary-retry.ts) | Transient summary-failure classification and cancellable backoff |
| [`src/config.ts`](src/config.ts) | Load-time validation and routed-model policy resolution |
| [`src/types.ts`](src/types.ts) | `BasicCompactionConfig` and resolved policy vocabulary |
| — | No runtime invariant companion is published; this package exposes no independent event sequence or mutable data relation beyond contracts enforced at its owning seam. The durable bracket remains observable in the session log. |

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read these pages when the package-level contract is not enough; they move from the shared seam to the optional companions and the decision evidence.

- [Compaction seam](../compaction/README.md) — the condensation contract this backend implements.
- [Compaction subsystem reference](../../../docs/subsystems/compaction.md) — the condensation vocabulary, results, and service behavior.
- [Tool-result pruner](../compaction-tool-result-pruner/README.md) — the optional companion that trims oversized tool outputs first.
- [Human /compact command](../command-compact/README.md) — on-demand condensation without waiting for pressure.
- [Token meter](../../llm/token-meter/README.md) — the measurement service that decides when to condense.
- [Generated configuration catalog](../../../docs/config-catalog.md#bake-compaction-basic) — every accepted config field and its source declaration.

-----

<a id="model-experience"></a>
## Model Experience

### Conversation history

#### What the model sees

After a successful step crosses the threshold, oversized tool results are first rewritten when the optional pruner is loaded. If summarization remains necessary, the next request receives the checkpoint preamble below, a blank line, `<compacted-summary>`, the data-dependent summary, `</compacted-summary>`, and then any non-empty working-state sections below. Overflow recovery rebuilds the immediate retry from whatever replacement advanced the surface. A checkpoint replaces the selected older range and is followed by the retained recent units.

##### Conversation checkpoint preamble

```markdown
This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. Treat the captured context as established background and build on it without restating it. Continue the task directly from the messages that follow, without acknowledging this checkpoint.
```

##### Working-state sections

Each section appears only when it has entries, in this order, separated by a blank line. The sections immediately follow `</compacted-summary>` in a separate text block. A list longer than its cap begins with `... N earlier paths not shown`, since file lists keep the newest paths.

```text
<read-files>
src/a.ts
docs/b.md
</read-files>

<modified-files>
src/index.ts
</modified-files>
```

#### Token effect

The working-state sections add one line per listed path, bounded at 50 per list, and count toward the shrink check that every checkpoint must pass. Model-free pruning can avoid the auxiliary call entirely; otherwise it reduces that call's transcript before the summary replaces an older range. The replacement reduces future input history rather than appending a second copy. A summary remains until a later compaction replaces it, while an indivisible non-tool unit can still exceed the budget.

#### KV Cache effect

Replacing rather than append-only. Each checkpoint or pruned tool result invalidates reuse from the first replaced history token; the unchanged request prefix before that range remains reusable. The prune-only ceiling exists to bound how often that happens: a pass either frees at least half a summary's headroom or also summarizes, so pressure settles with one cache rebuild instead of a rebuild every few steps near the threshold.

### Auxiliary summarizer request

#### What the model sees

The summarization model receives the conversation replayed verbatim — the same system prompt, tool schemas, and messages the last routed request sent for the shadowed region — followed by one final user message: the compaction instruction below. The conversation model never sees this private request or its reasoning; only returned text is stored.

##### Compaction instruction (final user message)

```markdown
You are now acting as a compaction engine for this AI coding assistant. Condense the conversation ABOVE into a structured checkpoint that lets another model resume the work with no loss of essential context.

Output EXACTLY the Markdown structure below: keep every section, in order. Use terse bullets, not prose paragraphs. Write "(none)" for an empty section — never drop a section.

## Primary Request and Intent
- [the user's original and evolving goals; quote verbatim where the exact wording matters]

## Key Technical Concepts
- [technologies, frameworks, patterns, and conventions in play]

## Files and Code
- [exact path: why it matters, key changes or snippets]

## Errors and Fixes
- [error: how it was resolved, plus any related user feedback]

## Pending Jobs
- [explicitly requested work not yet completed]

## Current Work
- [precisely what was in progress at this checkpoint]

## Next Step
- [the single next action, directly in line with the most recent request, or "(none)"]

## Critical Context
- [decisions and their rationale, constraints, user preferences, open questions, data needed to continue]

Rules:
- Write concise English engineering prose. Preserve exact file paths, commands, error strings, identifiers, numeric values, function signatures, and syntax fragments.
- Capture user feedback and explicit instructions faithfully, especially corrections.
- Do NOT mention this summarization request or that the context was compacted.
- Output only the checkpoint text: do not call any tool or take any other action.
- If the conversation already contains a <compacted-summary> block, it is a PRIOR checkpoint. Do not copy it forward verbatim: preserve still-true facts, drop stale ones, and merge newer information into a single consolidated summary under the same structure.
```

When the replayed request exceeds the summarizing model's context window, the retry sends no system prompt and no tools: one user message holding the lead-in below, a blank line, and the span as a transcript inside `<conversation>` tags, followed by the same compaction instruction. Transcript entries are `[User]: `, `[Assistant]: `, `[Assistant reasoning]: `, `[Assistant tool call]: name(arguments)`, `[Tool result]: `, and `[Tool error]: `, separated by blank lines; images and files appear as `[image]` and `[file]`. A cut entry ends with `[... N more characters truncated]`.

##### Bounded transcript lead-in

```markdown
The conversation to condense is serialized below as a transcript. Tool results, tool-call arguments, and reasoning longer than 2,000 characters are cut, with a marker giving the number of characters removed.
```

#### Token effect

This is a separate model call: the replayed conversation prefix plus the fixed instruction as input, with `maxTokens`-capped output. Convergence retries, transient-failure retries, and bounded overflow attempts can pay this cost more than once; a bounded attempt's input is at most the span's text with long tool output cut.

#### KV Cache effect

The summarizer can reuse a matching request prefix before its final instruction. A different provider/model, a non-head range, or a prefix missing tool-update anchors can prevent that reuse; provider caching determines the actual savings. A bounded transcript attempt shares no prefix with the conversation and is always a cache miss.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>


These limits define when automatic condensation is a poor fit or needs special care; they are the current package constraints.

- **Meter accuracy follows the fixed heuristic** — missing reusable provider usage falls back to character count plus structural overhead rather than exact tokenization; image occurrences carry provider-exact visual tokens only on routes whose adapter declares request-image pricing.
- **Overflow classification is adapter-maintained** — provider wording can change; the pi-ai adapter normalizes recognized context-limit failures to `CONTEXT_WINDOW_EXCEEDED`.
- **Some indivisible-unit and envelope-only overflow remains outside surface compaction** — recovery cannot shrink system/tools/prefix, split an indivisible non-tool node, or repair a tool unit whose non-prunable remainder still exceeds the window. The optional pruner can shrink text-bearing tool-result bulk inside an otherwise indivisible pair.
- **`compactRegion` requires an open turn** — a manual call on a fully-closed session throws ("no open turn") rather than compacting.
- **The log does not record which summarizer input form ran** — `compaction/summary` keeps the summary, route, and usage, but not whether the replayed prefix or a bounded transcript produced it, or how many attempts failed first; only the warning log does. The checkpoint text itself is durable.
- **Working-state lists see only file-tool calls** — paths changed through `bash` or `pwsh` are not listed, even when a shell change report recorded them, because that report is display-only by design; paths are listed as the model wrote them, without resolving them against the working directory.
- **Summarization failure preserves the latest durable surface** — before any replacement, the auto path logs a warning and proceeds with full over-budget history. If pruning already landed, a later summarization failure proceeds from that durable pruned surface. Summarization truncation at `maxTokens`, which hidden reasoning tokens can consume, follows the same rule.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

This Dev Note is working context for maintainers and is explicitly non-authoritative; shipped behavior lives in the sections above, the package code, and the linked Agent Notes.

- **Default ratios, undecided** — `thresholdRatio: 0.8` and `retainRatio: 0.16` are fixed defaults; per-model tuning via `modelPolicies` exists, but no corpus-backed guidance on ideal values is recorded.
- **Tokenizer-accurate measurement, deferred** — the token meter's four-characters-per-token heuristic underprices CJK text and JSON Schema documents; exact tokenization remains an open direction for the measurement service.
- **Overflow recovery beyond canonical errors, undecided** — recovery triggers on `CONTEXT_WINDOW_EXCEEDED` only; other provider-side context failures are not classified.

</details>
