# Bake profile launcher

English | [中文](README.zh.md)

The `@deepseek-ai/dsh` package launches Bake's Node process through named Cordis profiles. `tui` starts the terminal agent; `headless` runs a single task and exits; `desktop` is the long-lived bridge the Bake Desktop app launches. Each profile initializes on first use. The package identifier and `dsh` command remain compatible with the shared runtime's package resolution.

## Profiles

Each `$DSH_HOME/profiles/<name>` contains a package manifest listing ordered bundles and an optional user `cordis.patch.yml`. The terminal template selects `@deepseek-ai/dsh-base` and `@dsh-tui/app`. The headless template selects base and `@deepseek-ai/dsh-headless`, and the desktop template selects base and `@deepseek-ai/dsh-desktop`.

Layers apply in order: bundle patches, profile patch, home patch, then invocation `--patch` files. Missing bundles fail loudly. Existing user profiles keep their bundle selection; startup does not rewrite it to match a template.

`--from-default-profile <template>` creates a custom profile at a new name from one shipped template. `--dump-default-config` and `--dump-config` inspect composition without booting the agent. The launcher forwards application arguments after its own flags without interpreting them.

## Development

From the repository root, `bun run build` builds the runtime and terminal bundle. `bun run start` runs the terminal with production React. `bun run start --help` shows its options. Use `bun apps/tui/scripts/tui.ts e2e` for recorded, keyless verification through a real PTY.

`bun run dsh` runs the built launcher with Bake's `~/.bake` home, shared with `bun run start`; an explicit `DSH_HOME` overrides it. The raw shared-runtime launcher retains its upstream home defaults. Use the Bun commands for Bake, and rebuild after launcher changes.

The [direct download archive](../../distribution/README.md) installs a `bake` wrapper around this launcher. It opens the `tui` profile by default, forwards `bake tui`, `bake headless`, `bake plugin`, `bake update`, and `bake --profile` to the profile CLI, and selects `~/.bake` unless `DSH_HOME` is set. `dsh update` replaces that install with the newest signed release, and `dsh update --check` only reports, exiting 10 when a newer release is available; a leading `update` is reserved for this, as `plugin` is, so a profile named `update` is reached with `--profile update`. See [Updating an install](../../distribution/README.md#updating-an-install).

Interactive updates show an ASCII bakery animation on stderr and clear it before the result or an error. `BAKE_NO_ANIMATION=1` disables motion; `NO_COLOR=1` disables color. Checks, redirected output, CI, and dumb terminals keep plain reports.

## External plugins

`bun run dsh plugin --profile <name>` delegates package operations to the profile's pnpm configuration. This is separate from Bake's Bun-managed source workspace. The profile manager owns installation approval, locking, rollback, and configuration reload; see [Plugin Manager](../../packages/boot/plugin-manager/README.md).

The launcher preserves startup diagnostics, proxy settings, profile reload, and bounded shutdown. [App boot](../../packages/boot/app-boot/README.md) owns shared startup behavior; [`src/args.ts`](src/args.ts) owns argument parsing.
