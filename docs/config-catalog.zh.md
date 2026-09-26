<!-- 英文源文件由 scripts/gen-config-catalog.ts 生成；本中文文件是通过双语配对维护的经评审对侧。
     更新时先运行 `bun run gen-config-catalog` 更新英文，再更新本文件并运行 `bun run verify-translation-pairing --write docs/config-catalog.md` 重新记录配对。 -->

# 插件配置目录

[English](config-catalog.md) | 中文

每个 `config:` 块均可由 `cordis.yml` 条目设置：针对每个可加载的 harness 包，原样列出其 `apply` 函数或服务构造函数接收的配置声明（包括 JSDoc），并附上所有引用类型——包内类型直接粘贴，其他类型则提供链接。粘贴的内容是插件声明的完整配置类型——运行时 schema 有意排除的字段是仅供运行时使用的 seam（其自身的 JSDoc 会如此说明），不能通过 `cordis.yml` 设置。这是以**部署**为轴的参考文档——插件作者所依据的连接方式请参阅各[子系统页面](subsystems/core.zh.md)中的生成 `cordis-surface` 区域，面向模型的工具 schema 请参阅[工具目录](tool-catalog.zh.md)，而 [subsystems/](subsystems/core.zh.md) 则记录了这些声明所引用的类型。

英文源文件由源代码（`scripts/gen-config-catalog.ts`）生成，并通过 `bun scripts/gen-config-catalog.ts --check` 检查；本中文文件作为经评审对侧通过双语配对维护。声明块使用 `ts config-catalog` 围栏（doc-typecheck 会跳过它，因为单独引用导入项的声明无法独立编译）。英文生成器还会将运行时 schemastery schema 与粘贴的声明进行交叉核对——每个经 schema 验证的键（包括嵌套键）都必须能在声明的配置类型中找到——因此，粘贴内容无法隐藏加载器接受的字段。

`Requires:` 行列出插件通过 `inject` 注入的服务键：其 `cordis.yml` 树还必须加载这些服务的提供者。范围限定为 harness 层级（`packages/`）；配置树还可能加载的 vendored cordis 插件（控制台日志记录器等）固定为上游源代码（参见 [vendoring policy](../vendor/README.md)），未收录于此目录。

## `@deepseek-ai/dsh-agent-default-model`

```ts config-catalog
/** Composition entry for the default model selection. */
export interface Config {
  /** Registered provider route. */
  provider: string
  /** Provider-owned model id. */
  model: string
}
```

来源：[`packages/core/agent-default-model/src/index.ts:41`](../packages/core/agent-default-model/src/index.ts)

<a id="deepseek-aidsh-agent-instructions"></a>

## `@deepseek-ai/dsh-agent-instructions`

需要：`sessionProjections`

```ts config-catalog
/** User-facing workspace instruction loader configuration. */
export interface Config {
  /** Harness home containing the fixed user-global `AGENTS.md`; defaults to `$DSH_HOME` or `~/.dsh`. */
  dshHome?: string
  /** Directory entries that identify the project root while walking upward from the session cwd. */
  projectRootMarkers?: string[]
  /** UTF-8 byte cap for one rendered baseline or dynamic batch; non-positive or non-finite disables loading. */
  maxBytes: number
  /** Maximum UTF-8 bytes read from one instruction file; larger files are ignored. */
  maxSourceBytes?: number
  /**
   * Ordered same-directory project candidates; every existing file loads, with
   * per-directory trimmed-content duplicates collapsed to the earliest candidate.
   */
  instructionFileCandidates?: string[]
  /**
   * Ordered same-directory local-overlay candidates loaded after the base files
   * under the same per-directory trimmed-content dedup; empty disables the overlay.
   */
  localInstructionFileCandidates?: string[]
}
```

来源：[`packages/context/agent-instructions/src/config.ts:18`](../packages/context/agent-instructions/src/config.ts)

<a id="deepseek-aidsh-agent-loop"></a>

## `@deepseek-ai/dsh-agent-loop`

需要：`agents` · `sessions` · `llm` · `tools` · `systemPrompt` · `sessionProjections`

```ts config-catalog
/** Agent-loop plugin configuration. */
export interface Config {
  /**
   * Maximum parallel-safe calls in flight per agent step. `1` is serial;
   * omission defaults to {@link DEFAULT_MAX_PARALLEL_TOOL_CALLS}.
   */
  maxParallelToolCalls?: number
  /** Agents created or resumed at plugin startup. */
  agents: (AgentOptions & {
    /** Stable config label used in logs and as the fresh combined-id prefix. */
    id: string
    /** Optional stable identity; remounts resume its materialized history, while first use creates it fresh. */
    sessionId?: SessionId
    /** Optional workspace for a fresh session. */
    cwd?: string
    /** Persisted session to resume instead of creating a fresh session. */
    resumeSessionId?: SessionId
  })[]
}
```

Depends on: [`AgentOptions`](subsystems/core.zh.md) · [`SessionId`](subsystems/core.zh.md)

来源：[`packages/core/agent-loop/src/index.ts:318`](../packages/core/agent-loop/src/index.ts)

<a id="deepseek-aidsh-agent-presets"></a>

## `@deepseek-ai/dsh-agent-presets`

需要：`loader` · `sessionProjections`

```ts config-catalog
/** Plugin config: which preset is the default, and where presets live. */
export interface Config {
  /** Preset id mounted when a caller names none. Missing at mount time fails loud. */
  default: string
  /** Scanned roots in precedence order; an earlier root wins a duplicate id. */
  roots: PresetRoot[]
  /**
   * Prepend this package's bundled shipped presets as a `system` root, before
   * every configured root, so the shipped set always mounts and wins a
   * duplicate id. The default survives a whole-`config` patch replacement;
   * only an explicit `false` — a deployment supplying purely its own presets,
   * or an embedder using the roster as bare machinery — drops the set.
   */
  includeShippedRoot: boolean
  /**
   * Append the harness home's `USER_PRESET_DIR` as a `user` root, after every
   * configured root. False mounts a roster without the derived writable root.
   */
  includeUserRoot: boolean
}

/** One directory scanned for preset subdirectories. */
export interface PresetRoot {
  /** Directory holding one subdirectory per preset; a leading `~` expands. */
  path: string
  /** Trust recorded on every preset discovered under this root. */
  trust: PresetTrust
}

/**
 * Where a preset's composition came from. A `system` preset ships with the
 * deployment; a `user` preset was authored locally, by a person or by an
 * agent, and therefore carries the same trust as shell access.
 */
export type PresetTrust = 'system' | 'user'
```

来源：[`packages/preset/agent-presets/src/preset.ts:52`](../packages/preset/agent-presets/src/preset.ts)

<a id="deepseek-aidsh-agent-tool-presentation"></a>

## `@deepseek-ai/dsh-agent-tool-presentation`

需要：`tools`

```ts config-catalog
/** Plugin config. */
export interface Config {
  /**
   * The form this agent's model sees. `native` sends every visible schema,
   * `ptc` sends only `run_code` plus a generated SDK, `both` sends both.
   * Required rather than defaulted: the deployment default is what a preset
   * without this row already gets, so an omitted value would mean the row was
   * composed for nothing.
   */
  mode: ToolPresentationMode
}
```

Depends on: [`ToolPresentationMode`](subsystems/tools.zh.md)

来源：[`packages/core/agent-tool-presentation/src/index.ts:38`](../packages/core/agent-tool-presentation/src/index.ts)

<a id="deepseek-aidsh-api-gateway"></a>

## `@deepseek-ai/dsh-api-gateway`

需要：`typert`

```ts config-catalog
/** Gateway transport configuration. */
export interface Config {
  /** WebSocket Ping interval from 1 through 2,147,483,647 milliseconds. @default 2000 */
  readonly websocketHeartbeatIntervalMs?: number
}
```

来源：[`packages/api/gateway/src/index.ts:119`](../packages/api/gateway/src/index.ts)

## `@deepseek-ai/dsh-api-settings-controller`

```ts config-catalog
/** Native document-opening policy. */
export interface Config {
  /** Override platform desktop-opener detection. */
  readonly nativeOpen?: boolean
}
```

来源：[`packages/api/settings-controller/src/index.ts:36`](../packages/api/settings-controller/src/index.ts)

## `@deepseek-ai/dsh-attachment-local`

```ts config-catalog
/** Local attachment backend configuration. */
export interface Config {
  /** Explicit harness home; omitted follows `DSH_HOME`, then `~/.dsh`. */
  dshHome?: string
  /** Maximum encoded bytes accepted for one submitted image. Default: 20 MiB. */
  maxImageBytes?: number
  /** Maximum image count accepted in one submitted message. Default: 20. */
  maxImagesPerMessage?: number
  /** Maximum aggregate encoded image bytes accepted in one submitted message. Default: 200 MiB. */
  maxMessageImageBytes?: number
  /** Maximum intrinsic width multiplied by height accepted for one submitted image. Default: 64,000,000. */
  maxImagePixels?: number
  /** Maximum intrinsic width and maximum intrinsic height accepted for one submitted image. Default: 8192px. */
  maxImageDimension?: number
  /** Total-pixel budget of the stored provider-independent normalized image. */
  normalizedImageMaxPixels?: number
  /** Long-edge pixel cap of the stored provider-independent normalized image, applied after the total-pixel budget. */
  normalizedImageMaxDimension?: number
  /**
   * Encoded-byte target of the stored provider-independent normalized image;
   * the smallest quality-ladder output is kept when no quality fits.
   */
  normalizedImageMaxBytes?: number
  /** Maximum simultaneous normalization or request-image transformations in this service instance. */
  imageCompressionConcurrency?: number
}
```

来源：[`packages/attachment/attachment-local/src/index.ts:61`](../packages/attachment/attachment-local/src/index.ts)

<a id="deepseek-aidsh-bash-local"></a>

## `@deepseek-ai/dsh-bash-local`

需要：`subprocess`

```ts config-catalog
/** Plugin config (all optional — `static Config` supplies the defaults). */
export interface Config {
  /** Default working directory for commands (default: process.cwd()). */
  cwd?: string
  /** Default foreground timeout in milliseconds. */
  timeoutMs?: number
  /** Upper bound for per-call timeout overrides. */
  maxTimeoutMs?: number
  /** Per-stream in-memory output cap; overflow spills to a temp file. */
  maxOutputBytes?: number
  /** Per-stream spill-file cap; larger streams retain only their in-memory tail. */
  maxSpillBytes?: number
  /** Grace period for kill escalation and inherited pipes; at most `MAX_TIMER_DELAY_MS`. */
  graceMs?: number
}
```

来源：[`packages/shell/bash-local/src/index.ts:41`](../packages/shell/bash-local/src/index.ts)

<a id="deepseek-aidsh-bash-sandbox"></a>

## `@deepseek-ai/dsh-bash-sandbox`

需要：`subprocess` · `sandbox` · `sandboxPolicy`

```ts config-catalog
/**
 * Plugin config: the local executor's knobs, verbatim. The sandbox policy —
 * the default mode and fallback `workspace-write` root — is NOT here: it lives
 * on `ctx.sandboxPolicy` (`@deepseek-ai/dsh-sandbox-policy`), which resolves
 * each calling session's mode and cwd for every enforcing capability. The runner
 * choice is likewise the `ctx.sandbox` provider's config, not this executor's.
 */
export type Config = LocalConfig
```

