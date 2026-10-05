/**
 * Upstream package names that released profiles, patch layers, and
 * out-of-tree plugins may still use, mapped to the Bake package that replaced
 * each one. Boot reads this map to migrate profile manifests, to rename
 * Loader rows in patch files, and to resolve legacy module specifiers through
 * the profile module fallback. The aliases are deprecated and slated for
 * removal in a later release.
 *
 * `scripts/legacy-package-names.test.ts` checks the map against the workspace
 * manifests, so a renamed or removed package cannot leave it stale.
 * @module bake-app-boot/legacy-package-names
 */

/** Packages renamed from `@deepseek-ai/dsh-<name>` to `bake-<name>`. */
const RENAMED_DSH_PACKAGES = [
  'agent',
  'agent-default-model',
  'agent-instructions',
  'agent-loop',
  'agent-loop-testkit',
  'agent-presets',
  'agent-tool-presentation',
  'anonymous-user-id',
  'api-gateway',
  'api-settings-controller',
  'app-boot',
  'atomic-write',
  'attachment',
  'attachment-local',
  'authorization',
  'base',
  'bash-local',
  'bash-sandbox',
  'brand',
  'chunked-list',
  'client-connection',
  'cmdline',
  'command-compact',
  'command-feedback',
  'command-goal',
  'commands',
  'compaction',
  'compaction-basic',
  'compaction-image-offload',
  'compaction-tool-result-pruner',
  'cordis-host-runner',
  'credentials',
  'credentials-local',
  'deque',
  'desktop',
  'file-reference',
  'file-reference-local',
  'fs',
  'fs-local',
  'fs-observation-policy',
  'fs-sandbox',
  'goal',
  'goal-round-driver',
  'headless',
  'hmr',
  'home-paths',
  'hook-protocol',
  'hooks-claude-code',
  'hooks-codex',
  'host-plugin-inventory',
  'host-webserver',
  'http-proxy',
  'invariants',
  'jobs',
  'jobs-local',
  'launch-environment',
  'lazy-require',
  'llm',
  'llm-mock-server',
  'llm-pi-ai',
  'llm-replay',
  'llm-retry',
  'loader-smoke',
  'mcp-client',
  'mcp-resources',
  'native-command',
  'output-retention',
  'package-manifest',
  'permission-presets',
  'persona',
  'plugin-manager',
  'ptc-runtime',
  'ptc-runtime-codemode',
  'pwsh-local',
  'pwsh-sandbox',
  'remote-mock',
  'repeat-tool-reminder',
  'runtime-watchdog',
  'sandbox',
  'sandbox-local',
  'sandbox-policy',
  'sandbox-windows-acl',
  'schedule',
  'scope',
  'session',
  'session-checkpoint-policy',
  'session-format',
  'session-format-catalog',
  'session-format-v0-to-v1',
  'session-format-v1-to-v2',
  'session-format-v2-to-v3',
  'session-persistence',
  'session-persistence-jsonl',
  'session-projection',
  'session-projection-cache',
  'session-query',
  'session-query-sqlite',
  'session-reference',
  'session-telemetry',
  'session-telemetry-otel',
  'session-title',
  'session-title-first-prompt-llm',
  'session-title-llm',
  'settings',
  'settings-file',
  'shell',
  'shell-change-report',
  'shell-env',
  'skill',
  'skill-badge',
  'skill-filesystem',
  'spill',
  'spill-local',
  'spill-policy',
  'storage',
  'storage-domain',
  'storage-json',
  'subagent',
  'subagent-in-process-driver',
  'subagent-spawn-in-process',
  'subprocess',
  'subprocess-local',
  'system-prompt',
  'terminal',
  'terminal-bash',
  'timeout',
  'token-meter',
  'tool-ask-user',
  'tool-bash',
  'tool-bash-persistent',
  'tool-call-timeout-policy',
  'tool-cordis',
  'tool-fs',
  'tool-fs-search',
  'tool-goal',
  'tool-jobs',
  'tool-pwsh',
  'tool-pwsh-persistent',
  'tool-skill',
  'tool-subagent',
  'tool-subagent-control',
  'tool-web',
  'tools',
  'typert-generator',
  'typert-loader',
  'typert-protocol',
  'typert-registry',
  'updater',
  'user-approval',
  'user-questions',
  'util-crypto',
  'util-time',
  'util-values',
  'web',
  'web-fetch-http',
  'web-search-deepseek',
  'win32-process',
] as const

/** Legacy package name → current package name, for every renamed workspace package. */
export const LEGACY_PACKAGE_NAMES: ReadonlyMap<string, string> = new Map(
  RENAMED_DSH_PACKAGES.map(name => [`@deepseek-ai/dsh-${name}`, `bake-${name}`]),
)

/**
 * Split a bare module specifier into its package name and subpath.
 * @param specifier - module specifier, such as `@scope/name/sub`.
 * @returns the package name and the remaining subpath (empty or `/`-prefixed).
 */
function splitPackageSpecifier(specifier: string): { name: string; subpath: string } {
  const first = specifier.indexOf('/')
  const end = specifier.startsWith('@') && first >= 0 ? specifier.indexOf('/', first + 1) : first
  return end < 0
    ? { name: specifier, subpath: '' }
    : { name: specifier.slice(0, end), subpath: specifier.slice(end) }
}

/**
 * Return the current spelling of a module specifier whose package was renamed.
 * @param specifier - a package name or package subpath, such as `@deepseek-ai/dsh-llm/types`.
 * @returns the specifier under the current package name, or undefined when its package was not renamed.
 */
export function renamedModuleSpecifier(specifier: string): string | undefined {
  const { name, subpath } = splitPackageSpecifier(specifier)
  const current = LEGACY_PACKAGE_NAMES.get(name)
  return current === undefined ? undefined : current + subpath
}

/**
 * Return the current name of a package, mapping a legacy name to its replacement.
 * @param name - a bare package name.
 * @returns the replacement for a legacy name, otherwise `name` itself.
 */
export function currentPackageName(name: string): string {
  return LEGACY_PACKAGE_NAMES.get(name) ?? name
}
