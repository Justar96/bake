---
description: "Interactive terminal profile with durable session resume, scoped human decisions, and harness commands."
kind: "package-bundle"
---

# @dsh-tui/app

English | [中文](README.zh.md)

## Summary

This private workspace bundle adds a terminal runner and flag provider to a base-backed `dsh` profile. It records session composition, resumes exact persisted sessions, and routes human input through harness services. The runtime is Node; Ink owns terminal modes. The bundle is developed inside this checkout and is not published independently.

## Table of Contents

- [Use this package](#use-this-package)

- [Understand the implementation](#understand-the-implementation)

- [Model Experience](#model-experience)

- [Known Limitations](#known-limitations)

<a id="use-this-package"></a>

## Use this package

Build from the repository root, then start Bake. The shipped `tui` profile initializes with `dsh-base` and this bundle, including the standard preset and local file-reference provider. Bun emits production JSX; `bun run start` runs existing artifacts with the matching Node React/Ink runtime. Bake uses `~/.bake` unless `DSH_HOME` is set. TTY input and output are required; interactive rendering remains enabled when `CI` is set.

```sh

bun run build

bun run start

```

Enter submits; while working it steers the next step. Shift-Tab steps the selected model's reasoning effort to the next the route offers, wrapping to the provider default, and shows the choice as a notice and in the status line's `Think` badge; a change during a turn takes effect from its next step, and a route without efforts says so. Escape closes an open completion menu, then cancels the displayed question, active command, or running Agent. Press Ctrl-C twice to quit: the first shows `Press Ctrl-C again to quit` for `doubleInterruptMs` (2 seconds by default), and the prompt goes when that window ends or when any other key is pressed. Paste inserts text without submitting. `/login` opens a selectable sign-in target picker, with each target's `Configured` or `Not set` state in its own column; `/login <target>` starts that target directly. Choose CLIProxyAPI, or run `/login cliproxyapi`, to enter its URL and API key in two steps. The saved URL is offered when reconfiguring. Bake validates `GET /v1/models?client_version=pi`, stores the key in its credential store, and adds the returned models under `cliproxyapi/` to `/model`. The URL may be a server root, `/v1`, or `/backend-api` address; inference uses that server’s OpenAI-compatible `/v1/responses` endpoint. Repeat the login to refresh models or change the connection. An empty model catalog leaves the existing route unchanged. `/help` lists only the composed commands. `/changelog` writes the running root package version’s section of the root `CHANGELOG.md` to the transcript. `/model` opens the model picker while idle; `/model <provider/model> [effort]` selects a route directly.

The runner supplies the root package version through optional `AppProps.version`. On mounting with an empty committed transcript outside child inspection, the UI prints a `BAKE v<root version>` welcome block once through `Static`, carrying the session line the heading would otherwise print. Resumed history has no welcome banner.

New sessions default to `workspace-write` with approval policy `ask`; explicit `DSH_PERMISSION_MODE` or permission-default settings take precedence. The status line shows `Access <mode>` from the session’s `permissions` projection. `/permission` lists the modes, and `/permission read-only`, `/permission workspace-write`, or `/permission danger-full-access` changes the current session immediately. Resume preserves its recorded mode; `/new` uses the configured default. Child inspection shows the child’s own permission mode. Profiles without that projection omit the indicator. The `Think` indicator reports the selected model’s explicit reasoning effort, or its advertised default. Models with no reasoning controls show no level; switching `/model` updates it, and resume restores the selected level.

Left/Right moves the cursor by one visible character. Home/End or Ctrl-A/E moves to the current line’s start/end. Backspace deletes before the cursor; Delete deletes after it. Shift-Enter inserts a newline, and Enter submits the entire draft from any cursor position. Paste inserts at the cursor and preserves line breaks.

Up/Down recalls older/newer human input when completion is closed; Ctrl-P/N recalls it even with completion open. Recall includes committed user messages and pending human input from the current session. Browsing past the newest entry restores the unsent draft and its cursor. Editing a recalled entry leaves the Session log unchanged, and recall never submits automatically. Questions and login prompts have cursor editing but no history recall; their answers stay out of composer history.

For a question, use Up/Down to focus an option or Other answer. Enter chooses the focused option; for a question allowing several answers, Space toggles options and Enter submits them. Typing or pasting starts an Other answer, which stays visible while you browse options. For several answers, Other text can be submitted together with selected options. Escape dismisses the question.

In the model picker, type any words of a route to filter (`v4 flash`) and use Up/Down and Enter to choose; a catalog name that only restates the route is not repeated beside it. Models with reasoning controls open a second picker containing the provider’s advertised efforts and a provider-default option. The selection changes only after the final choice; Escape cancels either stage. A turn starting during selection cancels the change. Partial catalog failures remain visible, and exact routes remain valid even when absent from advisory catalogs.

Type `/` to discover the current session’s commands and user-invocable skills. The menu shows `/model`, `/resume`, `/new`, and `/clear` first when available. Type a prefix to filter and use Up/Down to choose. Enter runs the selected command; Tab inserts its name for editing. Enter inserts a selected skill name, or submits it when its full name is already typed. Registered commands take precedence over skills with the same name. Other slash input is sent unchanged to the Harness, which loads recognized skill instructions at its pre-step boundary and records them in the Session.

`/compact` summarizes older completed history when a useful range exists. While it runs, the TUI shows preparation, summarization, then saving with elapsed time when space allows; Escape cancels the attempt. Enter cannot queue another prompt during compaction and keeps the draft for later submission. The command result reports the number of history items and estimated tokens folded, or says when no history is compactable. The transcript marks the summary without displaying its model-facing text.

Use `/usage` to read remaining DeepSeek API credit. The result lists total, granted, and topped-up balances by currency and whether API calls are available. It reads the configured DeepSeek endpoint and credential; a custom gateway needs a compatible `/user/balance` endpoint. This is account balance, not token usage for the current conversation.

Type `@` at the start of a word to discover workspace paths. Up/Down selects; Tab replaces the token at the cursor with a file reference or descends into a directory, preserving subsequent text. Paths with spaces use `@"path with spaces"`; directory completion leaves the cursor inside the closing quote. Enter submits the exact draft. Discovery is bounded by the session workspace and the Harness provider’s configured exclusions and limits. Missing providers and search failures appear in the menu without preventing manual input.

Use `/attach notes with spaces.bin` to stage a file for the next ordinary prompt. The entire trimmed remainder is one literal path, relative to the session workspace; do not add shell quotes. `/remove-attachment <number>` removes the displayed item, and `/clear-attachments` discards all staged items. Enter sends text and attachments together, including an attachment-only prompt. PNG, JPEG, WebP, and GIF extensions select image admission; the Harness validates their bytes and normalizes images. Other files retain their exact bytes.

Attachment submission holds the composer until admission finishes. Escape cancels it; failures and cancellation preserve the text, cursor, and staged sources for retry. Submission during `/attach` reads is refused until the read settles. Sources clear after the Agent accepts the message into its inbox. Pending input and resumed transcripts show attachment metadata; recalling text does not restage attachments. Send or clear staged items before `/sessions` or another registered command that consumes input; `/model`, `/login`, `/help`, `/changelog`, and pending-input controls remain available. Unsaved sources disappear on exit.

Agent interruption retains queued input. `/clear-pending` discards queued human messages from both the next-step and next-turn inboxes; plugin context remains queued. The pending panel displays this action. Context usage shows the Harness’s projected occupancy as an estimate prefixed with `~`, including output growth and compaction. When `/model` changes routes, the status line withholds occupancy until that route reports usage; session-wide billed totals remain visible. A narrow terminal shows `ctx ~N%` when the full reading cannot fit beside the access boundary. Child inspection reads its own context and billed totals, whether the child is live or saved, without borrowing the parent’s. The branch panel shows this session’s delegated agents, including saved children and active remote runs. “Live” means the child session is resident; “Working” follows the child Agent or an active subagent run. Settled children also show their latest recorded outcome: “Completed”, “Failed”, or “Stopped”, alongside residency such as “Saved”. Only the child’s own turn boundaries count; a new turn clears the previous outcome, and missing or unreadable outcomes do not imply success. Press Ctrl+G or run `/agents`, filter by label or id, then use Up/Down and Enter to open a child’s read-only session. Its transcript updates while the parent continues working; Escape returns to the parent and preserves the unsent draft. Parent approvals return focus to the parent. Opening saved history does not resume the child. Remote runs without local transcripts and unreadable records explain why they cannot open. The status line names the model first, then shows the session's billed `in` and `out` tokens from the token meter's `tokenUsage` projection once a request has reported usage. Input counts uncached, cache-read, and cache-write tokens together. `cache hit N%` is the cache-read share of that input; it appears only when the provider reports cache traffic, as DeepSeek does through `prompt_cache_hit_tokens`.

In an install the installers made, the status line names a newer release as `update v<version> · bake update`, the last bounded field and the first a narrow line gives up. The answer comes from a check cached for a day under the Bake home, read before the first frame and refreshed in the background, which the runner awaits on exit; it never installs anything. A source checkout shows no notice, and `BAKE_NO_UPDATE_CHECK=1` turns the check off. See [Updating an install](../../../../distribution/README.md#updating-an-install).

When the composed profile has plan mode, the status line shows `Plan` while it is active. `Plan pending` or `Plan exit pending` means a requested change is waiting for the next accepted step. These labels read the Harness plan projection and update after resume or a `/plan` command.

Use `--resume <id>` from the session’s recorded workspace. The stored preset remains authoritative; a conflicting `--preset` is rejected. A legacy session with no recorded preset requires an explicit `--preset`. Unknown ids never create replacement sessions. Fresh empty sessions follow the harness persistence policy and need not survive exit. Resume restores the model and explicit effort from the last recorded request; provider-default effort remains implicit.

Use `/sessions` or its alias `/resume` to choose a saved session from the current workspace or create a new session. `/new` and `/clear` start a fresh session directly; earlier output remains in terminal scrollback. A green filled dot and a `Current` status mark the current session, dim open dots mark saved history newest first, and each row shows how long ago the session was created beside a short id when it has a title. An ocean blue plus marks the new-session action, pinned under the list so a long history never scrolls it away. The labels remain visible without color. Type to filter by title or id, use Up/Down and Enter to choose, and press Escape to cancel. The list excludes subagents and sessions with another live Agent owner. Navigation requires an idle Agent and an empty inbox, including plugin context; finish queued work or use `/clear-pending` for human input. Activity or new queued input cancels an open picker. Failed or canceled preparation keeps the current session displayed.

A successful switch resets the composer and history recall, prints a labeled transcript section, and drains the previous Agent handle. Earlier output remains in terminal scrollback. Resume restores the recorded preset and last requested model configuration; new sessions use profile defaults. The picker does not change workspaces or supply a preset for legacy sessions; use `--resume <id> --preset <name>` for those sessions. Session listing and title reads use Harness services; unavailable titles remain selectable by id.

The runner’s [configuration table](../../DESIGN.md#8-configuration) includes locale, quit timing, and credential references. [The keyless PTY check](../../scripts/pty-smoke.ts) creates its own temporary profile and workspace; it does not require changes to a user profile.

<a id="understand-the-implementation"></a>

## Understand the implementation

<details>

<summary>Session and terminal ownership</summary>

`session.ts` owns registry setup and recorded composition. `controller.ts` joins query history with live events and dispatches commands. `catalog.ts` observes session-scoped command and skill metadata, cancels superseded reads, and stops discovery during teardown. `references.ts` delegates path search to `agent.ctx.fileReferences.list`, aborts superseded queries, and drains outstanding reads at shutdown. `model.ts` reads provider catalogs and validates exact routes and efforts before the controller updates the Harness selection reference. `attachments.ts` stages bounded reads through the scoped filesystem, delegates image batches and file storage to Harness, and drains uncancellable provider calls before teardown. `interactions.ts` queues scoped requests and settles them on abort or disposal. `navigation.ts` finishes the `/sessions` or `/resume` command before preparing a replacement, rechecks the old Agent’s status and inbox, and retires its handle only after replacement replay succeeds. `runner.ts` releases Ink synchronously before draining navigation and disposing handles. `output.ts` strips SGR styling when `NO_COLOR` is non-empty, preserving terminal controls, and writes each of Ink's renders to the terminal as one write, drawing a frame that prints transcript rows over the previous frame instead of erasing it first, and starts the first frame and each resize replay on the terminal's bottom row; the runner flushes it synchronously when it releases the terminal. `syntax.ts` wraps Shiki as the synchronous `highlight` the presentation layer calls for response code, tool source, structured output, and diffs: common languages load before the first frame, any other loads its grammar the first time a file in it is drawn, and until then its lines stay plain; the runner closes it after draining navigation. The launcher’s fatal-release hook disposes the same Cordis effect.

No new persistence types or runtime invariants are declared: session storage, inbox state, and Agent lifetime remain owned by their harness services. See [wiring](../../DESIGN.md) and [dependency documentation](../../DEPENDENCIES.md).

</details>

<a id="model-experience"></a>

## Model Experience

Ordinary human input becomes a logged user message. `@` file selections remain literal path references; choosing one does not read or inject its contents. The composed Harness provider supplies path-reference guidance when the Agent has a `read` tool. Explicit attachments become durable image/file blocks. Harness projects generic files into readable file handles and prepares image inputs for the selected model. Commands own any model-visible effects through the harness registry. Model selection uses the Harness request-routing reference; the next request records its configuration and any model-switch notice. Approval and question answers return to their requesting services; login secrets never enter model input or Session history.

### KV Cache effect

The TUI does not assemble model requests or alter cache settings.

<a id="known-limitations"></a>

## Known Limitations and Deferred Work

- One displayed root session at a time; session navigation is confined to the current workspace. The picker reads all matching session records and titles, with a bounded number of visible rows.

- Clipboard images, attachment arguments to registered commands, and inline image previews are not supported. Image submission checks the current selected model; Harness owns final request admission.

- A model choice not yet used by a request is not persisted; selection does not change profile defaults.

- Tool execution qualification uses the built profile; source-launch module duplication is documented in [the runbook](../../PLAN.md#132-build-from-a-clean-checkout).

- Terminal scrollback keeps original output after compaction.

- Long-history resume has substantial transient memory cost; the [whole-process diagnostic](performance/README.md) records reproducible workloads, measurements, and the unresolved large-history failure.

### Dev Note

None.
