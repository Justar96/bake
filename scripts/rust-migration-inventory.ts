/**
 * Generate and check the scope-00 Rust migration inventory at
 * `docs/roadmap/rust-0.4/scope-00/inventory.json`. The inventory lists what the
 * current sources ship and test: workspace packages, test files and their
 * runner lanes, recorded snapshot scenarios, built-profile PTY scenarios,
 * profile templates, presets, tool packages, and pinned model surfaces. Each
 * item names its owning roadmap scopes and a disposition from the explicit
 * tables below. It is a source inventory, not measured coverage: a listed test
 * proves only that the file exists in a named lane.
 *
 * Discovery fails on anything the tables do not classify, so a new package
 * group, PTY scenario, preset, profile, or tool package must be classified
 * before the inventory regenerates. A new test in a known area is classified
 * by its owner, and the read-only check then fails until the inventory is
 * rewritten and reviewed. Default mode is `--check`; `--write` rewrites.
 */

import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import * as yaml from 'js-yaml'
import type { Nodes } from 'mdast'
import ts from 'typescript'
import { withoutRepositoryGitEnv } from './git-env.ts'
import { parseMarkdown, visitMarkdown } from './markdown.ts'

/** Repository-relative path of the committed inventory. */
export const INVENTORY_PATH = 'docs/roadmap/rust-0.4/scope-00/inventory.json'
const SCHEMA = 'bake-rust-migration-inventory/1'

/** The roadmap's linear scopes. */
export const SCOPES = ['00', '01', '02', '03', '04', '05', '06', '07', '08', '09', '10', '11', '12', '13', '14', '15', '16', '17'] as const
/** A roadmap scope number. */
export type Scope = typeof SCOPES[number]

const DISPOSITIONS = {
  'port-contract': 'The native implementation must reproduce this observable contract.',
  'reuse-oracle': 'Kept as a TypeScript or frozen oracle that native work is compared against.',
  'replace-tooling': 'TypeScript-workspace tooling that native work replaces or extends instead of porting.',
  'explicit-exclusion': 'Deliberately not ported; the reason states what remains read-only.',
} as const
/** How native work treats an inventoried item. */
export type Disposition = keyof typeof DISPOSITIONS

/** Repository files and their text, as discovered by the caller. */
export interface InventorySources {
  /** Repository-relative slash paths of every tracked or unignored file. */
  readonly files: readonly string[]
  /** Read one repository-relative file; throws when it is absent. */
  readonly read: (path: string) => string
}

/** A classification or source failure; the message names the input to fix. */
export class InventoryError extends Error {}

// ---------------------------------------------------------------- classification

/** One classification: owning scopes, disposition, and the domain reason. */
export interface Rule {
  readonly scopes: readonly Scope[]
  readonly disposition: Disposition
  readonly reason: string
  /** A scope-00 support decision this item waits on. */
  readonly pending?: string
}

const PENDING_EXTENSIONS = 'Extension strategy: native protocol, legacy whole-profile compatibility, or approved support change.'
const PENDING_TRANSPORT = 'Trace retained api/client/host/typert consumers before porting or removing them.'

const transportConsumers: Rule = { scopes: ['13'], disposition: 'port-contract', reason: 'Gateway, connection, host, and type-runtime transports; preserve required wire behavior for retained consumers only.', pending: PENDING_TRANSPORT }

/** Rules for workspace package groups: `packages/<group>`, or an application/native root. */
const PACKAGE_GROUPS: Readonly<Record<string, Rule>> = {
  'packages/api': transportConsumers,
  'packages/attachment': { scopes: ['10'], disposition: 'port-contract', reason: 'Attachment admission, image variants, and stored references that model input depends on.' },
  'packages/boot': { scopes: ['05'], disposition: 'port-contract', reason: 'Profile composition, layered configuration, and service lifetimes.' },
  'packages/bundle': { scopes: ['09', '13'], disposition: 'port-contract', reason: 'Shipped profile bundles whose composition decides which services and tools are active.' },
  'packages/client': transportConsumers,
  'packages/compaction': { scopes: ['10'], disposition: 'port-contract', reason: 'Compaction and pruning that replace model-visible history and must stay reconstructable.' },
  'packages/context': { scopes: ['10'], disposition: 'port-contract', reason: 'Instruction discovery and file/session references assembled into model context.' },
  'packages/core': { scopes: ['02', '05', '06', '07', '08', '09'], disposition: 'port-contract', reason: 'Agent, session, tool, and prompt core contracts every later scope builds on.' },
  'packages/credentials': { scopes: ['05', '07'], disposition: 'port-contract', reason: 'Credential precedence, managed secret storage, and provider authorization.' },
  'packages/extensions': { scopes: ['12'], disposition: 'port-contract', reason: 'Cordis host runner and inspection tools that execute JavaScript plugin operations.', pending: PENDING_EXTENSIONS },
  'packages/feedback': { scopes: ['13', '17'], disposition: 'port-contract', reason: 'Local feedback and explicit-feedback export rules.' },
  'packages/fs': { scopes: ['04', '08'], disposition: 'port-contract', reason: 'Filesystem access, observation guards, and the read/write/edit/search tools.' },
  'packages/goal': { scopes: ['11'], disposition: 'port-contract', reason: 'Durable goals and automatic round continuation.' },
  'packages/guard': { scopes: ['08'], disposition: 'port-contract', reason: 'Tool timeout policy and repeated-call reminders that change model input.' },
  'packages/hooks': { scopes: ['12'], disposition: 'port-contract', reason: 'Hook protocols and external hook processes for Claude Code and Codex formats.' },
  'packages/host': transportConsumers,
  'packages/identity': { scopes: ['13'], disposition: 'port-contract', reason: 'Anonymous identity consumed by telemetry opt-outs and feedback.' },
  'packages/interaction': { scopes: ['08', '15'], disposition: 'port-contract', reason: 'Permission presets, approvals, user questions, and commands.' },
  'packages/jobs': { scopes: ['11'], disposition: 'port-contract', reason: 'Background jobs, their completion notices, and job tools.' },
  'packages/llm': { scopes: ['06', '07'], disposition: 'port-contract', reason: 'Provider-neutral streaming, retry, token metering, and pi-ai provider routes.' },
  'packages/mcp': { scopes: ['12'], disposition: 'port-contract', reason: 'MCP client, server-qualified dynamic tools, and resources.' },
  'packages/preset': { scopes: ['05', '10'], disposition: 'port-contract', reason: 'Agent preset discovery, personas, and preset-scoped composition.' },
  'packages/ptc-runtime': { scopes: ['12'], disposition: 'port-contract', reason: 'run_code keeps JavaScript semantics through a confined embedded engine.' },
  'packages/runtime-diagnostics': { scopes: ['13', '17'], disposition: 'port-contract', reason: 'Runtime invariants and watchdog; V8-specific measurements are replaced by native ones.' },
  'packages/sandbox': { scopes: ['04'], disposition: 'port-contract', reason: 'Platform sandbox backends and enforcement-completeness reporting.' },
  'packages/schedule': { scopes: ['11'], disposition: 'port-contract', reason: 'Scheduled follow-ups and persisted recurrence.' },
  'packages/session': { scopes: ['02', '03'], disposition: 'port-contract', reason: 'Session formats, migrations, generations, leases, and projections.' },
  'packages/session-query': { scopes: ['03'], disposition: 'port-contract', reason: 'Session listing and query indexing, with exact reads when search is disabled.' },
  'packages/settings': { scopes: ['05'], disposition: 'port-contract', reason: 'Settings files, watching, and field-wise merging.' },
  'packages/shell': { scopes: ['04', '08'], disposition: 'port-contract', reason: 'Bash and PowerShell executors, persistent shells, and change reporting.' },
  'packages/skill': { scopes: ['10'], disposition: 'port-contract', reason: 'Skill discovery and invocation that adds model context.' },
  'packages/spill': { scopes: ['08'], disposition: 'port-contract', reason: 'Output spill limits and retention for tool results.' },
  'packages/storage': { scopes: ['03'], disposition: 'port-contract', reason: 'Storage domains and JSON documents under the Bake home.' },
  'packages/subagent': { scopes: ['11'], disposition: 'port-contract', reason: 'Child agents, routing, inheritance, and control tools.' },
  'packages/subprocess': { scopes: ['04'], disposition: 'port-contract', reason: 'Process spawning, process groups, Windows jobs, and timeout outcomes.' },
  'packages/terminal': { scopes: ['04', '14'], disposition: 'port-contract', reason: 'Terminal sessions and PTY ownership behind shell tools.' },
  'packages/test-support': { scopes: ['01'], disposition: 'reuse-oracle', reason: 'Replay, mock servers, and loader smokes that drive the TypeScript oracle.' },
  'packages/typert': transportConsumers,
  'packages/util': { scopes: ['01', '02', '05', '06', '07', '08', '09'], disposition: 'port-contract', reason: 'Shared value, time, path, and atomic-write helpers; port the contracts, consolidate the implementation.' },
  'packages/web': { scopes: ['12'], disposition: 'port-contract', reason: 'Agent web search and fetch tools (not the removed web application).' },
  'apps/cli': { scopes: ['09', '13'], disposition: 'port-contract', reason: 'Launcher flags, exit statuses, profile selection, headless output, and aliases.' },
  'apps/tui/packages/app': { scopes: ['14', '15'], disposition: 'port-contract', reason: 'Terminal lifecycle and interactive workflows over runtime projections.' },
  'apps/tui/packages/ui': { scopes: ['14', '15'], disposition: 'port-contract', reason: 'Layout, transcript presentation, and localized copy.' },
  'apps/tui/packages/harness': { scopes: ['14'], disposition: 'replace-tooling', reason: 'Ink component preview and recording; a native frontend needs its own preview tooling.' },
  'native/system': { scopes: ['04', '16'], disposition: 'port-contract', reason: 'Native flock and launcher primitives; proven C may stay until a replacement is qualified.' },
}

