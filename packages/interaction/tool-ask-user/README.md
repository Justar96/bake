---
description: "The model-facing ask_user_question tool over the user-questions seam, for users and maintainers composing or debugging interactive agent surfaces."
kind: "package-reference"
---

# @deepseek-ai/dsh-tool-ask-user

English | [中文](README.zh.md)

## Summary

`ask_user_question` lets a model pause work and ask the human for confirmation, a choice, or missing information. It accepts one or more questions and returns their answers as compact JSON. The call waits until an answer is accepted or the turn is cancelled; if no answer handler accepts it, the model receives an error. A live child agent owned by another agent cannot call this tool and must report unresolved questions in its final result. The package does not render or collect input, so callers must provide a compatible user interaction surface.

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

Compose this plugin wherever the model should be able to pause for a human decision: it provides the `ask_user_question` tool and needs the `ctx.userQuestions` seam with an answerer that accepts the scoped request. Without one, the tool call fails with an error instead of degrading.

### When to call the tool

The model calls `ask_user_question` when it needs confirmation, a choice, or missing information before proceeding. Send one or more questions, each with a stable `id` that is echoed in the answer; a recommended option goes first with `(Recommended)` appended to its label. The schema asks for brevity: a question is one short sentence ending with a question mark, a header is about 12 characters at most, an option label is 1-5 words, and an option description is one short sentence. This is guidance only; longer text is accepted and shown as sent.

```json
{
  "questions": [
    {
      "id": "cleanup",
      "question": "Proceed with the destructive cleanup?",
      "header": "Confirm",
      "options": [
        { "label": "Yes, delete them (Recommended)", "description": "Removes the three stale files." },
        { "label": "No, keep them", "description": "Aborts the cleanup." }
      ]
    }
  ]
}
```

### What the model gets back

The tool returns one answer object per question: `selected` holds the chosen option labels, and `custom` carries a free-form answer — supplementing `selected` for a multi-select question and overriding it for a single-select question. The Native renderer preserves the compact JSON text shape.

```json
{ "answers": [{ "id": "cleanup", "selected": ["Yes, delete them (Recommended)"] }] }
```

### How a UI shows the call

The tool declares its own presentation, so a UI such as the terminal transcript draws the call compactly instead of dumping its arguments. The pending call is titled by the question headers, or by a question's own words when it has none: `Ask: Choose scope`, or `Ask 2 questions: Choose scope, Runtime fixes`. The completed call shows each answer on its own line, the chosen labels and then any free-form answer in quotes, `scope → Tooling swaps, Shared versions, "skip CI for now"`, with `—` for a question left unanswered. The question card the user answers comes from the interaction surface itself. A failed call, and a result that is not the canonical answers JSON, keep the UI's generic rendering of the raw result.

### When the call fails

The tool call blocks until the human answers and cancels only through the turn's signal. No accepting answerer, an aborted call, or a caller that is not the exact live runtime root each settles as an error the model sees in the tool result — most notably, a live child agent owned by another agent is rejected (`DELEGATED_CALLER`) and must include the unresolved question or decision in its final result.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

The observable behavior is covered in [Use this package](#use-this-package); this section explains the tool definition and its relationship to the seam.

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | Tool registration: `ask_user_question` schema, execute path, result render |
| [`src/presentation.ts`](src/presentation.ts) | Pure `presentCall` / `presentResult` views: the question-header title and one answer line per question |
| — | No runtime invariant companion is published; this model-facing adapter has no independent lifecycle stream; execution relations are owned by the capability seam it calls. |

### Consumer role

The plugin registers one `defineTool` entry on `ctx.tools` with injects `['tools', 'userQuestions']`. `execute` maps model arguments into an `AskUserQuestionRequest`, forwards the exact calling agent and the turn's signal, and maps the accepted answer back into the canonical `answers` array. The seam owns identity checks, intent validation, waterfall dispatch, and the error taxonomy; this package only translates.

### Result rendering

The `render` output projects the structured value to a single text block via `JSON.stringify`, which is why the model-facing result is compact JSON rather than a richer content-block vocabulary.

### UI presentation

`presentCall` and `presentResult` live in `src/presentation.ts` and are pure. `defineTool` validates logged arguments softly before calling them, so obsolete arguments fall back to generic rendering. `presentResult` reads the answers back from the model-facing JSON text rather than from `presentationMeta`, which leaves the result text and the persisted `tool/result` event unchanged; any text it cannot narrow to the canonical shape, and every failure, returns no view. Titles are cut at 80 characters, a question standing in for a missing header at 48, and a free-form answer at 160, counted in code points; a UI measures display width itself.

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read these pages when the package-level contract is not enough. They move from the tool surface to the seam contract and its answerer waterfall.

- [User interaction subsystem reference](../../../docs/subsystems/user-questions.md) — the service contract, question vocabulary, and answerer waterfall behind this tool.
- [Tool catalog](../../../docs/tool-catalog.md#deepseek-aidsh-tool-ask-user) — the generated `ask_user_question` schema.
- [user-questions package](../user-questions/README.md) — the seam this tool consumes.
- [Interaction group map](../README.md) — adjacent approval and command surfaces.

-----

<a id="model-experience"></a>
## Model Experience

### Tool schema

#### What the model sees

The model sees the generated [`ask_user_question` schema](../../../docs/tool-catalog.md#deepseek-aidsh-tool-ask-user), including question ids, prompts, headings, options, and multi-select flags. The field descriptions ask for a one-sentence question ending with a question mark, a header of about 12 characters, 1-5 word option labels, and one-sentence option descriptions. The text is static, so it does not vary between requests.

#### Token effect

Fixed schema cost on every request where the tool is visible.

#### KV Cache effect

Prefix-stable while the definition and visibility are unchanged. Plugin lifecycle or scoped restrictions may invalidate reuse from this schema.

### Tool-call history and result

#### What the model sees

The model's full questions remain in the assistant tool-call arguments. After the human answers, the next step sees compact JSON in the exact shape `{"answers":[{"id":"<id>","selected":["<label>"],"custom":"<text>"}]}`; `custom` is omitted when unused and `selected` can contain zero, one, or several labels. UI interaction while the call is pending is not model context.

#### Token effect

Arguments and answer JSON are data-dependent retained tokens; there is no token cost while waiting for the human.

#### KV Cache effect

Append-only; newly visible content follows the reusable request prefix and does not invalidate existing KV-cache entries.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>


These limits define when the tool is a poor fit. They are current package constraints, not a UI backlog.

- **A pending question blocks the tool call until the human answers** — the tool declares no `timeout-policy` budget; cancellation rides the turn's `exec.signal` only.
- **Runtime-owned subagents cannot ask the user** — `ask_user_question` rejects a live child owned by another agent with `DELEGATED_CALLER`; the child must include the unresolved question or decision in its final result. Durable lineage does not decide this boundary, so a lineage-bearing session resumed as a runtime root may ask normally.
- **Native answers render as JSON text** — the canonical value remains structured, but the model-facing result uses compact JSON rather than a richer content-block vocabulary. Only a UI's presentation reformats it, one answer per line.
- **Brevity is guidance, not validation** — the schema describes short questions, headers, and labels, but the tool rejects none for length, so a long question still reaches the user whole.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
