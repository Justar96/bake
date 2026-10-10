# Bake

Bake is an independent terminal coding agent. `origin` is Bake's own repository. `upstream` is a read-only reference: review its DeepSeek Harness releases there, then port changes selectively. Never merge from or push to `upstream` automatically.

## Porting from upstream

Bake removed upstream's web client, desktop app, ACP, Python SDK, docs website, benchmarks, and upstream CI and review automation, along with their Agent Notes and docs. The `desktop` profile bundle in `packages/bundle/desktop` is Bake's own: the separate Bake Desktop app launches it with `dsh --profile desktop`, so keep it and its tests. When porting:

- A modify/delete conflict on a removed note or doc keeps the deletion (`git rm <file>`). Do not restore docs for removed products.
- `packages/session/session-format-catalog/src/retired-vocabulary.ts` keeps event types that released Session logs carry but no plugin writes, so those logs still open. Remove an entry only with a Session-format version bump and migration; regenerate with `bun run gen-persistence-catalog` and confirm no persistence digest moves.
- Leave `.agents/notes/archived/`, `docs/persistence-changes/historical-formats/`, and `snapshots/` as they are: they are frozen history, and their links may point at removed code.
- Some docs still describe upstream gates Bake does not ship, including the Agent Note format, archive, and classification verifiers named in `.agents/notes/README.md` and the `dsh-archive-agent-notes` skill. Trust `package.json` scripts over prose; fix the wording when you touch it.

## Workspace

Bun owns the TypeScript workspace: dependency installation, `bun.lock`, workspace scripts, builds, and hooks. Reproduce a checkout with `bun install --frozen-lockfile`, and do not add a root pnpm or npm lockfile. The separate, opt-in `rust/` workspace uses Cargo, its own `Cargo.lock`, and the toolchain pinned in `rust/rust-toolchain.toml`; run Cargo from that directory so the pin applies. Its preview is excluded from the 0.3 launcher and release archives. The shipped agent process runs on Node, with Bun building and launching it. The runtime's external-profile package manager is a separate concern and does not belong to this workspace. Bake launch commands default to `~/.bake`; setting `BAKE_HOME` selects a different home instead, without migrating upstream data. Bake reads each `BAKE_<name>` environment setting first and falls back to its `DSH_<name>` spelling, so `DSH_HOME` still works when `BAKE_HOME` is unset.

- `apps/tui/packages/app/`: profile composition, agent control, terminal lifecycle.
- `apps/tui/packages/ui/`: side-effect-free Ink components, projection, layout, localized copy.
- `apps/tui/packages/harness/`: component development and recording.
- `apps/cli/`: Node profile launcher and external-plugin management.
- `packages/`: shared agent runtime and its tests; read [the architecture](docs/architecture.md) before changing it.
- `native/`, `vendor/`: native support and pinned Cordis sources. Preserve licenses and upstream attribution.
- `snapshots/`: recorded session evidence. Never overwrite, move, or delete a committed session generation.
- `evals/`: per-version agent-loop metrics and the live paired runner that produces them.

## Commands

```sh
bun install --frozen-lockfile
bun run build             # Node runtime and TUI artifacts
bun run start             # built terminal agent
bun run dev               # hot component preview, no agent or model key
bun run dev:tui           # build and run the real Node agent
bun run dev:rust          # build and run the Rust TUI preview (no model connection)
bun run check:rust        # locked Rust format, lint, test, and build checks
bun run check             # TUI types, tests, layout, peer identity, and docs
bun run test              # pure and Node integration tests
bun run test:runtime <file>  # focused shared-runtime tests
bun run test:e2e           # keyless built-profile PTY scenarios
bun run test:integration   # *.e2e.ts suites: built profiles, sandboxes, artifacts; keyless
bun run build:native-system  # native addon the JSONL persistence specs need
bun run typecheck         # workspace sources; not packages/**/tests
bun run doc-sync          # entry documents and terminal Markdown
bun run gen-rust-migration-inventory  # after a Rust port change; verify-rust-migration-inventory checks it
bun run lint              # Oxlint over apps, packages, scripts, and evals
bun run preflight         # every CI gate before a PR; --fast for the static half
bun run verify            # preflight with the whole runtime suite
bun run eval              # live paired agent-loop eval; needs a model route
bun run eval:record       # commit an eval's metrics and flag regressions
```

