# Agent Note: On-demand tool details

Status: implemented

## Problem

Every request resends the native schema of every visible tool. In the standard preset's first request, tool schemas were about 20 KB of the 21 KB body, and the prompt and first message were under 1 KB. Prompt caching hides that cost only on some routes. Anthropic served 96% to 98% of input from cache and OpenAI about 65%, while Gemini through the shipped gateway cached almost none of it. The largest schema was `workflow`, at 3.7 KB, and most of that was the script API reference. No recorded session in the 75-session audit called `workflow`, so every request paid for a reference that almost none used.

## Decision

`ToolDefinition` gains an optional `details` string: a usage reference kept out of the native schema. While a visible tool declares `details`, the registry adds its reserved `tool_help` tool (`{ name }`), which returns the details of a tool visible to the calling scope. The tool's description tells the model to call `tool_help` before first use. Under `ptc` mode the SDK is already prompt text, so a binding's details join its documentation and `tool_help` is not a binding.

`workflow` moves its script API reference into `details`, and its description keeps the explicit-request policy plus the instruction to read that reference first. Its schema drops from 3,757 to 2,041 bytes, and `tool_help` adds 290.

`tool_help` is reserved like `run_code`. Registration and `restrict()` reject the name, and the reader follows the tools it serves: a scope whose filter hides every tool with details also loses the reader.

## Why the tool list stays static

The reader's presence depends only on which tools are visible, so it changes exactly when the tool set does. Loading a reference adds one tool result to history and changes no schema, so the cached prefix survives. A deferred tool that joins the request only after the model asks for it would change the tools block mid-session. Tools come first in the Anthropic and OpenAI cache order, so that invalidates the whole cached prefix, and routes without a tool-update channel start a new request series.

## Alternatives considered

**A skill that carries the reference.** The skill catalog and the `skill` tool are resent too, and a skill is discovered by task, not tied to one tool. Hiding the tool also leaves a skill behind that describes an API the agent cannot call.

**An empty `workflow` script returns the guide.** This works for one tool only, and it makes a failed or trivial call carry a different meaning.

**Deferred schemas: send a name and a one-line summary, and load the schema on request.** This is the larger saving, but it changes the tool list mid-session, which breaks the cache as described above.

**Shorter descriptions only.** The in-place trims to `list_agents`, `interrupt_agent`, and `subagent_fork` save about 580 bytes. `workflow`'s reference cannot shrink that way without losing the contract.

## Consequences

The standard preset's native tool schemas shrink by about 2.4 KB per request, including plan mode's removal (`exit_plan_mode`, 501 bytes). A model that writes a workflow script spends one extra tool call to read the reference, and a model that skips `tool_help` writes the script from the parameter descriptions alone. `workflow` then fails with the engine's own errors, which name the hook or option at fault. A new tool with a long reference should use `details` instead of a longer description.

## Verification

`packages/core/tools/tests/tool-help.spec.ts` covers native schemas without details, reader visibility, execution, scoped and restricted views, reserved names, and the PTC mode SDK. `packages/workflow/tool-workflow/tests/tool-workflow.spec.ts` pins the shortened description and the details. `packages/core/tools/tests/gen-tool-catalog.spec.ts` shows `tool_help` once, under the registry.

A paired live evaluation compared the previous build with this one on three models, over seven edit tasks and one workflow task, three trials each. The first request shrank by 2,477 bytes. On the edit tasks, total tokens fell by 10.3% for Sonnet, 13.8% for Gemini, and 11.0% for GPT, with request counts unchanged. On the workflow task, every model called `tool_help` before its first script, in 9 of 9 runs. That task cost 28% more tokens for Sonnet, because of the extra request and the reference carried after it. Gemini and GPT cost more too, but with wide intervals over two matched pairs. The trade favours details while few sessions write workflow scripts.