/** Narrower scopes or dispositions for individual packages, keyed by directory. */
const PACKAGE_OVERRIDES: Readonly<Record<string, Partial<Rule>>> = {
  'packages/boot/updater': { scopes: ['16'], reason: 'Signed manifest and update/rollback; old updaters expect a Node entry point.' },
  'packages/boot/plugin-manager': { scopes: ['12'], reason: 'npm/Cordis plugin installation for profiles.', pending: PENDING_EXTENSIONS },
  'packages/boot/cmdline': { scopes: ['13'] },
  'packages/bundle/desktop': { scopes: ['13'], reason: 'Bake Desktop profile bundle launched with `dsh --profile desktop` over stdio or an Electron port.', pending: 'Desktop launch: native child spawning or a tested Electron port-to-stdio adapter.' },
  'packages/core/agent-loop': { scopes: ['09'] },
  'packages/core/session': { scopes: ['02', '03'] },
  'packages/core/system-prompt': { scopes: ['10'] },
  'packages/core/tools': { scopes: ['08'] },
  'packages/session/session-telemetry': { scopes: ['13'] },
  'packages/session/session-telemetry-otel': { scopes: ['13'] },
  'packages/session/session-title': { scopes: ['09'] },
  'packages/session/session-title-llm': { scopes: ['09'] },
  'packages/session/session-title-first-prompt-llm': { scopes: ['09'] },
  'packages/llm/llm-pi-ai': { scopes: ['07'] },
  'native/system/packages/darwin-arm64': { scopes: ['16'] },
  'native/system/packages/darwin-x64': { scopes: ['16'] },
  'native/system/packages/linux-arm64': { scopes: ['16'] },
  'native/system/packages/linux-x64': { scopes: ['16'] },
}

/** Tests outside any workspace package, by area; the first matching prefix wins. */
const TEST_AREAS: readonly (Rule & { readonly id: string; readonly prefixes: readonly string[] })[] = [
  { id: 'scripts:conformance', prefixes: ['scripts/rust-conformance/'], scopes: ['01'], disposition: 'reuse-oracle', reason: 'Synthetic comparison fixtures and observation checks qualify the migration harness, not runtime parity.' },
  { id: 'scripts:migration', prefixes: ['scripts/rust-migration-'], scopes: ['00'], disposition: 'replace-tooling', reason: 'Scope-00 migration bookkeeping; retired with the TypeScript oracle in scope 17.' },
  { id: 'scripts:release', prefixes: ['scripts/release/'], scopes: ['16'], disposition: 'replace-tooling', reason: 'Release packing, installers, and manifest tooling; scope 16 changes the archive layout for native artifacts.' },
  { id: 'scripts:persistence', prefixes: ['scripts/persistence-', 'scripts/render-persistence-schema', 'scripts/gen-session-format-catalog', 'scripts/session-', 'scripts/snapshot-'], scopes: ['02', '03'], disposition: 'reuse-oracle', reason: 'Pins released Session formats, persistence digests, and frozen fixture layout that native readers must honor.' },
  { id: 'scripts:cordis', prefixes: ['scripts/cordis-', 'scripts/gen-cordis-catalog', 'scripts/gen-config-catalog', 'scripts/loader-', 'scripts/volatile-config', 'scripts/verify-cordis-config', 'scripts/verify-config-source-ownership', 'scripts/legacy-package-names', 'scripts/package-invariants', 'scripts/test-invariants', 'scripts/verify-built-package-invariants'], scopes: ['05', '12'], disposition: 'replace-tooling', reason: 'Cordis composition, configuration, and package-invariant gates over TypeScript packages; native composition needs its own validation.' },
  { id: 'scripts:workspace', prefixes: ['scripts/bun-invocation', 'scripts/change-scope', 'scripts/clean', 'scripts/doc-typecheck-paths', 'scripts/gen-doc-graphs', 'scripts/gen-tsconfig-paths', 'scripts/git-env', 'scripts/lint-rule-fingerprint', 'scripts/oxlint-contract', 'scripts/package-graph', 'scripts/preflight', 'scripts/project-reference-faces', 'scripts/repo-files', 'scripts/rescope-vendor', 'scripts/run-oxlint', 'scripts/test-bake-environment', 'scripts/test-live-keys', 'scripts/test-proxy-environment', 'scripts/verify-application-entrypoints', 'scripts/verify-dsh-package-licenses', 'scripts/verify-md-links', 'scripts/verify-no-bare-dispatcher', 'scripts/verify-optional-dependency-imports', 'scripts/workspace'], scopes: ['01'], disposition: 'replace-tooling', reason: 'Bun/TypeScript workspace gates; scope 01 adds native checks beside them while TypeScript remains.' },
  { id: 'evals', prefixes: ['evals/'], scopes: ['01', '09'], disposition: 'replace-tooling', reason: 'Paired agent-loop eval runner; scope 01 adapts its Node/Cordis launch interface to a native arm.' },
  { id: 'apps/tui', prefixes: ['apps/tui/tests/'], scopes: ['14'], disposition: 'replace-tooling', reason: 'Guards the TUI against Bun-only runtime imports, a TypeScript packaging concern.' },
]

/** Recorded snapshot surfaces: `snapshots/<surface>/`. */
const SNAPSHOT_SURFACES: Readonly<Record<string, Rule & { readonly owner: string | null }>> = {
  session: { owner: 'snapshots/session/headless.snapshot.ts', scopes: ['02', '03'], disposition: 'reuse-oracle', reason: 'Recorded sessions replayed through the shipped headless profile; generations are frozen read-only inputs.' },
  acp: { owner: 'snapshots/acp/acp.snapshot.ts', scopes: ['02'], disposition: 'explicit-exclusion', reason: 'Removed upstream ACP interface; its generations stay frozen evidence for historical reads only.' },
  sdk: { owner: 'snapshots/sdk/sdk.snapshot.ts', scopes: ['02'], disposition: 'explicit-exclusion', reason: 'Removed upstream SDK interface; its generations stay frozen evidence for historical reads only.' },
  web: { owner: null, scopes: ['02'], disposition: 'explicit-exclusion', reason: 'Removed upstream web client with no owner script; generations and UI expectations stay frozen.' },
}

