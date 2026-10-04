# Bake profile launcher

The `@deepseek-ai/dsh` package launches Bake's Node process through named Cordis profiles. `tui` starts the terminal agent; `headless` runs a single task and exits; `desktop` is the long-lived bridge the Bake Desktop app launches. Each profile initializes on first use. The package identifier and `dsh` command remain compatible with the shared runtime's package resolution.

## Profiles

Each `$DSH_HOME/profiles/<name>` contains a package manifest listing ordered bundles and an optional user `cordis.patch.yml`. The terminal template selects `@deepseek-ai/dsh-base` and `@dsh-tui/app`. The headless template selects base and `@deepseek-ai/dsh-headless`, and the desktop template selects base and `@deepseek-ai/dsh-desktop`.

Layers apply in order: bundle patches, profile patch, home patch, then invocation `--patch` files. Missing bundles fail loudly. Existing user profiles keep their bundle selection; startup does not rewrite it to match a template.

`--from-default-profile <template>` creates a custom profile at a new name from one shipped template. `--dump-default-config` and `--dump-config` inspect composition without booting the agent. The launcher forwards application arguments after its own flags without interpreting them.

Use `--dump-default-config` and `--dump-config` to inspect the composed tree without booting it. `--dump-config-schema` imports the composed tree's declared plugin schemas and prints JSON Schema for entries and patches instead of configuration values; read the [schema-dump safety and scope](reference/README.md#config-schema-dump) before inspecting untrusted plugins.

## Development

From the repository root, `bun run build` builds the runtime and terminal bundle. `bun run start` runs the terminal with production React. `bun run start --help` shows its options. Use `bun apps/tui/scripts/tui.ts e2e` for recorded, keyless verification through a real PTY.

`bun run dsh` runs the built launcher with Bake's `~/.bake` home, shared with `bun run start`; an explicit `DSH_HOME` overrides it. The npm `dsh` entry and direct `node apps/cli/lib/bin.js` launches use the same default home. Rebuild after launcher changes.

The [direct download archive](../../distribution/README.md) installs a `bake` wrapper around this launcher. It opens the `tui` profile by default, forwards `bake tui`, `bake headless`, `bake plugin`, `bake update`, and `bake --profile` to the profile CLI, and selects `~/.bake` unless `DSH_HOME` is set. `dsh update` replaces that install with the newest signed release, and `dsh update --check` only reports, exiting 10 when a newer release is available; a leading `update` is reserved for this, as `plugin` is, so a profile named `update` is reached with `--profile update`. See [Updating an install](../../distribution/README.md#updating-an-install).

Interactive updates draw their progress on stderr: a heading, a line for each finished step, and one live row for the step running, whose meter fills with the download, then a summary; a failed step is marked before the error. `BAKE_NO_ANIMATION=1` disables motion; `NO_COLOR=1` disables color. Checks, redirected output, CI, and dumb terminals keep plain reports.

## Startup and shutdown

The npm `dsh` entry restarts Node with `--report-exclude-env --report-exclude-network --diagnostic-dir=<Bake home>/diagnostics` before loading the application. It creates the diagnostics directory owner-only when possible. On POSIX, replacement preserves the process ID and terminal signal delivery; on Windows, the parent and child share console events, and the parent waits for the child’s exit status. Release and development launchers already supply these arguments and run directly. The arguments are not added to `NODE_OPTIONS`, so commands started by the agent do not inherit them. The runtime watchdog enables fatal-error reports; heap snapshots remain opt-in.

The entry turns on Node's module compile cache before it loads the application, so later launches reuse the compiled code of the launcher and the profile's modules. The cache is Node's default `<tmpdir>/node-compile-cache`, used only when the current user owns it and no other user can write to it. Node keys each entry by its version, the user, and the file's path and content. The launcher writes the cache once the profile has booted, and Node adds modules loaded later when the process exits. `NODE_DISABLE_COMPILE_CACHE=1` turns the cache off, and a `NODE_COMPILE_CACHE` directory replaces the default. The processes Bake starts do not inherit the cache. A missing or unwritable cache never stops startup.

Each launch restores the profile's root `cordis.yml` to an empty entry list. An unchanged file is left alone. A changed one is replaced atomically, so a concurrent launch of the same profile never reads a partial file.

Until the profile is ready, an unhandled rejection or an uncaught exception fails the launch: one `dsh: fatal load failure:` or `dsh: fatal uncaught exception:` line on stderr, a bounded release of the terminal, and exit status 1. Readiness is committed once the plugin tree has booted. From then on an uncaught exception stays fatal, but an unhandled rejection, such as an async listener that Cordis `emit` does not await, no longer ends the session. The launcher appends a record to `rejections.<YYYYMMDD>.<HHMMSS>.<pid>.jsonl` in `<Bake home>/diagnostics`, the directory that also holds the runtime watchdog's records and Node's fatal-error reports. The file is created owner-only, and each record holds only the error's name, message, and stack. The launcher logs the rejection through the Cordis logger and offers it to the running surface through the `app/unhandled-rejection` event: the terminal names it on its notice line. With no surface to show it, as in `headless`, one `dsh: warning: unhandled rejection after startup: …; the session continues; details in <record>` line goes to stderr, never stdout. Each distinct error is shown and recorded once, and at most five per minute; repeats and the excess are counted in the next record's `suppressed`. The same holds while the app shuts down, so a rejection during disposal neither changes the exit status nor goes unreported: once the terminal has been released, it gets the stderr line.

`SIGINT` exits 130 and `SIGTERM` exits 0, each after disposing the application: disposal flushes the session and stops managed subprocesses. Before unloading the plugin tree, the launcher awaits `app/shutdown` so active agents can cancel and write their closing events while session persistence is still mounted. Disposal is bounded at 5 seconds, and a second signal forces exit. `SIGHUP`, which arrives when the terminal closes (on Windows, when the console closes), runs the same disposal and exits 129. Writes to the lost terminal fail, so from then on the launcher ignores stdio errors. A repeated `SIGHUP` waits for the running disposal instead of forcing exit.

## External plugins

`bun run dsh plugin --profile <name>` delegates package operations to the profile's pnpm configuration. This is separate from Bake's Bun-managed source workspace. The profile manager owns installation approval, locking, rollback, and configuration reload; see [Plugin Manager](../../packages/boot/plugin-manager/README.md).

The launcher preserves startup diagnostics, proxy settings, profile reload, and bounded shutdown. [App boot](../../packages/boot/app-boot/README.md) owns shared startup behavior; [`src/args.ts`](src/args.ts) owns argument parsing.
