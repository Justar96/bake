# dsh TUI — development plan

Fork-local planning document. Lives in `tui/`, a directory upstream does not own, so syncing
`deepseek-ai/deepseek-harness` never conflicts with it.

- **Status:** TUI alignment findings 1–7 are implemented: safe paste, exact resume, persisted presets, scoped human decisions, registry commands, live state, and strict TypeScript checks. Validation and current behavior are owned by [DESIGN.md](DESIGN.md); remaining milestone work below includes the component development loop, attachments, model selection, and publishing.
- **Runtime:** Node **26.9.0** primary, **24.21.0** floor — both verified booting the harness.
- **Decisions:** TypeScript · Ink · in-process Cordis plugin on the **Node** runtime · Bun as build,
  test, and component-harness toolchain · shipped as a `dsh` profile bundle.

## Table of contents

1. [What this repo is](#1-what-this-repo-is)
2. [Measured facts](#2-measured-facts)
3. [Runtime architecture](#3-runtime-architecture)
4. [Bun integration](#4-bun-integration)
5. [Package layout](#5-package-layout)
6. [Integration seams](#6-integration-seams)
7. [UI architecture](#7-ui-architecture)
8. [Development loop](#8-development-loop)
9. [Milestones](#9-milestones)
10. [Testing strategy](#10-testing-strategy)
11. [Risks](#11-risks)
12. [Upstream sync policy](#12-upstream-sync-policy)
13. [Runbook](#13-runbook)
14. [Open questions](#14-open-questions)

-----

## 1. What this repo is

DeepSeek Harness (`dsh`) is an **all-plugin Cordis agent harness** in TypeScript/ESM. Nothing is a
library you call; everything is a plugin contributing into a `ctx` tree.

| Path | Contents |
|---|---|
| `packages/*/*` | ~60 groups of `@deepseek-ai/dsh-<name>` workspaces (core/session, llm, shell, tools, sandbox, skill, subagent, api, client, host, …) |
| `apps/cli` | the only supported Node application launcher: the `dsh` bin |
| `packages/bundle/*` | profile bundles (`base`, `web-app`, `headless`, `acp-app`, `sdk-app`, `sdk-minimal`), each declaring `dsh.bundle.patch` |
| `apps/web`, `apps/desktop` | browser GUI and its Electron shell |
| `python/` | Python SDK over the same JSON-RPC protocol |

### 1.1 Application launch rule (hard constraint)

`docs/architecture.md#application-launch`: **supported Node apps launch only through named `dsh`
profiles**. Package bins, demos, and inline Cordis trees are rejected by
`scripts/verify-application-entrypoints.ts`. The TUI must therefore be a **profile**, not a bin.

### 1.2 Upstream already expects a TUI

Not speculation — it is in shipped code and docs:

- `apps/cli/src/args.ts`, `apps/cli/README.md`, `apps/cli/tests/expected/launcher-help.txt` use
  `dsh --profile tui` / `dsh tui --resume <session>` as the worked example of an installed,
  out-of-tree profile.
- `packages/boot/app-boot/src/index.ts` — `installFailLoud(binName, proc, release)` documents `release`
  as "the terminal owner's chance to hand it back", naming raw mode, bracketed paste and the keyboard
  protocol. **A terminal-restore hook with no in-repo caller.**
- `packages/api/remotes/README.md` — "reused by Web or a future TUI that provides the same React-free
  `ctx.remote` contract".
- `packages/client/ui-skill/README.md` — "from the Web composer, TUI, and ACP".
- `packages/client/ui-user-questions/README.md` + `src/index.ts` — "the TUI composition, which has no
  presets".
- `apps/cli/tests/web-agent-presets.e2e.ts:260` — "the TUI composition e2e".

Read: a first-party TUI exists or is planned inside DeepSeek, and the seams are public.

-----

## 2. Measured facts

Host: darwin-x64 (Intel), 2026-09-21.

### 2.1 Bootstrap actually performed

```sh
npx pnpm@11.7.0 install       # pnpm 12 refuses: @pnpm/exe@11.7.0 ships no darwin-x64 binary
tsc -b tsconfig.host.json                                  # 98s
tsdown --env.DSH_BUILD_FACE host                           # 13s
tsx native/system/scripts/build.ts --host-addon-only       # 2s
```

All three are required before any profile boots. Without `lib/`, every plugin reports "failed to
import". Without the native addon, boot dies on a missing
`native/system/packages/darwin-x64/bin/system.node` (`flock`, reached through session persistence).

Result: `dsh --profile headless "say hi"` boots the whole tree and stops exactly at
`MISSING_CREDENTIAL` (no `DEEPSEEK_API_KEY`). Two known non-blocking warnings: `typert` and
`typert-gateway` fail to import because the full `pnpm run build` generators have not run.

### 2.2 Bun cannot run the harness

| Command | Node | Bun |
|---|---|---|
| `bin.ts --help` | ok | ok |
| `bin.ts --profile headless --dump-config` | ok | ok |
| `bin.ts --profile headless "say hi"` | boots the tree, stops at `MISSING_CREDENTIAL` | **fails before any plugin mounts** |

```
dsh: host preparation failed: node-addon-require-builtin unsupported:
  Unsupported/no-context (required V8 current-context symbols were not found)
  at boot (packages/boot/app-boot/src/index.ts:979)
      addon.requireBuiltin('internal/modules/esm/loader')
```

`app-boot` reaches **V8 current-context symbols** to obtain `internal/modules/esm/loader` and install
its immutable package-resolution generation into Node's ESM and CJS resolvers. Bun runs on
JavaScriptCore, which has no V8 contexts. This is not a compatibility gap awaiting a fix, and it
happens in *host preparation* — before the first plugin mounts, so nothing can route around it.

**The dsh runtime is Node-only. Bun cannot host an in-process dsh plugin.**

### 2.3 Toolchain loss and recovery

Mid-session the host lost its Node toolchain: `~/.nvm` was removed and `node`, `npm`, `npx`, `pnpm`
left `PATH`. `~/.bun/bin/bun` survived but changed version (1.4.3 → 1.3.14). The repo, `node_modules/`,
and the §2.1 build products were untouched.

Recovered by installing Node directly from `nodejs.org` into `~/.local/opt`, symlinked into
`~/.local/bin` (already on `PATH`, so every shell sees it without sourcing nvm):

| Binary | Version |
|---|---|
| `node`, `node26` | v26.9.0 |
| `node24` | v24.21.0 |
| `npm` | 11.19.1 |

`nvm` itself is avoided for installs: `nvm ls-remote` downloads and shell-parses the complete Node
release index and is pathologically slow. A direct tarball fetch installs both versions in ~15s.
The two binaries make the supported engine range (`^22.19.0 || >=24.0.0`) testable as a matrix.

### 2.3.1 M0 result — verified service reachability

`tui/experiments/probe.ts`, inserted by patch-relative path and run under the tsx source launch:

```sh
node --import tsx/esm apps/cli/src/bin.ts --profile headless \
  --patch ./tui/experiments/probe.patch.yml "say hi"
```

```
TUI-PROBE: mounted on node v26.9.0
TUI-PROBE: reached (15): agents, agentDefaultModel, sessions, sessionQuery, commands, tools,
                         approval, skills, goals, jobs, fs, llm, settings, credentials, subagents
TUI-PROBE: missing  (4): sessionProjection, agentPresets, cmdline, workspace
```

**Byte-identical under `node24` (v24.21.0).** Three facts established:

1. **An out-of-tree `.ts` plugin loads from source with no build and no publishing** — `./probe.ts` in a
   `--patch` overlay resolved to a `file://` URL and executed. The §8.1 dev loop is real.
2. **The core TUI seams are already in `dsh-base`**: `commands`, `approval`, `skills`, `sessionQuery`,
   `agents`, `sessions`. No upstream change is needed to reach any of them.
3. **Some services are not base rows**, and the `tui` patch must add what it wants. This settles
   [open question 1](#14-open-questions): "the TUI composition, which has no presets" describes base
   composition, so presets are opt-in, not a default we would be removing.

**Correction from M1:** two of the four "missing" names above were wrong keys, not absent services.
`cmdlineArgs` (not `cmdline`) is provided by the **launcher**, which is why the headless bundle injects
it without inserting a provider row; and the projection service is `sessionProjections` (plural), a base
row. Only `agentPresets` and `workspace` are genuinely absent from base. A probe is only as good as the
keys it asks for — M1 caught this by inserting a `cmdline` row that failed to import while the surface
worked anyway.

Two API details cost three iterations and are worth recording:

- Vendored Cordis `inject` has **no optional form**. Every entry is required, gates `ctx` property
  access (`cannot get property "agents" without inject`), and an unsatisfiable entry leaves the plugin
  `pending` forever. `{ required, optional }` is not a shape it understands — it reads those two words
  as service names.
- Reachability is meaningless before settlement. Loader siblings mount concurrently, so a probe in
  `apply` sees only `loader`. The correct pattern, lifted from `packages/bundle/headless/src/index.ts`,
  is `await ctx.get('loader')?.await()` and then `ctx.get(name)` — which `tui-runner` must also use
  before creating its Agent.

### 2.4 Published-package state

`@deepseek-ai/dsh-*` are on npm, but the published set is fragmented and behind this checkout
(`0.1.6-alpha.2`):

| Package | npm `latest` |
|---|---|
| `@deepseek-ai/dsh` | 0.1.5-rc.2 |
| `@deepseek-ai/dsh-base` | 0.0.1-rc.1 |
| `@deepseek-ai/dsh-agent` | 0.1.0-rc.6 |
| `@deepseek-ai/dsh-session` | 0.0.1-rc.1 |
| `@deepseek-ai/dsh-app-boot` | 0.1.0-rc.6 |
| `@deepseek-ai/dsh-headless` | 0.0.1-rc.1 |
| `@deepseek-ai/dsh-client-store` | 0.1.2-alpha.2 |
| `@deepseek-ai/dsh-api-gateway` | 0.0.1-rc.1 |
| `@deepseek-ai/cordis` | 4.0.2 |

**Consequence:** a truly out-of-tree package cannot be built against published versions today. Develop
**in-repo** as a workspace member under `tui/`, and treat publishing as milestone M7, gated on upstream
version alignment. This costs one line in `pnpm-workspace.yaml` — the only upstream file we touch.

### 2.5 Bun capability research

All verified locally on bun 1.3.14:

| Capability | Result |
|---|---|
| `bun build --target=node --format=esm` | Clean Node ESM out, 40ms. Injects only a `createRequire` shim; **no Bun globals**. Safe to ship to Node. |
| `bun test` | Works, 103ms end to end including startup. |
| `bun install ink react @deepseek-ai/cordis` | 41 packages in 3.26s. |
| **Ink 7.1.1 + React 19.3.0 under Bun** | **Renders correctly** — `<Static>`, `<Box borderStyle>`, colors, yoga-layout WASM all fine. |
| Ink engine requirements | `type: module`, `engines.node >=22`, peers `react >=19.2.0`, `@types/react >=19.2.0`, `react-devtools-core >=6.1.2`. Compatible with dsh's `^22.19 \|\| >=24`. |

-----

## 3. Runtime architecture

**In-process Cordis plugin, Node runtime, loaded as a profile bundle.**

```
┌─ node (the only possible runtime) ─────────────────────────────────┐
│  dsh --profile tui                                                 │
│    app-boot → package-resolution generation → mounts the tree      │
│                                                                    │
│    @deepseek-ai/dsh-base rows  (agent-loop, session, llm, tools…)  │
│    @dsh-tui/app rows:                                              │
│      tui-startup   cmdline flags → ctx.tuiStartup                  │
│      tui-runner    owns stdin raw mode + the `release` teardown    │
│      tui-ui        Ink root, transcript, composer, approvals       │
└────────────────────────────────────────────────────────────────────┘
```

Why in-process rather than a client over ACP or the SDK protocol:

| | in-process | ACP (`--profile acp`) | SDK JSON-RPC (`--profile sdk`) |
|---|---|---|---|
| approvals | `approval/request` waterfall | `session/request_permission` | — |
| commands (`/…`) | `ctx.commands` | rejected by design | — |
| plans, todos, terminals, elicitation | yes | rejected by design | — |
| transcript replay for `--resume` | `ctx.sessionQuery` | **`session/load` unsupported** | live events only |
| model / reasoning-effort switch | `installModelSelection` | `session/set_config_option` | `initialize` only |

ACP's `session/resume` without `session/load` means a resumed session cannot render its history — so
`dsh tui --resume <session>`, upstream's own worked example, would open on a blank pane. That single
row decides the architecture.

-----

## 4. Bun integration

The runtime is Node and cannot be Bun ([§2.2](#22-bun-cannot-run-the-harness)). Bun's value is therefore
in **build, test, and the component development loop** — where it is genuinely strong. What follows is
the maximum honest extraction, not a wish list.

### 4.1 Where Bun is used

| Use | Tool | Why it wins | Status |
|---|---|---|---|
| **Component dev harness** | `bun --hot tui/harness/dev.tsx` | The headline win. Ink components are pure React over props; the harness feeds them recorded session-event fixtures with **no dsh runtime, no Node boot, no API key**. Sub-second hot reload against a ~100s cold path. Verified: Ink renders correctly under Bun ([§2.5](#25-bun-capability-research)). | verified |
| **Unit tests for pure modules** | `bun test` | Transcript folding, ANSI width math, keymap parsing, diff rendering, event→row projection. 103ms vs a Node+vitest boot. | verified |
| **Fixture tooling** | `bun` scripts, `bun:sqlite` | Recording, indexing, and querying session-event fixtures for the harness. `bun:sqlite` is fine here because it never enters the shipped plugin. | planned |
| **Distribution bundling** | `bun build --target=node --format=esm` | 40ms, clean Node ESM, no Bun globals. Used at M7 when the package is published out-of-tree. | verified |
| **Script running** | `bun run`, `bun x` | Faster than `npx` for repo-local tooling. | trivial |

### 4.2 Where Bun is *not* used, and why

| Not used | Reason |
|---|---|
| Shipped plugin runtime | Impossible — §2.2. The plugin is loaded by dsh's own Node resolver. |
| `Bun.file`, `Bun.spawn`, `Bun.serve`, `bun:sqlite` **in plugin code** | Same. Any such import makes the plugin unloadable by `dsh`. Enforced by lint rule (M1). |
| `bun install` at the repo root | The root is pnpm's: `overrides`, `link:vendor/*`, `patches/`, `onlyBuiltDependencies`, `peerDependencyRules`, and the `@pnpm/exe` pin do not transfer, and CI is pnpm. Two lockfiles in one tree is a support hazard. Root stays pnpm, always. |
| `bun install` for `tui/` | Not while `tui/` is a pnpm workspace member ([§2.4](#24-published-package-state)). Returns at M7 if the package moves to its own repo. |
| `bun test` for anything touching dsh services | Those tests must run on the runtime we ship to. Node + vitest, no exceptions. |
| `bun --hot` for the plugin itself | dsh ships its own HMR (`dsh-hmr` watches the profile manifest and both patch files). Use the platform's reload, not a second one. |
| `bun build --compile` | Single-binary distribution is an Option-2 feature. Node is required regardless, so it buys nothing here. |

### 4.3 The rule that keeps this honest

> Bun may touch anything that **never enters the dsh process**. Everything the plugin ships is built
> for Node, tested on Node, and typechecked against Node types.

The dev harness is the one place this tension lives: components run under Bun there and under Node in
production. They are pure React over plain-data props, so the risk is confined to terminal I/O — which
is why the harness renders into a **string buffer**, not a TTY, and why every terminal-owning concern
(raw mode, bracketed paste, resize, the `release` teardown) lives in `tui-runner`, which the harness
never loads. Component behavior is verified twice: `bun test` in the harness, and a Node/vitest render
test at M4.

-----

## 5. Package layout

```
tui/
  PLAN.md                  this document
  README.md                how to run it (M1)
  experiments/             throwaway probes (probe.ts, probe.patch.yml)
  packages/
    app/                   @dsh-tui/app — the bundle: cordis.patch.yml + startup + runner
      cordis.patch.yml     rows over @deepseek-ai/dsh-base
      src/index.ts         tui-runner: terminal ownership, lifecycle, release hook
      src/startup.ts       tui-startup: cmdline flags → ctx.tuiStartup
      src/session.ts       agent creation / adoption / resume
    ui/                    @dsh-tui/ui — Ink components, pure over props
      src/transcript.tsx   <Static> committed rows
      src/live.tsx         in-flight turn region
      src/composer.tsx     input, slash commands, references
      src/approval.tsx     approval prompt
      src/project.ts       SessionEvent → view rows (pure, bun-testable)
  harness/
    dev.tsx                bun --hot entry, renders ui/ against fixtures
    fixtures/*.jsonl       recorded session-event streams
```

`@dsh-tui/ui` is pure: no Cordis, no Node built-ins, no I/O. That purity is what makes the Bun harness
and `bun test` possible, and it is worth defending.

`pnpm-workspace.yaml` gains one line: `- tui/packages/*`.

-----

## 6. Integration seams

Every seam below is an existing, documented upstream API. Nothing here requires an upstream change.

| Need | Seam | Notes |
|---|---|---|
| Create / adopt an agent | `ctx.agents.create({ sessionId, setup })` | `packages/bundle/headless/src/index.ts` is the reference implementation |
| Preset composition | `ctx.agentPresets.mount(agentCtx, 'standard')` | or no preset — upstream notes "the TUI composition, which has no presets" |
| Live transcript | `SessionEvent` stream on the agent's session | required-on-read; unknown types refuse the log unless `ignorable: true` |
| Replay for `--resume` | `ctx.sessionQuery` | the capability ACP lacks; adoption rules are in the headless README |
| **Approvals** | `ctx.on('approval/request', handler)` | a **waterfall** — `ApprovalService.decide` dispatches `ctx.waterfall(scopeTarget(agent, agent), 'approval/request', req, fallback)`. A listener that does not answer **must call `next()`**. Throwing or missing answerers fail closed as `'unavailable'`; the policy `'never'` is decided before dispatch and never reaches us. |
| Slash commands | `ctx.commands` (`CommandRuntime`, `parseCommand`) | same literal commands as Web and ACP |
| Skills | `ctx.skills` via `/name` | `ui-skill` documents identical resolution across surfaces |
| Model / reasoning effort | `installModelSelection` (`@deepseek-ai/dsh-agent`) | as headless does |
| Flags (`--resume`, …) | `ctx.cmdline` → own startup service | mirrors `headless-startup`/`headless-runner`; per AGENTS.md defaulting is an explicit `resolve(request): Spec` step |
| Fatal teardown | `installFailLoud('dsh', process, release)` | `release` restores the terminal; awaited under `FAIL_LOUD_RELEASE_TIMEOUT_MS` (2s) |

Repository conventions that bind this work (from `AGENTS.md`): registrations go through `ctx.effect()` /
`ctx.on()` and a registry's `register()` returns the disposer; waterfall listeners must call `next()`;
model-visible ⟺ logged; no hardcoded tunables — deployment-varying choices are validated `Config`
fields; closed unions end in `assertNever`; opaque cross-boundary ids are branded; files end with
exactly one trailing newline.

-----

## 7. UI architecture

**Ink 7 + React 19**, matching the repo's existing React client composition model.

### 7.1 Render strategy

The known failure mode for agent TUIs is repainting a long transcript on every token. The fix, proven by
Claude Code and Gemini CLI:

- `<Static items={committedRows}>` — committed history. Ink writes these **once** and never repaints
  them; they scroll in the terminal's own scrollback.
- A small dynamic region below it — the in-flight turn: streaming assistant text, the active tool call,
  spinner, token meter, interrupt hint.
- A row moves from dynamic to `<Static>` exactly when its session event **commits**. This mirrors the
  harness's own rule — headless projects `text`/`thinking` from committed assistant messages, "so a
  retried or discarded attempt never reaches the stream". Same rule, same reason.

### 7.2 Component inventory (M4–M6)

| Component | Responsibility |
|---|---|
| `Transcript` | committed rows in `<Static>` |
| `LiveTurn` | streaming text, reasoning, active tool call, spinner |
| `ToolCard` | per-tool presentation; mirrors Web's keyed per-tool views |
| `Composer` | multiline input, `/` command palette, `@` file references, paste |
| `ApprovalPrompt` | allow-once / reject, bound to the tool call it interrupts |
| `StatusBar` | model, session id, cwd, token meter, approval policy |
| `DiffView` | edit/write tool diffs |

### 7.3 Terminal ownership

`tui-runner` owns and must restore: raw mode, alternate screen (if used), bracketed paste, the keyboard
protocol, and cursor visibility. Restoration runs from **three** paths — normal exit, Cordis disposal,
and the `installFailLoud` `release` hook — all delegating to one idempotent `releaseTerminal()`.
`app-boot`'s own JSDoc explains why the third path exists: the Loader mounts entries concurrently, so a
sibling entry can reject while we already hold the terminal.

-----

## 8. Development loop

### 8.1 Loading the plugin without publishing

`app-boot`: "Inserted plugin names may be absolute filesystem paths, file URLs, or package specifiers.
Patch loading converts absolute paths and patch-relative `./` or `../` paths to file URLs within
`insert` rows." So a patch overlay can insert our source directly:

```yaml
# tui/experiments/probe.patch.yml
- insert:
    - id: tui-probe
      name: ./probe.ts
```

```sh
dsh --profile headless --patch ./tui/experiments/probe.patch.yml "say hi"
```

Under the source launch (`node --import tsx/esm`) this should load `.ts` with no build step.
**Unverified** — the probe was written and staged but Node vanished before it ran ([§2.3](#23-toolchain-loss-and-recovery)).
It is the first thing to run once Node is back, and M0's acceptance criterion.

### 8.2 The three loops

| Loop | Command | Feedback |
|---|---|---|
| Component | `bun --hot tui/harness/dev.tsx` | sub-second, no dsh, no key |
| Pure logic | `bun test tui/packages/ui` | ~100ms |
| Integration | `dsh --profile tui` with `dsh-hmr` enabled | seconds; HMR watches the profile manifest and both patch files |

-----

## 9. Milestones

Each milestone states the acceptance criterion that closes it.

| # | Milestone | Acceptance |
|---|---|---|
| ~~**M0**~~ | ~~Restore Node; run the staged probe~~ | **done** — Node 26.9.0 + 24.21.0; 15 of 19 services reachable ([§2.3.1](#231-m0-result--verified-service-reachability)) |
| ~~**M1**~~ | ~~Scaffold `@dsh-tui/app` + `@dsh-tui/ui`; workspace line; Bun-import guard~~ | **done** — boots and exits 0 on Node 26.9.0 and 24.21.0; `--help`, `--preset`, and flag rejection verified; 8 `bun test` assertions in 32ms; the guard proven to fail on a planted `Bun.file` |
| ~~**M2**~~ | ~~`tui-runner`: agent creation, terminal ownership, `release` teardown, Ctrl-C~~ | **done** — `tui/scripts/pty-smoke.sh` passes on 26.9.0 and 24.21.0: renders, accepts typed input, Ctrl-C ×2 exits 0, and `[?2004l` confirms the terminal was handed back. 12 `bun test` assertions in 32ms |
| **M3** | Fixture recorder + Bun harness | `bun --hot` renders a recorded session with no dsh runtime |
| **M4** | Transcript + LiveTurn; commit-boundary promotion | 10k-row transcript scrolls without repaint; Node/vitest render test passes |
| **M5** | Composer: input, `/` commands, `@` references | `/goal` and a skill invoke identically to Web |
| **M6** | Approvals, model selection, status bar | approval waterfall answers correctly; non-answers call `next()` |
| **M7** | Publish out-of-tree; exact resume is implemented through `agents.resume` and query replay | `dsh tui --resume <id>` replays history; `bun build --target=node` output installs through `dsh plugin --profile tui add` |

M7's publish step is gated on upstream version alignment ([§2.4](#24-published-package-state)).

-----

## 10. Testing strategy

| Layer | Runner | Scope |
|---|---|---|
| Pure projection and layout | `bun test` | event→row projection, folding, ANSI width, keymaps, diffs |
| Component render | Node + vitest + `ink-testing-library` | snapshot of rendered frames against fixtures |
| Plugin integration | Node + vitest | mount into a test Cordis tree, assert registrations and disposers |
| Terminal lifecycle | Node + vitest | raw mode set/restored across all three teardown paths |
| End to end | `dsh --profile tui` + recorded LLM replay | keyless, following `snapshots/AGENTS.md` conventions |

Fork-local tests live under `tui/`; the repository's own suites stay untouched, so `pnpm run test`
continues to mean exactly what upstream means by it.

-----

## 11. Risks

| Risk | Severity | Mitigation |
|---|---|---|
| ~~Node toolchain absent~~ | resolved | Node 26.9.0 + 24.21.0 installed under `~/.local/opt` ([§2.3](#23-toolchain-loss-and-recovery)) |
| Pre-stable `@deepseek-ai/dsh-*` APIs churn | high | develop in-repo against the workspace; pin on publish; AGENTS.md: "update every consumer" |
| `SessionEventMap` is required-on-read; unknown events refuse the log | high | switch on discriminant tags, `assertNever` closed unions, document the merge-extensible default |
| Published versions fragmented | medium | M7 gate; in-repo until then |
| Ink repaint cost on long transcripts | medium | `<Static>` + commit-boundary promotion; M4 measures 10k rows |
| **Tools are broken under the tsx source launch** | medium (upstream, not ours) | `dsh-tools` keys its scheduler with `Symbol('…')`; the source launch gets two instances of the package, so the lookup misses and every tool call fails with `reading 'prepare'`. Upstream `headless` fails identically from source and works from `lib/`. Mitigation: `tui/scripts/build.sh` plus `cordis.built.patch.yml`, exercised by `pty-smoke.sh --built` |
| ~~Ink unresolvable inside the dsh process~~ | resolved | Ink 7.1.1 + React 19.3.0 import and render inside the harness; the package-resolution generation does not interfere, provided the importing package declares the dependency |
| Bun/Node divergence in components | medium | components pure over props; harness renders to a string buffer; double-verified at M4 |
| Upstream ships its own TUI | medium | our seams are all public API; adopt or diverge cheaply |
| Intel-mac toolchain gaps (`@pnpm/exe` already broke) | low | `npx pnpm@11.7.0`; document in the runbook |

-----

## 12. Upstream sync policy

- Local branch `main` tracks `origin/master` (`deepseek-ai/deepseek-harness`), so `git pull` still syncs.
- Upstream-owned files we modify: `pnpm-workspace.yaml` adds the TUI workspace glob, and `pnpm-lock.yaml` records its declared dependency graph. The lockfile is a required second exception: pnpm owns reproducible resolution, including React types, Ink component tests, and the harness services the app imports. Source, fixtures, checks, and documentation remain under `tui/`.
- Keep upstream lockfile importers unchanged when adding TUI React dependencies; the upstream DOM test graph uses React 18, while Ink uses React 19. Validate the edited lockfile with a frozen install.
- With a fork remote: `git remote rename origin upstream`, add the fork as `origin`, then
  `git branch -u origin/main` and sync with `git fetch upstream && git merge upstream/master`.
- Re-run `tsc -b tsconfig.host.json` + `tsdown` after any sync that touches `packages/`; `lib/` is not
  committed and stale output shows up as "failed to import".

-----

## 13. Runbook

### 13.1 Install the toolchain

Direct tarball install — faster and more predictable than `nvm install`, whose `ls-remote` shell-parses
the entire Node release index:

```sh
mkdir -p ~/.local/opt ~/.local/bin && cd ~/.local/opt
for V in v26.9.0 v24.21.0; do
  curl -fsS --max-time 240 -o "n-$V.tar.gz" "https://nodejs.org/dist/$V/node-$V-darwin-x64.tar.gz"
  tar -xzf "n-$V.tar.gz" && mv "node-$V-darwin-x64" "node-$V" && rm "n-$V.tar.gz"
done
ln -sf ~/.local/opt/node-v26.9.0/bin/node  ~/.local/bin/node
ln -sf ~/.local/opt/node-v26.9.0/bin/npm   ~/.local/bin/npm
ln -sf ~/.local/opt/node-v26.9.0/bin/npx   ~/.local/bin/npx
ln -sf ~/.local/opt/node-v26.9.0/bin/node  ~/.local/bin/node26
ln -sf ~/.local/opt/node-v24.21.0/bin/node ~/.local/bin/node24
```

Develop on `node` (26.9.0); check the floor with `node24` before every push. Adjust `darwin-x64` for
other hosts.

### 13.2 Build from a clean checkout

```sh
npx pnpm@11.7.0 install
node --max-old-space-size=4096 ./node_modules/typescript/bin/tsc -b tsconfig.host.json
./node_modules/.bin/tsdown --env.DSH_BUILD_FACE host
./node_modules/.bin/tsx native/system/scripts/build.ts --host-addon-only
```

### 13.3 Verify

```sh
node --import tsx/esm apps/cli/src/bin.ts --profile headless "say hi"
# expect: MISSING_CREDENTIAL without a key; a real answer with one in .env

for N in node26 node24; do
  "$N" --import tsx/esm apps/cli/src/bin.ts --profile headless \
    --patch ./tui/experiments/probe.patch.yml "say hi" 2>&1 | grep TUI-PROBE
done
# expect: identical reachability on both engines
```

Notes: pnpm 12 cannot run this repo's pinned `@pnpm/exe@11.7.0` on darwin-x64 — use
`npx pnpm@11.7.0`. `typert` and `typert-gateway` import failures are expected until a full
`pnpm run build` runs its generators.

-----

## 14. Open questions

1. ~~**Presets or not.**~~ Partly settled by [§2.3.1](#231-m0-result--verified-service-reachability):
   `agentPresets` is not a base row, so presets are an opt-in the `tui` patch adds. Remaining call: add
   `agentPresets` and mount `standard`, or compose the tool roster directly in the patch.
2. **Alternate screen.** Full-screen app, or inline like Claude Code? Inline keeps native scrollback and
   pairs naturally with `<Static>`; full-screen enables panes. Recommend inline through M6.
3. **Multi-session.** One session per process (like headless), or in-TUI session switching via
   `ctx.sessionQuery`? Recommend one session through M6, switching at M7.
4. **Locale.** Client UI copy is locale-owned upstream (`verify-client-ui-i18n`). Do we route TUI strings
   through typed dictionaries from the start, or English-only until M7?
5. **Terminal tools.** Does the TUI surface `terminal_*` tool output as a pane? `tool-terminal`'s README
   states its schema has no TUI surface, so this would be presentation-only.