/** Feature owners added to `session` snapshot scenarios, by name prefix; the first match wins. */
const SESSION_FEATURES: readonly (readonly [prefix: string, scope: Scope])[] = [
  ['packed-chunks', '03'], ['session-query', '03'],
  ['background-confinement', '04'], ['foreground-confinement', '04'], ['missing-sandbox', '04'], ['partial-landlock', '04'], ['session-sandbox-root', '04'], ['pty-tools-sandbox', '04'], ['workflow-confinement', '04'],
  ['deepseek-', '07'], ['pi-ai-', '07'], ['provider-cwd', '07'], ['model-switch', '07'], ['empty-response', '06'], ['error-finish', '06'],
  ['fs-', '08'], ['bash-', '08'], ['pwsh-', '08'], ['persistent-pwsh', '08'], ['parallel-tool-calls', '08'], ['tool-call-turn', '08'], ['repeat-tool-reminder', '08'], ['workspace-edit', '08'], ['both-mode-turn', '08'],
  ['text-turn', '09'], ['system-prompt-in-history', '09'],
  ['agent-instructions', '10'], ['compaction-', '10'], ['read-image', '10'], ['skill-', '10'], ['office-skills', '10'], ['session-reference', '10'], ['todo-write', '10'], ['workspace-dependencies', '10'],
  ['subagent-', '11'], ['product-subagent', '11'], ['background-job', '11'], ['ralph-loop', '11'],
  ['hook-', '12'], ['mcp-', '12'], ['web-', '12'], ['browser-use', '12'], ['computer-use', '12'], ['ptc-', '12'], ['plugin-manager', '12'], ['cordis-inspect', '12'], ['lsp-definition', '12'], ['advanced-toolchain', '12'],
  ['workflow-run', '02'],
]

const PTY_REASONS = {
  engine: 'Terminal rendering, modes, and restoration in a real PTY; scope 14 compares emulator cells, not ANSI bytes.',
  workflow: 'Interactive workflow whose externally visible success condition needs a native PTY counterpart.',
  provider: 'Provider route setup or login driven through the terminal against a loopback server.',
  orchestration: 'Child, job, or goal activity presented in the terminal.',
  context: 'Context, compaction, or attachment behavior driven through the terminal.',
  session: 'Session ownership, resume, or corrupt-log handling driven through the terminal.',
} as const

/** Every PTY scenario in `apps/tui/scripts/pty-smoke.ts` and its owning scopes. */
/** A shared reason for a family of PTY scenarios. */
export type PtyReason = keyof typeof PTY_REASONS

const PTY_SCENARIOS: Readonly<Record<string, readonly [reason: PtyReason, scopes: readonly Scope[]]>> = {
  'fresh': ['workflow', ['14', '15']], 'welcome': ['workflow', ['15']], 'terminal-setup': ['workflow', ['15']],
  'status-colour': ['engine', ['14']], 'git-status': ['workflow', ['15']], 'thinking': ['workflow', ['15']],
  'permissions': ['workflow', ['08', '15']], 'questions': ['workflow', ['08', '15']], 'edit': ['engine', ['14']],
  'shell-edit': ['engine', ['08', '14']], 'tool-colour': ['engine', ['14']], 'live-output': ['engine', ['08', '14']],
  'code-mode': ['engine', ['12', '14']], 'background-job': ['orchestration', ['11', '15']], 'arrow-wave': ['engine', ['14']],
  'fullscreen': ['engine', ['14']], 'markdown': ['engine', ['14']], 'tables': ['engine', ['14']],
  'no-default': ['provider', ['07', '15']], 'cliproxyapi': ['provider', ['07', '15']], 'cliproxyapi-upgrade': ['provider', ['05', '07']],
  'auto-route': ['orchestration', ['11', '15']], 'agents': ['orchestration', ['11', '15']], 'presets': ['workflow', ['05', '15']],
  'settings': ['workflow', ['05', '15']], 'settings-compaction': ['workflow', ['10', '15']], 'settings-agent': ['workflow', ['11', '15']],
  'inspect-agent': ['orchestration', ['02', '15']], 'goal-compact': ['orchestration', ['10', '11', '15']], 'compact-history': ['context', ['10', '15']],
  'thai': ['engine', ['14']], 'rendering': ['engine', ['14']], 'resume': ['session', ['14', '15']],
  'navigate': ['session', ['15']], 'session-in-use': ['session', ['03', '15']], 'corrupt-picker': ['session', ['03', '15']],
  'cancel': ['workflow', ['09', '15']], 'fatal-exception': ['engine', ['14']], 'late-rejection': ['workflow', ['09', '15']],
  'hangup': ['engine', ['14']], 'resume-cleared': ['session', ['15']], 'attachments': ['context', ['10', '15']],
}

const PROFILES: Readonly<Record<string, Rule>> = {
  tui: { scopes: ['13', '14', '15'], disposition: 'port-contract', reason: 'Default terminal profile: base runtime plus the TUI bundle.' },
  headless: { scopes: ['09', '13'], disposition: 'port-contract', reason: 'One-task command-line profile with JSON stream output.' },
  desktop: { scopes: ['13'], disposition: 'port-contract', reason: 'Bake Desktop profile driven over stdio or an Electron parent port.', pending: 'Desktop launch: native child spawning or a tested Electron port-to-stdio adapter.' },
}

const PRESETS: Readonly<Record<string, Rule>> = {
  standard: { scopes: ['05', '10'], disposition: 'port-contract', reason: 'Default coding preset with editing, shell, search, skills, goals, and subagents.' },
  minimal: { scopes: ['05', '10'], disposition: 'port-contract', reason: 'Single persistent-shell preset.' },
  ptc: { scopes: ['10', '12'], disposition: 'port-contract', reason: 'Tools exposed through run_code; depends on the embedded JavaScript engine.' },
  cordis: { scopes: ['05', '12'], disposition: 'port-contract', reason: 'Creation preset with Cordis inspection and persistent plugin management.', pending: PENDING_EXTENSIONS },
}

/** Tool packages in `docs/tool-catalog.md`, by package name. */
const TOOL_PACKAGES: Readonly<Record<string, Rule>> = {
  'bake-tool-fs': { scopes: ['08'], disposition: 'port-contract', reason: 'read/write/edit/read_image schemas, results, and stale-edit guards.' },
  'bake-tool-fs-search': { scopes: ['08'], disposition: 'port-contract', reason: 'glob and grep schemas, ordering, and result bounds.' },
  'bake-tool-bash': { scopes: ['04', '08'], disposition: 'port-contract', reason: 'One-shot bash with sandbox, timeout, and spill semantics.' },
  'bake-tool-pwsh': { scopes: ['04', '08'], disposition: 'port-contract', reason: 'One-shot PowerShell for Windows hosts.' },
  'bake-tool-bash-persistent': { scopes: ['04', '08'], disposition: 'port-contract', reason: 'Persistent bash session shared across calls.' },
  'bake-tool-pwsh-persistent': { scopes: ['04', '08'], disposition: 'port-contract', reason: 'Persistent PowerShell session shared across calls.' },
  'bake-tool-ask-user': { scopes: ['08', '15'], disposition: 'port-contract', reason: 'ask_user_question schema and answer/cancel results.' },
  'bake-tools': { scopes: ['08', '12'], disposition: 'port-contract', reason: 'run_code nested dispatch through the same tool pipeline.' },
  'bake-tool-goal': { scopes: ['11'], disposition: 'port-contract', reason: 'Goal create/get/update tools and budgets.' },
  'bake-schedule': { scopes: ['11'], disposition: 'port-contract', reason: 'Schedule create/list/delete tools and recurrence.' },
  'bake-tool-jobs': { scopes: ['11'], disposition: 'port-contract', reason: 'Background job list/output/kill tools.' },
  'bake-tool-subagent': { scopes: ['11'], disposition: 'port-contract', reason: 'subagent and model-listing tools, including configured aliases.' },
  'bake-tool-subagent-control': { scopes: ['11'], disposition: 'port-contract', reason: 'Child control: list, message, interrupt.' },
  'bake-tool-skill': { scopes: ['10'], disposition: 'port-contract', reason: 'skill invocation adding model context.' },
  'bake-tool-web': { scopes: ['12'], disposition: 'port-contract', reason: 'web_search and web_fetch with destination and payload limits.' },
  'bake-mcp-resources': { scopes: ['12'], disposition: 'port-contract', reason: 'MCP resource listing and reading tools.' },
  'bake-tool-cordis': { scopes: ['12'], disposition: 'port-contract', reason: 'Cordis inspection over a generated JavaScript API catalog that cannot be presented as a Rust API unchanged.', pending: PENDING_EXTENSIONS },
  'bake-plugin-manager': { scopes: ['12'], disposition: 'port-contract', reason: 'plugin_manager installs and toggles JavaScript profile plugins.', pending: PENDING_EXTENSIONS },
}

