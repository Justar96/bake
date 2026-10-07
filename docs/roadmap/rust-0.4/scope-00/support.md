# Scope 00: product support decisions and acceptance matrix

## Summary

This page lists what Bake ships today, what the Rust 0.4 line must preserve, and which questions only the product owner can answer. The inventory comes from source at `origin/develop` `ae5eb51ab6` and the `v0.3.8` tag `dcb26d756e`, read on 2026-10-07. It belongs to [scope 00](README.md) of the [Rust migration roadmap](../README.md).

The default is conservative: every behavior the final 0.3 release ships is the 0.4.0 parity target, and nothing is dropped without an approved, documented support change. The approved exception is the whole-profile Node/TypeScript runtime: 0.4 ships Rust only, so custom Cordis/JavaScript profiles need migration ([D5](#decision-register)). This page tests nothing; isolated qualification probes do not constitute a native implementation. Provider, platform, and Desktop rows say what evidence is missing; none claims native parity.

## Table of Contents

- [How to read decisions](#how-to-read-decisions)
- [Decision register](#decision-register)
- [Shipped surface](#shipped-surface)
- [Profile migration and native extension strategy](#profile-migration-and-native-extension-strategy)
- [Unreleased develop work](#unreleased-develop-work)
- [Release lines and support window](#release-lines-and-support-window)
- [Incremental delivery under the develop to main rule](#incremental-delivery-under-the-develop-to-main-rule)
- [Acceptance matrix](#acceptance-matrix)
- [Native consumer coverage gaps](#native-consumer-coverage-gaps)
- [Dev Note](#dev-note)

## How to read decisions

Each decision has one of three classes. Only the first two are settled.

| Class | Meaning | Who can change it |
|---|---|---|
| **Decided by request** | The current user request or a repository instruction already settles it | The user, through a new request |
| **Scope-00 default** | A conservative default adopted here so work can continue; it preserves current behavior | The product owner, through a reviewed change to this page |
| **Owner decision** | A recommendation that changes support, release routing, or packaging; it is not in force until the owner approves it | The product owner only |

## Decision register

Scope 01 may start on the settled rows. Scope 00 stays open until every **Owner decision** row is answered or explicitly deferred to a named scope.

| ID | Subject | Class | Decision or recommendation |
|---|---|---|---|
| D1 | Release lines | Decided by request | 0.3.x takes bug fixes only. 0.4.x is the Rust implementation. |
| D2 | Parity target | Scope-00 default | Every behavior the final 0.3 release ships is the 0.4.0 target. A narrower target needs an owner-approved support change, release notes, and a migration path. |
| D3 | Removed products | Decided by request | The web client, upstream desktop app, ACP, Python SDK, docs site, and upstream automation stay removed ([AGENTS](../../../../AGENTS.md#porting-from-upstream)). The `desktop` bundle is Bake's own and is kept. |
| D4 | Session data | Scope-00 default | Keep Session format 3, its adjacent migrations, and the retired vocabulary. A Rust port is not a reason for a format change. |
| D5 | TypeScript runtime and custom profiles | Decided by request, 2026-10-07 | The product owner approved: "Ship Rust only; custom JavaScript profiles would need migration." 0.4 ships no bundled or explicitly selectable Node/TypeScript compatibility runtime. During development the TypeScript runtime stays runnable as the comparison oracle. A custom Cordis/JavaScript profile needs migration; Bake never silently rewrites or executes a construct the native runtime does not support ([strategy](#profile-migration-and-native-extension-strategy)). This decision alone drops no provider, Desktop support, `run_code` language, legacy name, persisted data, or update and rollback obligation. |
| D6 | Native extension protocol | **Owner decision** | Recommendation: 0.4.0 offers MCP servers and hook processes as its native extension surfaces and adds no new plugin API. A versioned out-of-process protocol is designed in a later 0.4.x release, around named consumers. |
| D7 | Cordis preset and Cordis tooling | **Owner decision** | The `cordis` preset, `cordis_inspect_*`, `plugin_manager`, the Cordis host runner, and `bake plugin` operate on JavaScript plugins that 0.4 cannot run ([D5](#decision-register)). Each needs a designed migration or native replacement, or a separately approved support change under [D2](#decision-register). None is approved for removal yet. |
| D8 | Provider breadth | Scope-00 default for the target; **Owner decision** for tiers | Every route reachable today is the target. The [provider section](#provider-routes-and-authentication) recommends qualification tiers. Each family needs native qualification or a separately approved support change; no Node fallback discharges it. |
| D9 | Bake Desktop launch | **Owner decision**, with the Desktop maintainers | Keep `dsh --profile desktop` and protocol version 1. How Desktop launches the native executable is open; candidates include spawning it over stdio or bridging Electron `parentPort` to that stdio, and none is chosen. Bake's TypeScript runtime cannot serve as the bridge ([D5](#decision-register)). The chosen path needs a test with the real Desktop consumer. |
| D10 | Legacy names | Scope-00 default; removal timing is an **Owner decision** | Keep the `dsh` alias, `DSH_*` fallbacks, `dsh.profile`, project `.dsh/skills`, and legacy package aliases through the 0.3 support window. Never remove them in the cutover PR. |
| D11 | Default home for direct entries | **Owner decision** | The release launcher resolves `BAKE_HOME`, then `DSH_HOME`, then `~/.bake`. The shared helper still defaults to `~/.dsh` when no launcher has set either variable. Recommendation: native uses the launcher rule everywhere and never reads `~/.dsh` implicitly. |
| D12 | Unreleased `develop` work | **Owner decision** | Recommendation: release it as the last feature-bearing 0.3 patch (0.3.9), after the gates in [the triage table](#unreleased-develop-work). Then freeze 0.3 to bug fixes and use that tag as the oracle. |
| D13 | 0.3 maintenance channel and window | **Owner decision**; duration open | Recommended design in [release lines](#release-lines-and-support-window). Implement it in a separate governance PR before the 0.4 default switch. |
| D14 | Packaging and Node | **Owner decision** for the transition | Shipped 0.4 artifacts are Rust only and need no Node ([D5](#decision-register)). The transition from released 0.3 launchers and updaters, which launch-check `apps/cli/lib/bin.js` with Node, is not designed yet; it must end in Rust-only shipped artifacts ([packaging](#packaging-update-and-rollback)). |
| D15 | Telemetry and feedback | Scope-00 default | Keep export disabled without a configured collector, both opt-out spellings, local-only feedback, and `BAKE_NO_UPDATE_CHECK`. |
| D16 | Platforms | Scope-00 default | The five release targets are the native matrix. Minimum OS, kernel, libc, and macOS versions are missing evidence. |
| D17 | Code mode | Scope-00 default | `run_code` keeps its language: model-written TypeScript in a confined QuickJS VM with the same bindings and limits. |
| D18 | Performance budgets | Scope-00 default | Budgets come from the scope-00 baseline and are frozen before any candidate measurement. No numbers are set yet. |
| D19 | Native TUI direction | Decided by request | Keep the current UI recognizable, with cleaner presentation and better agent/composer handling. The [terminal direction](../terminal.md) defines the acceptance cases and the Ratatui/Crossterm starting stack for scopes 14–15. |

## Shipped surface

Each subsection lists one product surface, links its source, and gives its 0.4 disposition. "Native parity" means the Rust implementation must match current behavior. "Migration" means 0.4 cannot run the surface as it is, because it needs the Node/TypeScript runtime that 0.4 does not ship ([D5](#decision-register)); its migration or replacement path is still to be designed.

### Profiles

[Profile templates](../../../../packages/boot/app-boot/src/profile.ts) initialize three shipped profiles under `$BAKE_HOME/profiles/<name>`. A name with no template starts from `bake-base`, or from `--from-default-profile <template>`.

| Profile | Bundles | Entry and transport | Current oracle | 0.4 disposition |
|---|---|---|---|---|
| `tui` | `bake-base`, `bake-tui-app` | `bake tui`; inline or fullscreen Ink terminal | [App tests](../../../../apps/tui/packages/app/tests/), the 42 scenarios in the [PTY driver](../../../../apps/tui/scripts/pty-smoke.ts), and [preset model surfaces](../../../../apps/tui/packages/app/tests/expected/model-surface/) | Native parity (scopes 13–15) |
| `headless` | `bake-base`, `bake-headless` | `bake headless <task>`; answer on stdout, or NDJSON events with `--json`; diagnostics on stderr | [Built-profile tests](../../../../apps/cli/tests/profiles/) and [model surface](../../../../apps/cli/tests/profiles/expected/model-surface/headless.md) | Native parity (scopes 09, 13) |
| `desktop` | `bake-base`, `bake-desktop` | The separate Bake Desktop app runs `dsh --profile desktop`; NDJSON stdio or Electron `parentPort` ([transport](../../../../packages/bundle/desktop/src/transport.ts)), `PROTOCOL_VERSION = 1` ([protocol](../../../../packages/bundle/desktop/src/protocol.ts)) | [Desktop tests](../../../../packages/bundle/desktop/tests/) and [model surface](../../../../apps/cli/tests/profiles/expected/model-surface/desktop.md); the external consumer is untested | Native parity plus a launch adapter (scope 13, [D9](#decision-register)) |
| Custom profile | User-chosen bundles, user patch layer, pnpm-installed plugins | `bake <name>` | [Profile tests](../../../../packages/boot/app-boot/tests/profile.spec.ts) | Native when every row is native-recognized; otherwise refused before startup and reported for migration ([strategy](#profile-migration-and-native-extension-strategy)) |

### Presets and model-facing tools

The [shipped presets](../../../../packages/preset/agent-presets/presets/) are `standard` (order 1), `ptc` (order 2), `minimal` (order 3), and `cordis`, titled "Creation mode" (order 4). Users can add presets under `$BAKE_HOME/.agent-presets` ([discovery](../../../../packages/preset/agent-presets/src/discovery.ts)), and `bake tui --preset <name>` selects one. The rosters below come from the Linux model-surface snapshots. On Windows, `pwsh` replaces `bash` through `process.platform` rows.

| Surface | Tools sent on the first request | Disposition |
|---|---|---|
| `standard` in `tui` (21) | `ask_user_question`, `bash`, `create_goal`, `edit`, `get_goal`, `glob`, `grep`, `interrupt_agent`, `job_kill`, `job_list`, `job_output`, `list_agents`, `read`, `read_image`, `send_message`, `skill`, `subagent`, `update_goal`, `web_fetch`, `web_search`, `write` | Native parity, byte-identical schemas and text |
| `headless` and `desktop` (20 each) | The `standard` set without `ask_user_question` | Native parity |
| `ptc` (1) | `run_code`, which exposes the standard tools as bindings | Native parity, keeping the embedded JavaScript engine ([D17](#decision-register)) |
| `minimal` (1) | `bash` from the persistent shell (`pwsh` on Windows) | Native parity |
| `cordis` (24) | The `standard` set plus `cordis_inspect_list`, `cordis_inspect_query`, and `plugin_manager` | Migration ([D7](#decision-register)) |
| MCP tools, conditional | `mcp__<server>__<tool>` when a profile configures [MCP servers](../../../../packages/mcp/mcp-client/README.md); none ship enabled | Native parity |

The tools come from these packages: [shell](../../../../packages/shell/), [filesystem](../../../../packages/fs/), [jobs](../../../../packages/jobs/), [goals](../../../../packages/goal/), [subagents](../../../../packages/subagent/), [skills](../../../../packages/skill/), [user questions](../../../../packages/interaction/), [web](../../../../packages/web/), [code mode](../../../../packages/ptc-runtime/), [Cordis tooling](../../../../packages/extensions/), [plugin manager](../../../../packages/boot/plugin-manager/), and [MCP](../../../../packages/mcp/). Any change to their text or schemas needs a paired eval record under the [eval policy](../../../../evals/README.md).

### CLI commands, flags, and exits

Both bin names run [one launcher](../../../../apps/cli/src/args.ts), which parses its own flags and hands every later token to the booted app. Stderr diagnostics keep the `dsh:` prefix. The [launcher help snapshot](../../../../apps/cli/tests/expected/launcher-help.txt) pins the help text.

| Owner | Surface | Disposition |
|---|---|---|
| Launcher | `bake`/`dsh`, `[--profile] <name>`, `--from-default-profile <name>`, repeatable `--patch <path>`, `--dump-config`, `--dump-default-config`, `--dump-config-schema`, `-V/--version`, launcher `-h` only when no profile is given, hidden `--self-check` | Native parity |
| `plugin` | `bake plugin --profile <name> <pnpm args>` forwards to host pnpm and reports exit 127 when pnpm is absent ([source](../../../../apps/cli/src/plugin.ts)) | Migration ([D7](#decision-register)); native profiles reject npm plugins before boot ([strategy](#profile-migration-and-native-extension-strategy)) |
| `update` | `bake update`, `--check` (exit 0 when current, 10 when newer, 1 on failure), `--rollback` | Native parity plus transition work ([packaging](#packaging-update-and-rollback)) |
| `tui` app | `--resume <id>`, hidden `--session-id`, `--preset <name>`, `--screen inline\|fullscreen` ([source](../../../../apps/tui/packages/app/src/startup.ts)) | Native parity |
| `headless` app | `[task...]`, `-` reads stdin, `--json`, `--resume <id>`, hidden `--session-id`; a busy session exits 75 ([source](../../../../packages/bundle/headless/src/startup.ts), [test](../../../../apps/cli/tests/headless-session-in-use.spec.ts)) | Native parity |
| Slash commands | Terminal: `/agents`, `/attach`, `/changelog`, `/clear-attachments`, `/help`, `/login`, `/logout`, `/model`, `/remove-attachment`, `/settings`, `/terminal-setup`, `/thinking`, `/update` ([controller](../../../../apps/tui/packages/app/src/controller.ts)). Runtime: `/compact`, `/goal`, `/feedback`, `/permission`. Headless and Desktop register no slash commands. | Native parity (scope 15) |

### Provider routes and authentication

Bake routes every model request through [`bake-llm-pi-ai`](../../../../packages/llm/llm-pi-ai/README.md) over `@earendil-works/pi-ai` 1.0.0, pinned in `bun.lock`. There are two route kinds, and the Rust port must keep them apart.

**Generic wire overrides.** A configured route can name one of three protocols: `openai-completions`, `openai-responses`, or `anthropic-messages` ([provider table](../../../../packages/llm/llm-pi-ai/src/provider.ts)). The base bundle ships one route, `deepseek-official`, over Anthropic Messages ([base bundle](../../../../packages/bundle/base/cordis.patch.yml)). The terminal's [CLIProxyAPI setup](../../../../apps/tui/packages/app/src/cliproxyapi.ts) writes a route on these same protocols, defaulting to `http://127.0.0.1:8317`.

**Catalog providers.** A route that keeps its catalog protocol reuses pi-ai's own provider implementation ([catalog](../../../../packages/llm/llm-pi-ai/src/catalog.ts)). A local, offline read of the installed package found 42 provider data files. Their model entries name 13 API families:

| Catalog API | Model entries | Covered by a generic override? |
|---|---:|---|
| `openai-completions` | 717 | Yes |
| `anthropic-messages` | 348 | Yes |
| `openai-responses` | 131 | Yes |
| `bedrock-converse-stream` | 180 | No; pi-ai depends on the AWS Bedrock runtime SDK |
| `openrouter-images` | 57 | No |
| `azure-openai-responses` | 44 | No |
| `mistral-conversations` | 32 | No |
| `google-generative-ai` | 29 | No; pi-ai depends on the Google GenAI SDK |
| `pi-messages` | 28 | No |
| `google-vertex` | 14 | No |
| `typesafe-system-one` | 14 | No |
| `openai-codex-responses` | 9 | No |
| `cloudflare-workers-ai-system-one` | 1 | No |

**Authentication.** Each route resolves an API key reference per request through the credential store: environment first, then the managed `$BAKE_HOME/.credentials.yaml`, then project and user `.env` files ([credentials](../../../../packages/credentials/credentials-local/src/index.ts)). Interactive login ([source](../../../../packages/llm/llm-pi-ai/src/login.ts)) offers `oauth` when the provider defines it and `api-key` when the provider collects its key interactively. It relays browser-URL, device-code, prompt, and progress events. pi-ai ships OAuth modules for `anthropic`, `github-copilot`, `kimi-coding`, `meta`, `openai-chatgpt`, `openai-codex`, `openrouter`, `radius`, and `xai`, using PKCE with a local callback server or a device code. OAuth grants are stored verbatim under the `llm-pi-ai` credential scope, and refresh runs under the store's cross-process lock ([auth](../../../../packages/llm/llm-pi-ai/src/auth.ts)).

**Recommendation ([D8](#decision-register), owner decision).**

- **Tier 1, required for the 0.4.0 default:** `deepseek-official`, the three generic protocols with custom base URLs and proxies, CLIProxyAPI, environment and managed-key resolution, and every OAuth flow the terminal offers. Each needs sanitized wire fixtures and a key-gated smoke test.
- **Tier 2, catalog APIs outside the generic three:** port and qualify each family natively. Until a family is qualified, a native session that selects it, at startup or through `/model`, must refuse visibly and never silently re-route. No Node fallback exists in 0.4 ([D5](#decision-register)). Dropping a family is a support change under [D2](#decision-register).
- No route is claimed as qualified until its own fixtures and smoke tests pass. Missing credentials are recorded as missing evidence.

### Executable YAML, custom plugins, and extensions

Profiles are Cordis compositions. Bundle and user patch layers accept `!!js` expressions, and the profile patch template says so ([profile](../../../../packages/boot/app-boot/src/profile.ts)). The shipped bundles use `!!js` for `BAKE_*`/`DSH_*` environment fallbacks, `process.platform` checks, `process.cwd()`, `dshHomePath(...)`, `ctx.get('profileContext')`, and startup values such as `ctx.tuiStartup.resume`.

| Construct | Current behavior | Disposition |
|---|---|---|
| Shipped bundle `!!js` | Evaluated at load | Native: translate the exact shipped expressions into typed configuration. Pin them by row id and source text, so any edit to a bundle is detected. |
| User `!!js`, unknown package rows, custom bundles | Evaluated or loaded by Cordis | Migration; detected and reported before any agent starts, without executing or rewriting the construct |
| npm plugins installed with `bake plugin` | Resolved through the profile's `node_modules` and the shared fallback | Migration ([D7](#decision-register)) |
| [Hooks](../../../../packages/hooks/): Claude Code and Codex configurations | Optional plugins that run external hook processes | Native parity; external processes are already language-neutral |
| [MCP client](../../../../packages/mcp/mcp-client/README.md) | Optional rows; stdio and Streamable HTTP | Native parity |
| [Plugin manager](../../../../packages/boot/plugin-manager/README.md), [Cordis host runner](../../../../packages/extensions/cordis-host-runner/README.md), [Cordis tools](../../../../packages/extensions/tool-cordis/README.md) | JavaScript runtime inspection and profile mutation | Migration ([D7](#decision-register)) |

### Settings, home, environment, and legacy names

Native must reproduce the launcher's behavior, not only the shared helper's default ([analysis](../analysis.md#home-defaults-have-two-layers)).

| Item | Current behavior | Source and tests |
|---|---|---|
| Home | The launcher picks `BAKE_HOME`, then `DSH_HOME` when `BAKE_HOME` is unset or blank, then `~/.bake`, and exports both variables. The shared helper defaults to `~/.dsh`. | [Unix launcher](../../../../scripts/release/bake), [Windows launcher](../../../../scripts/release/bake.cmd), [helper](../../../../packages/util/home-paths/src/index.ts), [launcher tests](../../../../scripts/release/launcher.test.ts), [helper tests](../../../../packages/util/home-paths/tests/home-paths.spec.ts) |
| Environment settings | `readBakeEnv` reads `BAKE_<name>`, then `DSH_<name>`, and treats blank as unset. An opt-out takes effect under either name. Names in use include `PERMISSION_MODE` (default `workspace-write`; `danger-full-access` disables approval prompts), `TOOLS_MODE`, `TELEMETRY_OTLP_URL`, `TELEMETRY_MODE`, `TELEMETRY_DISABLED`, `NO_UPDATE_CHECK`, `NO_ANIMATION`, `RENDERER`, `AGENTS_HOME`, `RELEASE_BASE_URL`, `INSTALL_ROOT`, and `BIN_DIR`. | [Base bundle](../../../../packages/bundle/base/cordis.patch.yml); the inventory must classify every name |
| Model shell facts | Shell commands receive session facts under both spellings, for example `BAKE_SESSION_ID` and `DSH_SESSION_ID`. Outer `BAKE_*` session facts are not inherited. | [Changelog](../../../../CHANGELOG.md) Unreleased section, [shell packages](../../../../packages/shell/) |
| Settings | `$BAKE_HOME/settings.yaml`, or a configured `.yml`/`.json` path. Writes are synced to disk before replacement. | [settings-file](../../../../packages/settings/settings-file/src/index.ts), [tests](../../../../packages/settings/settings-file/tests/) |
| Profile manifest | `bake.profile.bundles` is read first, then `dsh.profile.bundles`. Migration adds `bake.profile`, keeps `dsh.profile` unchanged for rollback, and saves `package.json.bak`. | [profile](../../../../packages/boot/app-boot/src/profile.ts), [profile tests](../../../../packages/boot/app-boot/tests/profile.spec.ts) |
| Legacy package names | `@deepseek-ai/dsh-*`, `@deepseek-ai/dsh`, and `@dsh-tui/*` resolve to `bake-*` packages with a deprecation warning | [map](../../../../packages/boot/app-boot/src/legacy-package-names.ts), [map test](../../../../scripts/legacy-package-names.test.ts) |
| Skills roots | Project `.dsh/skills` and `.agents/skills`; user `$BAKE_HOME/skills` and `$BAKE_AGENTS_HOME/skills` (default `~/.agents/skills`) | [skill-filesystem](../../../../packages/skill/skill-filesystem/src/index.ts) |

### Persisted data

Rollback means 0.3 can still read whatever 0.4 has written. Each domain below needs a two-direction test at scope 03 or 05.

| Domain | Location | Version or format | Source |
|---|---|---|---|
| Session logs | `$BAKE_HOME/sessions`; plain or Zstandard JSONL generations | Writer format 3; v0→v1→v2→v3 migrations; retired vocabulary kept | [writer constant](../../../../packages/core/session/src/types.ts), [release record](../../../session-format-status.md), [persistence catalog](../../../persistence-catalog.md), [retired vocabulary](../../../../packages/session/session-format-catalog/src/retired-vocabulary.ts) |
| Writer lease | Per-session lock file | POSIX flock and Windows LockFileEx | [persistence tests](../../../../packages/session/session-persistence-jsonl/tests/) |
| Attachments | `$BAKE_HOME/attachments/v1`, content-addressed; derived variants under `$BAKE_HOME/cache/attachments` | Layout `v1` | [attachment-local](../../../../packages/attachment/attachment-local/src/index.ts) |
| Projection cache | `$BAKE_HOME/storages`, domain `session_projcache`, per-record JSON | Version 7; reads versions 3 to 6; invalid records are backed up and skipped | [spec](../../../../packages/session/session-projection-cache/src/spec.ts) |
| Session search | SQLite, `:memory:` with `openAt: never` by default; durable only when a patch sets a path | Opt-in index | [base bundle](../../../../packages/bundle/base/cordis.patch.yml) |
| Credentials | `$BAKE_HOME/.credentials.yaml`, including pi-ai OAuth grants | `DOCUMENT_VERSION = 1` | [credentials-local](../../../../packages/credentials/credentials-local/src/index.ts) |
| Settings | `$BAKE_HOME/settings.yaml`, which also holds terminal preferences and the default model | Unversioned YAML; field-wise merge | [settings-file](../../../../packages/settings/settings-file/src/index.ts) |
| Identity | `$BAKE_HOME/.anonymous-user-id` | Random UUID | [anonymous-user-id](../../../../packages/identity/anonymous-user-id/README.md) |
| Profiles | `$BAKE_HOME/profiles/<name>/{package.json, cordis.patch.yml, pnpm-workspace.yaml, node_modules}` and the shared `profiles/node_modules` fallback | `bake.profile` plus `dsh.profile` | [profile](../../../../packages/boot/app-boot/src/profile.ts) |
| User presets | `$BAKE_HOME/.agent-presets` | `preset.yml` and `agent.cordis.yml` | [discovery](../../../../packages/preset/agent-presets/src/discovery.ts) |
| Update state | `$BAKE_HOME/update-check.json`; install root `current` link (Unix) or `current.txt` (Windows) beside versioned release directories | No version field | [check](../../../../packages/boot/updater/src/check.ts), [updating an install](../../../../distribution/README.md#updating-an-install) |
| Diagnostics | `$BAKE_HOME/diagnostics`: `watchdog.*.jsonl`, `rejections.*.jsonl`, and pnpm logs | Append-only, owner-only | [watchdog](../../../../packages/runtime-diagnostics/runtime-watchdog/README.md), [late rejections](../../../../apps/cli/src/late-rejections.ts) |
| Terminal setup | Edits terminal configuration outside the Bake home, such as VS Code `keybindings.json`, Windows Terminal `settings.json`, and Ghostty config | User-owned files | [terminal setup](../../../../apps/tui/packages/app/src/terminal-setup.ts) |

### Sandbox and platforms

| Target | Current confinement | Native helper | Disposition |
|---|---|---|---|
| `linux-x64`, `linux-arm64` | bubblewrap, then Landlock; reports `full` or `partial`; refuses with `SANDBOX_UNAVAILABLE` instead of running unconfined | [native/system](../../../../native/system/) prebuilt packages | Native parity on capable hosts per backend (scope 04) |
| `darwin-arm64`, `darwin-x64` | Seatbelt | [native/system](../../../../native/system/) prebuilt packages | Native parity (scope 04) |
| `win32-x64` | Restricted token and ACLs; deliberately `partial` ([Windows ACL](../../../../packages/sandbox/sandbox-windows-acl/README.md)); PowerShell tools | No `native/system` package; Win32 process support is in TypeScript | Native parity, including the documented partial guarantee |

Selection lives in [sandbox-local](../../../../packages/sandbox/sandbox-local/README.md). Targets come from `RELEASE_TARGETS` in the [updater manifest](../../../../packages/boot/updater/src/manifest.ts). CI uses Node 24; the root `engines` field allows Node `^22.19.0 || >=24.0.0`, and the installers require Node 24. Minimum OS and libc versions are not recorded anywhere and remain missing evidence.

### Diagnostics, feedback, and telemetry

| Surface | Current behavior | Disposition |
|---|---|---|
| Session telemetry | OTLP export only when `BAKE_TELEMETRY_OTLP_URL` (or `DSH_`) is set. The default mode is then `FEEDBACK_ONLY`, which releases a log prefix only after explicit feedback. `*_TELEMETRY_DISABLED` with any non-empty value opts out. Shutdown drain is bounded to about 3 s. ([otel](../../../../packages/session/session-telemetry-otel/README.md), [test](../../../../apps/cli/tests/telemetry-switch.spec.ts)) | Native parity; a local collector test proves no export without consent |
| Feedback | `/feedback` records locally and never reaches the model ([command-feedback](../../../../packages/feedback/command-feedback/README.md)) | Native parity |
| Anonymous identity | One UUID per home, attached to exports | Native parity |
| Runtime watchdog | Samples V8 heap and event-loop delay and writes rate-limited records | Replace with native measurements; keep the file location and the no-stdout rule |
| Late rejections | Appended to `rejections.*.jsonl`; the session continues | Native equivalent for task panics: record, notify, continue |
| Update check | Hourly cached check, counted by the download service; `BAKE_NO_UPDATE_CHECK=1` disables both | Native parity |
| Self-check | Hidden `--self-check` loads the shipped profiles, preset plugins, and runner modules ([self-check](../../../../apps/cli/src/self-check.ts), [test](../../../../apps/cli/tests/self-check.e2e.ts)) | Native parity, and still runnable by old updaters |

### Packaging, update, and rollback

The current archive contains the Node launcher. The installers ([sh](../../../../distribution/host/install.sh), [PowerShell](../../../../distribution/host/install.ps1)) require Node 24, install into `~/.local/share/bake` (overridable with `BAKE_INSTALL_ROOT`), and link `bake` and `dsh` from `~/.local/bin` (overridable with `BAKE_BIN_DIR`). The updater downloads the single signed `latest.json` from `https://bake.justar.dev`, checks size and SHA-256, and runs the candidate's `apps/cli/lib/bin.js --self-check` under the running Node ([verify](../../../../packages/boot/updater/src/verify.ts)). Only then does it move `current`. `--rollback` moves to the newest older installed release.

Obligations for 0.4:

1. A released 0.3 updater must be able to install, launch-check, and roll back the 0.4.0 archive, and the route must still leave the shipped 0.4 artifacts Rust only ([D14](#decision-register)). A preparatory 0.3.x updater is one candidate; the route is not chosen. It needs a test that starts from the oldest supported 0.3 updater.
2. `bake update --rollback` from 0.4 must return to a working 0.3 release, and that release must read every [persisted domain](#persisted-data) 0.4 wrote.
3. The `dsh` link, `dsh.cmd`, and user-owned aliases survive install, update, and rollback.
4. No archive may overwrite `latest.json` with an older version, and a maintenance manifest can never replace the stable one ([D13](#decision-register)).

## Profile migration and native extension strategy

[D5](#decision-register) is decided: 0.4 ships Rust only, and a profile the native runtime cannot run needs migration. [D6](#decision-register) and [D7](#decision-register) remain recommendations. The TypeScript runtime stays runnable in development as the comparison oracle; no shipped command selects it.

**Profile classification.** Before any agent starts, the native launcher composes the profile's layers and checks every row and expression against a closed table of native-recognized rows. Each row in that table is pinned by package name, row id, and the exact source text of any shipped `!!js`. If every row matches, the profile runs natively. Otherwise the launcher reports the first unrecognized construct and its file and does not start the profile. It never executes the construct and never rewrites the profile to remove it. The migration path for such profiles is still to be designed. `--dump-config` and `--self-check` report whether a profile is native-recognized.

**Boundaries that hold regardless of implementation:**

- **Native code cannot transparently host arbitrary Cordis JavaScript.** Cordis plugins depend on scoped services, waterfall events, the Loader, and in-process module identity. A JSON-RPC shim around individual plugins would not reproduce those semantics. Arbitrary plugins therefore need migration to a native surface; 0.4 does not run them.
- **The Electron utility process needs integration work.** Bake Desktop can deliver messages through `process.parentPort`, which exists only inside an Electron utility process running JavaScript. A native executable cannot be that process. The launch path is open ([D9](#decision-register)); whichever path the Desktop maintainers approve needs a test against the real Desktop consumer.
- **Code mode stays JavaScript.** `run_code` executes model-written TypeScript in a confined QuickJS VM ([codemode](../../../../packages/ptc-runtime/ptc-runtime-codemode/README.md)). A native host embeds an equivalent confined engine. It does not change the language the model writes.
- **Development comparison.** Work that runs in the TypeScript runtime never closes a native scope. A native profile never delegates to it.

**Recommended native extension surfaces in 0.4.0 ([D6](#decision-register)):** MCP servers (stdio and Streamable HTTP) and hook processes. Both are already out-of-process and language-neutral. A versioned native plugin protocol, covering tool, command, and prompt contributions with explicit lifetimes, is recommended for a later 0.4.x release. It must be designed around named consumers, and its catalog must be generated from its real interface, never from the Cordis API catalog.

## Unreleased develop work

`v0.3.8` is the merge commit `dcb26d756e` on `main`, and its tree equals `develop` commit `a1ec50245e`. Since then, `develop` has merged PRs #43 to #50, 40 non-merge commits across 1,847 changed files. The table omits documentation-only commits. [The changelog](../../../../CHANGELOG.md) lists the user-visible items under Unreleased, and `evals/agent-loop/versions/unreleased/` holds two records: `2026-10-05-bake-core-names` and `2026-10-06-eval-fidelity`. This page reverts nothing. Each row needs an owner decision under [D12](#decision-register).

| Change | Commits | Kind | Recommended handling for the last 0.3 release |
|---|---|---|---|
| Terminal loss before `SIGHUP` no longer races shutdown | `f0b5b602d0` | Bug fix | Include |
| Core, internal, plugin, and bundle packages renamed to `bake-*`, with deprecated aliases, automatic profile rewrite, and `package.json.bak` | `277a05fc73`, `f87310e428`, `eaba1648f6`, `b8ee76c01f`, `e3a537e9db`, `c835a8c06d` | Identity change that rewrites user profile files | Include only after this test passes: a profile migrated by `develop` boots on a real `v0.3.8` install after `bake update --rollback`, and an out-of-tree plugin importing an old name still loads |
| `bake` becomes the launcher, with a `dsh` alias, installer and updater `dsh` links, and preservation of a `dsh` that belongs to another install | `e5ac162881`, `7f8c3834aa`, `9ae05abe27` | Identity and Desktop compatibility | Include after a Bake Desktop `dsh --profile desktop` launch test on Unix and Windows |
| `BAKE_*` environment names with `DSH_*` fallback, blank read as unset, session facts under both names | `645fc92b46`, `2aeaf7f1e3`, `b9780f0ad0` | Compatibility | Include; verify that a `DSH_*`-only environment behaves as on 0.3.8 |
| `bake.profile` manifest, with `dsh.profile` kept and the migration locked | `49c9c66fb5`, `5b72f18e5f` | Persisted-file change | Include, under the same rollback test as the rename |
| TUI bundle inlining and bin classification | `c24576ab39`, `e104ea4db0` | Build | Include; gate on `release:preflight` and `--self-check` on all five targets |
| Removal of unused upstream leftovers | `ff53e9e691` | Cleanup | Include after confirming that no shipped consumer is affected |
| Eval fidelity, model-surface snapshots, eval records, preflight eval step, code-mode test timing | `2cce0125b7`, `6fd3b82bcb`, `eac853e994`, `e408faf28c`, `a47903e760`, `8a77fb7514`, `a20da688f0`, `a3853cd5d6`, `56991523c4`, `0070a2221d`, `7f18687d5f` | Development only | Include; no runtime effect |

**Recommendation.** Release this work as 0.3.9 once the listed gates pass, then freeze 0.3 to bug fixes. Holding it back would require an immediate maintenance branch from `v0.3.8`, and the current rules do not allow one ([release lines](#release-lines-and-support-window)). The rename also defines the names and aliases that the native port must reproduce, so shipping it makes the 0.3 oracle and the 0.4 compatibility contract the same thing. Until the owner decides, the scope-00 baseline captures both `v0.3.8` and `develop` and records every difference.

## Release lines and support window

[D1](#decision-register) fixes the release lines. Everything else in this section is an **Owner decision** ([D13](#decision-register)). This page changes no branch protection, workflow, or AGENTS instruction.

**Before 0.4.0.** 0.3.x patch releases continue through the existing `develop` → `main` flow. Each 0.3 fix adds a language-independent regression case and names the Rust scope that must adopt it.

**Recommended post-0.4 design**, implemented by one separate governance PR that updates AGENTS, CONTRIBUTING, branch checks, release scripts, and the updater together:

1. A protected maintenance branch for 0.3 (proposed name `release/0.3`), created from the last qualified 0.3 tag. Patch PRs target it, and each fix is also ported forward to `develop`.
2. A separately signed maintenance manifest, for example `channels/0.3/latest.json` (proposed path), that the release workflow can never publish as `latest.json`.
3. A preparatory 0.3.x updater that lets an install pin itself to the 0.3 channel. Released 0.3 updaters read only `latest.json`, so unpinned installs receive 0.4.0 on their next `bake update`. That makes [packaging obligation 1](#packaging-update-and-rollback) a cutover blocker.
4. The governance PR lands before the scope-17 default-switch PR.

**Support window.** Recommendation: after 0.4.0, the 0.3 maintenance line takes security, data-loss, update, and rollback fixes only, for a fixed window. A 90-day window, or until 0.4.2 ships if that is later, is a placeholder. **Duration: awaiting owner decision.** The owner also names the maintenance owner and the end-of-support notice.

## Incremental delivery under the develop to main rule

Every Rust scope merges to `develop` through ordinary PRs. `main` accepts only a `develop` merge, so any release cut before 0.4.0 also ships whatever Rust code is on `develop`. Incremental delivery is safe while these four conditions hold:

1. Rust code stays in its own workspace, is excluded from release archives, and is never selected by default startup. Scope 01 adds a release check for this.
2. Release notes and the changelog never claim native behavior that is not yet the default.
3. A 0.3 hotfix during 0.4 development is a PR to `develop` followed by a normal release. This works only while the Rust code on `develop` is inert.
4. Once a change on `develop` stops being inert, 0.3 releases need the maintenance branch. The first such change is the default switch, but a packaging change or a shared-data change also counts. So the governance PR must land first.

## Acceptance matrix

Each row is one support entry. Rows link to their current oracle tests, and scope 00 closes only when every row has its evidence or an approved disposition. "Gap" names evidence that is missing today, not a failing test.

| ID | Entry | Current oracle | Disposition | Scope | Acceptance evidence | Gap |
|---|---|---|---|---|---|---|
| A1 | `tui` profile, all four presets | PTY driver, app tests, preset model surfaces | Native; `cordis` preset needs migration ([D7](#decision-register)) | 13–15 | Every PTY scenario mapped to a native counterpart with the same success condition; byte-identical model surface per preset | No Windows ConPTY scenarios |
| A2 | `headless` profile | Built-profile tests, headless model surface | Native | 09, 13 | Flags, stdin, `--json` events, exit 75, signal exits, resume | None identified |
| A3 | `desktop` profile and protocol v1 | Desktop tests, desktop model surface | Native plus Desktop launch integration ([D9](#decision-register)) | 13 | ready/init/message/cancel/shutdown, approval withdrawal, spans, launched by the real Desktop app | External consumer never tested |
| A4 | Launcher, `plugin`, `update`, dumps, `--self-check` | [CLI tests](../../../../apps/cli/tests/), help snapshot | Native; `plugin` needs migration ([D7](#decision-register)) | 13, 16 | Help, errors, and exit codes match; self-check runs under an old updater | — |
| A5 | Model-facing tools and text | Model-surface snapshots, tool package tests | Native, byte-identical | 08–12 | Exact schemas, results, and errors; paired eval record | Windows `pwsh` roster not snapshotted |
| A6 | Generic protocols, `deepseek-official`, CLIProxyAPI | [pi-ai tests](../../../../packages/llm/llm-pi-ai/tests/), [CLIProxyAPI test](../../../../apps/tui/packages/app/tests/cliproxyapi.test.ts) | Native, Tier 1 | 06–07 | Wire fixtures, local stream replay, key-gated smoke tests | No live smoke on record |
| A7 | Catalog APIs outside the generic three | Catalog tests only | Tier 2: native port, or an approved support change | 07 | Per-family fixtures, or an approved support change | No per-family fixtures |
| A8 | Login: API key, OAuth, device code | [login spec](../../../../packages/llm/llm-pi-ai/tests/login.spec.ts), [terminal login](../../../../apps/tui/packages/app/tests/login.test.ts) | Native | 05, 07, 15 | Cancel exposes no secret; refresh races; failed refresh | No recorded provider OAuth wire flow |
| A9 | Profile composition, `!!js`, npm plugins | [boot tests](../../../../packages/boot/app-boot/tests/), [plugin manager tests](../../../../packages/boot/plugin-manager/tests/) | Native classification; migration for the rest | 05, 12 | Unrecognized construct detected before any agent or file write, and never executed or rewritten; real fixture plugins exercise the chosen migration path | No corpus of real third-party plugins |
| A10 | Home, environment, legacy names | Home-path and launcher tests, legacy map test | Native parity | 05, 13 | Isolated-home launcher runs, blank values, `DSH_*`-only environments | — |
| A11 | Persisted domains | Persistence, projection cache, credentials, settings, attachment tests | Native read and write; 0.3 reads 0.4 output | 02–05, 10 | Two-direction tests per domain against every release in the rollback set | Rollback set not chosen |
| A12 | Sandbox per OS | [sandbox](../../../../packages/sandbox/sandbox-local/tests/), [subprocess](../../../../packages/subprocess/subprocess-local/tests/) | Native parity | 04 | Real denied effects and no surviving processes on each OS and backend | Landlock-only and Windows host runs |
| A13 | MCP and hooks | [MCP tests](../../../../packages/mcp/mcp-client/tests/), [hook tests](../../../../packages/hooks/) | Native | 12 | Local protocol servers, reconnect, cancel, shutdown | — |
| A14 | Code mode | [codemode tests](../../../../packages/ptc-runtime/ptc-runtime-codemode/tests/) | Native host with an embedded JavaScript engine | 12 | Limits, nested approvals, and denied ambient access match | Engine not chosen |
| A15 | Telemetry, feedback, identity, diagnostics | [otel](../../../../packages/session/session-telemetry-otel/tests/), [feedback](../../../../packages/feedback/command-feedback/tests/), [identity](../../../../packages/identity/anonymous-user-id/tests/), [watchdog](../../../../packages/runtime-diagnostics/runtime-watchdog/tests/) | Native; watchdog re-measured | 13 | No export without consent; bounded shutdown; no credentials in diagnostics | Native measurement design |
| A16 | Install, update, rollback, aliases | [updater tests](../../../../packages/boot/updater/tests/), [release script tests](../../../../scripts/release/) | Native plus transition | 16 | Old updater to 0.4 and back; held Windows files; `dsh` link kept | Oldest supported updater not chosen |
| A17 | Performance | [terminal performance driver](../../../../apps/tui/packages/app/performance/README.md) | Baseline first | 00, 17 | Frozen budgets from repeated baseline runs | [Initial measurements](baseline-2026-10-07/README.md) and [shutdown observations](terminal-2026-10-07/README.md) captured; full qualification and budgets remain open |
| A18 | Removed products | — | Excluded | — | No restored web, ACP, SDK, upstream desktop, or docs site | — |

## Native consumer coverage gaps

These consumers or behaviors have no test that a native implementation could be checked against today:

- **Bake Desktop:** the external app, its Electron utility-process launch, and its pinned protocol copy. Access to that checkout is a dependency.
- **Third-party plugins and user `!!js` layers:** no fixture corpus exists. Real plugins are needed to test classification and migration.
- **Catalog providers:** no live smoke tests and no per-family wire fixtures for the ten non-generic API families. pi-ai's nine OAuth providers have no recorded flows. Bedrock and Vertex authentication goes through their SDKs and has not been inventoried per provider.
- **Windows terminal:** the PTY driver is POSIX-only, and no ConPTY scenario exists.
- **Old updaters:** no test runs a released 0.3 updater against a changed archive layout.
- **Retained `api`, `client`, `host`, and `typert` packages:** `bake-typert-registry`, `bake-typert-loader`, and `bake-api-gateway` are composed in the base bundle. Their consumers, including the `sessionFeedback` Host Remote, have not been traced.
- **Terminal setup:** edits to external terminal configuration files have no cross-runtime test.
- **`bake plugin`:** depends on pnpm being installed on the host and installs JavaScript plugins that 0.4 cannot run. Its migration or replacement is undecided ([D7](#decision-register)).
- **Platform minimums:** no recorded minimum OS, kernel, glibc, or macOS deployment target.
- **Performance:** the instrumentation is Node-specific, and several interactive workloads are excluded.

## Dev Note

Non-authoritative. The pi-ai counts come from grepping `node_modules/@earendil-works/pi-ai/dist/providers/data/*.json` and listing `dist/auth/oauth/` locally. No pi-ai code was executed, and no network call was made. A planned offline enumeration script could not run in this session. The scope-00 inventory slice should regenerate these counts from source and replace this table with a link. Tool rosters come from the committed model-surface snapshots, not from a run.
