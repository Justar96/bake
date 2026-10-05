# Development guide

This guide covers building Bake from source, the day-to-day development loops, the checks to run before a change lands, and where code lives. [`CONTRIBUTING.md`](../CONTRIBUTING.md) covers how changes are reviewed and how upstream DeepSeek Harness fixes are ported. [`AGENTS.md`](../AGENTS.md) holds the engineering rules every change follows.

## Prerequisites

- **Bun**, at the version pinned in [`package.json`](../package.json) (`packageManager`). Bun owns dependency installation, `bun.lock`, workspace scripts, builds, and Git hooks. Do not add a pnpm or npm lockfile.
- **Node.js 24 or newer.** The agent itself runs on Node, because its boot loader depends on V8 internals that Bun's engine lacks. Never substitute `bun --bun` for the Node process.
- **A C/C++ toolchain with Node headers** for the native modules.
- **Linux or macOS for the terminal scenarios.** Builds, type checks, and unit tests also run on Windows, but the PTY scenarios (`bun run test:e2e`) need Linux or macOS. WSL 2 works; keep the checkout inside the Linux filesystem and install dependencies separately there.

## First build

From the repository root:

```sh
bun install --frozen-lockfile
bun run build
bun run start
```

`bun install` also installs the Lefthook Git hooks. `start` runs the existing build output without rebuilding; it needs an interactive terminal. `bun run start --help` prints the TUI flags without starting a session.

Source runs use the same home as an installed `bake`: `~/.bake`, or the directory in `DSH_HOME`. An override selects that directory's existing profiles and sessions, not only its credentials. Existing `~/.dsh` data is never moved or changed.

For real model requests, sign in with `/login` or set `DEEPSEEK_API_KEY` in the environment or in a gitignored `.env` at the repository root. To use another DeepSeek endpoint, set `baseURL` on the `deepseek-official` route under `llm-pi-ai` in `settings.yaml`, as the [model configuration guide](user/guide/providers.md) shows. Never commit keys or `.env`.

## Development loops

| Work | Command | Behavior |
|---|---|---|
| Ink components | `bun run dev` | Hot-reloads a recorded component preview; no agent, network, or model key. |
| Streaming preview | `bun run dev --replay` | Plays recorded rows into the preview. |
| Full agent | `bun run dev:tui` | Builds the runtime and TUI, then starts the Node agent; no automatic restart. |
| TUI-only edits | `bun run build:tui && bun run start` | Rebundles terminal code; needs an existing runtime build. |
| Shared runtime edits | `bun run build && bun run start` | Rebuilds runtime packages and terminal code. |

The preview accepts input for layout testing but does not submit tasks. Ctrl-C exits the preview; the real agent needs two Ctrl-C presses to quit. Stop the real agent before rebuilding its files.

`bun run dsh --help` exposes the built profile and plugin launcher with the same Bake home. External profile-plugin installation stays separate from Bun's source workspace.

## Checks

While you work, run the checks that cover your change rather than the whole suite. Any terminal behavior change also needs the PTY scenarios.

```sh
bun run check          # workspace, tsconfig paths, TUI types, tests, peer identity, layout, docs
bun run test           # TUI unit and spec tests
bun run test:runtime <file-or-dir>   # focused shared-runtime tests (Vitest on Node)
bun run test:e2e       # keyless PTY scenarios against the built profile
bun run test:integration   # *.e2e.ts suites against built output (Vitest on Node)
bun run lint           # Oxlint over apps, packages, and scripts
```

`bun run check` runs every target it is given and lists the ones that failed, rather than stopping at the first.

The PTY scenarios replay recorded model responses while running real tools, then check persisted sessions, screen contents, and terminal restoration. They need no model key and do not touch your Bake home.

The integration suites (`*.e2e.ts`) boot the assembled profiles, the real sandbox backends, and the built libraries, so run them after `bun run build`. They remove every `*_API_KEY` variable first, which makes the provider smokes skip themselves. `DSH_E2E_LIVE=1 bun run test:integration` keeps the keys and makes real, billed model calls. Seatbelt suites run only on macOS, the ACL suite only on Windows, and the Landlock suites only on Linux after `bun native/system/scripts/build.ts` builds the `landlock-run` launcher, which needs `musl-gcc`; `bun run build` builds only the Node addon.

