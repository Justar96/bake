# Testing policy

Bake tests the shared runtime, terminal application, and built profile at the layer where each behavior is observable. The root [AGENTS.md](../AGENTS.md) owns the current commands.

## Choose a check

- Run focused runtime specs with `bun run test:runtime <file>`. Cordis and Ink tests execute on Node; pure modules and tooling tests execute on Bun.
- Run `bun run test` for the shared pure and Node integration suites.
- Run `bun apps/tui/scripts/tui.ts check docs` for terminal documentation and `bun apps/tui/scripts/tui.ts check types` for TUI types.
- Run `bun run test:e2e` for keyless built-profile PTY scenarios when terminal behavior changes.
- Run `bun run test:integration` for the `*.e2e.ts` suites when a change reaches a profile composition, a sandbox backend, or built output. They run keyless unless `DSH_E2E_LIVE=1`.
- Run `bun run preflight` before a pull request: every CI gate, with the runtime specs the change reaches. `bun run verify` runs the same gates with the whole runtime suite.

Report checks that actually ran, including failures and skipped work. Do not bypass hooks without explicit approval.

## Test real behavior

Product-visible plugins need a non-unit composition test that boots test-only `cordis.yml` through the Loader and app or process. Mock external nondeterminism such as the model, network, and clock; keep the runtime under test real. Assert model-visible output, durable state, or user-visible behavior through the public entry path. An end-to-end assertion re-reads a file or re-runs a command instead of trusting an agent's self-report.

Tests own temporary paths, ports, process-global changes, and child processes. Teardown restores shared state and awaits owned work. A spec that passes only by itself is a defect in the spec.

## Pin the model surface

Two expected-output suites pin what each shipped composition sends on its first model request: the system prompt, every context message (workspace instructions, runtime context, skill catalog), and every tool with its exact description and JSON schema, in request order. Each snapshot is a Markdown file that starts with its measured sizes: system-prompt characters, tool JSON characters, context-message characters, and tool count.

| Suite | Compositions | Snapshots | Gate |
|---|---|---|---|
| [`apps/cli/tests/profiles/model-surface.expected.e2e.ts`](../apps/cli/tests/profiles/model-surface.expected.e2e.ts) | `headless` and `desktop`, launched from the built CLI | `apps/cli/tests/profiles/expected/model-surface/` | `integration` |
| [`apps/tui/packages/app/tests/model-surface.spec.ts`](../apps/tui/packages/app/tests/model-surface.spec.ts) | the terminal profile with each preset: `standard`, `ptc`, `cordis`, `minimal` | `apps/tui/packages/app/tests/expected/model-surface/` | `tui-spec` |

Both drive the real composition to its first request through a keyless adapter, so no test code assembles a prompt. They plant a fixed workspace (an `AGENTS.md`, one skill, and a `.git` boundary) and replace the workspace, homes, temporary directory, model id, and dates with placeholders such as `<cwd>` and `<model>`. The snapshots record the provider-neutral request every adapter receives, not one provider's wire body. Windows ships `pwsh` instead of `bash`, so the cases skip there.

After an intended change, rebuild and rewrite the snapshots, then review the diff:

```sh
bun run build
bun run test:integration apps/cli/tests/profiles/model-surface.expected.e2e.ts -u
node node_modules/vitest/vitest.mjs run --config apps/tui/vitest.config.ts apps/tui/packages/app/tests/model-surface.spec.ts -u
```

A diff in these snapshots is a model-visible change, so the pull request needs an [eval record](../evals/README.md#when-to-record).

## Preserve session evidence

Model-visible input must be reconstructable from the session log. Never overwrite, move, or delete a committed session generation to satisfy a test. [Session format status](session-format-status.md) owns released format and migration rules.