Depends on: [`LocalConfig`](#deepseek-aidsh-bash-local)

来源：[`packages/shell/bash-sandbox/src/index.ts:36`](../packages/shell/bash-sandbox/src/index.ts)

<a id="deepseek-aidsh-client-connection"></a>

## `@deepseek-ai/dsh-client-connection`

需要：`credentials`

```ts config-catalog
/** Browser authentication, request limits, and connection recovery configuration. */
export interface ConnectionConfig {
  /** Browser recovery timing, injected into each served page. */
  recovery?: ConnectionRecoveryConfig
  /**
   * Authorities this deployment serves beyond loopback: exact `host:port`, or
   * port-less `host` matching any port. The /api trust fence refuses any
   * request whose Host is neither loopback nor listed here, so a
   * non-loopback (`0.0.0.0`) deployment must declare the names it is reached
   * by; the Web runtime derives LAN IP literals from an active all-interface
   * bind. An entry that is not a bare, canonical authority fails plugin load.
   */
  trustedHosts?: string[]
  /** Absolute browser-session lifetime in days. Default: 30. */
  cookieMaxAgeDays?: number
  /** Maximum buffered JSON body for every `/api` request. Default: 300 MiB. */
  maxRequestBodyBytes?: number
}

/** Timing for generation readiness and automatic reconnection. */
export interface ConnectionRecoveryConfig {
  /** First-retry delay cap in ms; actual delay is 50–100% of the cap. Default: 500. */
  backoffBaseMs?: number
  /** Finite growth factor of at least 1 per failed attempt; 1 keeps a fixed cap. Default: 2. */
  backoffFactor?: number
  /** Maximum retry delay cap in ms; retries continue at this cap. Default: 10000. */
  backoffMaxMs?: number
  /**
   * Delay before reporting a slow handshake, without cancelling it. Default: 3000.
   * Omitted when readiness, failure, cancellation, or the hard deadline occurs first.
   */
  generationReadyWarnMs?: number
  /** Deadline in ms for readiness, including physical connection setup. Default: 15000. */
  generationReadyTimeoutMs?: number
}
```

来源： [`packages/client/connection/src/index.ts:87`](../packages/client/connection/src/index.ts)

## `@deepseek-ai/dsh-compaction-basic`

需要：`llm` · `tokenMeter` · `sessions`

```ts config-catalog
/** Basic compaction configuration with an optional exact-target policy table. */
export interface BasicCompactionConfig extends CompactionPolicyConfig {
  /** Exact provider/model overrides; duplicate targets fail plugin load. */
  modelPolicies?: ModelCompactPolicyConfig[]
  /** Enable automatic step-boundary pressure and overflow-recovery listeners. Defaults to `true`. */
  auto?: boolean
}

/** Policy fields shared by the default policy and exact model overrides. */
export interface CompactionPolicyConfig {
  /** Compact at this fraction of the model's context window. Defaults to `0.8`. */
  thresholdRatio?: number
  /** Recent context retained as a fraction of the model's window. Defaults to `0.16`. */
  retainRatio?: number
  /** Absolute recent-context budget; mutually exclusive with `retainRatio`. */
  retainTokens?: number
  /** Summary provider; set together with `summarizationModel`, or inherit the conversation target. */
  summarizationProvider?: string
  /** Summary model; set together with `summarizationProvider`, or inherit the conversation target. */
  summarizationModel?: string
  /** Provider generation cap for summarization. Defaults to `8192`. */
  maxTokens?: number
  /** Extra attempts after the first compaction when pressure remains above threshold. Defaults to `1`. */
  compactionRetries?: number
  /** Maximum retries after canonical context overflow; `0` disables recovery. Defaults to `1`. */
  maxOverflowRetries?: number
}

/** Exact provider/model override merged over the default compaction policy. */
export interface ModelCompactPolicyConfig extends CompactionPolicyConfig {
  /** Registered provider route to match. */
  provider: string
  /** Exact routed model id to match within `provider`. */
  model: string
}
```

来源：[`packages/compaction/compaction-basic/src/types.ts:38`](../packages/compaction/compaction-basic/src/types.ts)

<a id="deepseek-aidsh-compaction-tool-result-pruner"></a>

## `@deepseek-ai/dsh-compaction-tool-result-pruner`

需要：`tokenMeter`

```ts config-catalog
/** Character-budget policy for deterministic tool-result pruning. */
export interface ToolResultPruneConfig {
  /** Prune when total text exceeds this many Unicode code points. Defaults to `8192`. */
  thresholdChars?: number
  /** Maximum leading Unicode code points retained. Defaults to `4096`. */
  headChars?: number
  /** Maximum trailing Unicode code points retained. Defaults to `1024`. */
  tailChars?: number
}
```

来源：[`packages/compaction/compaction-tool-result-pruner/src/types.ts:5`](../packages/compaction/compaction-tool-result-pruner/src/types.ts)

<a id="deepseek-aidsh-cordis-host-runner"></a>

## `@deepseek-ai/dsh-cordis-host-runner`

需要：`tools`

```ts config-catalog
/** Runner configuration. */
export interface Config {
  /** Maximum synchronous VM evaluation time in milliseconds. */
  vmTimeoutMs?: number
}
```

来源：[`packages/extensions/cordis-host-runner/src/index.ts:88`](../packages/extensions/cordis-host-runner/src/index.ts)

<a id="deepseek-aidsh-credentials-local"></a>

## `@deepseek-ai/dsh-credentials-local`

```ts config-catalog
/** Plugin config: file location and hot-reload behavior. */
export interface Config {
  /** Credentials document path; defaults to `.credentials.yaml` under the harness home. */
  path?: string
  /** Harness home used when `path` is omitted; defaults to `$DSH_HOME` or `~/.dsh`. */
  dshHome?: string
  /** Watch the document and hot-publish external edits; defaults to true. */
  watch?: boolean
  /** Watcher write-settle window in milliseconds; defaults to 100. */
  debounceMs?: number
}
```

来源：[`packages/credentials/credentials-local/src/index.ts:64`](../packages/credentials/credentials-local/src/index.ts)

## `@deepseek-ai/dsh-experimental-ptc-runtime-python`

```ts config-catalog
/** Plugin config: every cap, changeable from `cordis.yml` (no hardcoded tunables). */
export interface Config {
  /**
   * RLIMIT_CPU in whole seconds (a positive integer — `setrlimit` in the child
   * rejects a float). The child sets the soft limit to `cpuSeconds` and the
   * hard limit to `cpuSeconds + 1`: the kernel delivers SIGXCPU at the soft
   * limit, which the host classifies as a `timeout`; the +1s hard limit is a
   * SIGKILL backstop for a program that traps SIGXCPU. Granularity is whole seconds.
   */
  cpuSeconds?: number
  /** Wall-clock ceiling in milliseconds; backstops CPU time for programs awaiting a promise nobody resolves. */
  maxWallMs?: number
  /**
   * RLIMIT_AS in mebibytes; caps address space so a runaway allocation fails
   * cleanly. Not applied on Darwin, where the dyld shared cache mapped into
   * every process at exec exceeds any practical cap and the kernel rejects
   * the call; `cpuSeconds` and `maxWallMs` still bound the run there. Bounds
   * `maxLogBytes`/`maxValueBytes` at load on EVERY platform (this static check
   * runs on Darwin too, where only the runtime `setrlimit` is skipped): each
   * budget times a worst-case Unicode expansion must fit this byte count minus a
   * fixed interpreter baseline, so a near-budget output cannot breach the address
   * space during the child's build-and-encode.
   */
  addressSpaceMb?: number
  /**
   * Shared byte budget for captured log text (host-side ledger). Bounded at load
   * against `addressSpaceMb`: the child builds and encodes a near-budget entry
   * under RLIMIT_AS with several copies live at once, so this cap times the
   * worst-case Unicode expansion must fit the address space left after the
   * interpreter baseline (see `addressSpaceMb`) — a load-time rejection, not a
   * runtime clamp. Also bounded at load by the host's configured heap like
   * `maxValueBytes` (see its JSDoc): the effective frame cap minus the frame
   * envelope.
   */
  maxLogBytes?: number
  /**
   * Byte cap for the completion value. Bounded at load against `addressSpaceMb`
   * the same way `maxLogBytes` is: the child builds and encodes a near-budget
   * value under RLIMIT_AS with several copies live at once, so this cap times the
   * worst-case Unicode expansion must fit the address space left after the
   * interpreter baseline. Both budgets are ALSO bounded at load by the host's
   * configured heap: the effective frame cap (the protocol cap, or a lower
   * heap-derived ceiling when the host heap cannot safely parse a near-cap
   * frame — see `hostFrameParseCeiling`) minus the frame envelope, so a budget
   * whose honest frame could OOM the host's own JSON.parse is rejected up
   * front.
   */
  maxValueBytes?: number
  /** SIGTERM→SIGKILL grace period on kill, matching bash-local's default. */
  graceMs?: number
  /**
   * Absolute path, relative path, or basename of a CPython 3.10+ interpreter.
   * Resolved and validated once at plugin load under a five-second force-kill
   * deadline; a basename searches `PATH`.
   */
  pythonBin?: string
}
```

来源：[`packages/experimental/ptc-runtime-python/src/index.ts:42`](../packages/experimental/ptc-runtime-python/src/index.ts)

## `@deepseek-ai/dsh-file-reference-local`

需要：`agents` · `sessionProjections`

```ts config-catalog
/** Local file-reference discovery configuration. */
export interface Config {
  /** Maximum ranked candidates returned for one query. */
  maxResults?: number
  /** Maximum indexed files and directories per agent workspace. */
  maxEntries?: number
  /** Directory basenames never traversed or offered. */
  excludedDirectories?: string[]
}
```

来源：[`packages/context/file-reference-local/src/index.ts:34`](../packages/context/file-reference-local/src/index.ts)

<a id="deepseek-aidsh-fs-local"></a>

## `@deepseek-ai/dsh-fs-local`

```ts config-catalog
/** Configuration for the local filesystem backend. */
export interface Config {
  /** Base directory for relative paths. Defaults to `process.cwd()`. */
  cwd?: string
  /**
   * Exclusive UTF-8 byte limit on each overwrite-diff side, capped by the
   * runtime's safe allocation/decode maximum. Defaults to 10 MiB.
   */
  diffBasisMaxBytes?: number
}
```

来源：[`packages/fs/fs-local/src/index.ts:43`](../packages/fs/fs-local/src/index.ts)

<a id="deepseek-aidsh-fs-sandbox"></a>

## `@deepseek-ai/dsh-fs-sandbox`

需要：`sandboxPolicy`

```ts config-catalog
/**
 * Plugin config: the local backend's knobs verbatim (`cwd` resolution default
 * and `diffBasisMaxBytes` overwrite-presentation bound). The sandbox default
 * (mode + `workspace-write` fallback root) is NOT here — `ctx.sandboxPolicy`
 * resolves each calling session for every enforcing capability.
 */
export type Config = LocalConfig
```

Depends on: [`LocalConfig`](#deepseek-aidsh-fs-local)

来源：[`packages/fs/fs-sandbox/src/index.ts:45`](../packages/fs/fs-sandbox/src/index.ts)

<a id="deepseek-aidsh-goal"></a>

## `@deepseek-ai/dsh-goal`

需要：`agents` · `sessionProjections`

```ts config-catalog
/** Deployment defaults for goal creation. */
export interface Config {
  /** Total rounds used when a create request omits its own cap. */
  defaultMaxGoalRounds?: number
}
```

来源：[`packages/goal/goal/src/index.ts:172`](../packages/goal/goal/src/index.ts)

<a id="deepseek-aidsh-headless"></a>

## `@deepseek-ai/dsh-headless`

需要：`agentDefaultModel` · `agents` · `sessions`

```ts config-catalog
/** Plugin config: the task and run options resolved from this app's injected provider service. */
export interface Config {
  /** The prompt text for the single run; absent when the task arrives on stdin. */
  task?: string
  /** Exact Session identity to adopt; absent for a fresh random identity. An id with no stored Session fails. */
  sessionId?: string
  /** Whether stdout carries the machine-readable event stream instead of final text. */
  json?: boolean
}
```

来源：[`packages/bundle/headless/src/index.ts:42`](../packages/bundle/headless/src/index.ts)

<a id="deepseek-aidsh-hmr"></a>

## `@deepseek-ai/dsh-hmr`

```ts config-catalog
/** Module roots and watcher timing, with Chokidar deployment options. */
export interface HmrConfig extends ChokidarOptions {
  /** Directory resolved against the owning context's base URL. */
  base?: string
  /** Module watch roots; an empty list leaves only explicit configuration watches. */
  root: string[]
  /** Milliseconds for combining module changes. */
  debounce: number
  /** Glob patterns excluded from module watching. */
  ignored: string[]
}
```

Depends on: `ChokidarOptions` (`chokidar`)

来源： [`packages/boot/hmr/src/index.ts:51`](../packages/boot/hmr/src/index.ts)

<a id="deepseek-aidsh-hooks-claude-code"></a>

## `@deepseek-ai/dsh-hooks-claude-code`

需要：`shell` · `sessionProjections`

```ts config-catalog
/** Plugin config: where the CC hook config lives + substitution roots. */
export interface Config {
  /**
   * Path to a `hooks.json` or a settings file whose `hooks` key holds the config.
   * Process-level: read once at load, a relative path resolves against the process
   * launch cwd, so one config applies to the whole process.
   * TODO(per-session-hook-config): per-session discovery of a project-local
   * `hooks.json` from each `session/new.cwd`.
   */
  configPath: string
  /**
   * Replaces `${CLAUDE_PLUGIN_ROOT}` in command strings (the plugin's root dir).
   */
  pluginRoot?: string
  /**
   * Replaces `${CLAUDE_PROJECT_DIR}` in command strings AND is exported as the
   * `CLAUDE_PROJECT_DIR` env var for hook processes. When omitted, the env var
   * defaults per-run to the agent's session workspace (`session.header.cwd`, the
   * same dir the hook runs in) — Claude Code always exports this var, and common
   * unmodified hooks reference `$CLAUDE_PROJECT_DIR` for project-relative paths.
   */
  projectDir?: string
  /** Default per-hook timeout in ms when a hook sets none (CC default: 600000). */
  defaultTimeoutMs?: number
  /** Character cap for the `hook/result` event's persisted stderr summary. */
  stderrSummaryMaxChars?: number
}
```

来源：[`packages/hooks/hooks-claude-code/src/index.ts:44`](../packages/hooks/hooks-claude-code/src/index.ts)

<a id="deepseek-aidsh-hooks-codex"></a>

## `@deepseek-ai/dsh-hooks-codex`

需要：`shell` · `sessionProjections`

```ts config-catalog
/** Plugin config: where the Codex hooks.json lives + the model name for payloads. */
export interface Config {
  /**
   * Path to a Codex `hooks.json`. Process-level: read once at load, a relative
   * path resolves against the process launch cwd.
   * TODO(per-session-hook-config): per-session project-local discovery from each
   * `session/new.cwd`.
   */
  configPath: string
  /** The model name stamped on every payload (Codex includes `model` on each event). */
  model?: string
  /** Default per-hook timeout in ms when a hook sets none (Codex default: 600000). */
  defaultTimeoutMs?: number
  /** Character cap for the `hook/result` event's persisted stderr summary. */
  stderrSummaryMaxChars?: number
}
```

来源：[`packages/hooks/hooks-codex/src/index.ts:43`](../packages/hooks/hooks-codex/src/index.ts)

## `@deepseek-ai/dsh-host-webserver`

```ts config-catalog
/** Web server listen and response-compression config. */
export interface Config {
  /** Listen host; the two supported values are loopback and all-interfaces. */
  host: '127.0.0.1' | '0.0.0.0'
  /** Listen port; zero requests an OS-assigned port. */
  port: number
  /** Response compression for socket-backed HTTP requests. @default 'none' */
  compression?: 'none' | 'gzip'
  /** Gzip DEFLATE level from 0 through 9. @default 1 */
  compressionLevel?: number
  /** Minimum known response length eligible for gzip; unknown-length streams are eligible. @default 1024 */
  compressionThresholdBytes?: number
}
```

来源：[`packages/host/webserver/src/index.ts:59`](../packages/host/webserver/src/index.ts)

<a id="deepseek-aidsh-invariants"></a>

## `@deepseek-ai/dsh-invariants`

```ts config-catalog
/** Runtime invariant selection configured on the service plugin. */
export interface Config {
  /** Global switch; defaults to `true`. */
  readonly enabled?: boolean
  /** Case-sensitive JavaScript regex sources that admit package names; empty admits all. */
  readonly package_allowlist?: string[]
  /** Case-sensitive JavaScript regex sources that exclude package names after allowlist matching. */
  readonly package_blocklist?: string[]
}
```

来源：[`packages/runtime-diagnostics/invariants/src/index.ts:15`](../packages/runtime-diagnostics/invariants/src/index.ts)

<a id="deepseek-aidsh-jobs-local"></a>

## `@deepseek-ai/dsh-jobs-local`

```ts config-catalog
/** Configuration for the process-local job registry. */
export interface Config {
  /**
   * Maximum `running` plus `stopping` jobs per exact owner or in the shared unowned bucket;
   * omission defaults to 10.
   */
  maxConcurrentJobsPerOwner?: number
}
```

来源：[`packages/jobs/jobs-local/src/index.ts:31`](../packages/jobs/jobs-local/src/index.ts)

<a id="deepseek-aidsh-llm-deepseek"></a>

## `@deepseek-ai/dsh-llm-deepseek`

需要： `llm`

```ts config-catalog
/**
 * Plugin config, validated by the same-named schemastery schema and doubling
 * as the `llm-deepseek` settings-section shape. Every field is optional in
 * yml: a missing API key resolves through {@link Config.apiKeyEnv} at each
 * request (a request without any key fails with `MISSING_CREDENTIAL`, not at
 * plugin load), omitted thinking mode uses the provider default, and omitted
 * reasoning effort resolves to `high`.
 */
export interface Config {
  /** Wire protocol; defaults to messages. Configure through Cordis YAML. */
  protocol?: DeepSeekProtocol
  /** Credential reference (environment-variable name) resolved per request; defaults to `DEEPSEEK_API_KEY`. */
  apiKeyEnv?: string
  /** Endpoint base; falls back to $DEEPSEEK_BASE_URL from a trusted environment layer, then the public API. */
  baseURL?: string
  /** Deployment thinking policy; `disabled` limits every conversation request to `off`. */
  thinking?: 'enabled' | 'disabled'
  /** Default thinking effort (default `high`); `off` disables thinking per request. */
  reasoningEffort?: 'off' | 'low' | 'high' | 'max'
  /** Default per-request output cap (default 256,000); a model's own cap and explicit request values win. */
  maxTokens?: number
  /** Positive context capacity used when the selected model has no exact value (default 1,000,000). */
  defaultContextWindow?: number
  /** Advisory models shown by discovery consumers; defaults to V41 Flash and V4 Pro. */
  models?: DeepSeekCatalogModel[]
  /** Maximum provider idle time while one stream read is outstanding (default five minutes). */
  streamIdleTimeoutMs?: number
  /** Maximum accumulated file-referenced image bytes per chat request (default 128 MiB). */
  maxRequestFilesBytes?: number
  /** Maximum accumulated base64 image payload after Files API fallback (default 20 MiB). */
  maxInlineRequestImageBytes?: number
  /** Maximum number of represented images per chat request (default 600). */
  maxImagesPerRequest?: number
  /** Raw-byte removal step after the request exceeds its file bound (default 64 MiB). */
  imageOffloadByteQuantum?: number
  /** Base64-byte removal step after inline fallback exceeds its bound (default 10 MiB). */
  inlineImageOffloadByteQuantum?: number
  /** Image-count removal step after the request exceeds its count bound (default 20). */
  imageOffloadCountQuantum?: number
  /** Maximum duration of one request-image Files API resolution (default one minute). */
  filesApiTimeoutMs?: number
  /** Explicit lifetime assigned to each uploaded image (default seven days). */
  fileExpiresAfterSeconds?: number
  /** Remaining lifetime below which an indexed file is replaced (default one hour). */
  fileRefreshMarginSeconds?: number
  /** Oldest harness-owned files deleted before one quota-recovery upload retry (default 100). */
  fileQuotaCleanupBatch?: number
  /** Provider-owned model-request retry policy; omission uses normal mode with five retries. */
  retryPolicy?: RetryPolicyConfig
}

/** Supported wire implementations; Responses is not yet implemented. */
export type DeepSeekProtocol = 'chat-completions' | 'messages'

/** One optional model entry advertised by the direct-fetch adapter. */
export interface DeepSeekCatalogModel {
  /** Wire model id accepted by the configured endpoint. */
  id: string
  /** Selector label; defaults to {@link id}. */
  name?: string
  /** Optional selector detail for deployments with similar model variants. */
  description?: string
  /** Known combined request/response context capacity; omitted when deployment metadata is unavailable. */
  contextWindow?: number
  /** Per-request output cap for this model; omission falls back to the profile's {@link DeepSeekConnectionOptions.maxTokens}. */
  maxTokens?: number
  /** Accepted request modalities; omission is text-only. */
  inputModalities?: ModelModality[]
  /**
   * Total-pixel budget replacing the published token-grid projection for one
   * deterministic request preview, or the 512-by-512 `low` preset; omission
   * projects onto the token grid.
   */
  imagePixelBudget?: number | 'low'
  /** Encoded-byte target for one deterministic request preview; the smallest quality-ladder output is used when no quality fits. */
  imageMaxBytes?: number
  /**
   * `'in-history'` declares that the endpoint reads the latest `system`
   * message at any position of the conversation as the complete effective
   * system prompt; omission means only a leading system message is read.
   */
  systemPromptUpdate?: SystemPromptUpdate
}
```

Depends on: [`ModelModality`](../packages/llm/llm/src/index.ts) · [`RetryPolicyConfig`](../packages/llm/llm/src/index.ts) · [`SystemPromptUpdate`](../packages/llm/llm/src/index.ts)

来源：[`packages/llm/llm-deepseek/src/config.ts:25`](../packages/llm/llm-deepseek/src/config.ts)

<a id="deepseek-aidsh-llm-pi-ai"></a>

## `@deepseek-ai/dsh-llm-pi-ai`

需要：`llm`

```ts config-catalog
/** Plugin configuration: the provider routes this instance owns. */
export interface Config {
  /**
   * pi-ai provider routes, keyed by provider. An empty (or omitted) dict is
   * the dormant settings-driven posture: the adapter mounts with no routes
   * and registers them the moment a settings section supplies profiles.
   */
  providers?: Record<string, PiAiProviderProfile>
}

/** Configuration for one pi-ai provider route; the `providers` dict key IS the route. */
export interface PiAiProviderProfile {
  /** Credential reference (environment-variable name) resolved per request through `ctx.credentials`. */
  apiKeyEnv?: string
  /** Name shown by configuration surfaces; defaults to the route key. */
  displayName?: string
  /**
   * Wire protocol every model on this route speaks. Omission keeps each
   * installed catalog model's own protocol, which is why a catalog route needs
   * no protocol at all; a route the catalog does not ship must name one.
   */
  api?: string
  /** Endpoint for this route's models; defaults to the installed catalog's endpoint. */
  baseURL?: string
  /**
   * This route's model catalog. Omission serves the installed catalog for the
   * route unchanged; an explicit list replaces it, each entry defaulting its
   * unset fields from the installed model of the same id.
   */
  models?: PiAiModelProfile[]
  /**
   * Installed-catalog customizations by model id: each entry reshapes that
   * one model with the same fields a {@link models} entry takes, while the
   * rest of the catalog keeps serving untouched. Only meaningful on a catalog
   * route with no `models` list — `models` already replaces the catalog, so
   * an override beside it, on a route the catalog does not ship, or naming a
   * model the catalog does not describe is refused rather than skipped.
   */
  modelOverrides?: Record<string, PiAiModelOverride>
  /**
   * pi-ai wire-compatibility switches defaulting every model on this route
   * whose protocol declares them; each model's own `compat` overrides per
   * field. What neither sets keeps the installed catalog entry's value, then
   * pi-ai's own detection. A switch no model on the route could read is
   * refused rather than left looking applied.
   */
  compat?: PiAiCompatProfile
  /**
   * Context capacity for a model this route lists that neither the entry nor
   * the installed catalog sizes (default 262,144). A guess by construction, so
   * a deployment whose gateway serves smaller models corrects it here.
   */
  defaultContextWindow?: number
  /**
   * Output capability for a model this route lists that neither the entry nor
   * the installed catalog sizes (default 32,768). This sizes the model; it
   * never becomes a per-request cap on its own.
   */
  defaultMaxTokens?: number
  /**
   * Request modalities for a model this route lists that neither its entry's
   * {@link PiAiModelProfile.input} nor the installed catalog declares (default
   * `[text]`). A fallback like the capacities above, not an override: a
   * catalog model keeps the modalities the catalog records for it, and this
   * value never narrows one. A gateway serving vision models the catalog does
   * not describe declares `[text, image]` once here instead of on every entry.
   * Unlike an entry's list, this one may not be empty — nothing sits below it
   * to answer instead.
   */
  defaultInput?: PiAiModality[]
  /** Provider request headers, validated against Fetch when the profile resolves; Harness attribution wins reserved names. */
  headers?: Record<string, string>
  /** Provider-neutral pi-ai reasoning level. */
  reasoning?: ModelThinkingLevel
  /** Token budgets used by reasoning providers that support them. */
  thinkingBudgets?: ThinkingBudgets
  /** Prompt-cache retention preference. */
  cacheRetention?: CacheRetention
  /** Streaming transport preference. */
  transport?: Transport
  /** HTTP/provider SDK timeout in milliseconds. */
  timeoutMs?: number
  /** WebSocket connection timeout in milliseconds. */
  websocketConnectTimeoutMs?: number
  /** Maximum provider idle time while one stream read is outstanding. */
  streamIdleTimeoutMs?: number
  /**
   * Maximum base64-encoded image payload per request. When a request's
   * accumulated images exceed it, the oldest images are replaced by text
   * placeholders until the request fits, so a long session keeps completing
   * requests instead of being rejected by a request-size cap.
   */
  maxRequestImageBytes?: number
  /** Total-pixel budget for each deterministic inline request version. */
  requestImagePixelBudget?: number
  /**
   * Raw encoded-byte target for each deterministic inline request version;
   * the smallest quality-ladder output is used when no quality fits.
   */
  requestImageMaxBytes?: number
  /** Provider-owned model-request retry policy; omission uses normal mode with five retries. */
  retryPolicy?: RetryPolicyConfig
}

/** One configured model entry: an id plus the catalog fields it overrides. */
export interface PiAiModelProfile {
  /** Model id sent to the provider and accepted by {@link GenerateOptions.model}. */
  id: string
  /** Display name for selectors; defaults to the catalog name, then the id. */
  name?: string
  /** Maximum combined request and response context in tokens. */
  contextWindow?: number
  /**
   * Maximum output tokens. Configuring one also makes it this model's
   * per-request default; a value inherited from the installed catalog, or the
   * route's fallback, is the model's capability and never becomes a request
   * default on its own.
   */
  maxTokens?: number
  /**
   * Request modalities this model accepts. Absent — or empty, which describes
   * a model that accepts nothing and so states no answer either — keeps the
   * installed catalog entry's modalities, then the route's `defaultInput`.
   * Declaring images is what makes a hand-declared vision model usable, and
   * declaring text alone corrects a catalog model whose gateway does not serve
   * what the catalog records. This is a claim about the endpoint, not a check
   * of it: nothing interrogates a gateway for what it accepts, so a model
   * claiming images its endpoint refuses is refused by the provider instead,
   * mid-turn.
   */
  input?: PiAiModality[]
  /**
   * Selectable reasoning efforts. Absent inherits the installed catalog
   * entry's capability (a hand-declared model has none and does not reason);
   * `false` declares a non-reasoning model, which is how a profile strips
   * reasoning from a catalog model its gateway cannot serve; a non-empty dict
   * declares the offered levels and their wire spellings.
   */
  reasoningEfforts?: false | PiAiReasoningEfforts
  /** pi-ai wire-compatibility switches for this model, winning over the route's per field; one its protocol does not declare is refused. */
  compat?: PiAiCompatProfile
}

/**
 * Customization of one installed catalog model, keyed by its id in the
 * route's `modelOverrides` dict — the same fields a `models` entry may set,
 * with the id living in the key. Unlike a `models` list, overrides leave the
 * rest of the catalog serving untouched, which is what makes "correct one
 * model, keep the other thirty-seven" a three-line edit.
 */
export type PiAiModelOverride = Omit<PiAiModelProfile, 'id'>

/**
 * pi-ai wire-compatibility switches, set on the route (its models' default) or
 * per model (winning over the route, field by field).
 *
 * pi-ai decides each of these from the provider id and baseURL when no layer
 * sets it, and a private gateway's URL says nothing: for an endpoint it does
 * not recognize the detection answers as though it were OpenAI itself, which
 * is wrong for most OpenAI-compatible gateways. So every field here is one a
 * deployment must be able to state because nothing can infer it, while the
 * fields pi-ai's catalog sets for a named vendor stay withheld.
 *
 * A field belongs to the protocols whose upstream compat type declares it: a
 * model-level switch its protocol does not take fails resolution, and a
 * route-level one skips past models it cannot fit. "The three Responses
 * protocols" below means `openai-responses`, `azure-openai-responses`, and
 * `openai-codex-responses`, which pi-ai gives one shared compat type, so a
 * switch settable on one is settable on all three.
 */
export interface PiAiCompatProfile {
  /** Whether the endpoint accepts `store`; `openai-completions`. */
  supportsStore?: boolean
  /**
   * Whether the endpoint accepts the `developer` role for the system prompt,
   * which pi-ai sends only to a reasoning model; `false` keeps `system`.
   * `openai-completions` and the three Responses protocols.
   */
  supportsDeveloperRole?: boolean
  /** Whether the endpoint accepts `reasoning_effort`; `openai-completions`. */
  supportsReasoningEffort?: boolean
  /** Whether the endpoint accepts `stream_options: {include_usage: true}`; `openai-completions`. */
  supportsUsageInStreaming?: boolean
  /**
   * Whether streams include `finish_reason`; `false` lets pi-ai infer the
   * terminal reason when the stream ends; `openai-completions`.
   */
  supportsFinishReason?: boolean
  /** Which output-cap field the endpoint reads; `openai-completions`. */
  maxTokensField?: NonNullable<OpenAICompletionsCompat['maxTokensField']>
  /** Whether tool results must carry `name`; `openai-completions`. */
  requiresToolResultName?: boolean
  /** Whether a user message after tool results needs an assistant message between; `openai-completions`. */
  requiresAssistantAfterToolResult?: boolean
  /** Whether thinking blocks must travel as text in `<thinking>` delimiters; `openai-completions`. */
  requiresThinkingAsText?: boolean
  /** Whether replayed assistant messages need an empty `reasoning_content` while reasoning is on; `openai-completions`. */
  requiresReasoningContentOnAssistantMessages?: boolean
  /** Reasoning parameter format the endpoint expects; `openai-completions`. */
  thinkingFormat?: PiAiThinkingFormat
  /**
   * Kwargs sent as `chat_template_kwargs`, which pi-ai reads only under the
   * two `chat-template` thinking formats; `openai-completions`. Nothing checks
   * that pairing: the format in force may come from the installed catalog
   * entry or from pi-ai's own baseURL detection, neither of which resolution
   * can read, so kwargs set beside another format are sent nowhere.
   */
  chatTemplateKwargs?: NonNullable<OpenAICompletionsCompat['chatTemplateKwargs']>
  /** Arguments sent as `chat_template_args` under the `baseten` thinking format; `openai-completions`. */
  chatTemplateArgs?: NonNullable<OpenAICompletionsCompat['chatTemplateArgs']>
  /** Alias for `thinkingTokenBudgetField: "thinking_token_budget"`; an explicit field wins. `openai-completions`. */
  supportsThinkingTokenBudget?: boolean
  /** Request field carrying the reasoning budget from `thinkingBudgets`; omitted unless configured. `openai-completions`. */
  thinkingTokenBudgetField?: PiAiThinkingTokenBudgetField
  /** vLLM scheduler `priority`; lower runs earlier, and the server must enable priority scheduling. Omitted unless configured. */
  vllmPriority?: number
  /** Whether `openai-responses` accepts `max_output_tokens`; `false` omits it. Azure and Codex ignore this shared compat field. */
  supportsMaxOutputTokens?: boolean
  /**
   * Whether the endpoint accepts `strict` in tool definitions;
   * `openai-completions`, the three Responses protocols, `bedrock-converse-stream`.
   */
  supportsStrictMode?: boolean
  /** Prompt-cache marker convention; `openai-completions`. */
  cacheControlFormat?: NonNullable<OpenAICompletionsCompat['cacheControlFormat']>
  /**
   * Whether the endpoint accepts long prompt-cache retention;
   * `openai-completions`, the three Responses protocols, `anthropic-messages`.
   */
  supportsLongCacheRetention?: boolean
  /** Whether the endpoint accepts per-tool `eager_input_streaming`; `anthropic-messages`. */
  supportsEagerToolInputStreaming?: boolean
  /** Whether the endpoint accepts `cache_control` on tool definitions; `anthropic-messages`. */
  supportsCacheControlOnTools?: boolean
  /** Whether the endpoint accepts the `temperature` request field; `anthropic-messages`. */
  supportsTemperature?: boolean
  /** Whether to force adaptive thinking regardless of model id; `anthropic-messages`. */
  forceAdaptiveThinking?: boolean
  /** Whether to replay an empty thinking signature instead of converting thinking to text; `anthropic-messages`. */
  allowEmptySignature?: boolean
  /** Whether the endpoint accepts Anthropic strict tool schemas; `anthropic-messages`. */
  supportsStrictTools?: boolean
}

/** One request modality a pi-ai model may accept. */
export type PiAiModality = Model<Api>['input'][number]

/**
 * Selectable reasoning efforts for one model: each key is a level the model
 * offers (and selectors show), and its value is the wire spelling dispatch
 * sends for it. `off` alone may leave its value empty — "supported, send
 * nothing" — because for most providers not thinking is the parameter's
 * absence; every other declared level must name a wire value. A level absent
 * from the dict is not offered.
 */
export type PiAiReasoningEfforts = Partial<Record<ModelThinkingLevel, string | null>>

/** One reasoning-dispatch wire format a profile may name. */
export type PiAiThinkingFormat = NonNullable<OpenAICompletionsCompat['thinkingFormat']>

/** The reasoning-budget field spellings pi-ai accepts. */
export type PiAiThinkingTokenBudgetField = NonNullable<OpenAICompletionsCompat['thinkingTokenBudgetField']>
```

Depends on: `Api` (`@earendil-works/pi-ai`) · `CacheRetention` (`@earendil-works/pi-ai`) · `Model` (`@earendil-works/pi-ai`) · `ModelThinkingLevel` (`@earendil-works/pi-ai`) · `OpenAICompletionsCompat` (`@earendil-works/pi-ai`) · [`RetryPolicyConfig`](../packages/llm/llm/src/index.ts) · `ThinkingBudgets` (`@earendil-works/pi-ai`) · `Transport` (`@earendil-works/pi-ai`)

来源：[`packages/llm/llm-pi-ai/src/config.ts:221`](../packages/llm/llm-pi-ai/src/config.ts)

<a id="deepseek-aidsh-llm-replay"></a>

## `@deepseek-ai/dsh-llm-replay`

需要：`llm`

```ts config-catalog
/** Plugin config: the {@link ReplayConfig} inputs, each defaulting to its `DSH_SNAPSHOT_*` env var in `apply`. */
export interface Config {
  /** Override the fixture path; defaults to `$DSH_SNAPSHOT_FILE`. */
  file?: string
  /** Override the sidecar path; defaults to `$DSH_SNAPSHOT_OVERRIDE`. */
  overrideFile?: string
  /**
   * Override the child-log paths; defaults to `$DSH_SNAPSHOT_CHILD_FILES` (a
   * path-separator-delimited list). Each is a recorded subagent session log for
   * a nested-agent scenario; absent/empty for a single-session scenario.
   */
  childFiles?: string[]
  /** Optional replay-only provider catalog; absent or empty selects catch-all waterfall replay. */
  providers?: ReplayProviderConfig[]
  /** Optional per-chunk pacing delay in ms (see {@link ReplayConfig.paceMs}); absent keeps burst yield. */
  paceMs?: number
}

/** One provider route exposed by the replay adapter. */
export interface ReplayProviderConfig {
  /** Provider route used for replay requests. */
  id: string
  /** Selector label; defaults to {@link id}. */
  name?: string
  /** Advisory models exposed to replay scenarios that exercise discovery. */
  models?: ReplayModelConfig[]
  /** Optional provider-owned retry policy used by assembled recovery snapshots. */
  retryPolicy?: RetryPolicyConfig
}

/** One model exposed by a replay-only provider catalog. */
export interface ReplayModelConfig {
  /** Model id used for replay requests. */
  id: string
  /** Selector label; defaults to {@link id}. */
  name?: string
  /** Optional selector description. */
  description?: string
  /** Optional positive integer context capacity published by the replay adapter. */
  contextWindow?: number
  /** Optional declared input modalities, so a scenario can exercise capability gates (e.g. image-capable `read_image`). */
  inputModalities?: readonly ModelModality[]
  /**
   * Optional per-request output cap the replay route materializes when callers
   * omit one, so replay reconstructs the request header a live catalog produced.
   */
  defaultMaxTokens?: number
  /**
   * Optional flat visual-token price the replay route declares for every
   * retained request image, so keyless scenarios exercise route-priced
   * request pressure; each occurrence is priced at this value plus its
   * request-preview handle text. Requires {@link inputModalities} to include
   * `image` — a text-only route never sends visual tokens. Absent declares
   * no image pricing.
   */
  imageRequestTokens?: number
  /** Optional reasoning-effort ids the replay route accepts, in display order. */
  reasoningEfforts?: string[]
  /**
   * Optional effort materialized when callers omit one; must appear in
   * {@link reasoningEfforts} or call resolution rejects the route.
   */
  defaultReasoningEffort?: string
  /** Optional in-history system prompt replacement for a keyless replay route. */
  systemPromptUpdate?: SystemPromptUpdate
}
```

Depends on: [`ModelModality`](../packages/llm/llm/src/index.ts) · [`RetryPolicyConfig`](../packages/llm/llm/src/index.ts) · [`SystemPromptUpdate`](../packages/llm/llm/src/index.ts)

来源：[`packages/test-support/llm-replay/src/index.ts:1122`](../packages/test-support/llm-replay/src/index.ts)

<a id="deepseek-aidsh-llm-retry"></a>

## `@deepseek-ai/dsh-llm-retry`

需要：`agents` · `sessionProjections`

```ts config-catalog
/** This policy executor has no config; providers own `retryPolicy`. */
export type Config = Readonly<Record<string, never>>
```

来源：[`packages/llm/llm-retry/src/index.ts:25`](../packages/llm/llm-retry/src/index.ts)

## `@deepseek-ai/dsh-mcp-client`

需要：`tools`

```ts config-catalog
/** Configuration for one stdio or Streamable HTTP MCP server. */
export type Config = StdioConfig | StreamableHttpConfig

/** Config for connecting to an MCP server via a spawned child process over stdio. */
export interface StdioConfig {
  /** Selects child-process stdio transport. */
  transport: 'stdio'
  /**
   * Stable local namespace for this server's model-facing tool names
   * (`mcp__<serverName>__<rawName>`). Must match `[A-Za-z0-9_-]{1,32}` and be
   * unique across live mcp-client instances.
   */
  serverName: string
  /** Executable used to start the server. */
  command: string
  /** Arguments passed directly, without shell interpolation. */
  args: string[]
  /** Extra env vars merged on top of scrubbed ambient env. */
  env: Record<string, string>
  /** Working directory for the child process. */
  cwd: string
  /** Timeout per tool call or resource request in milliseconds. */
  toolCallTimeoutMs: number
  /** Fail plugin activation when the initial connection or tool synchronization fails. */
  failOnStartupError: boolean
  /** Maximum UTF-8 bytes of attributed server instructions (default 32768). */
  maxInstructionBytes?: number
  /** Automatic reconnect policy after a lost connection; omission uses the defaults. */
  reconnect?: ReconnectConfig
}

/** Config for connecting to an MCP server over Streamable HTTP (SSE). */
export interface StreamableHttpConfig {
  /** Selects Streamable HTTP transport. */
  transport: 'streamable-http'
  /**
   * Stable local namespace for this server's model-facing tool names
   * (`mcp__<serverName>__<rawName>`). Must match `[A-Za-z0-9_-]{1,32}` and be
   * unique across live mcp-client instances.
   */
  serverName: string
  /** MCP endpoint URL. */
  url: string
  /** Additional headers attached to MCP requests. */
  headers: Record<string, string>
  /** Timeout per tool call or resource request in milliseconds. */
  toolCallTimeoutMs: number
  /** Fail plugin activation when the initial connection or tool synchronization fails. */
  failOnStartupError: boolean
  /** Maximum UTF-8 bytes of attributed server instructions (default 32768). */
  maxInstructionBytes?: number
  /** Automatic reconnect policy after a lost connection; omission uses the defaults. */
  reconnect?: ReconnectConfig
}

/** Automatic reconnect policy for one MCP server connection. */
export interface ReconnectConfig {
  /** Reconnect automatically after a lost connection (default true). */
  enabled?: boolean
  /** First reconnect delay in milliseconds; doubles per consecutive failed attempt (default 500). */
  initialDelayMs?: number
  /** Backoff ceiling in milliseconds; also the uptime after which the attempt budget resets (default 30000). */
  maxDelayMs?: number
  /** Consecutive failed attempts per outage before giving up for good (default 10). */
  maxAttempts?: number
}
```

来源：[`packages/mcp/mcp-client/src/index.ts:104`](../packages/mcp/mcp-client/src/index.ts)

<a id="deepseek-aidsh-message-feedback"></a>

## `@deepseek-ai/dsh-message-feedback`

需要：`sessionPersistence` · `sessions`

```ts config-catalog
/** Required deployment policy for optional notes. */
export interface Config {
  /** Maximum UTF-8 byte length accepted for one note. */
  readonly maxNoteBytes: number
}
```

来源：[`packages/feedback/message-feedback/src/index.ts:40`](../packages/feedback/message-feedback/src/index.ts)

## `@deepseek-ai/dsh-permission-presets`

需要：`shell` · `approval` · `sessions` · `sessionProjections`

```ts config-catalog
/** The {@link PermissionPresetService} config: preset table and composition default. */
export interface Config {
  /**
   * The preset table: name → knob bundle. Defaults to `workspace-write`
   * (workspace-write + ask) and `danger-full-access` (danger-full-access +
   * never). The names `custom` and `auto` are reserved for derived state and
   * the Auto review integration respectively.
   */
  presets?: Record<string, PresetSpec>
  /**
   * Default for new sessions. When omitted, the preset matching the composed
   * sandbox and approval defaults is used.
   */
  defaultPreset?: string
}

/** One preset's sandbox/approval bundle and optional client presentation. */
export interface PresetSpec {
  /** The `sandbox/mode` value the preset writes through. */
  sandbox: SandboxMode
  /** The `approval/policy` value the preset writes through. */
  approval: ApprovalPolicy
  /** The display label a client shows for this preset; the raw table key when omitted. */
  name?: string
  /** One user-facing sentence on what the preset means; omitted when not configured. */
  description?: string
}
```

Depends on: [`ApprovalPolicy`](subsystems/approval.zh.md) · [`SandboxMode`](subsystems/sandbox.zh.md)

来源：[`packages/interaction/permission-presets/src/index.ts:156`](../packages/interaction/permission-presets/src/index.ts)

<a id="deepseek-aidsh-persona"></a>

## `@deepseek-ai/dsh-persona`

需要：`systemPrompt`

```ts config-catalog
/** Plugin config: the persona text this composition contributes. */
export interface Config {
  /**
   * Persona prose rendered as the `deployment:persona-prefix` section. A template:
   * complete `{{…}}` groups interpolate strictly against registered prompt
   * variables. Empty text drops the section at render, matching the registry.
   */
  prefix: string
  /**
   * Persona suffix template rendered after first-party guidance. Omitted or empty
   * text shadows the deployment suffix away; interpolation is strict.
   */
  suffix?: string
  /** Make the prefix the complete system prompt, suppressing the suffix and every other section. */
  complete?: boolean
  /** Suppress dynamic runtime-context snapshots for this persona's agent scope. */
  includeRuntimeContext?: boolean
}
```

来源：[`packages/preset/persona/src/index.ts:30`](../packages/preset/persona/src/index.ts)

<a id="deepseek-aidsh-plan-mode"></a>

## `@deepseek-ai/dsh-plan-mode`

需要：`tools` · `systemPrompt` · `sessionProjections`

```ts config-catalog
/** Deployment-owned plan guidance. */
export interface PlanModeConfig {
  /** Guidance rendered as the `plan:policy` prompt section while plan mode is active. */
  section: string
}
```

来源：[`packages/plan/plan-mode/src/index.ts:64`](../packages/plan/plan-mode/src/index.ts)

<a id="deepseek-aidsh-plugin-manager"></a>

## `@deepseek-ai/dsh-plugin-manager`

依赖： `loader` · `profileContext`

```ts config-catalog
/** The pnpm executable and the limits for package diagnostics and registry lookups. */
export interface Config {
  /** The pnpm executable name or path; resolved through `PATH` like the `dsh plugin` command. */
  pnpmCommand?: string
  /** Maximum retained pnpm diagnostic bytes per operation. */
  outputBytes?: number
  /** Maximum time to wait for another process's profile package operation. */
  lockWaitMs?: number
  /** Bound on one registry lookup an inspection runs, in milliseconds. */
  inspectTimeoutMs?: number
}
```

来源： [`packages/boot/plugin-manager/src/index.ts:33`](../packages/boot/plugin-manager/src/index.ts)

<a id="deepseek-aidsh-plugin-package-inventory-deepseek"></a>

## `@deepseek-ai/dsh-plugin-package-inventory-deepseek`

需要：`agents` · `deepseekLlmApiExtensions` · `loader`

```ts config-catalog
/** Plugin-package request contribution configuration. */
export interface Config {
  /** Contribute `dsh_plugin_packages` to official DeepSeek requests. Defaults to `true`. */
  enabled?: boolean
}
```

来源：[`packages/llm/plugin-package-inventory-deepseek/src/index.ts:32`](../packages/llm/plugin-package-inventory-deepseek/src/index.ts)

<a id="deepseek-aidsh-ptc-runtime-node"></a>

## `@deepseek-ai/dsh-ptc-runtime-node`

需要： `fs` · `subprocess` · `sandbox` · `sandboxPolicy`

```ts config-catalog
/** Deployment-varying runtime bounds and launch choices. */
export interface Config extends LaunchConfig {
  /** Default elapsed deadline, including nested tool and approval waits. */
  timeoutMs?: number
  /** Maximum numeric elapsed budget accepted by resolve. */
  maxTimeoutMs?: number
  /** Combined serialized logs, completion and diagnostic byte cap. */
  maxOutputBytes?: number
  /** V8 old-generation heap limit in MiB; native allocations are excluded. */
  maxOldGenerationSizeMb?: number
  /** Maximum control frame, outstanding argument and queued control-output bytes. */
  maxMessageBytes?: number
  /** Maximum simultaneous host binding calls accepted from a program. */
  maxPendingCalls?: number
  /** Managed process termination and output-drain grace in milliseconds. */
  graceMs?: number
}

/** Deployment-owned Node executable and optional preinstalled built bootstrap. */
export interface LaunchConfig {
  /** Executable in the subprocess world; defaults to the current Node executable. */
  nodeExecutable?: string
  /** Absolute preinstalled built bootstrap in the execution world. */
  bootstrapPath?: string
}
```

来源： [`packages/ptc-runtime/ptc-runtime-node/src/index.ts:26`](../packages/ptc-runtime/ptc-runtime-node/src/index.ts)

<a id="deepseek-aidsh-pwsh-local"></a>

## `@deepseek-ai/dsh-pwsh-local`

需要：`subprocess`

```ts config-catalog
/** Plugin config (all optional — `static Config` supplies the defaults). */
export interface Config {
  /** Default working directory for commands (default: process.cwd()). */
  cwd?: string
  /** Default foreground timeout in milliseconds. */
  timeoutMs?: number
  /** Upper bound for per-call timeout overrides. */
  maxTimeoutMs?: number
  /** Per-stream in-memory output cap; overflow spills to a temp file. */
  maxOutputBytes?: number
  /** Per-stream spill-file cap; larger streams retain only their in-memory tail. */
  maxSpillBytes?: number
  /** Grace period for kill escalation and inherited pipes; at most `MAX_TIMER_DELAY_MS`. */
  graceMs?: number
  /**
   * Explicit pwsh executable. When omitted, well-known Windows install
   * locations and PATH entries are probed in order (PowerShell 7 install,
   * PATH entries such as the Microsoft Store install, then Windows
   * PowerShell 5.1), falling back to a bare `pwsh` resolved through PATH.
   */
  pwshPath?: string
}
```

来源：[`packages/shell/pwsh-local/src/index.ts:58`](../packages/shell/pwsh-local/src/index.ts)

<a id="deepseek-aidsh-pwsh-sandbox"></a>

## `@deepseek-ai/dsh-pwsh-sandbox`

需要：`subprocess` · `sandbox` · `sandboxPolicy`

```ts config-catalog
/**
 * Plugin config: the local executor's knobs, verbatim. The sandbox policy —
 * the default mode and fallback `workspace-write` root — is NOT here: it lives
 * on `ctx.sandboxPolicy` (`@deepseek-ai/dsh-sandbox-policy`), which resolves
 * each calling session's mode and cwd for every enforcing capability. The
 * runner choice is likewise the `ctx.sandbox` provider's config, not this
 * executor's.
 */
export type Config = LocalConfig
```

Depends on: [`LocalConfig`](#deepseek-aidsh-pwsh-local)

来源：[`packages/shell/pwsh-sandbox/src/index.ts:40`](../packages/shell/pwsh-sandbox/src/index.ts)

<a id="deepseek-aidsh-repeat-tool-reminder"></a>

## `@deepseek-ai/dsh-repeat-tool-reminder`

```ts config-catalog
/**
 * Plugin config, validated by the same-named schemastery schema plus the
 * load-time checks in `apply` (misconfiguration fails loud: an empty
 * `thresholds` list, a non-integer, a value below 2, or a duplicate throws at
 * plugin load, never a silent fall-back). `include`/`exclude` entries are
 * `*`-wildcard predicates over tool names at call time, not references to
 * registry entries — a pattern matching no currently registered tool is valid
 * (`exclude: [mcp_*]` must stay legal in a deployment that loads no MCP tools).
 */
export interface Config {
  /** Consecutive-repeat counts that trigger a reminder (default `[3, 5, 8]`). */
  thresholds?: number[]
  /** Tool-name patterns to track; empty means every tool is tracked. */
  include?: string[]
  /** Tool-name patterns transparent to the chain (neither count nor reset). */
  exclude?: string[]
  /**
   * Maximum characters of canonical arguments quoted in the DETAILED reminder
   * (default 500). Large payloads (a `write` body, a long command) would
   * otherwise ride into the next request unbounded — precisely in a loop
   * scenario; the cap bounds the reminder, never the detection (the chain key
   * always compares the FULL canonical string).
   */
  argumentsPreviewChars?: number
}
```

来源：[`packages/guard/repeat-tool-reminder/src/index.ts:28`](../packages/guard/repeat-tool-reminder/src/index.ts)

<a id="deepseek-aidsh-sandbox-local"></a>

## `@deepseek-ai/dsh-sandbox-local`

```ts config-catalog
/** Plugin config. All optional — `static Config` supplies the defaults. */
export interface Config {
  /**
   * Override the runner argv; bwrap-compatible profile arguments are appended. A
   * non-empty override asserts full enforcement and skips built-in selection and
   * probing. A runner that starts but refuses its profile must be identifiable by
   * {@link runnerFailureSignatures}. Consumers classify a spawn rejection only after
   * confirming the workdir is usable. `ENOENT` or `EACCES` identifies the runner when
   * `error.path` equals argv[0] and `error.syscall` is `spawn` or `spawn <runner>`, or
   * when `error.path` is absent and `error.syscall` is exactly `spawn <runner>`.
   */
  runnerCommand?: string[]
  /**
   * Case-insensitive stderr substrings emitted when a configured
   * {@link runnerCommand} refuses its profile before executing the wrapped
   * command. Required and non-empty with `runnerCommand`; rejected without
   * it. Each entry is a non-empty, single-line, case-insensitive substring
   * covering the executable runner's own failure dialect.
   */
  runnerFailureSignatures?: string[]
  /** Positive timeout for each functional probe; zero would mean unbounded to Node. */
  probeTimeoutMs?: number
}
```

来源：[`packages/sandbox/sandbox-local/src/index.ts:44`](../packages/sandbox/sandbox-local/src/index.ts)

<a id="deepseek-aidsh-sandbox-policy"></a>

## `@deepseek-ai/dsh-sandbox-policy`

需要：`sessionProjections`

```ts config-catalog
/**
 * Plugin config: the deployment's sandbox default. All optional — `Config`
 * supplies the defaults (`mode: 'read-only'` is the fail-safe default; a
 * deployment that wants a workspace-writable agent opts in explicitly). The
 * runner choice is NOT here (it is the `ctx.sandbox` provider's config), nor
 * is any per-family knob: this is the one shared policy home.
 */
export interface Config {
  /** File-sandbox mode a session starts from (default: `read-only`). */
  mode?: SandboxMode
  /**
   * Absolute fallback root for agentless calls and sessions without a cwd (default:
   * `process.cwd()`). Normal agent calls use their session cwd instead.
   */
  workspaceRoot?: string
}
```

Depends on: [`SandboxMode`](subsystems/sandbox.zh.md)

来源：[`packages/sandbox/sandbox-policy/src/index.ts:71`](../packages/sandbox/sandbox-policy/src/index.ts)

## `@deepseek-ai/dsh-session-log-deepseek`

需要：`deepseekLlmApiExtensions` · `sessions`

```ts config-catalog
/** Session-log request contribution configuration. */
export interface Config {
  /** Contribute `dsh_session_log` to official DeepSeek requests. Defaults to `true`. */
  enabled?: boolean
}
```

来源：[`packages/session/session-log-deepseek/src/index.ts:38`](../packages/session/session-log-deepseek/src/index.ts)

## `@deepseek-ai/dsh-session-persistence-jsonl`

```ts config-catalog
/** Plugin config for the JSONL backend's root and physical encoding. */
export interface Config {
  /**
   * Root directory for all session files. Required (no default): a default of
   * `process.cwd()` would scatter session files as the process's cwd changes
   * (bash calls, subprocesses). Sessions group under human-readable project
   * directories, then per-session directories. An existing root must be a
   * readable directory; an absent root is created on first materialization.
   */
  root: string
  /** Physical encoding; defaults to checksummed Zstandard frames. */
  compression?: JsonlCompression
}

/** Physical encoding selected for JSONL session artifacts. */
export type JsonlCompression = 'zstd' | 'none'
```

来源：[`packages/session/session-persistence-jsonl/src/index.ts:88`](../packages/session/session-persistence-jsonl/src/index.ts)

<a id="deepseek-aidsh-session-projection-cache"></a>

## `@deepseek-ai/dsh-session-projection-cache`

需要：`storageDomain` · `sessionProjections` · `sessions`

```ts config-catalog
/**
 * Plugin config. Both throttle triggers are deployment choices with no
 * universally correct value, so the composition states them explicitly
 * (cordis.yml); the three mandatory write points (session creation,
 * `turn/end`, and session disposal) are policy, not tunables, and always
 * fire.
 */
export interface Config {
  /** Committed events per session that force a durable checkpoint write between mandatory points. */
  writeEveryEvents: number
  /** Longest time (milliseconds) a dirty checkpoint may stay unwritten between mandatory points. */
  writeIntervalMs: number
}
```

来源：[`packages/session/session-projection-cache/src/index.ts:63`](../packages/session/session-projection-cache/src/index.ts)

<a id="deepseek-aidsh-session-query-sqlite"></a>

## `@deepseek-ai/dsh-session-query-sqlite`

需要：`sessions`

```ts config-catalog
/** Combined session-query configuration backed by SQLite full-text search. */
export interface Config extends SessionQueryConfig {
  /**
   * Dedicated derived-index path; `:memory:` is supported for ephemeral
   * indexes. Missing directories and database files are created owner-only on
   * POSIX filesystems; existing modes are preserved.
   */
  path: string
  /**
   * Open the SQLite module and handle at service activation or the first
   * search, or `never` to disable full-text search: the inherited exact
   * reads, filters, and traces stay available, while `searchSessions` and
   * `searchEvents` fail with `SESSION_QUERY_SEARCH_DISABLED` and SQLite is
   * never imported or opened. Defaults to `startup`.
   */
  openAt?: OpenAt
  /** SQLite journal mode. Defaults to `wal`. */
  journalMode?: JournalMode
  /** Page size when a request omits `limit`. At most `Number.MAX_SAFE_INTEGER - 1`; defaults to 20. */
  defaultLimit?: number
  /** Largest accepted page size. At most `Number.MAX_SAFE_INTEGER - 1`; defaults to 100. */
  maxLimit?: number
  /** Maximum snippet length in Unicode code points. Defaults to 240. */
  snippetChars?: number
  /** Maximum concurrent persisted-log reads in one inherited batch read. Defaults to 4. */
  persistedReadConcurrency?: number
  /** Maximum cold prepared-Session observations the inherited reader retains for reuse. Defaults to 5. */
  preparedSessionCacheSize?: number
}

/** SQLite module/handle opening phase; `never` disables full-text search entirely. */
export type OpenAt = 'startup' | 'first-search' | 'never'

/** Supported SQLite journal modes. */
export type JournalMode = 'wal' | 'delete' | 'truncate' | 'persist'
```

Depends on: [`SessionQueryConfig`](../packages/session-query/session-query/src/index.ts)

来源：[`packages/session-query/session-query-sqlite/src/index.ts:92`](../packages/session-query/session-query-sqlite/src/index.ts)

<a id="deepseek-aidsh-session-reference"></a>

## `@deepseek-ai/dsh-session-reference`

需要：`sessionQuery`

```ts config-catalog
/** Session-reference service configuration. */
export interface Config {
  /** Maximum distinct source sessions referenced by one message, from one to three. */
  maxReferences?: number
  /** Default host candidate-list limit. */
  candidateLimit?: number
  /** Explicit maximum rendered UTF-8 bytes per source; absent uses the model-relative budget with a 64 KiB floor. */
  maxReferenceBytes?: number
  /** Fraction of the model context window per source, estimated at four bytes per token; between zero and one. */
  referenceContextFraction?: number
}
```

来源：[`packages/context/session-reference/src/config.ts:11`](../packages/context/session-reference/src/config.ts)

<a id="deepseek-aidsh-session-telemetry-otel"></a>

## `@deepseek-ai/dsh-session-telemetry-otel`

需要：`sessions`

```ts config-catalog
/**
 * Plugin configuration: one sharing policy, two verbatim SDK option objects,
 * and one DSH-owned shutdown bound. Uploading modes validate their endpoint
 * and shutdown deadline at plugin load; `DISABLED` reads neither.
 */
export interface Config {
  /** Defaults to `FEEDBACK_ONLY`: capture session history only when feedback is explicitly submitted. */
  mode?: SessionTelemetryMode
  /**
   * Passed verbatim to the SDK's OTLP/HTTP log exporter — the complete
   * `OTLPExporterNodeConfigBase` shape (`headers`, `timeoutMillis`,
   * `compression`, `keepAlive`, …), owned and documented by the SDK. `url`
   * is the one field this package requires and validates itself.
   */
  exporter?: OTLPExporterNodeConfigBase & {
    /** Full logs endpoint (e.g. `https://collector.example.com/v1/logs`). Required outside `DISABLED`; validated at load. */
    url?: string
  }
  /**
   * Passed verbatim to `BatchLogRecordProcessor` (minus the exporter slot,
   * which this plugin fills); the SDK owns and documents these knobs.
   */
  processor?: Omit<BatchLogRecordProcessorOptions, 'exporter'>
  /** Maximum time spent awaiting the SDK provider's complete shutdown path. */
  shutdownTimeoutMillis?: number
}

/** Session-sharing policy selected by {@link Config.mode}. */
export enum SessionTelemetryMode {
  FEEDBACK_ONLY = 'FEEDBACK_ONLY',
  DISABLED = 'DISABLED',
}
```

Depends on: `BatchLogRecordProcessorOptions` (`@opentelemetry/sdk-logs`) · `OTLPExporterNodeConfigBase` (`@opentelemetry/otlp-exporter-base`)

来源：[`packages/session/session-telemetry-otel/src/index.ts:100`](../packages/session/session-telemetry-otel/src/index.ts)

<a id="deepseek-aidsh-session-title"></a>

## `@deepseek-ai/dsh-session-title`

需要：`sessions`

```ts config-catalog
/** Required deterministic fallback and accepted-title limits. */
export interface Config {
  /** Maximum whitespace-delimited words in the built-in fallback. */
  readonly fallbackMaxWords: number
  /** Maximum UTF-8 bytes in the built-in fallback. */
  readonly fallbackMaxBytes: number
  /** Maximum UTF-8 bytes in any accepted title. */
  readonly maxTitleBytes: number
}
```

来源：[`packages/session/session-title/src/index.ts:56`](../packages/session/session-title/src/index.ts)

## `@deepseek-ai/dsh-session-title-first-prompt-llm`

需要：`sessionTitle` · `llm` · `sessions`

```ts config-catalog
/** Required LLM policy; this plugin adds no defaults. */
export type Config = SessionTitleLlmConfig
```

Depends on: [`SessionTitleLlmConfig`](../packages/session/session-title-llm/src/index.ts)

来源：[`packages/session/session-title-first-prompt-llm/src/index.ts:15`](../packages/session/session-title-first-prompt-llm/src/index.ts)

<a id="deepseek-aidsh-settings-file"></a>

## `@deepseek-ai/dsh-settings-file`

```ts config-catalog
/** Plugin config: file location and hot-reload behavior. */
export interface Config {
  /** Settings document path; defaults to `settings.yaml` under the harness home. */
  path?: string
  /** Harness home used when `path` is omitted; defaults to `$DSH_HOME` or `~/.dsh`. */
  dshHome?: string
  /** Watch the document and hot-publish external edits; defaults to true. */
  watch?: boolean
  /** Watcher write-settle window in milliseconds; defaults to 100. */
  debounceMs?: number
}
```

来源：[`packages/settings/settings-file/src/index.ts:22`](../packages/settings/settings-file/src/index.ts)

<a id="deepseek-aidsh-shell-env"></a>

## `@deepseek-ai/dsh-shell-env`

```ts config-catalog
/** Plugin config (all optional — the built-in facts resolve without defaults). */
export interface Config {
  /** DeepSeek Harness home directory exposed as `DSH_HOME`; defaults to `$DSH_HOME` or `~/.dsh`. */
  dshHome?: string
}
```

来源：[`packages/shell/shell-env/src/index.ts:28`](../packages/shell/shell-env/src/index.ts)

<a id="deepseek-aidsh-skill"></a>

## `@deepseek-ai/dsh-skill`

```ts config-catalog
/** Skill registry configuration. */
export interface Config {
  /** Maximum number of completed cwd/provider catalogs kept in memory. */
  readonly collectCacheMaxEntries?: number
}
```

来源：[`packages/skill/skill/src/index.ts:278`](../packages/skill/skill/src/index.ts)

<a id="deepseek-aidsh-skill-filesystem"></a>

## `@deepseek-ai/dsh-skill-filesystem`

需要：`skills`

```ts config-catalog
/** Local filesystem skill provider configuration. */
export interface Config {
  /** Unique provider name. Defaults to `filesystem`. */
  providerName?: string
  /** Whether project and user roots are included around custom roots. */
  includeDefaultRoots?: boolean
  /** DeepSeek Harness config root. Defaults to `$DSH_HOME` or `~/.dsh`. */
  dshHome?: string
  /** Shared agent config root. Defaults to `$DSH_AGENTS_HOME` or `~/.agents`. */
  agentsHome?: string
  /** Additional skill roots scanned after project roots and before user roots. */
  customSkillDirs?: string[]
  /** Whether host-local skill roots are watched for catalog changes. */
  watch?: boolean
  /** Whether Chokidar uses polling instead of native filesystem events. */
  watchUsePolling?: boolean
  /** Milliseconds a changed skill entry must remain stable before it is observed. */
  watchStabilityThresholdMs?: number
  /** Milliseconds between Chokidar stability or polling probes. */
  watchPollIntervalMs?: number
  /** Maximum distinct project roots whose skill directories remain watched. */
  watchMaxProjects?: number
  /** Whether watched symbolic links follow their target files. */
  watchFollowSymlinks?: boolean
  /** Bundled skill root; defaults to `$DSH_BUNDLED_SKILL_DIR` when default roots are included, otherwise mounts none. */
  bundledSkillDir?: string
}
```

来源：[`packages/skill/skill-filesystem/src/index.ts:49`](../packages/skill/skill-filesystem/src/index.ts)

## `@deepseek-ai/dsh-spill-local`

```ts config-catalog
/** Plugin config (all optional — `static Config` supplies the defaults). */
export interface Config {
  /**
   * Root directory for spill files. Omitted uses a lazily-created private
   * (0700) per-process directory under the OS temp dir — the safe default for
   * a local deployment. Set it to keep spill files under a known location.
   */
  root?: string
  /**
   * Age in days after which a spill file is eligible for the one-shot startup
   * cleanup sweep. Defaults to `30`; `0` disables cleanup entirely. Files whose
   * `mtime` is strictly older than the cutoff are deleted and emptied
   * directories are pruned; fresh files, symlinks, and unrelated entries are
   * left untouched. On POSIX, cleanup skips roots and session directories that
   * another local user could modify or replace. Retention is deliberate — a
   * resumed or forked session may still reference an older locator until it
   * ages out.
   */
  cleanupPeriodDays?: number
}
```

来源：[`packages/spill/spill-local/src/index.ts:31`](../packages/spill/spill-local/src/index.ts)

<a id="deepseek-aidsh-spill-policy"></a>

## `@deepseek-ai/dsh-spill-policy`

需要：`tools` · `sessionProjections`

```ts config-catalog
/** Plugin config. */
export interface Config {
  /**
   * The model-facing context cap for a plain-text tool result, in UTF-8 bytes.
   * Omitted disables the policy entirely (no-op). When set, a result larger than
   * this is spilled and replaced with a preview derived from this same budget.
   */
  maxInlineBytes?: number
}
```

来源：[`packages/spill/spill-policy/src/index.ts:61`](../packages/spill/spill-policy/src/index.ts)

## `@deepseek-ai/dsh-storage-domain`

需要：`storage`

```ts config-catalog
/**
 * Plugin config. Which backend serves which domain is decided here, not
 * globally on the hub: `backend` is the default route and `routes` overrides
 * it per domain name. A route naming an unregistered backend fails loud at
 * `open` with `backend-not-found`.
 */
export interface Config {
  /** Default backend name for every domain without an explicit route. Required: there is no universally correct medium. */
  backend: string
  /** Per-domain overrides: domain name → backend name. */
  routes?: Record<string, string>
}
```

来源：[`packages/storage/storage-domain/src/index.ts:52`](../packages/storage/storage-domain/src/index.ts)

<a id="deepseek-aidsh-storage-json"></a>

## `@deepseek-ai/dsh-storage-json`

需要：`storage`

```ts config-catalog
/**
 * Plugin configuration.
 * `root` has NO default on purpose: a `process.cwd()` fallback would scatter
 * unit files wherever the process happens to start; assemblies state the
 * location explicitly.
 */
export interface Config {
  /** Directory holding one `<unit>.json` file (or `<unit>/` tree) per unit. */
  root: string
}
```

来源：[`packages/storage/storage-json/src/index.ts:28`](../packages/storage/storage-json/src/index.ts)

## `@deepseek-ai/dsh-subagent`

```ts config-catalog
/** Host configuration for continuable subagent capacity. */
export interface Config {
  /** Maximum live children sharing uninterrupted continuable parent links; defaults to 8. */
  maxActiveSubagents?: number
  /** Default delegation depth for tools without an explicit limit; defaults to 1. */
  maxDepth?: number
}
```

来源： [`packages/subagent/subagent/src/index.ts:190`](../packages/subagent/subagent/src/index.ts)

## `@deepseek-ai/dsh-subagent-fork-in-process`

需要：`subagents`

```ts config-catalog
/** Config: the registry name to register the provider under. */
export interface Config {
  /** Provider name on `ctx.subagents` (default `fork`). */
  providerName: string
}
```

来源：[`packages/subagent/subagent-fork-in-process/src/index.ts:31`](../packages/subagent/subagent-fork-in-process/src/index.ts)

<a id="deepseek-aidsh-subagent-spawn-in-process"></a>

## `@deepseek-ai/dsh-subagent-spawn-in-process`

需要：`subagents`

```ts config-catalog
/** Config: the registry name to register the provider under. */
export interface Config {
  /** Provider name on `ctx.subagents` (default `spawn`). */
  providerName: string
}
```

来源：[`packages/subagent/subagent-spawn-in-process/src/index.ts:25`](../packages/subagent/subagent-spawn-in-process/src/index.ts)

<a id="deepseek-aidsh-system-prompt"></a>

## `@deepseek-ai/dsh-system-prompt`

```ts config-catalog
/** Plugin config: the deployment-authored fragment of the system prompt (see {@link Config.personaPrefix} for its contract). */
export interface Config {
  /** Include the fixed DeepSeek Harness identity before the deployment persona (default true). */
  includeHarnessIdentity?: boolean
  /** Include dynamic runtime-context snapshots in model history (default true). */
  includeRuntimeContext?: boolean
  /**
   * Deployment-wide persona prefix template before first-party guidance. A scoped section named
   * `deployment:persona-prefix` shadows it; `{{variable}}` references are strict.
   */
  personaPrefix?: string
  /**
   * Persona suffix template after first-party guidance. A scoped `deployment:persona-suffix`
   * section shadows it; `{{variable}}` references are strict. Defaults to empty.
   */
  personaSuffix?: string
  /**
   * Model-facing tool names in order, with {@link TOOL_ORDER_REST} exactly once.
   * Invalid fields fail at load and unknown names fail at assembly; known names
   * hidden in one scope may be absent there. Omitted means lexicographic order.
   */
  toolOrder?: string[]
}
```

来源：[`packages/core/system-prompt/src/index.ts:248`](../packages/core/system-prompt/src/index.ts)

<a id="deepseek-aidsh-terminal-bash"></a>

## `@deepseek-ai/dsh-terminal-bash`

需要：`terminals` · `sandboxPolicy` · `sessionProjections` · `subprocess`

```ts config-catalog
/** Public plugin configuration. */
export interface Config {
  /** Backend registry type (default: `shell`). */
  backendType?: string
  /** Interactive shell dialect (default: `bash`); selects the argv/env/startup defaults. */
  shellDialect?: ShellDialect
  /** Interactive shell executable (default per dialect: `/bin/bash`, or the resolved pwsh). */
  shellPath?: string
  /** Shell arguments (default per dialect: bash `--noprofile --norc -i`, pwsh `-NoLogo -NoProfile`). */
  shellArgs?: string[]
  /** Terminal rows. */
  rows?: number
  /** Terminal columns. */
  cols?: number
  /** Maximum retained logical lines. */
  scrollbackLines?: number
  /** Maximum retained UTF-8 bytes. */
  scrollbackMaxBytes?: number
  /** Maximum bytes returned by one read or settled viewport. */
  maxReadBytes?: number
  /** Readiness polling interval. */
  pollIntervalMs?: number
  /** Delay before Linux exact syscall probes. */
  exactProbeAfterMs?: number
  /** Silence duration that yields `inferred_idle`. */
  idleSilenceMs?: number
  /**
   * Extra wait beyond `idleSilenceMs`, once a prompt marker was seen, for the shell to
   * regain the foreground before `inferred_idle` settles; at least one `pollIntervalMs`.
   */
  handoffGraceMs?: number
  /** Absolute bound for one send and the complete pwsh startup sequence. */
  timeoutMs?: number
  /** Grace before teardown escalates to `SIGKILL`. */
  disposeGraceMs?: number
}

/** One supported interactive shell dialect. */
export type ShellDialect = 'bash' | 'pwsh'
```

来源：[`packages/terminal/terminal-bash/src/config.ts:10`](../packages/terminal/terminal-bash/src/config.ts)

<a id="deepseek-aidsh-time-context"></a>

## `@deepseek-ai/dsh-time-context`

需要：`agents` · `sessionProjections`

```ts config-catalog
/** Request-preparation clock formatting and append scheduling. Invalid values fail plugin load. */
export interface Config {
  /** Fallback display zone when the open turn has no unique browser zone. Omit to use the process zone. */
  timeZone?: string
  /** Minimum milliseconds between durable injections in one session. Omit or set to 0 to inject at every eligible step. */
  refreshIntervalMs?: number
}
```

来源：[`packages/context/time-context/src/index.ts:49`](../packages/context/time-context/src/index.ts)

<a id="deepseek-aidsh-tmux-context"></a>

## `@deepseek-ai/dsh-tmux-context`

需要：`agents` · `sessionProjections`

```ts config-catalog
/** Per-turn tmux-location scheduling. Invalid values fail plugin load. */
export interface Config {
  /** Minimum milliseconds between durable injections in one session. Omit or set to 0 to inject on every eligible change. */
  refreshIntervalMs?: number
}
```

来源：[`packages/context/tmux-context/src/index.ts:36`](../packages/context/tmux-context/src/index.ts)

<a id="deepseek-aidsh-token-meter"></a>

## `@deepseek-ai/dsh-token-meter`

需要：`sessionProjections`

```ts config-catalog
/** Token-meter plugin configuration; the fixed estimator has no settings. */
export type TokenMeterConfig = Record<string, never>
```

来源：[`packages/llm/token-meter/src/types.ts:13`](../packages/llm/token-meter/src/types.ts)

<a id="deepseek-aidsh-tool-bash"></a>

## `@deepseek-ai/dsh-tool-bash`

需要：`tools` · `shell` · `systemPrompt` · `shellEnv`

```ts config-catalog
/** Configuration for the bash tool. */
export interface Config {
  /** Expose `run_in_background` (default true); disabled calls are also rejected. */
  enableRunInBackground?: boolean
}
```

来源：[`packages/shell/tool-bash/src/index.ts:33`](../packages/shell/tool-bash/src/index.ts)

<a id="deepseek-aidsh-tool-bash-persistent"></a>

## `@deepseek-ai/dsh-tool-bash-persistent`

需要：`tools` · `terminals`

```ts config-catalog
/** Configuration for the persistent Bash tool. */
export interface Config {
  /** PTY backend used for each owner-isolated persistent shell (default `shell`). */
  backendType?: string
  /** Wall-clock limit for one command (default 300000). */
  timeoutMs?: number
  /** Maximum returned command-output characters before clipping (default 16000). */
  maxOutputChars?: number
  /** Model-facing tool description; deployments may describe their environment. */
  description?: string
}
```

来源：[`packages/shell/tool-bash-persistent/src/index.ts:435`](../packages/shell/tool-bash-persistent/src/index.ts)

<a id="deepseek-aidsh-tool-fs"></a>

## `@deepseek-ai/dsh-tool-fs`

需要：`tools` · `fs` · `systemPrompt`

```ts config-catalog
/** Plugin config (all optional — `Config` supplies the defaults). */
export interface Config {
  /** Default and maximum number of lines returned by one `read` call. */
  readLimit?: number
  /** Maximum characters returned for a single line before truncation. */
  readMaxLineLength?: number
  /** Maximum bytes returned for the selected lines of one `read` call. */
  readMaxBytes?: number
  /** Files at or above this size stream instead of loading whole into memory. */
  readStreamMinSize?: number
}
```

来源：[`packages/fs/tool-fs/src/index.ts:25`](../packages/fs/tool-fs/src/index.ts)

<a id="deepseek-aidsh-tool-fs-search"></a>

## `@deepseek-ai/dsh-tool-fs-search`

需要：`tools` · `systemPrompt` · `subprocess`

```ts config-catalog
/** Plugin config; over-cap glob sampling is an explicit deployment choice and the remaining fields have defaults. */
export interface Config {
  /** Whether an over-cap `glob` page is sampled across top-level entries instead of taking the modification-time head. */
  sampleOverCapGlobResults: boolean
  /** Max paths one `glob` call retains inline; later paths go to the formatted spill file. */
  globMaxResults?: number
  /** Max flat matches one `grep` call retains inline; later matches go to the formatted spill file. */
  grepMaxMatches?: number
  /** Max bytes retained for one matched-line preview (the cut preserves UTF-8 boundaries). */
  grepMaxLineBytes?: number
  /** Max bytes of one search's serialized `presentationMeta`; trailing groups/paths drop past it so the persisted card stays bounded. */
  searchMetaMaxBytes?: number
  /** Max complete raw `rg` stdout bytes a search will parse; larger raw output fails with `SEARCH_RAW_OUTPUT_OVERFLOW`. */
  rawOutputMaxBytes?: number
  /** Terminate-escalation grace (ms), handed to the subprocess seam and bounded by `MAX_TIMER_DELAY_MS`. */
  graceMs?: number
  /** Max bytes retained for one search's stderr tail; the excerpt is embedded in `SEARCH_*` error messages, never shown on success. */
  stderrMaxBytes?: number
  /**
   * Cooperative tool-call timeout budget (ms) on both tools, enforced by
   * `@deepseek-ai/dsh-tool-call-timeout-policy` through `exec.signal`.
   */
  timeoutMs?: number
}
```

来源：[`packages/fs/tool-fs-search/src/index.ts:73`](../packages/fs/tool-fs-search/src/index.ts)

<a id="deepseek-aidsh-tool-goal"></a>

## `@deepseek-ai/dsh-tool-goal`

需要：`agents` · `goals` · `tools` · `systemPrompt` · `sessionProjections`

```ts config-catalog
/** Model policy and hard lower bounds for goal-state updates. */
export interface Config {
  /** Minimum admitted goal rounds before the model may self-report `blocked`. */
  blockedAfterConsecutiveRounds?: number
}
```

来源：[`packages/goal/tool-goal/src/index.ts:25`](../packages/goal/tool-goal/src/index.ts)

<a id="deepseek-aidsh-tool-jobs"></a>

## `@deepseek-ai/dsh-tool-jobs`

需要：`tools` · `jobs` · `systemPrompt`

```ts config-catalog
/** Configures bounded `job_output` waits and completion-notice delivery. */
export interface Config {
  /** Wait duration applied when `job_output` sets `wait` without `timeout_ms` (default 30s). */
  waitTimeoutMs?: number
  /** Hard cap on any single wait; a larger model-supplied `timeout_ms` is clamped down to it (default 10min). */
  maxWaitTimeoutMs?: number
  /** Whether a completion opens a turn on an idle owner (default `wakeup`). */
  completionDelivery?: CompletionDelivery
  /**
   * Turns one owner may have opened by completion wakes before the next
   * notice degrades to injection, reset by any user-authored input (default 3).
   * Bounds the self-exciting chain where a woken turn starts the job whose
   * completion wakes it again.
   */
  maxConsecutiveWakes?: number
}

/**
 * How an unreported completion reaches an owner that is already idle: `wakeup`
 * opens a turn for it, `quiet` leaves it pending until something else wakes the
 * owner. A busy owner is injected either way.
 */
export type CompletionDelivery = 'quiet' | 'wakeup'
```

来源：[`packages/jobs/tool-jobs/src/index.ts:31`](../packages/jobs/tool-jobs/src/index.ts)

## `@deepseek-ai/dsh-tool-present`

依赖： `tools` · `fs` · `sessionProjections`

```ts config-catalog
/** Per-call delivery limit. */
export interface Config {
  /** Maximum number of files in one call. */
  maxFiles: number
}
```

来源：[`packages/deliverables/tool-present/src/index.ts:15`](../packages/deliverables/tool-present/src/index.ts)

<a id="deepseek-aidsh-tool-pwsh"></a>

## `@deepseek-ai/dsh-tool-pwsh`

需要：`tools` · `shell` · `systemPrompt` · `shellEnv`

```ts config-catalog
/** Configuration for the pwsh tool. */
export interface Config {
  /** Expose `run_in_background` (default true); disabled calls are also rejected. */
  enableRunInBackground?: boolean
}
```

来源：[`packages/shell/tool-pwsh/src/index.ts:51`](../packages/shell/tool-pwsh/src/index.ts)

<a id="deepseek-aidsh-tool-pwsh-persistent"></a>

## `@deepseek-ai/dsh-tool-pwsh-persistent`

需要：`tools` · `terminals`

```ts config-catalog
/** Configuration for the persistent pwsh tool. */
export interface Config {
  /** PTY backend used for each owner-isolated persistent shell (default `shell`). */
  backendType?: string
  /** Wall-clock limit for one command (default 300000). */
  timeoutMs?: number
  /** Maximum returned command-output characters before clipping (default 16000). */
  maxOutputChars?: number
  /** Model-facing tool description; deployments may describe their environment. */
  description?: string
}
```

来源：[`packages/shell/tool-pwsh-persistent/src/index.ts:472`](../packages/shell/tool-pwsh-persistent/src/index.ts)

<a id="deepseek-aidsh-tool-ralph"></a>

## `@deepseek-ai/dsh-tool-ralph`

需要：`tools` · `workflowEngine` · `subagents` · `systemPrompt`

```ts config-catalog
/** Deployment policy for the fixed Ralph workflow. */
export interface Config {
  /** Fresh structured-output provider used for every round (default `spawn`). */
  subagentProvider?: string
  /** Default and deployment ceiling for one call's round count (default 256). */
  maxRounds?: number
  /** Maximum serialized characters in one structured handoff (default 16384). */
  maxHandoffChars?: number
  /** Maximum characters in a successful parent-facing terminal text (default 16384). */
  maxResultChars?: number
}
```

来源：[`packages/workflow/tool-ralph/src/index.ts:21`](../packages/workflow/tool-ralph/src/index.ts)

## `@deepseek-ai/dsh-tool-skill`

需要：`agents` · `tools` · `skills`

```ts config-catalog
/** Model-facing skill catalog configuration. */
export interface Config {
  /** Maximum normalized description length rendered in the session catalog; minimum 3. */
  catalogDescriptionMaxLength?: number
}
```

来源：[`packages/skill/tool-skill/src/index.ts:61`](../packages/skill/tool-skill/src/index.ts)

<a id="deepseek-aidsh-tool-str-replace-editor"></a>

## `@deepseek-ai/dsh-tool-str-replace-editor`

需要：`tools` · `fs`

```ts config-catalog
/** Configuration for the string-replacement editor tool. */
export interface Config {
  /** Maximum returned view characters before clipping (default 16000). */
  maxOutputChars?: number
  /** Model-facing tool description. */
  description?: string
}
```

来源：[`packages/fs/tool-str-replace-editor/src/index.ts:505`](../packages/fs/tool-str-replace-editor/src/index.ts)

<a id="deepseek-aidsh-tool-subagent"></a>

## `@deepseek-ai/dsh-tool-subagent`

需要：`tools` · `subagents` · `systemPrompt` · `sessionProjections`

```ts config-catalog
/** Config: which registered provider this tool delegates to, plus child defaults. */
export interface Config {
  /** The `ctx.subagents` provider name to start runs on (e.g. `spawn`, `acp`). */
  provider: string
  /**
   * Model-facing tool name (default `subagent`). Each loaded instance must use
   * a distinct name.
   */
  toolName?: string
  /**
   * Sample the Host `subagent-model-selection` setting for each new top-level
   * Session and inherit that decision in its child Sessions.
   */
  modelSelectionSettings?: boolean
  /**
   * Expose `run_in_background` (default true). Disabled instances omit the
   * parameter and reject forced background calls.
   */
  enableRunInBackground?: boolean
  /**
   * Background execution policy (default `one-shot`). `one-shot` defaults calls
   * to foreground; `continuable` defaults them to background, requires a provider
   * with the `prepareContinuable` capability, and returns the durable child id.
   * Follow-up adapters remain independently optional.
   */
  backgroundMode?: 'one-shot' | 'continuable'
  /**
   * Agent options applied to every child; omitted fields use child-loop defaults.
   */
  agentOptions?: AgentOptions
  /**
   * Per-child persona that shadows `deployment:persona-prefix`. Requires the
   * provider's `persona` capability; omission preserves the deployment persona.
   */
  persona?: string
  /**
   * Tool filter applied to every child. Filtered tools disappear from its
   * prompt and reject execution. Requires the provider's `toolFilter`
   * capability; unknown names fail startup.
   */
  toolFilter?: {
    /** Global tool names the child keeps; everything else is removed. */
    allow?: string[]
    /** Global tool names removed from the child. */
    deny?: string[]
  }
  /**
   * Maximum child depth: a non-negative safe integer (`0` forbids delegation),
   * or `'provider-managed'` to send no cap. A numeric cap
   * requires the provider's `depthLimit` capability (mount fails loud
   * otherwise). The provider checks the calling agent's current depth at every
   * start; the tool remains model-visible so runtime policy owns rejection.
   * `'provider-managed'` is for an out-of-process provider whose recursion
   * budget belongs to the child runtime or its own deployment. Omission reads
   * the current Host subagent depth setting (default `1`) at each delegation.
   */
  maxDepth?: number | 'provider-managed'
}
```

Depends on: [`AgentOptions`](subsystems/core.zh.md)

来源：[`packages/subagent/tool-subagent/src/index.ts:48`](../packages/subagent/tool-subagent/src/index.ts)

## `@deepseek-ai/dsh-tool-todo`

需要：`tools`

```ts config-catalog
/** Model-facing todo tool configuration. */
export interface Config {
  /**
   * Required deployment choice for whether several todos may be `in_progress` at once. True suits
   * agents that run work concurrently — subagents, background commands, workflow fan-out — and the
   * description then instructs the model to mark every actively worked task. False restores the
   * single-active discipline: the description asks for exactly one, and a call marking more is
   * rejected.
   */
  allowParallelInProgress: boolean
}
```

来源：[`packages/todo/tool-todo/src/index.ts:29`](../packages/todo/tool-todo/src/index.ts)

<a id="deepseek-aidsh-tool-web"></a>

## `@deepseek-ai/dsh-tool-web`

需要：`tools` · `web` · `systemPrompt`

```ts config-catalog
/** Plugin config: which web tools to register, search bounds, per-tool budgets, and the fetch output cap. */
export interface Config {
  /** Register `web_search`. Defaults to true. */
  search?: boolean
  /** Register `web_fetch`. Defaults to true. */
  fetch?: boolean
  /** Upper bound on sources returned by one `web_search` call. */
  searchMaxResults?: number
  /** Upper bound on queries accepted by one `web_search` call. */
  searchMaxQueries?: number
  /** Cooperative timeout budget (ms) for `web_fetch`. Defaults to 30000. */
  fetchTimeoutMs?: number
  /** Cooperative timeout budget (ms) for `web_search`. Defaults to 30000. */
  searchTimeoutMs?: number
  /** Cap on source characters converted and complete `web_fetch` output characters. Defaults to 200000. */
  fetchMaxOutputChars?: number
}
```

来源：[`packages/web/tool-web/src/index.ts:37`](../packages/web/tool-web/src/index.ts)

<a id="deepseek-aidsh-tool-workflow"></a>

## `@deepseek-ai/dsh-tool-workflow`

需要：`tools` · `workflowEngine` · `systemPrompt`

```ts config-catalog
/** Config: the model-facing tool name plus result rendering caps. */
export interface Config {
  /** The model-facing tool name to register (default `workflow`). */
  toolName?: string
  /** Rendered-result ceiling, in characters: a longer JSON value is truncated with a notice (default 50000). */
  maxResultChars?: number
}
```

来源：[`packages/workflow/tool-workflow/src/index.ts:32`](../packages/workflow/tool-workflow/src/index.ts)

<a id="deepseek-aidsh-tools"></a>

## `@deepseek-ai/dsh-tools`

需要：`systemPrompt`

```ts config-catalog
/** Plugin config: how the registered tools are presented to the model. */
export interface Config {
  /**
   * Model presentation. `native` (default) sends every visible schema; `ptc`
   * sends only `run_code` plus a generated SDK prompt and collapses the
   * executor to the same surface (a model-direct call may only name
   * `run_code`; `run_code` SDK sub-dispatches keep every visible tool); `both`
   * sends both forms. PTC mode requires a `ctx.ptcRuntime` whose `language`
   * has a registered SDK renderer (TypeScript or Python) and fail prompt
   * assembly when it is absent or has no renderer. Under `ptc`, native names
   * in `toolOrder` are invalid.
   */
  mode?: ToolPresentationMode
  /**
   * Concurrency cap for a `run_code` program's overlapping sub-calls
   * (default 10, the loop scheduler's own default). Sub-calls follow the
   * native scheduling contract — only calls whose tools classify
   * concurrency-safe overlap; exclusive calls form barriers — so `1`
   * restores strictly serial dispatch. Must be a positive integer.
   */
  maxParallelSubCalls?: number
}

/** How the registry presents its tools to the model (see {@link Config.mode}). */
export type ToolPresentationMode = 'native' | 'ptc' | 'both'
```

来源：[`packages/core/tools/src/index.ts:656`](../packages/core/tools/src/index.ts)

<a id="deepseek-aidsh-typert-loader"></a>

## `@deepseek-ai/dsh-typert-loader`

需要：`typert` · `loader`

```ts config-catalog
/** Additional package artifacts whose owning plugins are nested behind another Loader entry. */
export interface Config {
  /** Exact npm package names that must resolve and export `./typert`. */
  packages?: string[]
}
```

来源：[`packages/typert/loader/src/index.ts:48`](../packages/typert/loader/src/index.ts)

<a id="deepseek-aidsh-user-approval"></a>

## `@deepseek-ai/dsh-user-approval`

```ts config-catalog
/** Plugin config. All optional — `static Config` supplies the defaults. */
export interface Config {
  /**
   * The deployment's default {@link ApprovalPolicy} for sessions without an
   * `approval/policy` override — `'ask'` delegates to the composed answerers
   * (fail-closed with none); `'never'` auto-rejects every ask without
   * prompting (the deterministic CI/unattended stance).
   */
  readonly policy?: ApprovalPolicy
}

/**
 * A session's approval policy — what happens to an {@link ApprovalService}
 * ask BEFORE any interactive answerer sees it:
 *
 * - `'ask'` (the default) — delegate to the composed answerers; with none
 *   composed the chain falls through to the fail-closed `'unavailable'`.
 * - `'never'` — never prompt anyone: every ask resolves `'rejected'`
 *   deterministically. The strict headless stance (CI, unattended runs) and
 *   the policy whose outcome is knowable without asking.
 */
export type ApprovalPolicy = 'ask' | 'never'
```

来源：[`packages/interaction/user-approval/src/index.ts:128`](../packages/interaction/user-approval/src/index.ts)

<a id="deepseek-aidsh-web"></a>

## `@deepseek-ai/dsh-web`

```ts config-catalog
/**
 * Config for the web seam. `searchProvider` / `fetchProvider` pin which provider
 * wins for each capability; both are optional (a single registered usable
 * provider auto-selects). Operational overrides such as environment variables
 * must feed these same fields rather than introduce a hidden priority chain.
 */
export interface WebRuntimeConfig {
  /** Explicit search provider id. Omitted = auto-select when exactly one usable. */
  readonly searchProvider?: string
  /** Explicit fetch provider id. Omitted = auto-select when exactly one usable. */
  readonly fetchProvider?: string
}
```

来源：[`packages/web/web/src/index.ts:55`](../packages/web/web/src/index.ts)

## `@deepseek-ai/dsh-web-fetch-http`

需要：`web`

```ts config-catalog
/** Plugin config: the provider's transport and size limits plus its `User-Agent` (all defaulted). */
export interface Config {
  /** Maximum response body size in bytes. */
  maxResponseBytes?: number
  /** Maximum decoded body length in characters. */
  maxBodyChars?: number
  /** Default fetch timeout in milliseconds, within Node's timer range. */
  timeoutMs?: number
  /** Maximum number of same-origin redirect hops to follow. */
  maxRedirects?: number
  /** `User-Agent` header sent on every request. */
  userAgent?: string
}
```

来源：[`packages/web/web-fetch-http/src/index.ts:32`](../packages/web/web-fetch-http/src/index.ts)

<a id="deepseek-aidsh-web-search-deepseek"></a>

## `@deepseek-ai/dsh-web-search-deepseek`

需要：`web`

```ts config-catalog
/** Plugin config (all optional — `apply` fills env-var and constant defaults). */
export interface Config {
  /** Literal DeepSeek API key; prefer {@link apiKeyEnv} so no secret enters configuration files. */
  apiKey?: string
  /** Credential reference resolved for each search; defaults to `DEEPSEEK_API_KEY`. */
  apiKeyEnv?: string
  /** Anthropic-compatible endpoint base; `/messages` is appended. */
  baseURL?: string
  /** Anthropic-format model name. Defaults to `deepseek-v4-flash`. */
  model?: string
  /** `anthropic-version` header value. Defaults to `2023-06-01`. */
  apiVersion?: string
  /** Upper bound on generated tokens for the Messages request. Defaults to 4096. */
  maxTokens?: number
  /** Maximum `web_search` server-tool uses per request. Defaults to 5. */
  maxUses?: number
}
```

来源：[`packages/web/web-search-deepseek/src/index.ts:46`](../packages/web/web-search-deepseek/src/index.ts)

<a id="deepseek-aidsh-web-search-exa"></a>

## `@deepseek-ai/dsh-web-search-exa`

需要：`web`

```ts config-catalog
/** Plugin config (all optional — `apply` fills env-var and constant defaults). */
export interface Config {
  /** Exa API key. Falls back to `$EXA_API_KEY`. Empty → provider unavailable. */
  apiKey?: string
  /** Endpoint base; `/search` is appended. Defaults to the public API. */
  baseURL?: string
  /** Retrieval mode sent as Exa's `type`. Defaults to `auto`. */
  searchType?: 'auto' | 'keyword' | 'neural'
  /** Default result count when a request carries no `maxResults`. Omitted = none. */
  numResults?: number
  /** Highlight sentences requested per result. Defaults to 1. */
  highlightsPerResult?: number
}
```

来源：[`packages/web/web-search-exa/src/index.ts:35`](../packages/web/web-search-exa/src/index.ts)

## `@deepseek-ai/dsh-webhook-github`

需要：`webServer` · `webhookRuntime` · `credentials`

```ts config-catalog
/** Required GitHub ingress configuration. */
export interface Config {
  /** Adapter instance name carried to rules. */
  readonly source: string
  /** Exact absolute route path. */
  readonly path: string
  /** Credential reference containing the shared webhook secret. */
  readonly secretEnv: string
  /** Positive raw body ceiling in bytes. */
  readonly maxBodyBytes: number
}
```

来源：[`packages/webhook/webhook-github/src/index.ts:17`](../packages/webhook/webhook-github/src/index.ts)

<a id="deepseek-aidsh-workflow-ptc"></a>

## `@deepseek-ai/dsh-workflow-ptc`

需要： `subagents` · `ptcRuntime` · `sandboxPolicy`

```ts config-catalog
/** Plugin config (all optional — `static Config` supplies the defaults). */
export interface Config {
  /** The `ctx.subagents` provider children run on (default `spawn`). */
  provider?: string
  /** Concurrent `agent()` ceiling; `0` (the default) auto-resolves to `min(16, max(1, cores - 2))`. */
  maxConcurrentAgents?: number
  /** Total `agent()` calls one run may start — the runaway-loop backstop (default 1000). */
  maxTotalAgents?: number
  /** Items accepted by a single `parallel()`/`pipeline()` call (default 4096). */
  maxItemsPerCall?: number
  /** VM timeout for the script's initial synchronous slice (default 5000 ms). */
  syncTimeoutMs?: number
}
```

来源： [`packages/workflow/workflow-ptc/src/index.ts:32`](../packages/workflow/workflow-ptc/src/index.ts)

## 无配置的可加载插件

这些插件通过 `cordis.yml` 中不含 `config:` 块的条目加载；它们未声明任何配置接口。

- `@deepseek-ai/dsh-agent`（[`packages/core/agent/src/index.ts`](../packages/core/agent/src/index.ts)）
- `@deepseek-ai/dsh-authorization` — 需要 `credentials`（[`packages/credentials/authorization/src/index.ts`](../packages/credentials/authorization/src/index.ts)）
- `@deepseek-ai/dsh-command-compact` — 需要 `commands` · `compact`（[`packages/compaction/command-compact/src/index.ts`](../packages/compaction/command-compact/src/index.ts)）
- `@deepseek-ai/dsh-command-feedback` — 需要 `commands`（[`packages/feedback/command-feedback/src/index.ts`](../packages/feedback/command-feedback/src/index.ts)）
- `@deepseek-ai/dsh-command-goal` — 需要 `commands` · `goals`（[`packages/goal/command-goal/src/index.ts`](../packages/goal/command-goal/src/index.ts)）
- `@deepseek-ai/dsh-commands`（[`packages/interaction/commands/src/index.ts`](../packages/interaction/commands/src/index.ts)）
- `@deepseek-ai/dsh-compaction-image-offload`，需要 `agents` 和 `sessions`（[`packages/compaction/compaction-image-offload/src/index.ts`](../packages/compaction/compaction-image-offload/src/index.ts)）
- `@deepseek-ai/dsh-deepseek-llm-api-extensions`（[`packages/llm/deepseek-llm-api-extensions/src/index.ts`](../packages/llm/deepseek-llm-api-extensions/src/index.ts)）
- `@deepseek-ai/dsh-fs-observation-policy`（[`packages/fs/fs-observation-policy/src/index.ts`](../packages/fs/fs-observation-policy/src/index.ts)）
- `@deepseek-ai/dsh-goal-round-driver` — 需要 `agents` · `goals` · `sessions`（[`packages/goal/goal-round-driver/src/index.ts`](../packages/goal/goal-round-driver/src/index.ts)）
- `@deepseek-ai/dsh-host-plugin-inventory` — 需要 `loader`（[`packages/host/plugin-inventory/src/index.ts`](../packages/host/plugin-inventory/src/index.ts)）
- `@deepseek-ai/dsh-llm`（[`packages/llm/llm/src/index.ts`](../packages/llm/llm/src/index.ts)）
- `@deepseek-ai/dsh-mcp-resources` — 需要 `tools`（[`packages/mcp/mcp-resources/src/index.ts`](../packages/mcp/mcp-resources/src/index.ts)）
- `@deepseek-ai/dsh-schedule` — 需要 `agents` · `sessions` · `tools` · `sessionPersistence`（[`packages/schedule/schedule/src/index.ts`](../packages/schedule/schedule/src/index.ts)）
- `@deepseek-ai/dsh-session`（[`packages/core/session/src/index.ts`](../packages/core/session/src/index.ts)）
- `@deepseek-ai/dsh-session-checkpoint-policy` — 需要 `llm` · `sessionPersistence` · `sessions` · `tools`（[`packages/session/session-checkpoint-policy/src/index.ts`](../packages/session/session-checkpoint-policy/src/index.ts)）
- `@deepseek-ai/dsh-session-projection`（[`packages/session/session-projection/src/index.ts`](../packages/session/session-projection/src/index.ts)）
- `@deepseek-ai/dsh-skill-badge` — 需要 `skills`（[`packages/skill/skill-badge/src/index.ts`](../packages/skill/skill-badge/src/index.ts)）
- `@deepseek-ai/dsh-storage`（[`packages/storage/storage/src/index.ts`](../packages/storage/storage/src/index.ts)）
- `@deepseek-ai/dsh-subprocess-local`（[`packages/subprocess/subprocess-local/src/index.ts`](../packages/subprocess/subprocess-local/src/index.ts)）
- `@deepseek-ai/dsh-terminal`（[`packages/terminal/terminal/src/index.ts`](../packages/terminal/terminal/src/index.ts)）
- `@deepseek-ai/dsh-tool-ask-user` — 需要 `tools` · `userInteraction`（[`packages/interaction/tool-ask-user/src/index.ts`](../packages/interaction/tool-ask-user/src/index.ts)）
- `@deepseek-ai/dsh-tool-call-timeout-policy` — 需要 `tools`（[`packages/guard/timeout-policy/src/index.ts`](../packages/guard/timeout-policy/src/index.ts)）
- `@deepseek-ai/dsh-tool-cordis` — 需要 `tools` · `systemPrompt` · `cordisInspect`（[`packages/extensions/tool-cordis/src/index.ts`](../packages/extensions/tool-cordis/src/index.ts)）
- `@deepseek-ai/dsh-tool-subagent-control` — 需要 `tools` · `subagents`（[`packages/subagent/tool-subagent-control/src/index.ts`](../packages/subagent/tool-subagent-control/src/index.ts)）
- `@deepseek-ai/dsh-user-questions`（[`packages/interaction/user-questions/src/index.ts`](../packages/interaction/user-questions/src/index.ts)）
- `@deepseek-ai/dsh-webhook` — 需要 `agents` · `agentDefaultModel` · `agentPresets` · `permissionPresets` · `sessionTitle` · `workspaceRegistry`（[`packages/webhook/webhook/src/index.ts`](../packages/webhook/webhook/src/index.ts)）
- `@deepseek-ai/dsh-workspace` — 需要 `storageDomain` · `sessionPersistence`（[`packages/workspace/workspace/src/index.ts`](../packages/workspace/workspace/src/index.ts)）

## Seam 包（不可直接加载）

抽象服务类——部署时应改为加载具体的实现包（参见[能力 seam](../.agents/notes/implemented/architecture/2026-06-13-capability-seams.zh.md)）。

- `@deepseek-ai/dsh-attachment` — 抽象 `AttachmentStore`（[`packages/attachment/attachment/src/index.ts`](../packages/attachment/attachment/src/index.ts)）
- `@deepseek-ai/dsh-compaction` — 抽象 `CompactionEngine`（[`packages/compaction/compaction/src/index.ts`](../packages/compaction/compaction/src/index.ts)）
- `@deepseek-ai/dsh-credentials` — 抽象 `Credentials`（[`packages/credentials/credentials/src/index.ts`](../packages/credentials/credentials/src/index.ts)）
- `@deepseek-ai/dsh-file-reference` — 抽象 `FileReferenceService`（[`packages/context/file-reference/src/index.ts`](../packages/context/file-reference/src/index.ts)）
- `@deepseek-ai/dsh-fs` — 抽象 `FileSystem`（[`packages/fs/fs/src/index.ts`](../packages/fs/fs/src/index.ts)）
- `@deepseek-ai/dsh-jobs` — 抽象 `JobRegistry`（[`packages/jobs/jobs/src/index.ts`](../packages/jobs/jobs/src/index.ts)）
- `@deepseek-ai/dsh-ptc-runtime` — 抽象 `PtcRuntime`（[`packages/ptc-runtime/ptc-runtime/src/index.ts`](../packages/ptc-runtime/ptc-runtime/src/index.ts)）
- `@deepseek-ai/dsh-sandbox` — 抽象 `SandboxProvider`（[`packages/sandbox/sandbox/src/index.ts`](../packages/sandbox/sandbox/src/index.ts)）
- `@deepseek-ai/dsh-session-persistence` — 抽象 `SessionPersistence`（[`packages/session/session-persistence/src/index.ts`](../packages/session/session-persistence/src/index.ts)）
- `@deepseek-ai/dsh-session-query` — 抽象 `SessionQueryEngine`（[`packages/session-query/session-query/src/index.ts`](../packages/session-query/session-query/src/index.ts)）
- `@deepseek-ai/dsh-settings` — 抽象 `Settings`（[`packages/settings/settings/src/index.ts`](../packages/settings/settings/src/index.ts)）
- `@deepseek-ai/dsh-shell` — 抽象 `ShellExecutor`（[`packages/shell/shell/src/index.ts`](../packages/shell/shell/src/index.ts)）
- `@deepseek-ai/dsh-spill` — 抽象 `SpillStore`（[`packages/spill/spill/src/index.ts`](../packages/spill/spill/src/index.ts)）
- `@deepseek-ai/dsh-subprocess` — 抽象 `SubprocessRuntime`（[`packages/subprocess/subprocess/src/index.ts`](../packages/subprocess/subprocess/src/index.ts)）
- `@deepseek-ai/dsh-workflow` — 抽象 `WorkflowEngine`（[`packages/workflow/workflow/src/index.ts`](../packages/workflow/workflow/src/index.ts)）
## 库包（无插件入口）

由其他包作为库导入；`cordis.yml` 无法加载它们。

- `@deepseek-ai/dsh-agent-loop-testkit`（[`packages/test-support/agent-loop-testkit/src/index.ts`](../packages/test-support/agent-loop-testkit/src/index.ts)）
- `@deepseek-ai/dsh-anonymous-user-id`（[`packages/identity/anonymous-user-id/src/index.ts`](../packages/identity/anonymous-user-id/src/index.ts)）
- `@deepseek-ai/dsh-app-boot`（[`packages/boot/app-boot/src/index.ts`](../packages/boot/app-boot/src/index.ts)）
- `@deepseek-ai/dsh-atomic-write`（[`packages/util/atomic-write/src/index.ts`](../packages/util/atomic-write/src/index.ts)）
- `@deepseek-ai/dsh-base`（[`packages/bundle/base/src/index.ts`](../packages/bundle/base/src/index.ts)）
- `@deepseek-ai/dsh-brand`（[`packages/util/brand/src/index.ts`](../packages/util/brand/src/index.ts)）
- `@deepseek-ai/dsh-chunked-list`（[`packages/util/chunked-list/src/index.ts`](../packages/util/chunked-list/src/index.ts)）
- `@deepseek-ai/dsh-cmdline`（[`packages/boot/cmdline/src/index.ts`](../packages/boot/cmdline/src/index.ts)）
- `@deepseek-ai/dsh-deque`（[`packages/util/deque/src/index.ts`](../packages/util/deque/src/index.ts)）
- `@deepseek-ai/dsh-home-paths`（[`packages/util/home-paths/src/index.ts`](../packages/util/home-paths/src/index.ts)）
- `@deepseek-ai/dsh-hook-protocol`（[`packages/hooks/hook-protocol/src/index.ts`](../packages/hooks/hook-protocol/src/index.ts)）
- `@deepseek-ai/dsh-http-proxy`（[`packages/util/http-proxy/src/index.ts`](../packages/util/http-proxy/src/index.ts)）
- `@deepseek-ai/dsh-launch-environment`（[`packages/util/launch-environment/src/index.ts`](../packages/util/launch-environment/src/index.ts)）
- `@deepseek-ai/dsh-lazy-require`（[`packages/util/lazy-require/src/index.ts`](../packages/util/lazy-require/src/index.ts)）
- `@deepseek-ai/dsh-llm-mock-server`（[`packages/test-support/llm-mock-server/src/index.ts`](../packages/test-support/llm-mock-server/src/index.ts)）
- `@deepseek-ai/dsh-loader-smoke`（[`packages/test-support/loader-smoke/src/index.ts`](../packages/test-support/loader-smoke/src/index.ts)）
- `@deepseek-ai/dsh-native-command`（[`packages/util/native-command/src/index.ts`](../packages/util/native-command/src/index.ts)）
- `@deepseek-ai/dsh-output-retention`（[`packages/util/output-retention/src/index.ts`](../packages/util/output-retention/src/index.ts)）
- `@deepseek-ai/dsh-package-manifest` ([`packages/util/package-manifest/src/index.ts`](../packages/util/package-manifest/src/index.ts))
- `@deepseek-ai/dsh-remote-mock`（[`packages/test-support/remote-mock/src/index.ts`](../packages/test-support/remote-mock/src/index.ts)）
- `@deepseek-ai/dsh-sandbox-windows-acl`（[`packages/sandbox/sandbox-windows-acl/src/index.ts`](../packages/sandbox/sandbox-windows-acl/src/index.ts)）
- `@deepseek-ai/dsh-scope`（[`packages/core/scope/src/index.ts`](../packages/core/scope/src/index.ts)）
- `@deepseek-ai/dsh-session-format`（[`packages/session/session-format/src/index.ts`](../packages/session/session-format/src/index.ts)）
- `@deepseek-ai/dsh-session-format-catalog`（[`packages/session/session-format-catalog/src/index.ts`](../packages/session/session-format-catalog/src/index.ts)）
- `@deepseek-ai/dsh-session-format-v0-to-v1`（[`packages/session/session-format-v0-to-v1/src/index.ts`](../packages/session/session-format-v0-to-v1/src/index.ts)）
- `@deepseek-ai/dsh-session-format-v1-to-v2`（[`packages/session/session-format-v1-to-v2/src/index.ts`](../packages/session/session-format-v1-to-v2/src/index.ts)）
- `@deepseek-ai/dsh-session-format-v2-to-v3`（[`packages/session/session-format-v2-to-v3/src/index.ts`](../packages/session/session-format-v2-to-v3/src/index.ts)）
- `@deepseek-ai/dsh-session-telemetry`（[`packages/session/session-telemetry/src/index.ts`](../packages/session/session-telemetry/src/index.ts)）
- `@deepseek-ai/dsh-session-title-llm`（[`packages/session/session-title-llm/src/index.ts`](../packages/session/session-title-llm/src/index.ts)）
- `@deepseek-ai/dsh-subagent-in-process-driver`（[`packages/subagent/subagent-in-process-driver/src/index.ts`](../packages/subagent/subagent-in-process-driver/src/index.ts)）
- `@deepseek-ai/dsh-timeout`（[`packages/util/timeout/src/index.ts`](../packages/util/timeout/src/index.ts)）
- `@deepseek-ai/dsh-typert-generator`（[`packages/typert/generator/src/index.ts`](../packages/typert/generator/src/index.ts)）
- `@deepseek-ai/dsh-typert-protocol`（[`packages/typert/protocol/src/index.ts`](../packages/typert/protocol/src/index.ts)）
- `@deepseek-ai/dsh-typert-registry`（[`packages/typert/registry/src/index.ts`](../packages/typert/registry/src/index.ts)）
- `@deepseek-ai/dsh-updater`（[`packages/boot/updater/src/index.ts`](../packages/boot/updater/src/index.ts)）
- `@deepseek-ai/dsh-util-crypto`（[`packages/util/crypto/src/index.ts`](../packages/util/crypto/src/index.ts)）
- `@deepseek-ai/dsh-util-time`（[`packages/util/time/src/index.ts`](../packages/util/time/src/index.ts)）
- `@deepseek-ai/dsh-util-values`（[`packages/util/values/src/index.ts`](../packages/util/values/src/index.ts)）
- `@deepseek-ai/dsh-win32-process`（[`packages/subprocess/win32-process/src/index.ts`](../packages/subprocess/win32-process/src/index.ts)）