For shorter iterations after a runtime build:

```sh
bun run test:e2e --list
bun run test:e2e --only rendering
bun run test:e2e --no-build
bun apps/tui/scripts/tui.ts spec packages/ui/tests/placement.spec.tsx
bun run test:runtime apps/cli/tests/args.spec.ts
```

`--only` includes a scenario's prerequisites. E2E normally rebuilds the TUI, not the shared runtime; `--no-build` uses existing artifacts unchanged. Failed scenarios report their wait condition and keep transcripts in `apps/tui/.smoke/`. `bun apps/tui/scripts/tui.ts help` lists watch modes, fixture recording, and performance diagnostics.

Tests that use Cordis or Ink run on Node; pure modules and tooling tests run on Bun. A failing check is not a reason to refresh every snapshot or bypass hooks. Review expected-output changes, keep CI detection intact, and never overwrite recorded session generations under `snapshots/`.

### Before a pull request

```sh
bun run preflight          # every CI gate, runtime tests limited to what the change reaches
bun run preflight --fast   # the static half, about 20 seconds: no build, Node suites, or PTY
bun run verify             # every CI gate with the whole runtime suite (preflight --full)
```

`bun run preflight` is what CI runs. It measures the change from `origin/develop` (or `develop`; `--base <ref>` picks another), including uncommitted and untracked files, and runs each gate in turn:

- **hygiene**: no compiled `.js` or `.d.ts` left under a `src/` directory, where it would load instead of the `.ts` beside it; no whitespace errors in the change; and a warning when shipped source changed without a `CHANGELOG.md` entry.
- **generated**: every `verify-*` script, so the workspace manifests, tsconfig paths, config, tool, and Cordis catalogs, doc graphs, module graph, and pasted types match their sources. `verify-cordis-config` also keeps Loader row metadata static and requires each named plugin to resolve from the manifest that owns the row; it also fails when a profile that mounts agent presets runs one of their rows on its host plane as well, whether the preset enables that row or disables it, unless the script's `SHARED_PLANE_ROWS` list names the row with the reason both copies are harmless. `verify-package-invariants` requires each package's invariant companion to be wired completely, or its omission to be explained in the package README.
- **types**, **lint** (Oxlint, and actionlint over the workflows when it is on `PATH`), and the Bun-run tooling tests.
- **build**, then the TUI check targets, the runtime suite, and the PTY scenarios against what it built. The runtime step runs the specs the change reaches through the import graph (`vitest --changed`), the whole suite when a workspace or config file changed, and nothing when no runtime source changed. `--full` always runs the whole suite.

When a Vitest step fails, the files that failed are run again on their own. Files that pass alone make the step a `WARN` naming them, since real-process tests can miss a deadline on a busy machine; any that fail again make it a `FAIL`, and so does an unhandled error Vitest could not tie to a test file, since no rerun can clear it. Every gate runs even after one fails, and the summary lists each result. A failing gate's output is in `.preflight/<step>.log`, and its last lines are printed at the end. `--only` and `--skip` take step or group names; `--list` prints them. Fix what fails, or say in the pull request which gate failed and why it is unrelated to the change.

### Git hooks

[`lefthook.yml`](../lefthook.yml) keeps the hooks fast:

- `pre-commit` lints staged TypeScript and JavaScript with Oxlint (applying fixes), rejects whitespace errors, and checks that changes under `vendor/*/src` update [`vendor/README.md`](../vendor/README.md).
- `pre-push` runs `bun run preflight --fast`: the generated-artifact checks, types, lint, and the unit, layout, and docs targets.

The hooks do not run the build, the Node suites, or the PTY scenarios; run `bun run preflight` before opening a pull request. Never skip hooks without the maintainer's agreement.

### CI

