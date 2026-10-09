# Scope 00: product support decisions and acceptance matrix

## Summary

This page lists what Bake ships today, what the Rust 0.4 line must preserve, and which questions only the product owner can answer. The inventory comes from source at `origin/develop` `ae5eb51ab6` and the `v0.3.8` tag `dcb26d756e`, read on 2026-10-07. It belongs to [scope 00](README.md) of the [Rust migration roadmap](../README.md).

Since [D22](#decision-register), Pi is the primary port source and Bake's TypeScript runtime a selective reference. The owner retained the CLIProxyAPI provider route, reading existing Session logs, Session format 3 for new writes, and the model-visible surface. The inventory below records what 0.3 ships so each behavior can be kept, replaced by Pi's, or retired by decision; it is no longer a parity target by default. The approved exceptions are the Node/TypeScript runtime and custom JavaScript/Cordis profiles: 0.4 ships Rust only and excludes those profiles, with no compatibility runtime or profile migration deliverable ([D5](#decision-register)). 0.4.0 must also ship a native plugin API ([D6](#decision-register)). This page tests nothing; isolated qualification probes do not constitute a native implementation. Provider, platform, and Desktop rows say what evidence is missing; none claims native parity.

## Table of Contents

- [How to read decisions](#how-to-read-decisions)
- [Decision register](#decision-register)
- [Shipped surface](#shipped-surface)
- [Profile support and native extension strategy](#profile-support-and-native-extension-strategy)
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
| D2 | Parity target | Superseded by [D22](#decision-register) | Every behavior the final 0.3 release ships is the 0.4.0 target. A narrower target needs an owner-approved support change, release notes, and an explicit transition policy; [D5](#decision-register) excludes custom-profile migration from that policy. |
| D3 | Removed products | Decided by request | The web client, upstream desktop app, ACP, Python SDK, docs site, and upstream automation stay removed ([AGENTS](../../../../AGENTS.md#porting-from-upstream)). The `desktop` bundle is Bake's own and is kept. |
| D4 | Session data | Scope-00 default; confirmed by [D22](#decision-register) | Keep Session format 3, its adjacent migrations, and the retired vocabulary. A Rust port is not a reason for a format change. |
| D5 | TypeScript runtime and custom profiles | Decided by request, updated 2026-10-07 | 0.4 ships Rust only. The owner now directs: "cut custom JavaScript/Cordis profiles." Custom Cordis compositions, user presets backed by `agent.cordis.yml`, user executable YAML, custom bundles, and npm profile plugins are excluded; supporting or converting them is not a 0.4.0 deliverable. This supersedes the earlier custom-profile migration requirement. The native entry rejects these inputs before execution or file changes. TypeScript remains a development oracle; no Node/TypeScript compatibility runtime ships. Built-in profiles, session data, Desktop, `run_code`, legacy names, and rollback keep their separate contracts ([strategy](#profile-support-and-native-extension-strategy)). |
| D6 | Native plugin API | Decided by request, 2026-10-07 | The product owner answered: "Require a native plugin API in 0.4.0." MCP servers and hook processes stay supported; 0.4.0 also requires a native plugin API. Scope 12 designs its execution model, protocol or ABI, contributions, permissions, lifetimes, and versioning, and proves it with a named consumer ([strategy](#profile-support-and-native-extension-strategy)). These design choices remain open. |
| D7 | Cordis preset and Cordis tooling | **Owner decision** | The `cordis` preset, `cordis_inspect_*`, `plugin_manager`, the Cordis host runner, and `bake plugin` operate on JavaScript plugins that 0.4 cannot run ([D5](#decision-register)). Each needs a designed migration of its shipped capability, a native replacement, or a separately approved retirement under [D2](#decision-register). This row concerns shipped Cordis tooling; the custom profiles it manages are excluded by [D5](#decision-register). Its migration, native replacement, or retirement still needs a separate decision; no option requires support for the excluded custom profiles. |
| D8 | Provider breadth | Reopened by [D22](#decision-register): CLIProxyAPI is retained; other routes are an **Owner decision** | Every route reachable today is the target. The [provider section](#provider-routes-and-authentication) recommends qualification tiers. Each family needs native qualification or a separately approved support change; no Node fallback discharges it. |
| D9 | Bake Desktop launch | **Owner decision**, with the Desktop maintainers; [D22](#decision-register) did not retain the Desktop contract, so retiring it is an option | Keep `dsh --profile desktop` and protocol version 1. How Desktop launches the native executable is open; candidates include spawning it over stdio or bridging Electron `parentPort` to that stdio, and none is chosen. Bake's TypeScript runtime cannot serve as the bridge ([D5](#decision-register)). The chosen path needs a test with the real Desktop consumer. |
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
| D20 | Pi agent reference | Decided by request, 2026-10-07; role superseded by [D22](#decision-register) | Reference the latest official Pi agent codebase when shaping the Rust harness; use its latest MCP client and lifecycle as the reference for native MCP in scope 12. [The source review](../pi-reference.md) pins the latest stable release and inspected main revision, identifies reusable ownership patterns, and separates them from Bake compatibility requirements. Review newer Pi releases before each relevant implementation scope; record any refreshed revision explicitly. |
| D21 | Session format admission domain | Decided by request, 2026-10-08 | The product owner accepted the recommendation. A native reader must decide every Session log a released Bake writer can produce exactly as the TypeScript reader does. A spelling no released writer produces may instead be refused with a named native limit, which is documented and witnessed by a shared case: negative zero, a fraction or exponent where a released validator requires an integer, an integer outside the safe range, a duplicate member, a non-string or `Object.prototype` event type, and a value that only JavaScript coercion or a `TypeError` decides. Spellings released writers do produce stay ports, not limits: fractions in free numeric data such as `llm/retry` `delayMs`, and any exponent `JSON.stringify` emits. Escaped lone surrogates stay named limits until writer reachability is settled. This keeps Session format 3 ([D4](#decision-register)); it defines what equivalence must cover ([evidence](#persisted-data)). |
| D22 | Primary port source | Decided by request, 2026-10-09 | The product owner directed: "prioritize port from Pi as main, and DeepSeek (Bake TS) is a selective port to meet our design." Pi's latest official source is the primary design and code source for the Rust implementation; Bake's TypeScript runtime is ported selectively. The owner retained four Bake contracts: the CLIProxyAPI provider route ([setup](../../../../apps/tui/packages/app/src/cliproxyapi.ts): URL forms, catalog discovery, per-model wire protocol, effort and limit backfill, login, refresh, and logout); reading existing Session logs ([D23](#decision-register)); Session format 3 for new writes, with its generations and writer lease ([D4](#decision-register)); and the model-visible surface: tool names, schemas, and results, prompts, and repair text. Desktop and CLI contracts were not retained: `dsh --profile desktop`, CLI flags and exits, and the settings and credentials files follow Pi's shapes unless a later decision keeps them. This supersedes the 0.3 parity target of [D2](#decision-register) and the reference-only role of [D20](#decision-register), and reopens [D8](#decision-register) and [D9](#decision-register). [D5](#decision-register), [D6](#decision-register), and [D21](#decision-register) stand. Adopted Pi code keeps its MIT notice and a recorded revision ([Pi reference](../pi-reference.md#compatibility-and-adoption)). |
| D23 | Existing Bake sessions | Decided by request, 2026-10-09 | 0.4 reads existing `~/.bake` Session logs: format 3, Zstandard generations, and the v0 to v2 migrations, through the `bake-session` readers, so upgrading loses no history. |
| D24 | Sandbox and permissions under D22 | Decided by request, 2026-10-09 | Pi runs tools without a sandbox or Bake's approval policy. The owner accepted the recommendation: 0.4 keeps Bake's sandbox backends, approval prompts, and permission presets, ported selectively into Pi's structure in scope 04. Denied-call results stay part of the retained model-visible surface ([plan](../pi-first-plan.md#owner-decisions)). |
| D25 | Existing settings and credentials under D22 | Decided by request, 2026-10-09 | Their file formats are not retained. The owner accepted the recommendation: on first run, 0.4 imports the CLIProxyAPI route and key and the default model from `~/.bake/settings.yaml` and the credentials file, read-only and leaving those files unchanged, so the retained provider works without signing in again ([plan](../pi-first-plan.md#owner-decisions)). |
| D26 | Session ids that are Windows-reserved names | Decided by request, 2026-10-09 | On a POSIX path platform, `PlainLogFile` encodes a configured `agents[].sessionId` such as `con`, `nightly.`, or `aux.txt` with `encodeSegment` alone and lays it out exactly as TypeScript does. On a Win32 path platform it refuses reserved device names and names ending in `.` as the named limit `windows-name`, because Node changes or rejects those paths there. A Windows TypeScript writer can still produce a `nightly.` log, so the refusal stays classified as a reachable gap; D26 accepts it as an owner exception to D21 ([classification](../scope-02/native-limits.md#reachable-gaps)). |
| D27 | Deeply nested Session payloads | Decided by request, 2026-10-09 | The owner chose a hand-written iterative parser over a stack-growing dependency or a kept depth limit. `parse_json`, std-only in `bake-session`, replaces serde_json for current-format scan, header, and generation-header reads with no nesting limit; parsed payloads are held in `Deep` and copied, compared, walked, and dropped iteratively through restoration (including the interrupted-turn closers), request derivation, the restored-log projections, and the CLI, so payloads of any depth are read. The older-generation migrations, their final check, and migrated restoration read payloads of any depth the same way, so no depth bound remains ([classification](../scope-02/native-limits.md#reachable-gaps)). |

## Shipped surface

Each subsection lists one product surface, links its source, and gives its 0.4 disposition. "Native parity" means the Rust implementation must match current behavior. "Migration" means 0.4 cannot run the surface as it is, because it needs the Node/TypeScript runtime that 0.4 does not ship ([D5](#decision-register)); its migration or replacement path is still to be designed.

### Profiles

[Profile templates](../../../../packages/boot/app-boot/src/profile.ts) initialize three shipped profiles under `$BAKE_HOME/profiles/<name>`. A name with no template starts from `bake-base`, or from `--from-default-profile <template>`.

| Profile | Bundles | Entry and transport | Current oracle | 0.4 disposition |
|---|---|---|---|---|
| `tui` | `bake-base`, `bake-tui-app` | `bake tui`; inline or fullscreen Ink terminal | [App tests](../../../../apps/tui/packages/app/tests/), the 42 scenarios in the [PTY driver](../../../../apps/tui/scripts/pty-smoke.ts), and [preset model surfaces](../../../../apps/tui/packages/app/tests/expected/model-surface/) | Native parity (scopes 13–15) |
| `headless` | `bake-base`, `bake-headless` | `bake headless <task>`; answer on stdout, or NDJSON events with `--json`; diagnostics on stderr | [Built-profile tests](../../../../apps/cli/tests/profiles/) and [model surface](../../../../apps/cli/tests/profiles/expected/model-surface/headless.md) | Native parity (scopes 09, 13) |
| `desktop` | `bake-base`, `bake-desktop` | The separate Bake Desktop app runs `dsh --profile desktop`; NDJSON stdio or Electron `parentPort` ([transport](../../../../packages/bundle/desktop/src/transport.ts)), `PROTOCOL_VERSION = 1` ([protocol](../../../../packages/bundle/desktop/src/protocol.ts)) | [Desktop tests](../../../../packages/bundle/desktop/tests/) and [model surface](../../../../apps/cli/tests/profiles/expected/model-surface/desktop.md); the external consumer is untested | Native parity plus a launch adapter (scope 13, [D9](#decision-register)) |
| Custom profile | User-chosen bundles, user patch layer, pnpm-installed plugins | `bake <name>` | [Profile tests](../../../../packages/boot/app-boot/tests/profile.spec.ts) | Excluded from 0.4; reject before execution or file changes, with no compatibility or migration deliverable ([D5](#decision-register)) |

### Presets and model-facing tools

The [shipped presets](../../../../packages/preset/agent-presets/presets/) are `standard` (order 1), `ptc` (order 2), `minimal` (order 3), and `cordis`, titled "Creation mode" (order 4). In 0.3, users can add presets under `$BAKE_HOME/.agent-presets` ([discovery](../../../../packages/preset/agent-presets/src/discovery.ts)), and `bake tui --preset <name>` selects one. User presets backed by `agent.cordis.yml` are excluded from 0.4 under [D5](#decision-register); discovery may report them as unsupported, and selection must refuse before any composition runs. The rosters below come from the Linux model-surface snapshots. On Windows, `pwsh` replaces `bash` through `process.platform` rows.

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
| Launcher | `bake`/`dsh`, `[--profile] <name>`, `--from-default-profile <name>`, repeatable `--patch <path>`, `--dump-config`, `--dump-default-config`, `--dump-config-schema`, `-V/--version`, launcher `-h` only when no profile is given, hidden `--self-check` | Preserve retained profile selection and native configuration values, precedence, dumps, and diagnostics. Scope 05 defines typed native overlays for `--patch`; Cordis YAML and `!!js` are refused. `--from-default-profile` cannot create a Cordis profile; its native configuration equivalent is designed in scope 05 ([D5](#decision-register)) |
| `plugin` | `bake plugin --profile <name> <pnpm args>` forwards to host pnpm and reports exit 127 when pnpm is absent ([source](../../../../apps/cli/src/plugin.ts)) | Migration ([D7](#decision-register)); native profiles reject npm plugins before boot ([strategy](#profile-support-and-native-extension-strategy)) |
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
| User `!!js`, unknown package rows, custom bundles | Evaluated or loaded by Cordis | Excluded from 0.4 ([D5](#decision-register)); reject before agent startup, without executing or rewriting the profile |
| npm plugins installed with `bake plugin` | Resolved through the profile's `node_modules` and the shared fallback | Custom npm profile plugins are excluded ([D5](#decision-register)); native plugin management remains part of [D7](#decision-register) |
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

Rollback means the supported rollback release can still read shared data written by 0.4. Shared domains below need two-direction tests at scope 03 or 05. Legacy Cordis profile and preset files are excluded inputs: test that 0.4 leaves them byte-for-byte unchanged instead. Native configuration uses a separate documented format and does not overwrite those files; scope 05 defines its location and scope 16 qualifies rollback.

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
| Profiles | `$BAKE_HOME/profiles/<name>/{package.json, cordis.patch.yml, pnpm-workspace.yaml, node_modules}` and the shared `profiles/node_modules` fallback | `bake.profile` plus `dsh.profile`; legacy files remain unchanged under D5 | [profile](../../../../packages/boot/app-boot/src/profile.ts) |
| User presets | `$BAKE_HOME/.agent-presets` | `preset.yml` and `agent.cordis.yml`; Cordis presets are excluded and their files remain unchanged | [discovery](../../../../packages/preset/agent-presets/src/discovery.ts) |
| Update state | `$BAKE_HOME/update-check.json`; install root `current` link (Unix) or `current.txt` (Windows) beside versioned release directories | No version field | [check](../../../../packages/boot/updater/src/check.ts), [updating an install](../../../../distribution/README.md#updating-an-install) |
| Diagnostics | `$BAKE_HOME/diagnostics`: `watchdog.*.jsonl`, `rejections.*.jsonl`, and pnpm logs | Append-only, owner-only | [watchdog](../../../../packages/runtime-diagnostics/runtime-watchdog/README.md), [late rejections](../../../../apps/cli/src/late-rejections.ts) |
| Terminal setup | Edits terminal configuration outside the Bake home, such as VS Code `keybindings.json`, Windows Terminal `settings.json`, and Ghostty config | User-owned files | [terminal setup](../../../../apps/tui/packages/app/src/terminal-setup.ts) |

A native reader's equivalence obligation follows [D21](#decision-register). On 2026-10-08, a scan of one development home (80 Zstandard format-3 logs, 34,705 rows; no v0 to v2 logs) found 54 fractional `llm/retry` `delayMs` values and no negative zero, exponent, unsafe integer, duplicate member, lone surrogate, or non-string or `Object.prototype` event type. That is supporting evidence for the writer domain, not proof: historical v0 to v2 writers and other homes remain unscanned. Before scope 02 closes, classify each conformance-table native limit as writer-reachable, which must be ported, or writer-unreachable, which D21 permits. The [native limit classification](../scope-02/native-limits.md) records that classification and its open gaps.

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

## Profile support and native extension strategy

[D5](#decision-register) excludes custom JavaScript/Cordis profiles from 0.4.0. The release does not provide a compatibility loader, translator, or a migration project for those profiles. [D6](#decision-register) requires a native plugin API. [D7](#decision-register) covers the remaining decision about shipped Cordis tooling. TypeScript remains a development oracle; no shipped command selects it.

**Native profile boundary.** Scope 05 translates shipped bundle behavior into typed native configuration. Pin shipped expressions in comparison fixtures; the Rust launcher does not interpret Cordis compositions or executable YAML. Requests to load, create, or install into a custom JavaScript/Cordis profile must fail before profile code, package scripts, an agent, or a configuration write runs. Report the unsupported profile and its source; leave its files unchanged. A custom profile is excluded even if its rows happen to match shipped rows. Retained `tui`, `headless`, and `desktop` profiles remain configurable through typed native settings and overlays, including MCP server definitions and hook-process configuration. Scope 05 defines their schema and the mapping of launcher flags; compatibility applies to retained values and precedence, not to loading legacy `cordis.patch.yml`. `--patch` accepts only the defined native schema, with no package rows, module resolution, or executable expressions. Native configuration and the plugin API receive their own documented interfaces. `--dump-config` and `--self-check` follow the same boundary.

**Existing built-in profile directories.** A nonempty legacy `cordis.patch.yml` must cause native startup to refuse with the path and a diagnostic explaining that its settings need to be entered in native configuration. Do not execute, import, rewrite, or silently ignore its entries, even when they configure only MCP, hooks, or session search. Empty and comment-only layers may be treated as unconfigured after non-executing format validation; malformed layers refuse. Scope 05 defines the native configuration location and a deliberate way to select it without overwriting the legacy files. A custom profile or Cordis user preset remains excluded even if its layer is empty.

**Boundaries that hold regardless of implementation:**

- **Native code cannot transparently host arbitrary Cordis JavaScript.** Cordis plugins depend on scoped services, waterfall events, the Loader, and in-process module identity. A JSON-RPC shim around individual plugins would not reproduce those semantics. Custom JavaScript/Cordis plugins are outside the native support target. Authors may write new native extensions once its API exists; Bake does not promise to convert their old plugins.
- **The Electron utility process needs integration work.** Bake Desktop can deliver messages through `process.parentPort`, which exists only inside an Electron utility process running JavaScript. A native executable cannot be that process. The launch path is open ([D9](#decision-register)); whichever path the Desktop maintainers approve needs a test against the real Desktop consumer.
- **Code mode stays JavaScript.** `run_code` executes model-written TypeScript in a confined QuickJS VM ([codemode](../../../../packages/ptc-runtime/ptc-runtime-codemode/README.md)). A native host embeds an equivalent confined engine. It does not change the language the model writes.
- **Development comparison.** Work that runs in the TypeScript runtime never closes a native scope. A native profile never delegates to it.

**Native extension surfaces in 0.4.0 ([D6](#decision-register)):** MCP servers (stdio and Streamable HTTP) and hook processes stay supported. They do not satisfy the plugin API requirement on their own. 0.4.0 must also ship a native plugin API. No API exists yet, and its form is not chosen. Scope 12 designs it and settles:

- the protocol or ABI, including whether plugins run in process or out of process;
- which contributions a plugin can make;
- permissions, and how plugin effects pass the same approval and sandbox rules as built-ins;
- lifetimes, including failure and a shutdown that awaits owned work;
- versioning and compatibility rules;
- proof through at least one named consumer exercised against the real native runtime.

Any model-visible catalog for the API must be generated from its real interface, never from the Cordis API catalog. The API does not by itself migrate Cordis/JavaScript plugins or replace Cordis tooling; [D7](#decision-register) covers those.

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
| A9 | Shipped composition and excluded custom JavaScript/Cordis profiles | [boot tests](../../../../packages/boot/app-boot/tests/), [plugin manager tests](../../../../packages/boot/plugin-manager/tests/) | Translate shipped behavior; custom-profile support and migration excluded ([D5](#decision-register)) | 05, 12, 13 | Native shipped profiles work; custom profiles, Cordis user presets, nonempty legacy built-in layers, executable YAML, and npm installs refuse before code, package scripts, agents, or file writes run | Native refusal fixtures are missing; a third-party migration corpus is not required |
| A10 | Home, environment, legacy names | Home-path and launcher tests, legacy map test | Native parity | 05, 13 | Isolated-home launcher runs, blank values, `DSH_*`-only environments | — |
| A11 | Persisted domains | Persistence, projection cache, credentials, settings, attachment tests | Native shared-domain reads and writes; excluded Cordis files preserved unchanged | 02–05, 10 | Two-direction tests for shared domains against the rollback set; byte-preservation tests for excluded files | Rollback set not chosen |
| A12 | Sandbox per OS | [sandbox](../../../../packages/sandbox/sandbox-local/tests/), [subprocess](../../../../packages/subprocess/subprocess-local/tests/) | Native parity | 04 | Real denied effects and no surviving processes on each OS and backend | Landlock-only and Windows host runs |
| A13 | MCP and hooks | [MCP tests](../../../../packages/mcp/mcp-client/tests/), [hook tests](../../../../packages/hooks/) | Native | 12 | Local protocol servers, reconnect, cancel, shutdown | — |
| A14 | Code mode | [codemode tests](../../../../packages/ptc-runtime/ptc-runtime-codemode/tests/) | Native host with an embedded JavaScript engine | 12 | Limits, nested approvals, and denied ambient access match | Engine not chosen |
| A15 | Telemetry, feedback, identity, diagnostics | [otel](../../../../packages/session/session-telemetry-otel/tests/), [feedback](../../../../packages/feedback/command-feedback/tests/), [identity](../../../../packages/identity/anonymous-user-id/tests/), [watchdog](../../../../packages/runtime-diagnostics/runtime-watchdog/tests/) | Native; watchdog re-measured | 13 | No export without consent; bounded shutdown; no credentials in diagnostics | Native measurement design |
| A16 | Install, update, rollback, aliases | [updater tests](../../../../packages/boot/updater/tests/), [release script tests](../../../../scripts/release/) | Native plus transition | 16 | Old updater to 0.4 and back; held Windows files; `dsh` link kept | Oldest supported updater not chosen |
| A17 | Performance | [terminal performance driver](../../../../apps/tui/packages/app/performance/README.md) | Baseline first | 00, 17 | Frozen budgets from repeated baseline runs | [Initial measurements](baseline-2026-10-07/README.md) and [shutdown observations](terminal-2026-10-07/README.md) captured; full qualification and budgets remain open |
| A18 | Removed products | — | Excluded | — | No restored web, ACP, SDK, upstream desktop, or docs site | — |
| A19 | Native plugin API | None; no native API exists | Required new surface ([D6](#decision-register)) | 12 | A versioned API with documented permissions and lifetimes; a named consumer exercises every lifetime transition the design defines, including failure and shutdown, through the real native runtime; any model-visible catalog is generated from the real interface | API design and named consumer are missing |

## Native consumer coverage gaps

These consumers or behaviors have no test that a native implementation could be checked against today:

- **Bake Desktop:** the external app, its Electron utility-process launch, and its pinned protocol copy. Access to that checkout is a dependency.
- **Excluded custom profiles:** native entry-path refusal fixtures are missing. They must prove that custom Cordis profiles, executable YAML, and npm package scripts never execute or change files; third-party migration is outside the target ([D5](#decision-register)).
- **Catalog providers:** no live smoke tests and no per-family wire fixtures for the ten non-generic API families. pi-ai's nine OAuth providers have no recorded flows. Bedrock and Vertex authentication goes through their SDKs and has not been inventoried per provider.
- **Windows terminal:** the PTY driver is POSIX-only, and no ConPTY scenario exists.
- **Old updaters:** no test runs a released 0.3 updater against a changed archive layout.
- **Retained `api`, `client`, `host`, and `typert` packages:** `bake-typert-registry`, `bake-typert-loader`, and `bake-api-gateway` are composed in the base bundle. Their consumers, including the `sessionFeedback` Host Remote, have not been traced.
- **Terminal setup:** edits to external terminal configuration files have no cross-runtime test.
- **Native plugin API:** no consumer is named and no API exists to test ([D6](#decision-register)).
- **`bake plugin`:** depends on pnpm being installed on the host and installs JavaScript plugins that 0.4 cannot run. Its migration or replacement is undecided ([D7](#decision-register)).
- **Platform minimums:** no recorded minimum OS, kernel, glibc, or macOS deployment target.
- **Performance:** the instrumentation is Node-specific, and several interactive workloads are excluded.

## Dev Note

Non-authoritative. The pi-ai counts come from grepping `node_modules/@earendil-works/pi-ai/dist/providers/data/*.json` and listing `dist/auth/oauth/` locally. No pi-ai code was executed, and no network call was made. A planned offline enumeration script could not run in this session. The scope-00 inventory slice should regenerate these counts from source and replace this table with a link. Tool rosters come from the committed model-surface snapshots, not from a run.