/** Model-surface snapshot directories and the test that owns each. */
const SURFACE_DIRS: readonly { readonly dir: string; readonly prefix: 'profile' | 'tui-preset'; readonly owner: string }[] = [
  { dir: 'apps/cli/tests/profiles/expected/model-surface/', prefix: 'profile', owner: 'apps/cli/tests/profiles/model-surface.expected.e2e.ts' },
  { dir: 'apps/tui/packages/app/tests/expected/model-surface/', prefix: 'tui-preset', owner: 'apps/tui/packages/app/tests/model-surface.spec.ts' },
]

/** Test runner lanes, mirroring the include/exclude globs of each runner; the first match wins. */
const LANES: readonly { readonly lane: string; readonly globs: readonly string[]; readonly runner: string; readonly gated: boolean }[] = [
  // Only the root config excludes client specs; apps/tui/vitest.config.ts would still run one under its packages.
  { lane: 'excluded-client-spec', globs: ['packages/*/*/tests/**/*.client.spec.ts', 'apps/cli/tests/**/*.client.spec.ts', 'scripts/**/*.client.spec.ts'], runner: 'excluded by vitest.config.ts; no runner includes it', gated: false },
  { lane: 'vitest-runtime', globs: ['packages/*/*/tests/**/*.spec.ts', 'apps/cli/tests/**/*.spec.ts', 'scripts/**/*.spec.ts'], runner: 'bun run test:runtime (vitest.config.ts)', gated: true },
  { lane: 'vitest-integration', globs: ['packages/*/*/tests/**/*.e2e.ts', 'apps/cli/tests/**/*.e2e.ts'], runner: 'bun run test:integration (vitest.e2e.config.ts)', gated: true },
  { lane: 'vitest-tui', globs: ['apps/tui/packages/*/tests/**/*.spec.ts', 'apps/tui/packages/*/tests/**/*.spec.tsx'], runner: 'tui.ts check spec (apps/tui/vitest.config.ts)', gated: true },
  { lane: 'bun-tui', globs: ['apps/tui/**/*.test.ts', 'apps/tui/**/*.test.tsx'], runner: 'tui.ts check unit (bun test in apps/tui)', gated: true },
  { lane: 'bun-scripts', globs: ['scripts/**/*.test.ts'], runner: 'preflight scripts-unit (bun test)', gated: true },
  { lane: 'bun-evals', globs: ['evals/**/*.test.ts'], runner: 'preflight evals-unit (bun test)', gated: true },
  { lane: 'native-package-script', globs: ['native/system/test/*.test.js'], runner: 'native/system package.json test script; not run by preflight or CI', gated: false },
  { lane: 'unwired-snapshot-owner', globs: ['snapshots/*/*.snapshot.ts'], runner: 'no package script or vitest config includes snapshots/', gated: false },
]

/** Every table discovery classifies against; tests substitute small fixtures. */
export interface Classification {
  readonly packageGroups: Readonly<Record<string, Rule>>
  readonly packageOverrides: Readonly<Record<string, Partial<Rule>>>
  readonly testAreas: readonly (Rule & { readonly id: string; readonly prefixes: readonly string[] })[]
  readonly snapshotSurfaces: Readonly<Record<string, Rule & { readonly owner: string | null }>>
  readonly sessionFeatures: readonly (readonly [prefix: string, scope: Scope])[]
  readonly ptyScenarios: Readonly<Record<string, readonly [reason: PtyReason, scopes: readonly Scope[]]>>
  readonly profiles: Readonly<Record<string, Rule>>
  readonly presets: Readonly<Record<string, Rule>>
  readonly toolPackages: Readonly<Record<string, Rule>>
  readonly surfaceDirs: readonly { readonly dir: string; readonly prefix: 'profile' | 'tui-preset'; readonly owner: string }[]
  readonly lanes: readonly { readonly lane: string; readonly globs: readonly string[]; readonly runner: string; readonly gated: boolean }[]
}

/** The repository's classification. */
export const CLASSIFICATION: Classification = {
  packageGroups: PACKAGE_GROUPS, packageOverrides: PACKAGE_OVERRIDES, testAreas: TEST_AREAS,
  snapshotSurfaces: SNAPSHOT_SURFACES, sessionFeatures: SESSION_FEATURES, ptyScenarios: PTY_SCENARIOS,
  profiles: PROFILES, presets: PRESETS, toolPackages: TOOL_PACKAGES, surfaceDirs: SURFACE_DIRS, lanes: LANES,
}

/** File names that make a path a test file; snapshot owners are added by their lane. */
const TEST_FILE = /\.(?:test|spec|e2e)\.[cm]?[jt]sx?$/u
const SNAPSHOT_OWNER = /^snapshots\/[^/]+\/[^/]+\.snapshot\.ts$/u

const LIMITATIONS = [
  'Lists current source files and declarations; it measures no coverage and runs no test.',
  'Commands, slash commands, provider routes and login flows, configuration forms, persisted artifact kinds, and release platforms are not yet inventoried here; scope 00 still owes them.',
  'Lanes mirror runner globs in this script; a runner config change needs a matching LANES update.',
  'TUI layout scenes under apps/tui/prototype run through tui.ts check layout and are not listed as test files.',
  'Snapshot scenarios are identified by snapshot.yml presence; their session generations are not read.',
]

// ---------------------------------------------------------------- discovery

function fail(message: string): never {
  throw new InventoryError(`rust-migration-inventory: ${message}`)
}

