# Contributing to Bake

Thanks for helping improve Bake. The [development guide](docs/development.md) covers building from source, development loops, checks, and repository layout; [`AGENTS.md`](AGENTS.md) holds the engineering rules every change follows.

## Making a change

1. Branch from `develop` and keep each change focused on one behavior. `develop` is Rust-first: see [The Rust agent and the frozen TypeScript agent](#the-rust-agent-and-the-frozen-typescript-agent) before changing TypeScript.
2. Update the owning README or JSDoc with the code, and write English only. Product text shown in the TUI belongs in [`copy.ts`](apps/tui/packages/ui/src/copy.ts).
3. Run the checks that cover the change while you work (see [Checks](docs/development.md#checks)). Terminal behavior changes also need the PTY scenarios.
4. Before opening the pull request, run `bun run preflight` ([Before a pull request](docs/development.md#before-a-pull-request)). It runs every gate CI runs and lists each result. Report what it said, including failures and anything skipped.
5. Open a pull request against `develop` describing the behavior change and how you verified it; the pull request template lists what to include.

Never commit credentials or `.env`, never overwrite recorded session generations under `snapshots/`, and do not bypass Git hooks. If a check fails, fix the cause rather than refreshing every snapshot.

## Stacked pull requests

When a change needs another that has not landed, stack them instead of waiting or growing one pull request. Each pull request in a stack is based on the branch below it, so its diff and review show only its own layer.

1. Branch the next layer from the one below it and open its pull request against that branch.
2. Link the chain bottom to top with the [`gh stack`](https://github.com/github/gh-stack) extension (`gh extension install github/gh-stack`): `gh stack link --base develop <bottom> … <top>`. `gh stack link <stack> <new-pr>` adds a layer to an existing stack.
3. Fix a review comment on the layer that introduced the code, then merge it forward into each layer above, as the [stack review guide](docs/cookbook/responding-to-pr-review-on-a-stack.md) describes. Run each layer's checks before pushing it.
4. Land the stack, or the layers up to one, with `gh stack merge <stack-or-pr> --merge`, never by merging and retargeting its pull requests one at a time.

## The Rust agent and the frozen TypeScript agent

Bake's Rust agent, `bake-rs` under [`rust/`](rust/README.md), is the product line on `develop`, built to the [Rust 0.4 roadmap](docs/roadmap/rust-0.4/README.md). The TypeScript agent is deprecated and frozen: it still builds and is what `bake` installs until the Rust release, and the Rust release removes it from `develop`.

- Do not add TypeScript features. On `develop`, change TypeScript only to keep its gates passing, or for tooling the Rust port uses, such as the evaluator under `evals/` and the scripts under `scripts/`.
- Fix a 0.3 bug on `release/0.3`, the TypeScript agent's maintenance line: branch from an up-to-date `origin/release/0.3` and open the pull request against it. That branch's own CONTRIBUTING and AGENTS.md describe its fixes-only rules and its releases. Nothing from `release/0.3` merges into `develop` or `main`.

0.3 bug and security maintenance ends when 0.4.0 ships. From that release, Bake ships and maintains only Rust; supported legacy `dsh`, profile, and package aliases retire with 0.3. The 0.4.0 release notes must announce retirement and the separate Rust installation. Native environment fallbacks and existing user-owned command links remain preserved, as [D10 and D13](docs/roadmap/rust-0.4/scope-00/support.md#decision-register) require.

### CI for pull requests into develop

CI runs the native job (Cargo checks, the Rust PTY scenarios, and the compiled eval fixture arm) on Linux and Windows. No CI job runs on macOS: run `bun run preflight --only native` on a Mac before opening the pull request, or `bun run preflight:mac` to run it on one over SSH, and paste its summary there. TypeScript checks run on Linux only: none when the pull request changes only `rust/` or `scripts/rust-preview-pty.ts`; the static half when the rest is Markdown; and both preflight parts and the Windows release check otherwise. A stack whose bottom targets `develop` is planned the same way. [`scripts/ci-scope.ts`](scripts/ci-scope.ts) makes the choice and states it in the run summary. Still run `bun run preflight` before opening the pull request.

## Upstream DeepSeek Harness

Bake is built on [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness). A checkout may add it as the read-only `upstream` remote; `origin` is Bake.

- Review upstream releases and port fixes selectively, together with their tests and license notices. Never merge from or push to `upstream` automatically, and do not open pull requests upstream on Bake's behalf.
- Map upstream imports and Loader rows from `@deepseek-ai/dsh-<name>` to `bake-<name>` for every runtime package under `packages/`, `@deepseek-ai/dsh` to `bake-cli`, and `@dsh-tui/<name>` to `bake-tui-<name>`. Keep vendored and native names as declared in their manifests. The [legacy package-name map](packages/boot/app-boot/README.md#renamed-packages) lists every renamed package.
- Before pruning dependencies, inspect package imports, TypeScript references, and profile YAML. Review session-format migrations before adopting persistence changes.
- After porting a fix, rebuild the affected runtime packages and rerun their focused tests and terminal scenarios.

There is no automated upstream release monitor, and seeing a release does not authorize applying it.

Report bugs in Bake to [Bake's issue tracker](https://github.com/Justar96/bake/issues), not to DeepSeek Harness, unless you have reproduced them on upstream itself.
