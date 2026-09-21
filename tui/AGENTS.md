# AGENTS.md — tui/

Fork-local terminal UI for DeepSeek Harness. Read [PLAN.md](PLAN.md) for the decisions and their
evidence, and [DESIGN.md](DESIGN.md) before changing wiring.

This file governs `tui/` only. The repository root [AGENTS.md](../AGENTS.md) still applies to everything
it covers; where the two disagree about `tui/`, this file wins.

## The one rule that shapes everything else

**`tui/` is the only directory this fork owns.** Upstream is `deepseek-ai/deepseek-harness` and we track
it for updates. Every change outside `tui/` is a future merge conflict, so it needs a reason that
survives the next sync.

The sanctioned exceptions are the TUI glob in `pnpm-workspace.yaml` and its dependency entries in
`pnpm-lock.yaml`, as recorded in PLAN.md §12. Record any further exception there with its justification.

Never edit upstream `packages/`, `apps/`, `docs/`, `scripts/`, or the root `AGENTS.md`/`CLAUDE.md`. If the
TUI appears to need an upstream change, it is nearly always reachable through an existing seam — check
[DESIGN.md](DESIGN.md) §3 first, and treat a genuine gap as a finding worth writing down.

## Runtime: Node only

The harness cannot run on Bun. `app-boot` reaches V8 current-context symbols for
`internal/modules/esm/loader`, which JavaScriptCore does not have, and it does so during host
preparation before the first plugin mounts ([PLAN.md §2.2](PLAN.md#22-bun-cannot-run-the-harness)).

- Shipped plugin code imports **no** `bun:*` module and touches **no** `Bun.*` global. A lint rule
  enforces this; do not add an exemption.
- Develop on `node` (26.9.0); verify the floor with `node24` before every push. Engines: `^22.19.0 || >=24.0.0`.
- Bun is for the component harness, `bun test` on pure modules, fixture tooling, and distribution
  bundling — everything that never enters the dsh process ([PLAN.md §4](PLAN.md#4-bun-integration)).
- Never run `bun install` at the repository root. The root is pnpm's: `overrides`, `link:vendor/*`,
  `patches/`, and `onlyBuiltDependencies` do not transfer, and CI is pnpm.

## Cordis rules that bite here

These cost real debugging time during M0; they are not restatements of the primer.

- **`inject` has no optional form.** Every entry is required, it gates `ctx` property access, and an
  unsatisfiable entry leaves the plugin `pending` forever with no error. `{ required, optional }` is not
  a recognized shape — those two words are read as service names. To use a service that may be absent,
  declare nothing and read it with `ctx.get(name)`.
- **Nothing is reachable before settlement.** Loader siblings mount concurrently, so a probe in `apply`
  sees only `loader`. Wait with `await ctx.get('loader')?.await()` before creating an Agent or reading
  optional services, exactly as `packages/bundle/headless/src/index.ts` does.
- **`approval/request` is a waterfall, not a callback.** Answer only for agents this TUI owns; for
  anything else call `next()`. Returning without calling it silently starves every other answerer
  ([semantics](../docs/cordis-primer.md#cordis-waterfall-semantics)).
- **Registrations are effects.** Contribute through `ctx.effect()` / `ctx.on()` and keep the returned
  disposer. A listener that outlives its plugin is a leak and, for a terminal owner, a corrupted terminal.
- **Session events carry `surfaceOp`.** `append` is new history; anything else is a rewrite — compaction
  replacing earlier entries. A transcript that has already committed a row cannot retract it, so handle
  rewrites deliberately ([DESIGN.md §4.3](DESIGN.md#43-the-rewrite-problem)).

## Display harness state; do not model it

The TUI is a view over the harness. Every fact it shows has one authority, and reading it is always
correct where inferring it is only usually correct ([DESIGN.md §3b](DESIGN.md#3b-one-authority-per-fact)).

- Agent activity is `agent.status` plus the `agent/status` event — never derived from `turn/start` and
  `turn/end`. `running` covers draining, closing, and checkpointing, so a turn-event copy disagrees
  exactly when it matters, during cancellation convergence.
- Turn and step boundaries are the `turnBoundary` projection; pending input is the `inbox` projection;
  tokens and context are `tokenUsage` and `contextPressure`. Read them with
  `sessionProjections.stateOf(session, key)` and watch with `onChanged()`. The values are live: never
  mutate them.
- The transcript is the session log through `project()`. There is no second message list.
- Action vocabularies are the harness's: `agent.cancel({ kind: 'user' }, …)` takes a tagged
  `AgentCancelCause`, not a string, and `followup` versus `steer` is chosen by reading `agent.status` at
  call time rather than a mirrored copy.

When the UI needs state the harness already folds, consume the projection. Adding a private reducer for
it is how two sources of truth start.

## Terminal ownership

`tui-runner` owns raw mode, bracketed paste, the keyboard protocol, cursor visibility, and the alternate
screen if used. All of it is restored through **one idempotent `releaseTerminal()`** reachable from three
paths: normal exit, Cordis disposal, and the `installFailLoud` `release` hook.

The third path is not optional. `app-boot`'s own JSDoc explains why: siblings mount concurrently, so
another entry can reject while we already hold the terminal, and exiting from that handler would strand
raw mode on the user's shell. Any change to teardown keeps all three paths working, and the test at
[DESIGN.md §7](DESIGN.md#7-teardown) proves it.

Never write to stdout outside the renderer. Diagnostics go to stderr.

## Layout

```
tui/
  AGENTS.md  PLAN.md  DESIGN.md
  experiments/            throwaway probes; not shipped, not tested
  packages/app/           @dsh-tui/app — bundle patch, startup, runner, terminal ownership
  packages/ui/            @dsh-tui/ui  — Ink components, pure over props
  harness/                bun --hot component loop + recorded fixtures
```

**`packages/ui` stays pure**: no Cordis, no Node built-ins, no I/O, no clock. Props in, elements out.
That purity is what makes the Bun harness and `bun test` possible, and it is the first thing to break
under deadline pressure. Anything impure belongs in `packages/app`.

## Testing

| Scope | Runner |
|---|---|
| Pure projection, folding, width math, keymaps, diffs | `bun test` |
| Component render | Node + vitest + `ink-testing-library` |
| Plugin integration, terminal lifecycle | Node + vitest |
| End to end | `dsh --profile tui` with recorded LLM replay |

Anything touching a dsh service runs on Node. Testing on a runtime we cannot ship to is how drift starts.

Tests live under `tui/`. Never modify upstream suites — `pnpm run test` must keep meaning what upstream
means by it.

## Commands

```sh
node --import tsx/esm apps/cli/src/bin.ts --profile tui \
  --patch ./tui/packages/app/cordis.patch.yml    # from source: fast, but NO tools (see below)
./tui/scripts/build.sh                           # bundle the plugin to Node ESM
node apps/cli/lib/bin.js --profile tui \
  --patch ./tui/packages/app/cordis.built.patch.yml   # production path: tools work
./tui/scripts/check.sh                         # strict types, pure Bun tests, Node integration
./tui/scripts/pty-smoke.sh                       # built profile: keyless replay and resume
./tui/scripts/pty-smoke.sh --live                # real DeepSeek tool turn using root .env
./tui/scripts/pty-smoke.sh node24                # the engine floor
bun --hot tui/packages/harness/dev.tsx           # component loop: no dsh, no agent, no key
bun tui/packages/harness/dev.tsx --replay --locale zh   # watch rows arrive; check a dictionary
node apps/cli/lib/bin.js --profile headless \
  --patch ./tui/packages/harness/record.patch.yml "…"   # record a fixture (needs a key)
```

`check.sh` is the gate: strict types for all three packages, then each test suite on its own runtime.
Run it before every push.

**Tools do not work under the tsx source launch.** `dsh-tools` keys its scheduler with `Symbol('…')`
rather than `Symbol.for('…')`, and the source launch ends up with two module instances of that package,
so `ctx.tools[TOOL_RUNTIME_SCHEDULER]` reads `undefined` and every tool call fails with
`Cannot read properties of undefined (reading 'prepare')`. This is not ours: upstream
`dsh --profile headless` fails identically from source and succeeds from `lib/`. Verify anything
touching tools on the built path.

**A piped run cannot test rendering.** Ink needs a TTY for raw mode, so a non-TTY invocation is refused
by design. Use `pty-smoke.sh`, which allocates one with Python `openpty` and waits for Ink paste mode before sending
keys — input typed before the app mounts is swallowed by the terminal and never reaches `useInput`.

A full `pnpm run build` is required once per checkout, not just `tsc` + `tsdown`: the generators it runs
are what let `typert` import at all.


Rebuild after any upstream sync that touches `packages/`; `lib/` is not committed and stale output
surfaces as "failed to import" ([PLAN.md §13.2](PLAN.md#132-build-from-a-clean-checkout)).

## Prose

Follow [dsh-prose-standard](../.agents/skills/dsh-prose-standard/SKILL.md). Every module and export gets
concise JSDoc stating its contract; function-like exports document `@param`/`@returns`. State current
behavior, not history: no "used to", no "this now", no review narration, no metaphors. Comments explain
why, never what the code already says. Files end with exactly one trailing newline.