/** A table entry by own key, so a discovered name such as `constructor` stays unclassified instead of inheriting. */
function own<T>(table: Readonly<Record<string, T>>, key: string): T | undefined {
  return Object.hasOwn(table, key) ? table[key] : undefined
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function readRequired(sources: InventorySources, path: string): string {
  if (!sources.files.includes(path)) fail(`missing required source ${path}`)
  return sources.read(path)
}

/** Compile a repository glob of `*` and `**` segments into an anchored expression. */
function globExpression(glob: string): RegExp {
  let out = ''
  for (const [index, segment] of glob.split('/').entries()) {
    const last = index === glob.split('/').length - 1
    if (segment === '**') {
      out += '(?:[^/]+/)*'
      continue
    }
    out += segment.split('*').map(part => part.replace(/[.+?^${}()|[\]\\]/gu, '\\$&')).join('[^/]*') + (last ? '' : '/')
  }
  return new RegExp(`^${out}$`, 'u')
}

function scopesOf(rule: Rule): string[] {
  return [...rule.scopes]
}

interface PackageItem { path: string; name: string; scopes: string[]; disposition: Disposition; basis: string }

function groupKey(dir: string): string {
  const parts = dir.split('/')
  if (parts[0] === 'packages') return `packages/${parts[1]}`
  if (dir.startsWith('native/system')) return 'native/system'
  return dir
}

function discoverPackages(sources: InventorySources, c: Classification, rules: Map<string, Rule>): PackageItem[] {
  const root = JSON.parse(readRequired(sources, 'package.json')) as { workspaces?: unknown }
  if (!Array.isArray(root.workspaces) || root.workspaces.some(entry => typeof entry !== 'string')) {
    fail('package.json has no string workspaces array')
  }
  const patterns = (root.workspaces as string[]).filter(pattern => !pattern.startsWith('vendor/')).map(pattern => globExpression(`${pattern}/package.json`))
  const items: PackageItem[] = []
  const overridesSeen = new Set<string>()
  for (const file of sources.files) {
    if (!patterns.some(pattern => pattern.test(file))) continue
    const dir = file.slice(0, -'/package.json'.length)
    const manifest = JSON.parse(sources.read(file)) as { name?: unknown }
    if (typeof manifest.name !== 'string' || manifest.name === '') fail(`${file} has no package name`)
    const group = groupKey(dir)
    const base = own(c.packageGroups, group)
    if (base === undefined) fail(`workspace package ${dir} is in unclassified group ${group}; add it to PACKAGE_GROUPS`)
    const override = own(c.packageOverrides, dir)
    if (override !== undefined) overridesSeen.add(dir)
    const rule = { ...base, ...override }
    // An override with its own reason or pending decision becomes its own rule;
    // a scope-only override keeps the group's rule, which it must not rewrite.
    const basis = override?.reason === undefined && override?.pending === undefined ? group : dir
    rules.set(basis, basis === group ? base : rule)
    items.push({ path: dir, name: manifest.name, scopes: scopesOf(rule), disposition: rule.disposition, basis })
  }
  const stale = Object.keys(c.packageOverrides).filter(dir => !overridesSeen.has(dir))
  if (stale.length > 0) fail(`package overrides name no workspace package: ${stale.join(', ')}`)
  const names = new Set<string>()
  for (const item of items) {
    if (names.has(item.name)) fail(`duplicate workspace package name ${item.name}`)
    names.add(item.name)
  }
  return items
}

interface TestItem { path: string; lane: string; owner: string; scopes: string[]; disposition: Disposition; basis: string }

function discoverTests(
  sources: InventorySources, c: Classification, packages: readonly PackageItem[], rules: Map<string, Rule>,
): TestItem[] {
  const lanes = c.lanes.map(entry => ({ ...entry, expressions: entry.globs.map(globExpression) }))
  const byDepth = [...packages].sort((left, right) => right.path.length - left.path.length)
  const items: TestItem[] = []
  for (const file of sources.files) {
    if (file.startsWith('vendor/') || file.startsWith('node_modules/')) continue
    const isOwner = SNAPSHOT_OWNER.test(file)
    if (!isOwner && !TEST_FILE.test(file)) continue
    const lane = lanes.find(entry => entry.expressions.some(expression => expression.test(file)))
    // Fixture inputs that merely look like tests are skipped unless a runner includes them.
    if (lane === undefined && file.includes('/fixtures/')) continue
    if (lane === undefined) fail(`test ${file} matches no runner lane; add it to LANES or move it into a runner`)
    const owner = byDepth.find(item => file.startsWith(`${item.path}/`))
    if (owner !== undefined) {
      const disposition = owner.disposition === 'port-contract' ? 'reuse-oracle' : owner.disposition
      items.push({ path: file, lane: lane.lane, owner: owner.path, scopes: owner.scopes, disposition, basis: owner.basis })
      continue
    }
    if (isOwner) {
      const surface = file.split('/')[1] as string
      const rule = own(c.snapshotSurfaces, surface)
      if (rule === undefined || rule.owner !== file) fail(`snapshot owner ${file} is not declared in SNAPSHOT_SURFACES`)
      rules.set(`snapshots/${surface}`, rule)
      items.push({ path: file, lane: lane.lane, owner: `snapshots/${surface}`, scopes: scopesOf(rule), disposition: rule.disposition, basis: `snapshots/${surface}` })
      continue
    }
    const area = c.testAreas.find(entry => entry.prefixes.some(prefix => file.startsWith(prefix)))
    if (area === undefined) fail(`test ${file} has no owning package or TEST_AREAS rule`)
    rules.set(area.id, area)
    items.push({ path: file, lane: lane.lane, owner: area.id, scopes: scopesOf(area), disposition: area.disposition, basis: area.id })
  }
  return items
}

interface SnapshotItem { path: string; surface: string; owner: string | null; scopes: string[]; disposition: Disposition; basis: string }

function discoverSnapshots(sources: InventorySources, c: Classification, rules: Map<string, Rule>): SnapshotItem[] {
  const items: SnapshotItem[] = []
  for (const file of sources.files) {
    const match = /^snapshots\/([^/]+)\/([^/]+)\/snapshot\.yml$/u.exec(file)
    if (match === null) continue
    const [, surface, name] = match as unknown as [string, string, string]
    const rule = own(c.snapshotSurfaces, surface)
    if (rule === undefined) fail(`snapshot surface ${surface} is not in SNAPSHOT_SURFACES`)
    if (rule.owner !== null && !sources.files.includes(rule.owner)) fail(`snapshot owner ${rule.owner} is missing`)
    rules.set(`snapshots/${surface}`, rule)
    const scopes = scopesOf(rule)
    if (surface === 'session') {
      const feature = c.sessionFeatures.find(([prefix]) => name.startsWith(prefix))
      if (feature === undefined) fail(`session snapshot ${name} has no SESSION_FEATURES owner`)
      if (!scopes.includes(feature[1])) scopes.push(feature[1])
      scopes.sort()
    }
    items.push({ path: `snapshots/${surface}/${name}`, surface, owner: rule.owner, scopes, disposition: rule.disposition, basis: `snapshots/${surface}` })
  }
  return items
}

const PTY_SOURCE = 'apps/tui/scripts/pty-smoke.ts'

function stringValue(node: ts.Expression, what: string): string {
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text
  if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.PlusToken) {
    return stringValue(node.left, what) + stringValue(node.right, what)
  }
  if (ts.isParenthesizedExpression(node)) return stringValue(node.expression, what)
  return fail(`${what} is not a static string`)
}

function propertyName(property: ts.ObjectLiteralElementLike, what: string): string {
  if (!ts.isPropertyAssignment(property)) fail(`${what} has a non-literal property`)
  const name = property.name
  if (ts.isIdentifier(name) || ts.isStringLiteral(name)) return name.text
  return fail(`${what} has a computed property name`)
}

interface PtyItem {
  name: string
  summary: string
  requires: string[]
  replayOnly: boolean
  scopes: string[]
  disposition: Disposition
  basis: string
}

interface PtyDriver { source: string; fixture: string; command: string; scenarios: number }

