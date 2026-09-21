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

From the repository root, build the local bundle and launch it over the prepared `tui` profile. The profile must include `dsh-base`; the supplied patch disables the headless runner and mounts the standard preset and the Harness local file-reference provider.

```sh

./tui/scripts/build.sh

node apps/cli/lib/bin.js --profile tui --patch ./tui/packages/app/cordis.built.patch.yml

```

Enter submits; while working it steers the next step. Escape closes an open completion menu, then cancels the displayed question, active command, or running Agent. Press Ctrl-C twice to quit. Paste inserts text without submitting. `/login` lists sign-in targets; `/login <target>` opens a masked prompt or authorization flow. `/help` lists the composed commands. `/model` opens the model picker while idle; `/model <provider/model> [effort]` selects a route directly.

Left/Right moves the cursor by one visible character. Home/End or Ctrl-A/E moves to the current line’s start/end. Backspace deletes before the cursor; Delete deletes after it. Shift-Enter inserts a newline, and Enter submits the entire draft from any cursor position. Paste inserts at the cursor and preserves line breaks.

Up/Down recalls older/newer human input when completion is closed; Ctrl-P/N recalls it even with completion open. Recall includes committed user messages and pending human input from the current session. Browsing past the newest entry restores the unsent draft and its cursor. Editing a recalled entry leaves the Session log unchanged, and recall never submits automatically. Questions and login prompts have cursor editing but no history recall; their answers stay out of composer history.

In the model picker, type to filter and use Up/Down and Enter to choose. Models with reasoning controls open a second picker containing the provider’s advertised efforts and a provider-default option. The selection changes only after the final choice; Escape cancels either stage. A turn starting during selection cancels the change. Partial catalog failures remain visible, and exact routes remain valid even when absent from advisory catalogs.

Type `/` to discover the current session’s commands and user-invocable skills. Type a prefix to filter, use Up/Down to choose, and press Tab to insert the name; Enter submits the draft. Registered commands take precedence over skills with the same name. Other slash input is sent unchanged to the Harness, which loads recognized skill instructions at its pre-step boundary and records them in the Session.

Type `@` at the start of a word to discover workspace paths. Up/Down selects; Tab replaces the token at the cursor with a file reference or descends into a directory, preserving subsequent text. Paths with spaces use `@"path with spaces"`; directory completion leaves the cursor inside the closing quote. Enter submits the exact draft. Discovery is bounded by the session workspace and the Harness provider’s configured exclusions and limits. Missing providers and search failures appear in the menu without preventing manual input.

Use `/attach notes with spaces.bin` to stage a file for the next ordinary prompt. The entire trimmed remainder is one literal path, relative to the session workspace; do not add shell quotes. `/remove-attachment <number>` removes the displayed item, and `/clear-attachments` discards all staged items. Enter sends text and attachments together, including an attachment-only prompt. PNG, JPEG, WebP, and GIF extensions select image admission; the Harness validates their bytes and normalizes images. Other files retain their exact bytes.

Attachment submission holds the composer until admission finishes. Escape cancels it; failures and cancellation preserve the text, cursor, and staged sources for retry. Submission during `/attach` reads is refused until the read settles. Sources clear after the Agent accepts the message into its inbox. Pending input and resumed transcripts show attachment metadata; recalling text does not restage attachments. Send or clear staged items before `/sessions` or another registered command that consumes input; `/model`, `/login`, `/help`, and pending-input controls remain available. Unsaved sources disappear on exit.

Agent interruption retains queued input. `/clear-pending` discards queued human messages from both the next-step and next-turn inboxes; plugin context remains queued. The pending panel displays this action. Context usage shows the Harness’s projected occupancy as an estimate prefixed with `~`, including output growth and compaction.

Use `--resume <id>` from the session’s recorded workspace. The stored preset remains authoritative; a conflicting `--preset` is rejected. A legacy session with no recorded preset requires an explicit `--preset`. Unknown ids never create replacement sessions. Fresh empty sessions follow the harness persistence policy and need not survive exit. Resume restores the model and explicit effort from the last recorded request; provider-default effort remains implicit.

Use `/sessions` to choose a saved session from the current workspace or create a new session. Type to filter by title or id, use Up/Down and Enter to choose, and press Escape to cancel. The list excludes subagents and sessions with another live Agent owner. Navigation requires an idle Agent and an empty inbox, including plugin context; finish queued work or use `/clear-pending` for human input. Activity or new queued input cancels an open picker. Failed or canceled preparation keeps the current session displayed.

A successful switch resets the composer and history recall, prints a labeled transcript section, and drains the previous Agent handle. Earlier output remains in terminal scrollback. Resume restores the recorded preset and last requested model configuration; new sessions use profile defaults. The picker does not change workspaces or supply a preset for legacy sessions; use `--resume <id> --preset <name>` for those sessions. Session listing and title reads use Harness services; unavailable titles remain selectable by id.

The runner’s [configuration table](../../DESIGN.md#8-configuration) includes locale, quit timing, and credential references. [The keyless PTY check](../../scripts/pty-smoke.py) creates its own temporary profile and workspace; it does not require changes to a user profile.

<a id="understand-the-implementation"></a>

## Understand the implementation

<details>

<summary>Session and terminal ownership</summary>

`session.ts` owns registry setup and recorded composition. `controller.ts` joins query history with live events and dispatches commands. `catalog.ts` observes session-scoped command and skill metadata, cancels superseded reads, and stops discovery during teardown. `references.ts` delegates path search to `agent.ctx.fileReferences.list`, aborts superseded queries, and drains outstanding reads at shutdown. `model.ts` reads provider catalogs and validates exact routes and efforts before the controller updates the Harness selection reference. `attachments.ts` stages bounded reads through the scoped filesystem, delegates image batches and file storage to Harness, and drains uncancellable provider calls before teardown. `interactions.ts` queues scoped requests and settles them on abort or disposal. `navigation.ts` finishes the `/sessions` command before preparing a replacement, rechecks the old Agent’s status and inbox, and retires its handle only after replacement replay succeeds. `runner.ts` releases Ink synchronously before draining navigation and disposing handles. The launcher’s fatal-release hook disposes the same Cordis effect.

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

### Dev Note

None.
