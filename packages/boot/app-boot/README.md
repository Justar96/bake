---
description: "Shared Loader boot support for dsh profiles: environment layers, patches, diagnostics, and configuration preview."
kind: "package-library"
---

# bake-app-boot

## Summary

`bake-app-boot` is the shared Loader boot library behind `dsh` profiles. It loads environment layers, composes profile bundles and patches, boots every plugin, and returns the running app or identifies the failed plugin and cause. Product applications use the `dsh` launcher instead of publishing separate bins; direct-config helpers remain only for lower-level embedders and tests. You can preview the effective configuration before booting, configure HMR through profile YAML, and let a terminal-owning app restore its terminal before a fatal exit.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Further Exploration](#further-exploration)
- [Model Experience](#model-experience)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

Starting an app with this package is a small, explicit entry point: you give it a config file and it runs the whole boot. This section covers what you can do and what you get; the helper calls behind each outcome are documented in the folded implementation section.

### When to use it

Use it when implementing the shared `dsh` launcher or embedding its lower-level boot helpers. Product features belong in profile bundles instead of new application bins; code that only adds plugins to an already-running app mounts those plugins directly.

### Starting the app

You give your entry point a config file, and the process starts the whole app: it loads your environment layers, applies patches and profiles, boots every plugin, and returns once the app is running. In replay mode it boots the sibling `cordis.snapshot.yml` instead, so a recorded session reproduces identically. The smallest entry point is two calls:

```text
installFailLoud('dsh')
const ctx = await boot('dsh', resolveConfigPath(argv[2], process.env.DSH_SNAPSHOT))
```

`installFailLoud` reports an unhandled rejection or uncaught exception to stderr with `util.inspect`, waits up to two seconds for the app's release hook, then exits 1. Control never returns to the failed operation; the event loop runs only until release settles or times out. A launcher ends that rule for rejections once its app is ready by calling the returned guard's `tolerateRejections(report)`: each later rejection goes to `report` and the process keeps running, because a rejection ended only the promise chain that dropped it. The `dsh` launcher does this when it commits readiness, and [records and shows each late rejection](../../../apps/cli/README.md#startup-and-shutdown). An uncaught exception stays fatal for the life of the process, since it unwound through whatever called the throwing callback. With that entry point, startup keeps every plugin that can activate. An enabled failed plugin produces a labelled warning. A failed required entry makes startup dispose the whole app and exit nonzero; required ids absent from a profile and disabled required entries do not affect startup. The global required list includes `agent-loop`, `tui-startup`, `tui-runner`, and `headless-runner`, plus recognized external application endpoint ids. Missing or disabled ids do not require a profile to mount that application.

<a id="profiles"></a>
### Profiles

Import profile and bundle declaration types from [`bake-package-manifest`](../../util/package-manifest/README.md). App-boot adapts `DshPackageManifest` to `ProfileManifest` with optional package identity because local profiles need no published version. App-boot owns profile loading, JSON validation, and resolved runtime data.

Bake ships `tui` and `headless` profile templates. Each profile lives at `$DSH_HOME/profiles/<name>` and combines ordered bundles with its own `cordis.patch.yml`; YAML controls HMR. `tui` selects base and `bake-tui-app`, while headless selects base and its one-shot runner. `dsh --profile <name> --from-default-profile <template>` initializes a new custom profile from a shipped template. A bundle's `dsh.bundle.patch` accepts one path or an ordered list of paths, relative to its package root. Each file's patch list is applied in order. Existing profile bundle lists remain unchanged. A missing bundle or one without a patch declaration fails startup loudly. `loadProfileDirectory` loads an already initialized directory directly.

Your machine-local preferences also live in the Harness home:

- **`.env`** — your ordinary environment layers: the invoking directory's file outranks the Harness-home file, and both sit below the inherited environment. Variables that decide how the process starts (`PATH`, `DSH_*`, `XDG_*` and similar) are rejected from files: export them instead. The four proxy names (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`) are accepted from the Harness-home file only, never from the invoking directory's, which arrives with a clone. For a non-product bin that just wants one directory's `.env`, a missing file is fine and an unloadable one prints one labelled warning line.
- **`cordis.patch.yml`** — your tweak layer, applied after every bundle layer (per-profile first, then the home-level file, which therefore outranks it): replace one entry's whole config (restating the fields you keep), insert new entries, or interpolate `!!js` expressions at boot. A patch naming an entry that does not exist prints a stderr warning; an empty or comments-only file fails boot — disable the layer with `[]` instead.

The enabled `bake-hmr` plugin watches the profile manifest and both user patch files, re-reads the ordered bundle layers, and applies the [reload failure policy](#startup-and-reload-failures). [DSH HMR](../hmr/README.md) serializes these reloads with [Plugin Manager](../plugin-manager/README.md) configuration writes; package operations run outside its queue. The launcher does not install HMR or watchers; disabled or absent HMR means changes require restart.

Inserted plugin names may be absolute filesystem paths, file URLs, or package specifiers. Patch loading renames [deprecated package names](#renamed-packages), then converts absolute paths and patch-relative `./` or `../` paths to file URLs within `insert` rows and their nested groups; existing-entry name assertions and replacement `config` values remain literal.

Before mounting profile rows, the `dsh` launcher computes one immutable package-resolution generation from the installation and ordered bundle dependency graphs. Runtime mode is the default: it installs the generation through Node's ESM and CommonJS resolvers without creating fallback links. Plain Node callers of `runProfile` may explicitly select link mode to materialize the generation, dual mode to materialize and verify it, or runtime mode. Packaged executables and the Electron Host always use runtime mode.

<a id="renamed-packages"></a>
#### Renamed packages

Bake renamed its runtime packages from `@deepseek-ai/dsh-<name>` to `bake-<name>`. [`src/legacy-package-names.ts`](src/legacy-package-names.ts) maps every old name to its replacement (`LEGACY_PACKAGE_NAMES`, `currentPackageName`, `renamedModuleSpecifier`), and boot accepts the old names in three places:

- **Profile manifests.** `loadProfile` rewrites old names in `dsh.profile.bundles` once (`migrateProfileManifest`): it copies the manifest to `package.json.bak`, replaces it atomically, and prints one line naming the renames. An application-owned profile loaded with `loadProfileDirectory` is not rewritten; its old names resolve for that launch with one deprecation warning.
- **Patch files.** Bundle patches, the profile and home `cordis.patch.yml` layers, and `--patch` overlays rename rows and name assertions that use an old name, including subpaths such as `@deepseek-ai/dsh-tool-subagent-control/list-agents`. Each old name prints one warning per file naming its replacement.
- **Module imports.** The package-resolution generation adds an entry for each old name of an installed renamed package. An out-of-tree plugin that imports `@deepseek-ai/dsh-llm` or one of its subpaths gets the `bake-llm` module instance, through the runtime resolver or a fallback link in link and dual modes. Its own profile-local packages still take precedence.

The old names are deprecated and slated for removal in a later release.

### Previewing the effective configuration

Before you boot, you can print the exact configuration the app will mount: the dump shows the composed entry list with `!!js` expressions verbatim, grouped under comments naming each source file and the patch layers that changed it, as one loadable YAML document. Patches that match no row are reported with their layer label; a missing, unparsable, or invalid config fails the dump.

### Inspecting plugin configuration schemas

`generateConfigSchema` takes a diagnostic bin name, a prepared on-disk profile, ordered patch lists, and an installation anchor, and returns `ConfigSchemaDump`. App-boot owns composition, runtime resolution, and collection diagnostics. The caller owns profile preparation, home/argv layer selection, process streams, and exit policy. `createConfigProjector`, `isNativeConfigSchema`, and `LOADER_EXPRESSION_SCHEMA` are exported for callers that project one live plugin Config without profile collection; projected value positions reference `#/$defs/loaderExpression`, so the enclosing document must define it.

The generated JSON Schema 2020-12 describes the composed entry list, with `$defs.patchList` for root-tree overlays and shared definitions projected from plugin Config graphs. It includes disabled entries, native groups, and literal YAML/JSON includes; builtin and canonical native package exports are matched using each tree's module-resolution base, including profile-local copies. Custom carriers are not inferred from their config fields. A missing include with literal `initial` entries is expanded in memory without writes. Discovery and projection diagnostics remain in `x-cordis`, including unknown Configs and partial constraints. The [CLI schema-dump reference](../../../apps/cli/reference/README.md#config-schema-dump) owns the output fields and editing semantics.

The projector preserves native omission behavior by checking literal defaults against generated schemas with Ajv, without executing native validators or transform callbacks. Regex compatibility checks and unsupported or recursive-default cases produce explicit limitations. Opaque input adaptations and lazy metadata effects widen validation rather than replaying native mutation. Non-JSON default/presentation annotations are omitted with limitations without losing the structural schema; an unrepresentable default leaves omission acceptance unknown unless the field is required. These dependencies load only when collection runs. Imports, Config getters, and lazy builders still execute trusted code; collection is not a sandbox. Do not overlap profile-resolution interceptions. The collector releases its interception before returning, while Node retains imported modules; runtime-created plugins and Agent preset instances remain outside discovery.

<a id="startup-and-reload-failures"></a>
### Startup and reload failures

Profile reconciliation returns diagnostics for unchanged inactive entries without failing an unrelated mutation. A new inactive entry, a changed configuration or fiber, or a changed diagnostic fails reconciliation; removed fibers must still finish disposal. Explicit enablement targets must activate even when their failure predates the operation.

After the Loader settles, app-boot warns when only optional entries are inactive. If an enabled required entry cannot activate, `boot()` rejects with `StartupError` after disposal. An independently owned logger exporter retains warning and error records through asynchronous disposal and is released before `boot()` settles. Its message groups all failed plugins and pending services, marks required entries, and retains original stacks, nested causes, and aggregate members. The CLI prints that message once and saves [full startup diagnostics](../../../apps/cli/reference/README.md#startup-diagnostics) before exiting with code 1; unrelated exceptions retain their normal stack output. In the table, stopping startup means disposing any mounted plugins and exiting nonzero without reporting readiness; continuing keeps successful plugins running. Later configuration HMR does not repeat the required-startup audit and does not roll back the whole update.

| Failure pattern | Optional entry at startup | Required entry at startup | Later configuration HMR |
|---|---|---|---|
| Root config or required overlay is missing, unreadable, malformed, or contains invalid entries | Stop startup | Stop startup | Malformed or invalid live patches are rejected without changing the running configuration; a valid edit applies |
| Module import fails or module evaluation throws | Warn; continue | Stop startup | Report the error; keep successful siblings; a corrected import can activate |
| Plugin config schema validation fails | Warn; continue | Stop startup | A new entry stays inactive; an existing entry retains its prior instance and config; a valid correction applies |
| Config `!!js` evaluation throws | Warn; continue | Stop startup | Report the error; keep successful siblings; a valid correction can activate |
| `disabled: !!js` evaluation throws | Warn; continue | Stop startup | Report the evaluation error rather than treating the entry as disabled; a valid correction can activate |
| Synchronous `apply()` throws | Warn; continue | Stop startup | Report the error; keep successful siblings; corrected config can activate |
| Asynchronous `apply()` throws | Warn after settlement; continue | Stop startup after settlement | Report the error after settlement; keep successful siblings; corrected config can activate |
| An injected service is unavailable | Warn; continue while the entry waits for its dependencies | Stop startup | Keep the entry waiting; adding the missing provider can activate it |
| HTTP port binding fails | Warn; continue without that endpoint | Stop startup | Keep the process running without the failed endpoint; corrected config can restore it |
| Detached asynchronous work outside the `apply()` return Promise produces an unhandled rejection | Fatal: dispose the app and exit nonzero | Fatal: dispose the app and exit nonzero | Under the `dsh` launcher, after readiness: record and show it, and keep running, regardless of entry id; fatal under a bin that never calls `tolerateRejections` |
| A synchronous callback or timer throws an uncaught exception | Fatal: release the app and exit nonzero | Fatal: release the app and exit nonzero | Fatal: release the app and exit nonzero, regardless of entry id |
| Entry is absent or explicitly disabled | Ignore it | Ignore it | Do not activate it; no required-startup audit |

The required list above includes `modules` and `connection`; Web startup cannot succeed when either enabled entry fails. Failure of an optional provider can also prevent a required consumer from activating. Schema rejection before an existing entry updates is not a transactional rollback of sibling changes.

The [app-boot tests](tests/app-boot.spec.ts) cover activation failures, required terminal entries, and root Include failures. [Terminal replay](../../../apps/tui/scripts/pty-smoke.ts) exercises the shipped TUI composition and terminal restoration.

If your app owns the terminal, it can hand the terminal back before the process exits, so your shell is never left in raw mode. The handoff is bounded: a stuck cleanup delays the fatal exit but never cancels it.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

This section explains how the outcomes above are realized and points at the code that realizes them; everything here is developer-facing and not needed to use the package.

### Design notes

- **Profile launch data.** `ctx.profileContext` contains only profile locations, startup bundle names, parsed invocation overlays and the telemetry opt-out value. `readProfilePatches()` composes the supplied startup profile or reads current files at those locations; callers schedule and apply the result. `isProfileGenerationApplied()` reports whether a composition equals the generation the root Include last received, so a watcher that starts after boot can skip reapplying it.
- **Process-local module resolution.** Runtime and dual modes install one generation on Node's internal ESM and CommonJS resolvers before profile rows mount; link mode leaves both resolvers unchanged. Node still owns exports, conditions, subpaths, module caches, and error codes; routed ESM failures report the original importer instead of the internal lookup anchor. `ctx.pluginPackages` exposes package metadata from the same generation without recording Entry imports; an installed generation is authoritative even for a miss, while low-level embedders that install the service without one retain native lookup.
- **Two Loader builtins.** `mountRootInclude` registers `cordis:include` and `cordis:group` as Loader builtins: a group row gives one `isolate` realm to a provider and its consumers together, and an agent preset outside this workspace cannot resolve `@deepseek-ai/cordis-plugin-group` by name. Both load through the ambient module pipeline rather than the included tree's own specifier resolution.
- **Consumer-owned strictness.** Ordinary Loader groups keep successful siblings. App-boot applies the global required-entry policy after initial settlement; agent presets and dynamic multi-entry compositions own and dispose their separate generation when they require all-or-nothing setup. App-boot reads failed fibers to report their recorded errors and coalesces duplicate Loader rejection notifications through one process checkpoint.
- **One fallback generation.** The installation-first and ordered-bundle breadth-first traversal produces both the runtime table and the retained disk materializer. Runtime mode creates no resolution links and ignores stale projections at their former lookup positions. External bare targets selected by package `imports` use the same package order, while Node retains mapping, conditions, and exact target resolution. Link mode materializes the same table; dual mode also compares Node's disk result with the table. A complete successor may add package names atomically, while changing or removing an existing mapping requires restart.
- **Application-owned profiles.** Link mode projects missing installation and bundle packages inside the profile without writing a shared Harness-home fallback. Runtime mode supplies the same installation and bundle generation without creating links. Package operations remove only profile links owned by dsh; pnpm-managed entries remain untouched.
- **Owned Workers.** Worker build banners import `bake-app-boot/worker/profile-resolution-bootstrap` before bundled business code. Each Worker installs the structured-cloned generation in its own isolate. The bootstrap bundle has no static package imports. Source Worker entries retain their self-contained dependency closure, and third-party Workers receive no injection.
- **Update completion.** App boot observes restart failures through the `internal/update` waterfall. Live patch reloads wait for the tree's fibers before auditing activation; `Fiber.update()` and `Entry.update()` alone do not establish restart success.
- **One rejection checkpoint.** `inactiveEntries` keeps the exact reasons it folds into the boot diagnostic visible through the next process rejection checkpoint, so `installFailLoud` coalesces Loader's duplicate notification, before and after readiness, while unrelated unhandled rejections remain fatal until the launcher tolerates them.
- **Rejections after readiness.** `tolerateRejections` swaps the rejection path's fatal report for the launcher's reporter and leaves the exception path alone. A fatal exit already in progress keeps swallowing later rejections, and a reporter that throws is contained to one `<bin>: warning:` stderr line, so neither a late reporter nor its failure can end the process.
- **Two-stage failure labels.** Outside startup audit failures, `boot()` distinguishes `host preparation failed` — `prepare` threw before any config-tree entry mounted — from `plugin tree failed to load`, and appends the deepest plugin error's stack. Plugin diagnostics retain nested causes and aggregate member failures; cyclic causes stop traversal without replacing the original error.

The startup error also retains inactive-entry metadata and raw startup warning/error records without retaining the Loader tree. Its `entries` and `startup` fields are non-enumerable: direct access and full diagnostic reports retain them, while ordinary error inspection omits them. Pending-only failures have no `cause`; failures with recorded errors retain their original values in an `AggregateError`. Import errors are collected through the logger before the Loader mounts because no failed Fiber exists for those imports. The temporary exporter is removed when boot settles.

### Helper behavior

The exports each own one stage of the boot: config resolution and snapshot replay, layered environment loading, fail-loud reporting, activation auditing, patch parsing, root-include mounting, config dump rendering, and profile composition. Per-export contracts live in the code, not this README — see [`src/index.ts`](src/index.ts) and [`src/profile.ts`](src/profile.ts).

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | Boot helpers: config resolution, environment loading, fail-loud guard, activation audit, patch parsing, config dump |
| [`src/profile.ts`](src/profile.ts) | Profile discovery, initialization, legacy manifest migration, bundle resolution, module fallback |
| [`src/legacy-package-names.ts`](src/legacy-package-names.ts) | Deprecated upstream package names and their replacements |
| [`src/config-schema/`](src/config-schema/) | Profile schema generation, discovery, native projection, and result types |
| [`src/profile-resolution/`](src/profile-resolution/) | Runtime resolver, package-metadata service, and built Worker bootstrap |
| — | No runtime invariant companion is published; one registration owns each resolver generation, and dual mode compares the independently materialized result at resolution time. |

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read these pages when the package-level contract is not enough. They move from the shared boot mechanics to the composition model and the decision evidence behind it.

- [Cordis primer](../../../docs/cordis-primer.md) — Loader, `!!js` config expressions, and include/group semantics.
- [dsh app](../../../apps/cli/README.md) — the `dsh` bin that consumes these helpers.
- [bake-cmdline](../cmdline/README.md) — the launcher-to-app command-line handoff the bins use.
- [Profile bundles](../../bundle/README.md) — installable patch layers composed into `dsh --profile`.
- [bake-home-paths](../../util/home-paths/README.md) — the Harness-home resolver (`resolveDshHome`).
- [Configuration source ownership](../../../.agents/notes/implemented/architecture/2026-08-04-configuration-source-ownership.md) — why a discovered file may not decide bootstrap behavior.
- [Profile plugin bundles](../../../.agents/notes/implemented/architecture/2026-08-05-profile-plugin-bundles.md) — the profile and bundle composition design.
- [User-patch HMR tests](../../../.agents/notes/implemented/testing/2026-09-09-user-patch-hmr-test-delivery.md) — ownership of live-patch behavior and native filesystem delivery.

-----

<a id="model-experience"></a>
## Model Experience

Indirectly, through the loaded plugin tree, which alone contributes model context; no export of this package adds model-visible text.

#### KV Cache effect

Boot itself changes no request prefix and adds no model-visible text; prefix stability depends entirely on the loaded plugin tree's contributions. Provider cache reuse is not guaranteed.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>


These limits describe when this boot library is a poor fit or needs special care. They are current package constraints, not a task backlog.

- **Runtime resolution depends on Node internals** — supported Node versions require the native builtin-access addon and executable compatibility coverage. Only built Harness-owned Workers receive the generation bootstrap; third-party Workers and custom `vm` linkers keep native resolution.
- **Snapshot replay swapping is basename-specific** — only a config ending in `cordis.yml` or `cordis.yaml` maps to the sibling `cordis.snapshot.yml`; custom config names require caller-managed selection.
- **Environment discovery is launch-scoped** — `loadLayeredEnv` reads only the invocation directory and Harness home once; it does not search parents or follow a workspace selected later. `loadEnv` remains the one-directory helper for non-product bins.
- **A user patch replaces the whole matched config** — an id-targeted patch does not deep-merge, so a profile override restates the bundle fields it keeps.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

This Dev Note is working context for maintainers: open design questions and directions that are not decided. It is explicitly non-authoritative — shipped behavior, limits, and accepted rationale live in the sections above, the package code, and the linked Agent Notes.

#### Open: YAML config dump stability

`renderConfigDump` output is a loadable YAML document whose `# ==` source comments and `!!js`-verbatim rendering serve the `--dump-config` diagnostic. Nothing promises byte stability across package versions; decide whether the dump becomes a serialization contract before anything consumes it programmatically. JSON Schema output follows the separate [pre-stable compatibility policy](../../../apps/cli/reference/README.md#config-schema-dump).

</details>
