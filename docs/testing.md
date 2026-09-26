# Testing policy

English | [中文](testing.zh.md)

Bake tests the shared runtime, terminal application, and built profile at the layer where each behavior is observable. The root [AGENTS.md](../AGENTS.md) owns the current commands.

## Choose a check

- Run focused runtime specs with `bun run test:runtime <file>`. Cordis and Ink tests execute on Node; pure modules and tooling tests execute on Bun.
- Run `bun run test` for the shared pure and Node integration suites.
- Run `bun apps/tui/scripts/tui.ts check docs` for terminal documentation and `bun apps/tui/scripts/tui.ts check types` for TUI types.
- Run `bun run test:e2e` for keyless built-profile PTY scenarios when terminal behavior changes.
- Run `bun run verify` when the complete build, check, and PTY path is required.

Report checks that actually ran, including failures and skipped work. Do not bypass hooks without explicit approval.

## Test real behavior

Product-visible plugins need a non-unit composition test that boots test-only `cordis.yml` through the Loader and app or process. Mock external nondeterminism such as the model, network, and clock; keep the runtime under test real. Assert model-visible output, durable state, or user-visible behavior through the public entry path. An end-to-end assertion re-reads a file or re-runs a command instead of trusting an agent's self-report.

Tests own temporary paths, ports, process-global changes, and child processes. Teardown restores shared state and awaits owned work. A spec that passes only by itself is a defect in the spec.

## Preserve session evidence

Model-visible input must be reconstructable from the session log. Never overwrite, move, or delete a committed session generation to satisfy a test. [Session format status](session-format-status.md) owns released format and migration rules.