function discoverPty(sources: InventorySources, c: Classification, rules: Map<string, Rule>): { driver: PtyDriver; scenarios: PtyItem[] } {
  const file = ts.createSourceFile(PTY_SOURCE, readRequired(sources, PTY_SOURCE), ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
  const topLevel = new Set<ts.Node>()
  const items: PtyItem[] = []
  let fixture: string | undefined
  for (const statement of file.statements) {
    if (ts.isVariableStatement(statement)) {
      for (const declaration of statement.declarationList.declarations) {
        if (!ts.isIdentifier(declaration.name) || declaration.name.text !== 'FIXTURE') continue
        const init = declaration.initializer
        const argument = init !== undefined && ts.isCallExpression(init) ? init.arguments[1] : undefined
        if (argument === undefined) fail(`${PTY_SOURCE} FIXTURE is not join(ROOT, path)`)
        fixture = stringValue(argument, `${PTY_SOURCE} FIXTURE`)
      }
    }
    if (!ts.isExpressionStatement(statement) || !ts.isCallExpression(statement.expression)) continue
    const call = statement.expression
    if (!ts.isIdentifier(call.expression) || call.expression.text !== 'scenario') continue
    topLevel.add(call)
    const [nameNode, summaryNode, optionsNode] = call.arguments
    if (nameNode === undefined || summaryNode === undefined || optionsNode === undefined) fail(`${PTY_SOURCE}: scenario call with missing arguments`)
    const name = stringValue(nameNode, `${PTY_SOURCE} scenario name`)
    if (!ts.isObjectLiteralExpression(optionsNode)) fail(`${PTY_SOURCE}: scenario ${name} options are not an object literal`)
    let requires: string[] = []
    let replayOnly = false
    for (const property of optionsNode.properties) {
      const key = propertyName(property, `scenario ${name} options`)
      const value = (property as ts.PropertyAssignment).initializer
      if (key === 'requires' && ts.isArrayLiteralExpression(value)) {
        requires = value.elements.map(element => stringValue(element, `scenario ${name} requires`))
      } else if (key === 'replayOnly' && (value.kind === ts.SyntaxKind.TrueKeyword || value.kind === ts.SyntaxKind.FalseKeyword)) {
        replayOnly = value.kind === ts.SyntaxKind.TrueKeyword
      } else {
        fail(`${PTY_SOURCE}: scenario ${name} has unsupported option ${key}`)
      }
    }
    const classification = own(c.ptyScenarios, name)
    if (classification === undefined) fail(`PTY scenario ${name} is not classified in PTY_SCENARIOS`)
    const [reason, scopes] = classification
    rules.set(`pty:${reason}`, { scopes, disposition: 'port-contract', reason: PTY_REASONS[reason] })
    items.push({ name, summary: stringValue(summaryNode, `scenario ${name} summary`), requires, replayOnly, scopes: [...scopes], disposition: 'port-contract', basis: `pty:${reason}` })
  }
  const visit = (node: ts.Node): void => {
    if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'scenario' && !topLevel.has(node)) {
      fail(`${PTY_SOURCE}: scenario registered outside a top-level statement cannot be inventoried`)
    }
    ts.forEachChild(node, visit)
  }
  visit(file)
  if (items.length === 0) fail(`${PTY_SOURCE} declares no scenarios`)
  const names = new Set<string>()
  for (const item of items) {
    if (names.has(item.name)) fail(`duplicate PTY scenario ${item.name}`)
    names.add(item.name)
  }
  for (const item of items) {
    for (const required of item.requires) if (!names.has(required)) fail(`PTY scenario ${item.name} requires unknown scenario ${required}`)
  }
  const stale = Object.keys(c.ptyScenarios).filter(name => !names.has(name))
  if (stale.length > 0) fail(`PTY_SCENARIOS names scenarios the driver no longer declares: ${stale.join(', ')}`)
  if (fixture === undefined) fail(`${PTY_SOURCE} declares no FIXTURE`)
  if (!sources.files.includes(fixture)) fail(`PTY fixture ${fixture} is missing`)
  return { driver: { source: PTY_SOURCE, fixture, command: 'bun run test:e2e', scenarios: items.length }, scenarios: items }
}

const PROFILE_SOURCE = 'packages/boot/app-boot/src/profile.ts'

interface ProfileItem { name: string; bundles: string[]; surfaces: string[]; scopes: string[]; disposition: Disposition; basis: string }

