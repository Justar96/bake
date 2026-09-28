# Bun in Bake

Bun builds the TUI, runs development tools, and drives real Node processes for qualification. The built application uses production React. The [performance report](packages/app/performance/README.md) owns measured startup, input, and memory results.

The workspace pins stable **Bun 1.4.2** in `package.json`. The [Bun bundler](https://bun.com/docs/bundler) and [terminal API](https://bun.com/docs/runtime/child-process#terminal-pty-support) document the APIs used below.

## Runtime ownership

Bun may run tooling that stays outside the dsh process. Harness runs on Node: `app-boot` uses `node-addon-require-builtin` to reach V8 current-context symbols for `internal/modules/esm/loader`. Bun's JavaScriptCore engine cannot load that V8 integration. Moving the agent process to Bun would require changing and validating that loader integration.

| Work | Runtime and benefit |
|---|---|
| Bundle the application | `Bun.build` compiles TypeScript/JSX and minifies production output; Node executes the resulting ESM. |
| Develop components | Bun's hot component harness gives a short edit/render loop without booting Harness. |
| Pure modules and tooling tests | `bun test` runs Bake's `.test.ts` files, including the workspace scripts' tooling tests. PTY tests start separate Node children. |
| Measure the application | Bun coordinates fixed workloads and owns a native PTY; Node runs Harness and authors durable fixtures. |
| Exercise Harness services and Ink integration | Node/vitest remains the owner of `.spec.ts(x)` tests, including script specs that load Cordis services, the Node module resolver, or built `lib/` output. The resolver-identity check also runs on Node. |
| Manage dependencies | Bun owns all source-workspace packages, `bun.lock`, patches, and installation. Use `bun install --frozen-lockfile`. |

CI reads the Bun pin from `package.json`. Workspace checks reject unpinned versions, duplicate package names, competing lockfiles, and missing workspace dependencies. An external dependency that two or more non-vendor manifests declare takes its range from the root `catalog` and is referenced as `catalog:`; peer ranges and `vendor/` manifests keep literal ranges. The same check rejects a `catalog:` reference without an entry, an unused entry, and a shared literal range unless `scripts/gen-workspace.ts` allowlists it with a reason. A catalog edit changes `bun.lock`, so run `bun install` and commit both files, or `--frozen-lockfile` fails. Bun upgrades require build and built-PTY checks because compiler changes affect the JavaScript Node executes. Runtime separation preserves Harness ownership; it does not make compiler upgrades validation-free.

## Shared production build

[`scripts/build.ts`](scripts/build.ts) owns the app, recorder, and diagnostic bundling options. It inlines `@dsh-tui/ui` and keeps `@deepseek-ai/*`, Ink, React, and Commander external, preserving host singleton identity. Production bundles compile with production JSX and minification. The dispatcher supplies `NODE_ENV=production` to built app, recorder, and PTY launches so external React and Ink use the same mode.

```sh
bun run build
bun run start
./tui/scripts/tui.ts perf --workload fresh --workload typical --samples 3 --output /tmp/bake-bun-native-production.json
```

The [development guide](../../CONTRIBUTING.md) owns the build/run workflow. `start` uses existing artifacts; `dev:tui` rebuilds the workspace before starting the agent. Bake's profile commands use `~/.bake` unless `DSH_HOME` is set. `dev` runs the separate hot component preview. The performance command accepts `--mode development` for a controlled baseline; it uses the same bundler and inputs. Production compilation reduces bundle size and development-renderer work. It does not bound the allocations required to render a complete history; the large-history failure remains recorded in the performance report.

The [build test](packages/app/tests/build.test.ts) executes compiled JSX under Node with external production React and rejects a missing entry. Restoring development bundling makes its production-runtime assertion fail. Strict tooling programs include the shared build, dispatcher, diagnostic, and Bun test drivers; Bun declarations are workspace development dependencies.

## Native PTY drivers

[`performance/terminal.ts`](packages/app/performance/terminal.ts) uses `Bun.spawn({ terminal: ... })` directly. Bun supplies the 120-column, 40-row terminal and the measured Node PID. The driver retains bounded output, counts historical answers, samples the Node heap, and awaits Node exit before closing the terminal. No `script`, `cat`, shell pipeline, PID file, or exit-status file is needed.

The driver distinguishes process exit from terminal EOF. Bun leaves `exitCode` null for signal termination, so the driver waits on `exited` and records `signalCode` independently. Bun reports Linux's EIO at the last slave close as PTY status 1, which the driver accepts only after a clean exit. The [PTY tests](packages/app/tests/performance-terminal.test.ts) cover native TTY dimensions, Node runtime identity, memory samples, escape stripping, failed exits, fatal signals, timeout cleanup, and cancellation.

The end-to-end [`pty-smoke.ts`](scripts/pty-smoke.ts) drives the built profile through the same API, without `bun:ffi`. `Bun.Terminal` makes the Node child a session leader with the PTY as its controlling terminal, so `resize()` reaches it as SIGWINCH. Waits re-read the screen, stripped by `Bun.stripANSI`, when output arrives, the child exits, or the stream ends; a 20 ms re-check covers predicates that also read files. Teardown waits for both the exit and the end of the stream, whatever its PTY status, so its checks read the whole transcript; the exit code and signal come from the process. It compares the input, output, local, and control termios flags from before the app started with those after it exits. `ready` first requires the running app to have changed them, so the comparison cannot pass against flags the driver cannot observe. `Bun.Terminal` does not expose control characters or line speeds; Node restores them in the same `tcsetattr` call as the flags.

Bun does not export the terminal's `name` option as `TERM`, so both drivers set `TERM` in the child environment. Both run on macOS and Linux only: `Bun.Terminal` is POSIX-only, and the performance diagnostic's Node memory preload uses SIGUSR2. Windows/ConPTY qualification is not claimed.

## Further tooling opportunities

Bun coverage reporting can help identify missing pure-projection cases before adopting a coverage threshold. Coverage policy needs its own focused change and negative control. JUnit output becomes useful when a TUI CI job consumes it. Test sharding or concurrency should follow measured suite cost, especially because process-level performance samples must run without competing CPU-heavy jobs.

`--packages=external` is unsuitable here: it would also externalize `@dsh-tui/ui`, which must be inlined. `--compile` creates a Bun executable, while the application is an in-process Node plugin loaded through a `dsh` profile. Bun's root workspace settings are independent of the runtime's external-profile package-manager configuration.
