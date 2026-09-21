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

From the repository root, build the local bundle and launch it over the prepared `tui` profile. The profile must include `dsh-base`; the supplied patch disables the headless runner and mounts the standard preset.

```sh

./tui/scripts/build.sh

node apps/cli/lib/bin.js --profile tui --patch ./tui/packages/app/cordis.built.patch.yml

```

Enter submits; while working it steers the next step. Escape cancels the displayed question, active command, or running Agent. Press Ctrl-C twice to quit. Paste inserts text without submitting. `/login` lists sign-in targets; `/login <target>` opens a masked prompt or authorization flow. Other slash commands use the composed command registry.

Use `--resume <id>` from the session’s recorded workspace. The stored preset remains authoritative; a conflicting `--preset` is rejected. A legacy session with no recorded preset requires an explicit `--preset`. Unknown ids never create replacement sessions. Fresh empty sessions follow the harness persistence policy and need not survive exit.

The runner’s [configuration table](../../DESIGN.md#8-configuration) includes locale, quit timing, and credential references. [The keyless PTY check](../../scripts/pty-smoke.py) creates its own temporary profile and workspace; it does not require changes to a user profile.

<a id="understand-the-implementation"></a>

## Understand the implementation

<details>

<summary>Session and terminal ownership</summary>

`session.ts` owns registry setup and recorded composition. `controller.ts` joins query history with live events and dispatches commands. `interactions.ts` queues scoped requests and settles them on abort or disposal. `runner.ts` releases Ink synchronously before disposing the Agent handle and draining commands. The launcher’s fatal-release hook disposes the same Cordis effect.

No new persistence types or runtime invariants are declared: session storage, inbox state, and Agent lifetime remain owned by their harness services. See [wiring](../../DESIGN.md) and [dependency documentation](../../DEPENDENCIES.md).

</details>

<a id="model-experience"></a>

## Model Experience

Ordinary human input becomes a logged user message. Commands own any model-visible effects through the harness registry. Approval and question answers return to their requesting services; login secrets never enter model input or Session history.

### KV Cache effect

The TUI does not assemble model requests or alter cache settings.

<a id="known-limitations"></a>

## Known Limitations and Deferred Work

- One root session per process; no interactive session or model picker.

- Tool execution qualification uses the built profile; source-launch module duplication is documented in [the runbook](../../PLAN.md#132-build-from-a-clean-checkout).

- Terminal scrollback keeps original output after compaction.

### Dev Note

None.