[`ci.yml`](../.github/workflows/ci.yml) runs `bun run preflight --full` on Linux and macOS for pull requests and direct pushes to `main`, in two jobs per system: the whole runtime suite, and every other gate. It skips a pull request's merge commit on `main`, which its pull request run already checked. A pull request from `develop` to `main` runs only the Linux jobs: each change it carries already ran on every platform in its own pull request, and the release tag that follows builds and checks every platform again. `main` takes changes only through merge-commit pull requests from `develop`: its ruleset requires the `develop only` check from [`main-source.yml`](../.github/workflows/main-source.yml), which fails a pull request from any other branch. [`release.yml`](../.github/workflows/release.yml) builds, signs, and publishes release archives; the [release guide](../distribution/README.md) covers the process.

## Repository layout

| Path | Contents |
|---|---|
| [`apps/tui/`](../apps/tui/DESIGN.md) | The terminal application: `packages/app` (profile composition, agent control, terminal lifecycle), `packages/ui` (side-effect-free Ink components, projection, layout, localized copy), `packages/harness` (component development and recording), fixtures, and dev tools. |
| [`apps/cli/`](../apps/cli/README.md) | Node launcher for the `tui` and `headless` profiles, and external-plugin management. |
| [`packages/`](../packages/README.md) | The shared agent runtime: agent loop, sessions, models, tools, sandbox, and plugin services. Read the [architecture](architecture.md) before changing it. |
| [`native/`](../native/README.md), [`vendor/`](../vendor/README.md) | Native support and pinned Cordis sources. Preserve their licenses and upstream attribution. |
| [`distribution/`](../distribution/README.md) | Release packaging, signing, and the download service. |
| [`snapshots/`](../snapshots/AGENTS.md) | Recorded session evidence, including retained historical generations. |

Useful references while working in the runtime:

- [Cordis primer](cordis-primer.md): plugins, services, and events.
- [Defensive patterns](defensive-patterns.md): read before lifecycle or concurrency work.
- [Session format status](session-format-status.md): read before any persistence change.

## Conventions

- ESM and strict TypeScript throughout. Local relative imports use `.ts`; cross-package imports use declared package names. Runtime packages under `packages/` use `bake-<name>`; the CLI, terminal, and vendored packages retain their declared names. Map upstream imports of migrated packages to the `bake-<name>` declared in each manifest; the [legacy package-name map](../packages/boot/app-boot/README.md#renamed-packages) lists every renamed package.
- The shared Node runtime builds through `tsconfig.host.json`; package `tsconfig.json` files reference their workspace dependencies. When you add or remove a package, update its references and run `bun run gen-workspace` and `bun run gen-tsconfig-paths`.
- External dependency versions shared by two or more manifests live once in the root `package.json` `catalog`, and each manifest references them as `"catalog:"`. To add or upgrade one, edit the catalog entry and run `bun install`, then commit `bun.lock` with it; `bun run verify-workspace` rejects shared literal ranges, missing entries, and unused entries. `vendor/` manifests and peer ranges keep literal ranges.
- Product text shown in the TUI lives in [`apps/tui/packages/ui/src/copy.ts`](../apps/tui/packages/ui/src/copy.ts).
- Documentation describes current behavior. Update the owning README or JSDoc with the code, and write English only.
- Mark known issues by urgency: `FIXME` blocks a release, `TODO` should be fixed soon, and `XXX` is a someday item.

### Documenting types verbatim

[Subsystem pages](subsystems/README.md) paste declarations exactly as they appear in source, with their JSDoc. Fence such a paste as ` ```ts type-equiv ` (or ` ```ts public-api ` for a class shown without implementation bodies) and register it in [`scripts/type-equiv.manifest.json`](../scripts/type-equiv.manifest.json) with its source file and symbol:

```json
{ "doc": "docs/subsystems/session.md", "symbol": "SessionEvent", "source": "packages/core/session/src/types.ts" }
```

`bun run verify-type-equiv` compares each block with the source declaration. When you change a documented declaration, update the paste and rerun the check.
