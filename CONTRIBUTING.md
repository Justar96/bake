# Contributing to Bake

Thanks for helping improve Bake. The [development guide](docs/development.md) covers building from source, development loops, checks, and repository layout; [`AGENTS.md`](AGENTS.md) holds the engineering rules every change follows.

## Making a change

1. Work on an up-to-date `develop` and keep each commit focused on one behavior. `develop` is Rust-first: see [The Rust agent and the frozen TypeScript agent](#the-rust-agent-and-the-frozen-typescript-agent) before changing TypeScript.
2. Update the owning README or JSDoc with the code, and write English only. Product text shown in the TUI belongs in [`copy.ts`](apps/tui/packages/ui/src/copy.ts).
3. Run the checks that cover the change while you work (see [Checks](docs/development.md#checks)). Terminal behavior changes also need the PTY scenarios.
4. Before pushing, run `bun run preflight` ([Before pushing](docs/development.md#before-pushing)). It runs every gate CI runs and lists each result. A change to `rust/`, native code, or terminal behavior also runs `bun run preflight:mac` on a Mac over SSH, and the native checks on Windows when a Windows host is available. Report what ran, including failures, skipped checks, and platforms not checked.
5. Rebase on `origin/develop` and push directly to `develop`. No pull request goes into `develop`, and `develop` is never force-pushed.

Never commit credentials or `.env`, never overwrite recorded session generations under `snapshots/`, and do not bypass Git hooks. If a check fails, fix the cause rather than refreshing every snapshot.

## The Rust agent and the frozen TypeScript agent

Bake's Rust agent, `bake-rs` under [`rust/`](rust/README.md), is the product line on `develop`, built to the [Rust 0.4 roadmap](docs/roadmap/rust-0.4/README.md). The TypeScript agent is deprecated and frozen: it still builds and is what `bake` installs until the Rust release, and the Rust release removes it from `develop`.

- Do not add TypeScript features. On `develop`, change TypeScript only to keep its gates passing, or for tooling the Rust port uses, such as the evaluator under `evals/` and the scripts under `scripts/`.
- Fix a 0.3 bug on `release/0.3`, the TypeScript agent's maintenance line: branch from an up-to-date `origin/release/0.3` and open the pull request against it. That branch's own CONTRIBUTING and AGENTS.md describe its fixes-only rules and its releases. Nothing from `release/0.3` merges into `develop` or `main`.

0.3 bug and security maintenance ends when 0.4.0 ships. From that release, Bake ships and maintains only Rust; supported legacy `dsh`, profile, and package aliases retire with 0.3. The 0.4.0 release notes must announce retirement and the separate Rust installation. Native environment fallbacks and existing user-owned command links remain preserved, as [D10 and D13](docs/roadmap/rust-0.4/scope-00/support.md#decision-register) require.

### CI

GitHub CI runs only on the release pull request from `develop` into `main`: TypeScript checks on Linux, the native job (Cargo checks, the Rust PTY scenarios, and the compiled eval fixture arm) on Linux and Windows, and the Windows release check. No CI job runs on macOS. Every push to `develop` relies on the local checks above, so run them before pushing.

## Upstream DeepSeek Harness

Bake is built on [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness). A checkout may add it as the read-only `upstream` remote; `origin` is Bake.

- Review upstream releases and port fixes selectively, together with their tests and license notices. Never merge from or push to `upstream` automatically, and do not open pull requests upstream on Bake's behalf.
- Map upstream imports and Loader rows from `@deepseek-ai/dsh-<name>` to `bake-<name>` for every runtime package under `packages/`, `@deepseek-ai/dsh` to `bake-cli`, and `@dsh-tui/<name>` to `bake-tui-<name>`. Keep vendored and native names as declared in their manifests. The [legacy package-name map](packages/boot/app-boot/README.md#renamed-packages) lists every renamed package.
- Before pruning dependencies, inspect package imports, TypeScript references, and profile YAML. Review session-format migrations before adopting persistence changes.
- After porting a fix, rebuild the affected runtime packages and rerun their focused tests and terminal scenarios.

There is no automated upstream release monitor, and seeing a release does not authorize applying it.

Report bugs in Bake to [Bake's issue tracker](https://github.com/Justar96/bake/issues), not to DeepSeek Harness, unless you have reproduced them on upstream itself.
