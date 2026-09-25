# Contributing to Bake

English | [中文](CONTRIBUTING.zh.md)

Thanks for helping improve Bake. The [development guide](docs/development.md) covers building from source, development loops, checks, and repository layout; [`AGENTS.md`](AGENTS.md) holds the engineering rules every change follows.

## Making a change

1. Branch from `develop` and keep each change focused on one behavior.
2. Update the owning README or JSDoc with the code, and keep English and Chinese pages aligned. Product text shown in the TUI belongs in [`copy.ts`](apps/tui/packages/ui/src/copy.ts), in both languages.
3. Run the checks that cover the change (see [Checks](docs/development.md#checks)). Terminal behavior changes also need the PTY scenarios. Report what you ran, including failures and anything skipped.
4. Open a pull request against `develop` describing the behavior change and how you verified it.

Never commit credentials or `.env`, never overwrite recorded session generations under `snapshots/`, and do not bypass Git hooks. If a check fails, fix the cause rather than refreshing every snapshot.

## Upstream DeepSeek Harness

Bake is built on [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness). A checkout may add it as the read-only `upstream` remote; `origin` is Bake.

- Review upstream releases and port fixes selectively, together with their tests and license notices. Never merge from or push to `upstream` automatically, and do not open pull requests upstream on Bake's behalf.
- Keep the shared runtime's `@deepseek-ai/*` package names so ported fixes apply cleanly.
- Before pruning dependencies, inspect package imports, TypeScript references, and profile YAML. Review session-format migrations before adopting persistence changes.
- After porting a fix, rebuild the affected runtime packages and rerun their focused tests and terminal scenarios.

There is no automated upstream release monitor, and seeing a release does not authorize applying it.

Report bugs in Bake to [Bake's issue tracker](https://github.com/Justar96/bake/issues), not to DeepSeek Harness, unless you have reproduced them on upstream itself.
