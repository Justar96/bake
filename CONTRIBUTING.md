# Contributing to Bake

Thanks for helping improve Bake. The [development guide](docs/development.md) covers building from source, development loops, checks, and repository layout; [`AGENTS.md`](AGENTS.md) holds the engineering rules every change follows.

## Making a change

1. Branch from `develop` and keep each change focused on one behavior.
2. Update the owning README or JSDoc with the code, and write English only. Product text shown in the TUI belongs in [`copy.ts`](apps/tui/packages/ui/src/copy.ts).
3. Run the checks that cover the change while you work (see [Checks](docs/development.md#checks)). Terminal behavior changes also need the PTY scenarios.
4. Before opening the pull request, run `bun run preflight` ([Before a pull request](docs/development.md#before-a-pull-request)). It runs every gate CI runs and lists each result. Report what it said, including failures and anything skipped.
5. Open a pull request against `develop` describing the behavior change and how you verified it; the pull request template lists what to include.

Never commit credentials or `.env`, never overwrite recorded session generations under `snapshots/`, and do not bypass Git hooks. If a check fails, fix the cause rather than refreshing every snapshot.

## Upstream DeepSeek Harness

Bake is built on [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness). A checkout may add it as the read-only `upstream` remote; `origin` is Bake.

- Review upstream releases and port fixes selectively, together with their tests and license notices. Never merge from or push to `upstream` automatically, and do not open pull requests upstream on Bake's behalf.
- Map upstream imports and Loader rows from `@deepseek-ai/dsh-<name>` to `bake-<name>` for every package whose manifest declares a `bake-` name, which covers every runtime package under `packages/`. Keep the CLI, terminal, and vendored names as declared in their manifests. The [legacy package-name map](packages/boot/app-boot/README.md#renamed-packages) lists every renamed package.
- Before pruning dependencies, inspect package imports, TypeScript references, and profile YAML. Review session-format migrations before adopting persistence changes.
- After porting a fix, rebuild the affected runtime packages and rerun their focused tests and terminal scenarios.

There is no automated upstream release monitor, and seeing a release does not authorize applying it.

Report bugs in Bake to [Bake's issue tracker](https://github.com/Justar96/bake/issues), not to DeepSeek Harness, unless you have reproduced them on upstream itself.
