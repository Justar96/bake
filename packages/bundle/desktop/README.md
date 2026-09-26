---
description: "Long-lived bridge that lets the Bake Desktop app drive one dsh Agent: streamed output, forwarded approvals, and three permission tiers, for desktop integrators."
kind: "package-bundle"
---

# @deepseek-ai/dsh-desktop

English | [中文](README.zh.md)

## Summary

`dsh-desktop` is the bundle the Bake Desktop app launches: `dsh --profile desktop` keeps one process alive for a workspace, drives one root Agent, and exchanges protocol messages with the desktop. It streams assistant text, reports tool calls and token usage, forwards every approval question to the desktop's approval card, and applies the desktop's permission tier (read-only, normal, or full access). It also reports harness spans, so a desktop trace follows one turn from the user's click through the model request and each tool call. The boundary: one root Agent per process, driven only by the desktop protocol.

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

The desktop app starts the process with the workspace as its working directory and speaks the protocol defined in [`src/protocol.ts`](src/protocol.ts). You rarely run it by hand; to try it, pipe protocol lines into the profile.

### Starting the bridge

```sh
cd /path/to/workspace
printf '%s\n' \
  '{"v":1,"type":"init","workspace":"'"$PWD"'","permission":"normal"}' \
  '{"v":1,"type":"user.message","text":"list the files here"}' \
  | dsh --profile desktop
```

Under Electron the desktop launches the same entry in a utility process, and every message travels as one string on the process's message port. Without that port the bridge reads newline-delimited JSON on stdin and writes it on stdout, which is how tests and benchmarks drive it. It announces `ready` once the composition is settled, answers `init` with `initialized`, and exits when stdin closes or a `shutdown` arrives.

### Permission tiers

| Tier | Preset | What asks | What is refused |
|---|---|---|---|
| `read-only` | `read-only` | Nothing | File writes, sandbox escalation, and tools outside the read, sub-agent, and shell sets |
| `normal` | `workspace-write` | Shell commands and tools outside the read, write, and sub-agent sets | Nothing beyond the sandbox |
| `full-access` | `danger-full-access` | Nothing | Nothing |

The bridge pins the tier's preset on the Session through `permissionPresets.set`, and `permission.set` changes it for later calls. In `read-only`, shell commands still run, confined to a read-only filesystem by the sandbox. In `normal`, reads, workspace writes, and sub-agent tools run without asking.

### Approvals

Every approval question for the root Agent becomes an `approval.request` carrying a one-line summary (the command, for shell tools) and, when one applies, a session grant key: `shell:<program>` for a simple command, `tool:<name>` for other tools. Compound commands (pipes, lists, substitutions, redirections) and sandbox escalations get no grant key. The desktop answers `approve_once`, `approve_session`, or `deny`; the bridge maps both approvals to `allowed-once`, because the desktop keeps session grants and answers later matching requests itself. A request whose call is cancelled first is withdrawn with `approval.withdrawn`.

### When to use it

Use this profile when an application owns the conversation and the user interface: it needs streamed text, approval questions it can show, and a permission tier it can switch. Use `dsh-headless` for one task from a script, and the terminal profile for interactive use.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

### Run flow

The bridge opens its carrier at mount, waits for the Loader to settle, and sends `ready`. `init` checks that the workspace equals the process's working directory and records the tier. The first `user.message` creates the root Agent (or resumes `resume_session_id`), pins the tier's preset, and reports `session.started`; each message is a `followup`. Assistant deltas are coalesced for 16 ms per message before they are sent.

### Spans

`bake.turn` is parented on the `traceparent` of the user message that started the turn. `bake.step`, `llm.request` (with `ttft_ms`), `tool.pipeline`, `tool.execute` (the body alone, measured by wrapping `tools/execute`), and `approval.wait` nest inside it. Spans are uploaded every second and before each `turn.done`.

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | The bridge plugin: inbound handling, Agent lifecycle, projection, approvals, spans |
| [`src/permission.ts`](src/permission.ts) | Tier presets and the per-call classifier |
| [`src/transport.ts`](src/transport.ts) | Message-port and stdio carriers and inbound envelope checks |
| [`src/spans.ts`](src/spans.ts) | The span recorder and `traceparent` parsing |
| [`src/protocol.ts`](src/protocol.ts) | The protocol, vendored byte-identical from the desktop repository |

### Invariant ownership

No runtime invariant companion is published; the bridge owns no durable Session event of its own, and its outbound projection and approval forwarding are exercised through the real-composition spec.

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

- [`@deepseek-ai/dsh-headless`](../headless/README.md) — the one-shot runner this bridge is modelled on.
- [`@deepseek-ai/dsh-permission-presets`](../../interaction/permission-presets/README.md) — the presets each tier selects.
- [`@deepseek-ai/dsh-user-approval`](../../interaction/user-approval/README.md) — the approval service whose questions the bridge answers.

-----

<a id="model-experience"></a>
## Model Experience

### Persona prefix

#### What the model sees

The bundle replaces the base persona prefix with the text below, where `{{model}}` is the selected model id.

##### Verbatim persona prefix

```markdown
You are a coding agent powered by the {{model}} model, working for a user in the Bake Desktop app. Some tool calls wait for the user's approval before they run.
```

#### Token effect

A fixed sentence of about 35 tokens in the system prompt, in place of the base prefix.

#### KV Cache effect

Prefix-stable: the text changes only when the selected model id changes.

### Tier refusals

#### What the model sees

In `read-only`, a refused call's tool result carries the reason `<tool> is not available in read-only mode`.

#### Token effect

One short result per refused call.

#### KV Cache effect

Append-only: the result joins the history like any other tool result.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>

- **One root Agent per process** — the desktop starts one process per workspace session; a second conversation needs a second process.
- **Sub-agents cannot ask** — children run with the approval policy `never`, so in `normal` a child's shell command is refused instead of reaching the desktop.
- **Children are not projected** — only the root Agent's stream, tool calls, and approvals reach the desktop.
- **No user-question answerer** — `exit_plan_mode` and `ask_user_question` fail with no provider, because the bridge does not yet forward `user-questions/request`.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

`src/protocol.ts` is copied from the desktop repository's `src/shared/protocol.ts` and must stay byte-identical; the desktop's tests compare the two files when both checkouts are present.

</details>