function discoverProfiles(
  sources: InventorySources, c: Classification, packages: readonly PackageItem[], rules: Map<string, Rule>,
): ProfileItem[] {
  const file = ts.createSourceFile(PROFILE_SOURCE, readRequired(sources, PROFILE_SOURCE), ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
  let literal: ts.ObjectLiteralExpression | undefined
  for (const statement of file.statements) {
    if (!ts.isVariableStatement(statement)) continue
    for (const declaration of statement.declarationList.declarations) {
      if (ts.isIdentifier(declaration.name) && declaration.name.text === 'PROFILE_TEMPLATES' && declaration.initializer !== undefined) {
        if (!ts.isObjectLiteralExpression(declaration.initializer)) fail(`${PROFILE_SOURCE} PROFILE_TEMPLATES is not an object literal`)
        literal = declaration.initializer
      }
    }
  }
  if (literal === undefined) fail(`${PROFILE_SOURCE} declares no PROFILE_TEMPLATES`)
  const packageNames = new Set(packages.map(item => item.name))
  const items: ProfileItem[] = []
  for (const property of literal.properties) {
    const name = propertyName(property, 'PROFILE_TEMPLATES')
    const value = (property as ts.PropertyAssignment).initializer
    if (!ts.isObjectLiteralExpression(value)) fail(`profile template ${name} is not an object literal`)
    const bundlesProperty = value.properties.find(entry => propertyName(entry, `profile template ${name}`) === 'bundles') as ts.PropertyAssignment | undefined
    if (bundlesProperty === undefined || !ts.isArrayLiteralExpression(bundlesProperty.initializer)) fail(`profile template ${name} has no bundles array`)
    const bundles = bundlesProperty.initializer.elements.map(element => stringValue(element, `profile template ${name} bundles`))
    for (const bundle of bundles) if (!packageNames.has(bundle)) fail(`profile template ${name} names bundle ${bundle}, which is no workspace package`)
    const rule = own(c.profiles, name)
    if (rule === undefined) fail(`profile template ${name} is not classified in PROFILES`)
    rules.set(`profile:${name}`, rule)
    items.push({ name, bundles, surfaces: [], scopes: scopesOf(rule), disposition: rule.disposition, basis: `profile:${name}` })
  }
  const stale = Object.keys(c.profiles).filter(name => !items.some(item => item.name === name))
  if (stale.length > 0) fail(`PROFILES names templates the source no longer ships: ${stale.join(', ')}`)
  return items
}

const PRESET_ROOT = 'packages/preset/agent-presets/presets/'

interface PresetItem { name: string; title: string; surface: string | null; scopes: string[]; disposition: Disposition; basis: string }

function discoverPresets(sources: InventorySources, c: Classification, rules: Map<string, Rule>): PresetItem[] {
  const items: PresetItem[] = []
  for (const file of sources.files) {
    if (!file.startsWith(PRESET_ROOT) || !file.endsWith('/preset.yml')) continue
    const name = file.slice(PRESET_ROOT.length, -'/preset.yml'.length)
    if (name.includes('/')) continue
    let document: unknown
    try {
      document = yaml.load(sources.read(file))
    } catch (error) {
      fail(`${file} is not valid YAML: ${(error as Error).message}`)
    }
    const title = (document as { name?: unknown } | null)?.name
    if (typeof title !== 'string' || title === '') fail(`${file} has no preset name`)
    const rule = own(c.presets, name)
    if (rule === undefined) fail(`preset ${name} is not classified in PRESETS`)
    rules.set(`preset:${name}`, rule)
    items.push({ name, title, surface: null, scopes: scopesOf(rule), disposition: rule.disposition, basis: `preset:${name}` })
  }
  if (items.length === 0) fail(`missing required source ${PRESET_ROOT}*/preset.yml`)
  const stale = Object.keys(c.presets).filter(name => !items.some(item => item.name === name))
  if (stale.length > 0) fail(`PRESETS names presets the source no longer ships: ${stale.join(', ')}`)
  return items
}

function headingText(node: Nodes): string {
  if (node.type === 'text' || node.type === 'inlineCode') return node.value
  return 'children' in node ? node.children.map(child => headingText(child)).join('') : ''
}

function headings(source: string): { depth: number; text: string; code: boolean }[] {
  const out: { depth: number; text: string; code: boolean }[] = []
  visitMarkdown(parseMarkdown(source), (node) => {
    if (node.type !== 'heading') return
    const only = node.children.length === 1 ? node.children[0] : undefined
    out.push({ depth: node.depth, text: headingText(node), code: only?.type === 'inlineCode' })
    return false
  })
  return out
}

const TOOL_CATALOG = 'docs/tool-catalog.md'

interface ToolPackageItem {
  package: string
  tools: string[]
  /** Surfaces exposing a tool only this package registers. */
  surfaces: string[]
  /** Surfaces exposing a tool name another package also registers. */
  sharedNameSurfaces: string[]
  scopes: string[]
  disposition: Disposition
  basis: string
}

function discoverToolPackages(
  sources: InventorySources, c: Classification, packages: readonly PackageItem[], rules: Map<string, Rule>,
): ToolPackageItem[] {
  const packageNames = new Set(packages.map(item => item.name))
  const items: ToolPackageItem[] = []
  let current: ToolPackageItem | undefined
  for (const heading of headings(readRequired(sources, TOOL_CATALOG))) {
    if (heading.depth <= 2) current = undefined
    if (heading.depth === 2 && heading.code) {
      const rule = own(c.toolPackages, heading.text)
      if (rule === undefined) fail(`tool package ${heading.text} in ${TOOL_CATALOG} is not classified in TOOL_PACKAGES`)
      if (!packageNames.has(heading.text)) fail(`${TOOL_CATALOG} names ${heading.text}, which is no workspace package`)
      if (items.some(item => item.package === heading.text)) fail(`${TOOL_CATALOG} lists ${heading.text} twice`)
      rules.set(`tool:${heading.text}`, rule)
      current = { package: heading.text, tools: [], surfaces: [], sharedNameSurfaces: [], scopes: scopesOf(rule), disposition: rule.disposition, basis: `tool:${heading.text}` }
      items.push(current)
    } else if (heading.depth === 3 && heading.code && current !== undefined) {
      current.tools.push(heading.text)
    }
  }
  for (const item of items) if (item.tools.length === 0) fail(`${TOOL_CATALOG} section ${item.package} lists no tools`)
  if (items.length === 0) fail(`${TOOL_CATALOG} lists no tool packages`)
  const stale = Object.keys(c.toolPackages).filter(name => !items.some(item => item.package === name))
  if (stale.length > 0) fail(`TOOL_PACKAGES names packages ${TOOL_CATALOG} no longer lists: ${stale.join(', ')}`)
  return items
}

interface SurfaceItem { id: string; file: string; owner: string; tools: string[] }

function discoverSurfaces(sources: InventorySources, c: Classification): SurfaceItem[] {
  const items: SurfaceItem[] = []
  for (const { dir, prefix, owner } of c.surfaceDirs) {
    const files = sources.files.filter(file => file.startsWith(dir) && file.endsWith('.md') && !file.slice(dir.length).includes('/'))
    if (files.length === 0) fail(`missing required source ${dir}*.md`)
    if (!sources.files.includes(owner)) fail(`model-surface owner ${owner} is missing`)
    for (const file of files) {
      let tools: string[] | undefined
      let declared = -1
      for (const heading of headings(sources.read(file))) {
        if (heading.depth <= 2) {
          if (tools !== undefined) break
          const match = heading.depth === 2 ? /^Tools \((\d+)\)$/u.exec(heading.text) : null
          if (match !== null) {
            tools = []
            declared = Number(match[1])
          }
        } else if (heading.depth === 3 && tools !== undefined) {
          tools.push(heading.text)
        }
      }
      if (tools === undefined) fail(`${file} has no "Tools (N)" section`)
      if (tools.length !== declared) fail(`${file} declares ${declared} tools but lists ${tools.length}`)
      items.push({ id: `${prefix}:${file.slice(dir.length, -'.md'.length)}`, file, owner, tools })
    }
  }
  return items
}

// ---------------------------------------------------------------- assembly

/** The generated inventory document. */
export interface Inventory {
  schema: string
  limitations: string[]
  scopes: string[]
  dispositions: Record<string, string>
  lanes: { lane: string; runner: string; gated: boolean }[]
  rules: Record<string, { reason: string; pending?: string }>
  packages: PackageItem[]
  tests: TestItem[]
  snapshotScenarios: SnapshotItem[]
  ptyDriver: PtyDriver
  ptyScenarios: PtyItem[]
  profiles: ProfileItem[]
  presets: PresetItem[]
  toolPackages: ToolPackageItem[]
  modelSurfaces: SurfaceItem[]
  gaps: { subject: string; missing: string }[]
}

/** Section name to the field that identifies each of its items. */
const KEYED_SECTIONS = {
  packages: 'path', tests: 'path', snapshotScenarios: 'path', ptyScenarios: 'name', profiles: 'name',
  presets: 'name', toolPackages: 'package', modelSurfaces: 'id', gaps: 'subject',
} as const satisfies Partial<Record<keyof Inventory, string>>

function byKey<T>(items: T[], key: (item: T) => string): T[] {
  return items.sort((left, right) => (key(left) < key(right) ? -1 : key(left) > key(right) ? 1 : 0))
}

/**
 * Build the inventory from discovered sources. Output depends only on file
 * contents and the set of paths, never on discovery order.
 * @throws {InventoryError} on a missing source, malformed declaration, or unclassified item.
 */
export function buildInventory(sources: InventorySources, c: Classification = CLASSIFICATION): Inventory {
  const seen = new Set<string>()
  for (const file of sources.files) {
    if (seen.has(file)) fail(`discovery listed ${file} twice`)
    seen.add(file)
  }
  const sorted: InventorySources = { files: [...sources.files].sort(), read: sources.read }
  const rules = new Map<string, Rule>()
  const packages = discoverPackages(sorted, c, rules)
  const tests = discoverTests(sorted, c, packages, rules)
  const snapshotScenarios = discoverSnapshots(sorted, c, rules)
  const pty = discoverPty(sorted, c, rules)
  const profiles = discoverProfiles(sorted, c, packages, rules)
  const presets = discoverPresets(sorted, c, rules)
  const toolPackages = discoverToolPackages(sorted, c, packages, rules)
  const modelSurfaces = discoverSurfaces(sorted, c)

  const toolOwners = new Map<string, ToolPackageItem[]>()
  for (const item of toolPackages) for (const tool of item.tools) toolOwners.set(tool, [...toolOwners.get(tool) ?? [], item])
  for (const surface of modelSurfaces) {
    const [kind, name] = surface.id.split(':') as [string, string]
    if (kind === 'profile') {
      const profile = profiles.find(item => item.name === name)
      if (profile === undefined) fail(`${surface.file} names unknown profile ${name}`)
      profile.surfaces.push(surface.id)
    } else {
      const preset = presets.find(item => item.name === name)
      const tui = profiles.find(item => item.name === 'tui')
      if (preset === undefined || tui === undefined) fail(`${surface.file} names unknown preset ${name} or no tui profile`)
      preset.surface = surface.id
      tui.surfaces.push(surface.id)
    }
    for (const tool of surface.tools) {
      const owners = toolOwners.get(tool)
      if (owners === undefined) fail(`${surface.file} exposes ${tool}, which ${TOOL_CATALOG} does not list`)
      // A name two packages register (bash, pwsh) cannot be attributed from the surface alone.
      const field = owners.length === 1 ? 'surfaces' : 'sharedNameSurfaces'
      for (const owner of owners) if (!owner[field].includes(surface.id)) owner[field].push(surface.id)
    }
  }

  const gaps: Inventory['gaps'] = []
  for (const lane of c.lanes.filter(entry => !entry.gated)) {
    const count = tests.filter(test => test.lane === lane.lane).length
    if (count > 0) gaps.push({ subject: `lane:${lane.lane}`, missing: `${count} test files run by no repository gate: ${lane.runner}` })
  }
  for (const preset of presets) if (preset.surface === null) gaps.push({ subject: `preset:${preset.name}`, missing: 'no pinned model surface' })
  for (const profile of profiles) if (profile.surfaces.length === 0) gaps.push({ subject: `profile:${profile.name}`, missing: 'no pinned model surface' })
  for (const item of toolPackages) {
    if (item.surfaces.length > 0) continue
    gaps.push({ subject: `tool:${item.package}`, missing: item.sharedNameSurfaces.length > 0
      ? 'its tool names are also registered by another package, so no pinned model surface attributes them to this package'
      : 'no tool appears in any pinned model surface; only the default-config catalog schema is pinned' })
  }

  const ruleTable: Inventory['rules'] = {}
  for (const [id, rule] of [...rules].sort(([left], [right]) => (left < right ? -1 : 1))) {
    ruleTable[id] = { reason: rule.reason, ...rule.pending === undefined ? {} : { pending: rule.pending } }
  }
  for (const item of [...profiles, ...toolPackages]) item.surfaces.sort()
  for (const item of toolPackages) item.sharedNameSurfaces.sort()
  return {
    schema: SCHEMA,
    limitations: LIMITATIONS,
    scopes: [...SCOPES],
    dispositions: { ...DISPOSITIONS },
    lanes: c.lanes.map(({ lane, runner, gated }) => ({ lane, runner, gated })),
    rules: ruleTable,
    packages: byKey(packages, item => item.path),
    tests: byKey(tests, item => item.path),
    snapshotScenarios: byKey(snapshotScenarios, item => item.path),
    ptyDriver: pty.driver,
    // Declaration order is execution order, so PTY scenarios keep it.
    ptyScenarios: pty.scenarios,
    profiles: byKey(profiles, item => item.name),
    presets: byKey(presets, item => item.name),
    toolPackages: byKey(toolPackages, item => item.package),
    modelSurfaces: byKey(modelSurfaces, item => item.id),
    gaps: byKey(gaps, item => item.subject),
  }
}

/** Render with one item per line inside each keyed section, so a diff names the item. */
export function renderInventory(inventory: Inventory): string {
  const lines = ['{']
  const entries = Object.entries(inventory)
  for (const [index, [key, value]] of entries.entries()) {
    const comma = index === entries.length - 1 ? '' : ','
    if (key in KEYED_SECTIONS) {
      const items = (value as unknown[]).map(item => `    ${JSON.stringify(item)}`)
      lines.push(`  ${JSON.stringify(key)}: [`, ...items.map((item, at) => (at === items.length - 1 ? item : `${item},`)), `  ]${comma}`)
    } else {
      lines.push(`  ${JSON.stringify(key)}: ${JSON.stringify(value, null, 2).replaceAll('\n', '\n  ')}${comma}`)
    }
  }
  lines.push('}')
  return `${lines.join('\n')}\n`
}

// ---------------------------------------------------------------- comparison

function validateCommitted(committed: Record<string, unknown>): string[] {
  const problems: string[] = []
  const scopes = new Set<string>(SCOPES)
  const rules = isRecord(committed.rules) ? committed.rules : {}
  for (const section of Object.keys(KEYED_SECTIONS)) {
    const items = committed[section]
    if (!Array.isArray(items)) {
      problems.push(`committed inventory has no ${section} array`)
      continue
    }
    const key = KEYED_SECTIONS[section as keyof typeof KEYED_SECTIONS]
    for (const [at, item] of (items as unknown[]).entries()) {
      if (!isRecord(item)) {
        problems.push(`${section}[${at}] is not an object`)
        continue
      }
      if (section === 'gaps' || section === 'modelSurfaces') continue
      const id = `${section} ${String(item[key])}`
      const itemScopes = item.scopes
      if (!Array.isArray(itemScopes) || itemScopes.length === 0 || itemScopes.some(scope => !scopes.has(scope as string))) {
        problems.push(`${id}: unknown scope ownership ${JSON.stringify(itemScopes)}`)
      }
      if (typeof item.disposition !== 'string' || !Object.hasOwn(DISPOSITIONS, item.disposition)) {
        problems.push(`${id}: unknown disposition ${JSON.stringify(item.disposition)}`)
      }
      if (typeof item.basis !== 'string' || !Object.hasOwn(rules, item.basis)) problems.push(`${id}: basis ${JSON.stringify(item.basis)} names no rule`)
    }
  }
  return problems
}

/**
 * Compare the expected inventory with committed text and describe every
 * difference: missing or extra items, changed classifications, invalid
 * ownership, and non-canonical order or formatting.
 * @returns problems; empty when the committed text is exactly the expected rendering.
 */
export function compareInventory(expected: Inventory, committedText: string | undefined): string[] {
  if (committedText === undefined) return [`${INVENTORY_PATH} is missing; run with --write`]
  const rendered = renderInventory(expected)
  if (committedText === rendered) return []
  let committed: Record<string, unknown>
  try {
    committed = JSON.parse(committedText) as Record<string, unknown>
  } catch (error) {
    return [`${INVENTORY_PATH} is not valid JSON: ${(error as Error).message}`]
  }
  if (typeof committed !== 'object' || committed === null || Array.isArray(committed)) return [`${INVENTORY_PATH} is not a JSON object`]
  const problems = validateCommitted(committed)
  for (const [section, value] of Object.entries(expected)) {
    const key = KEYED_SECTIONS[section as keyof typeof KEYED_SECTIONS] as string | undefined
    if (key === undefined) {
      if (JSON.stringify(committed[section]) !== JSON.stringify(value)) problems.push(`${section} differs from the sources`)
      continue
    }
    const actual = Array.isArray(committed[section]) ? (committed[section] as unknown[]).filter(isRecord) : []
    const actualByKey = new Map(actual.map(item => [String(item[key]), item]))
    const expectedByKey = new Map((value as Record<string, unknown>[]).map(item => [String(item[key]), item]))
    if (actualByKey.size !== actual.length) problems.push(`${section} lists an item twice`)
    for (const [id, item] of expectedByKey) {
      const found = actualByKey.get(id)
      if (found === undefined) problems.push(`${section}: missing ${id}`)
      else if (JSON.stringify(found) !== JSON.stringify(item)) problems.push(`${section}: ${id} changed`)
    }
    for (const id of actualByKey.keys()) if (!expectedByKey.has(id)) problems.push(`${section}: stale ${id} is no longer in the sources`)
  }
  for (const section of Object.keys(committed)) if (!Object.hasOwn(expected, section)) problems.push(`unexpected section ${section}`)
  if (problems.length === 0) problems.push(`${INVENTORY_PATH} is not in canonical order or formatting`)
  return problems
}

/** Build from sources and compare; generation failures are reported as problems. */
export function checkInventory(sources: InventorySources, committedText: string | undefined, c: Classification = CLASSIFICATION): string[] {
  try {
    return compareInventory(buildInventory(sources, c), committedText)
  } catch (error) {
    if (error instanceof InventoryError) return [error.message]
    throw error
  }
}

// ---------------------------------------------------------------- CLI

/** Discover tracked and unignored files that exist in the working tree. */
export function repositorySources(root: string): InventorySources {
  const listed = execFileSync('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard'], {
    cwd: root, encoding: 'utf8', env: withoutRepositoryGitEnv(process.env), maxBuffer: 64 * 1024 * 1024,
  })
  const files = [...new Set(listed.split('\0').filter(path => path !== ''))].filter(path => existsSync(resolve(root, path)))
  return { files, read: path => readFileSync(resolve(root, path), 'utf8') }
}

