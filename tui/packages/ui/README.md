---
description: "Pure Ink presentation of committed session rows, live output, pending input, and human interactions."
kind: "package-library"
---

# @dsh-tui/ui

English | [中文](README.zh.md)

## Summary

This library renders terminal state supplied by `@dsh-tui/app`. `App` displays committed history, live response text, pending input, status, and one human request. Components use typed locale dictionaries and callbacks. They do not access Cordis, Node services, storage, or a clock.

## Table of Contents

- [Use this package](#use-this-package)

- [Understand the implementation](#understand-the-implementation)

- [Model Experience](#model-experience)

- [Known Limitations](#known-limitations)

<a id="use-this-package"></a>

## Use this package

Import `App` and `AppProps` from the package entry point. Supply authoritative state and callbacks as demonstrated by the [runner](../app/src/runner.ts) and [component tests](tests/shell.spec.tsx). `project` maps a committed Session event to zero or more rows; `formatRow` provides a plain-text representation. This is a library dependency, with no Cordis mount row.

Use `dictionaries.en` or `dictionaries.zh` for application labels. Model text, tool output, question details, and service diagnostics remain verbatim. Committed rows must remain immutable and append-only because Ink `Static` prints them once.

<a id="understand-the-implementation"></a>

## Understand the implementation

<details>

<summary>Input and presentation</summary>

Ink `usePaste` inserts text without actions; `useInput` handles typed keys. `useComposer` handles text and Enter arriving together, while grapheme deletion preserves Unicode characters. `InteractionView` renders complete questions and plan details, masks secrets, and submits exact option labels. Request ids let the application reject stale callbacks.

See [the wiring reference](../../DESIGN.md) for ownership and [current dependency APIs](../../DEPENDENCIES.md) for the Ink and React versions. No independent runtime invariant is installed because these components only render their supplied props.

</details>

<a id="model-experience"></a>

## Model Experience

None directly; the application owns callbacks that submit logged messages and service answers.

### KV Cache effect

None; presentation does not construct model requests.

<a id="known-limitations"></a>

## Known Limitations and Deferred Work

- Inline terminal output has no virtual scrolling or rich attachment viewer.

- The composer appends text and supports Backspace; cursor movement and history editing are not implemented.

### Dev Note

None.
