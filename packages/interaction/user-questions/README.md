---
description: "Waterfall-based question and answer service for tools, permission plugins, local answerers, and Agent-scoped Web interactions."
kind: "package-reference"
---

# bake-user-questions

## Summary

User-interaction Service Definition. It owns `ctx.userQuestions`, the service a model-facing tool or permission plugin uses when it needs to pause work and ask the human for a decision. Use it when a consumer must suspend an operation until the user answers.

## Table of Contents

- [Service: `UserQuestionService` (ctx key: `userQuestions`)](#service-userquestionservice-ctx-key-userquestions)
- [Role](#role)
- [Model Experience](#model-experience)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="service-userquestionservice-ctx-key-userquestions"></a>
## Service: `UserQuestionService` (ctx key: `userQuestions`)

### Public API

- `ctx.userQuestions.ask(request): Promise<AskUserQuestionAnswer>` Dispatch the answerer waterfall and wait for the first accepted answer.

### Key Types

- `AskUserQuestionRequest` — `{ questions: [{ id, question, detail?, header?, options?, multiSelect? }], agent?, signal? }`; `detail` supplies supporting text that providers render with the question without turning it into an option label. When present, `agent` must be the registry's exact live runtime root.
- `AskUserQuestionOption` — `{ label, description? }`.
- `AskUserQuestionAnswer` — `{ answers: [{ id, selected, custom? }] }`.
- `UserQuestionError` — `HarnessError` subclass with codes such as `EMPTY_QUESTIONS`, `NO_PROVIDER`, `ASK_ABORTED`, `CALLER_NOT_LIVE`, and `DELEGATED_CALLER`.

For a single-select question, `custom` overrides the selected choice and `selected` is empty. For a multi-select question, `custom` may supplement the labels in `selected`. A UI may preserve a skipped item as `{ id, selected: [] }`, keeping the existing answer shape while retaining other answers in the batch.

When a request carries an agent, `ask()` authenticates its exact identity through the live `AgentRegistry` and admits only a runtime root. Durable lineage is not authority: a session with historical delegation depth may ask after it is resumed as a new runtime root, while a live child owned by another agent is rejected even if its durable depth is zero. The Web answerer receives only Agent-scoped requests; an agentless programmatic request remains available to unscoped local waterfall listeners and fails with `NO_PROVIDER` when none accepts it.

<a id="role"></a>
## Role

This is the Service Definition package. Consumers such as `bake-tool-ask-user` depend on this service; an interactive UI such as the terminal app contributes the answerer. The loop stays unchanged: a tool call awaits the waterfall result, and that result resumes the normal agent loop.

<a id="model-experience"></a>
## Model Experience

Indirectly, through `bake-tool-ask-user`, which retains a successful answer as compact JSON or one of these failures: `Error: ask_user_question was aborted before the user answered`, `Error: ask_user_question requires at least one question`, `Error: human interaction requires the exact live calling agent when an agent is supplied`, `Error: human interaction is unavailable while the calling agent is owned by another live agent; include the unresolved question or decision in the child agent's final result`, `Error: no user-questions answerer accepted the request`, or `Error: <message>`. Waiting for the human adds no tokens.

#### KV Cache effect

No direct invalidation; the named consumer owns any request-prefix changes.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>

- **Agent-scoped Web answering** — Remote Events route the shipped Web answerer only when the request carries a live Agent scope; agentless callers need an unscoped local waterfall listener.
- **The vocabulary is the question-form shape only** — selectable options plus optional custom text; richer interaction shapes (file pickers, diff-preview confirmations) have no seam vocabulary yet.


<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>

**Runtime invariant:** No companion is published. The answerer waterfall is resolved per request and returns directly to its caller; the seam publishes no independent request/answer audit stream.
