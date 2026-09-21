# Bun in this fork

What Bun is used for, what it is not used for, and which 1.4 features earn their place. Every claim here was checked against the installed binary.

**Installed: 1.4.2** (upgraded from 1.3.14; registry `latest` was 1.4.2 at the time).

## The standing rule

> Bun may touch anything that never enters the dsh process.

Bun **cannot** run dsh. `app-boot` uses `node-addon-require-builtin`, which reaches V8 current-context symbols to obtain `internal/modules/esm/loader`. JavaScriptCore has no such symbols, and the failure happens during host preparation, before any plugin mounts. This is not a gap to be closed by a newer Bun; it is an engine difference.

So Bun is toolchain only: bundling, pure-module tests, the component harness, and fixture tooling. Never `bun install` at the repository root — that discards pnpm `overrides`, `link:vendor/*`, and `patches/`.

Upgrading Bun is therefore low-risk by construction: no version of it reaches the runtime.

## Verified after the 1.4.2 upgrade

| Path | Result |
| --- | --- |
| `bun test .test.ts` | 17 pass, 0 fail, 27 ms |
| `bun build` in `scripts/build.sh` | unchanged, still produces the three bundles |
| prototype scripts (`node`) | unaffected; they run on Node by design |

## Adopt

### 1. Coverage gate for pure modules

`bun test --coverage` works today and `bunfig.toml` thresholds are enforced with a real exit code:

```
threshold 0.95 vs 40% coverage -> exit 1
threshold 0.30 vs 40% coverage -> exit 0
```

Current state of the fork's pure modules:

```
All files                      |   95.83 |   92.45 |
 packages/ui/src/editor.ts     |  100.00 |  100.00 |
 packages/ui/src/format.ts     |  100.00 |  100.00 |
 packages/ui/src/project.ts    |   83.33 |   69.81 | 47,49-54,72-73,100-106
 packages/ui/src/transcript.ts |  100.00 |  100.00 |
```

Upstream's CI gate is per-file 100% on `packages/*/*/src`; the fork sits outside that gate and has drifted below it. The uncovered lines in `project.ts` are not incidental — they are the `tool/call` projection, the error-notice branch, and `resultText`'s content-block handling. All three are user-visible, and the session-log projection is the module where a mistake silently shows the wrong thing.

Adopt in two steps: add the missing `project.ts` tests, then add `tui/bunfig.toml` with a threshold and wire `--coverage` into `check.sh`. Setting the threshold before the tests exist would only make the gate red.

### 2. `--reporter=junit --reporter-outfile`

Free once the fork publishes CI results. No work until there is a CI job to consume it.

## Reject, with reasons

### `--packages=external`

Tempting as a replacement for the explicit external list in `scripts/build.sh`, and wrong here. It externalizes every bare specifier, including `@dsh-tui/ui`, which the build deliberately **inlines** so the plugin ships as one file. The hand-written list (`@deepseek-ai/*`, `ink`, `react`, `commander`) expresses an intent the flag cannot: inline our own packages, externalize everything the host already has.

### `--compile` standalone executables

The plugin is loaded in-process by Node through `--patch`. A Bun executable cannot be loaded that way, and a separate binary would reintroduce the out-of-process split that was rejected because ACP has no `session/load`.

### `--shard` / `--timings` / `--concurrent`

Built for suites that take minutes. Ours takes 27 ms.

### `bun why`, `bun audit`, catalogs, isolated installs

All read or write `bun.lock`. The workspace is pnpm-managed and must stay that way.

## Worth considering later

### Bun Shell for Windows-portable scripts

`scripts/{build,check,pty-smoke}.sh` are bash, so a Windows contributor cannot run the gate. Bun Shell (`$`) runs the same script on Windows, and scripts never enter the dsh process, so the rule permits it.

Two caveats before anyone starts: `pty-smoke.py` uses `openpty` and is Unix-only regardless of shell, and upstream already carries `check:windows-wine`, so the fork should match however upstream expects Windows to be exercised rather than inventing a second answer.