Run the checks relevant to your change while you work, and `bun run preflight` before a PR; it reports every gate instead of stopping at the first failure. Full preflight requires the pinned Rust toolchain; `--fast` excludes native builds and PTY scenarios. Any terminal behavior change also requires the built-profile PTY scenarios. Tests that use Cordis or Ink run on Node; pure modules and tooling tests run on Bun; native workspace tests run under Cargo. Report only what you actually ran, including failures and skipped checks. Never bypass hooks without explicit approval.

## Branches and releases

`develop` is where work lands; `main` holds released code; `rust/0.4.0` collects the Rust 0.4 port. [CONTRIBUTING.md](CONTRIBUTING.md) covers pull requests and [distribution/README.md](distribution/README.md#release-with-github-actions) the release workflow.

- Start every change, hotfixes included, on a new branch from an up-to-date `origin/develop`, and open its pull request against `develop`. Never commit to `main`, `develop`, or `rust/0.4.0` directly.
- Rust port work, a change under the [0.4 roadmap](docs/roadmap/rust-0.4/README.md) (see [Rust 0.4 port](#rust-04-port)), starts instead from an up-to-date `origin/rust/0.4.0` and targets it. CI runs its native job on every OS and its TypeScript checks on Linux only as far as its files need. A pull request from `rust/0.4.0` into `develop` brings the line over with a merge commit and full CI; a pull request from `develop` into `rust/0.4.0` brings 0.3 fixes forward. A 0.3 fix still lands on `develop` first.
- Split dependent changes into a stack of pull requests, each based on the one below, and link them with `gh stack link --base <trunk> <bottom> … <top>` so GitHub owns their order. The [stack review guide](docs/cookbook/responding-to-pr-review-on-a-stack.md) covers review fixes, and the `dsh-merging-stacked-prs` skill covers landing with `gh stack merge`.
- `main` accepts only a merge-commit pull request from `develop`, which the `develop only` check enforces. Open one only to ship a release.
- To release, finish the change on its branch, then:
  1. Run `bun run release:prepare <version>`. Check the changelog section it writes, and rename `evals/agent-loop/versions/unreleased/` to `v<version>` if it exists.
  2. Run `bun run release:preflight --offline --tag v<version>`, then commit only those edits as `release: <version>`.
  3. Merge into `develop`, then open the pull request from `develop` to `main`.
  4. Once that merges, tag `main`'s merge commit `v<version>` and push only the tag. The tag publishes to every install, so push it only when the user asks for that release.

## Rust 0.4 port

The [roadmap](docs/roadmap/rust-0.4/README.md) and its [support register](docs/roadmap/rust-0.4/scope-00/support.md#decision-register) own the port's decisions; the [Pi-first scope plan](docs/roadmap/rust-0.4/pi-first-plan.md) governs every scope where older scope text still assumes 0.3 parity or compatibility.

- 0.4 is a new binary that users install; it does not migrate or interoperate with a TypeScript install (D32). It writes and reads Pi's session format and does not read, migrate, or lock Bake Session logs of any format. Do not add TypeScript-compatibility code, cross-runtime checks, or conformance tables against the TypeScript runtime.
- Port primarily from Pi's latest official release (D22). Re-check the release when a scope starts, record the Pi revision and files a PR adapts, keep Pi's MIT notice, and port Pi's tests with its code. Port Bake's TypeScript only where the plan, a decision, or a retained contract needs it, and name its source.
- Three Bake contracts are retained: the CLIProxyAPI provider route, Bake's sandbox, approvals, and permission presets (D24), and the `~/.bake` home (`BAKE_HOME`, then `DSH_HOME`) with its existing settings and credentials read as D25 describes. Tools, schemas, results, prompts, and repair text follow Pi.
- Prove a retained contract with fixture tests derived from its TypeScript source, which you cite; the TypeScript arm stays in TypeScript tests and never changes product source. Rust code never panics on untrusted input.
- With each change to a Rust public API, update the `rust/README.md` crate row and test paragraph and the roadmap Dev Note, then run `bun run gen-rust-migration-inventory`, `verify-rust-migration-inventory`, and `bun run doc-sync`.

## Evals

Every version keeps its agent-loop metrics so the next one can be checked for regressions. [evals/README.md](evals/README.md) has the procedure.

- A change that can alter what the model sees or how many round trips a task takes needs an eval record in its PR. That covers prompts, personas, tool schemas, descriptions, arguments, results, and errors, context assembly, compaction, caching, the agent loop, and LLM adapters.
- Measure the candidate in a paired run against a clean, built worktree of the PR base, on the standard suite with the standard and extended model sets listed in evals/README.md and three trials. Record it under `evals/agent-loop/versions/unreleased/<YYYY-MM-DD>-<topic>/`. A release renames `unreleased/` to its tag, as the changelog does.
- Report the record's regressions in the PR. Fix a flagged regression, or state its cause and why it is accepted, in the record's note and in the PR. Compare versions only through a paired run: absolute counts from different days drift with the gateway, the cache, and the models.
- Never edit or delete a committed record, and move one only in that release rename; supersede a wrong record with a new one. Raw output, which includes transcripts, stays in the ignored `.preflight/`.

## Engineering

- Use ESM and strict TypeScript in the TypeScript workspace. Rust belongs under `rust/`, with formatting and Clippy checks; keep terminal effects separate from pure editor and view state. Local TypeScript relative imports use `.ts`; cross-package imports use package names the importing package declares. Every workspace package outside `vendor/` and `native/` uses a `bake-` name: runtime packages under `packages/` use `bake-<name>`, the CLI is `bake-cli`, and the terminal packages are `bake-tui-<name>`. When porting fixes, map upstream `@deepseek-ai/dsh-<name>` imports to `bake-<name>`, `@deepseek-ai/dsh` to `bake-cli`, and `@dsh-tui/<name>` to `bake-tui-<name>`, and preserve vendored `@deepseek-ai/*` identifiers. Add each rename to the legacy package-name map in `packages/boot/app-boot/src/legacy-package-names.ts`.
- Extend behavior through Cordis plugins and documented events, not ad hoc hooks. Registrations are effects that must supply disposers; waterfall listeners call `next()` when delegating.
- Model-visible input must be reconstructable from the session log. Preserve released data and migration behavior; consult [session format status](docs/session-format-status.md) before any persistence change.
- Read [defensive patterns](docs/defensive-patterns.md) before lifecycle or concurrency work. Teardown must await owned work, restore terminal state, and leave no late callbacks.
- Tests own their temporary paths, ports, global mutations, and subprocesses. Mock external nondeterminism, never the runtime under test.
- Keep UI state authoritative: render logged events and runtime projections instead of maintaining competing copies. Localized product text belongs in `apps/tui/packages/ui/src/copy.ts`.
- When removing a feature, also remove its obsolete consumers, tests, and documentation. Before deleting a shared package, check imports, package dependencies, TypeScript references, and YAML compositions.
- Documentation describes current Bake behavior. Update the owning README and JSDoc alongside code changes, and write English only: Bake keeps no translated documentation or UI language. Historical upstream design material is reference only, never an instruction to restore removed products.
- Keep comments local and explain non-obvious obligations. Files end with one newline. Never commit credentials or `.env`.