function main(argv: readonly string[]): number {
  const write = argv.includes('--write')
  const unknown = argv.filter(arg => arg !== '--write' && arg !== '--check')
  if (unknown.length > 0 || (write && argv.includes('--check'))) {
    console.error('usage: bun scripts/rust-migration-inventory.ts [--check | --write]')
    return 2
  }
  const root = resolve(import.meta.dirname, '..')
  const target = resolve(root, INVENTORY_PATH)
  let inventory: Inventory
  try {
    inventory = buildInventory(repositorySources(root))
  } catch (error) {
    if (!(error instanceof InventoryError)) throw error
    console.error(error.message)
    return 1
  }
  if (write) {
    mkdirSync(dirname(target), { recursive: true })
    writeFileSync(target, renderInventory(inventory))
    console.log(`wrote ${INVENTORY_PATH}: ${inventory.packages.length} packages, ${inventory.tests.length} tests, ${inventory.ptyScenarios.length} PTY scenarios`)
    return 0
  }
  const problems = compareInventory(inventory, existsSync(target) ? readFileSync(target, 'utf8') : undefined)
  for (const problem of problems) console.error(problem)
  if (problems.length > 0) {
    console.error('Review the change, then run `bun scripts/rust-migration-inventory.ts --write`.')
    return 1
  }
  console.log(`${INVENTORY_PATH} matches the sources`)
  return 0
}

if (import.meta.main) process.exit(main(process.argv.slice(2)))
