/**
 * Probe plugin. Proves an out-of-tree `.ts` file mounts into a shipped profile
 * through a `--patch` insert row under the tsx source launch, and reports which
 * services a TUI plugin can reach once the application has settled.
 *
 * Loader siblings mount concurrently, so reachability is only meaningful after
 * `loader.await()` — the same settlement the headless runner waits on before it
 * creates an Agent. Vendored Cordis has no optional-inject form (every `inject`
 * entry is required and gates `ctx` property access), so the probe declares
 * nothing and reads services through `ctx.get(name)`.
 *
 * @module tui-probe
 */

import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/cordis-plugin-loader'

/** Cordis plugin name used by loader diagnostics. */
export const name = 'tui-probe'

/** Services a TUI would consume, probed without requiring any of them. */
const WANTED = [
  'agents', 'agentDefaultModel', 'sessions', 'sessionQuery', 'sessionProjection',
  'commands', 'tools', 'approval', 'agentPresets', 'skills', 'goals', 'jobs',
  'fs', 'llm', 'cmdline', 'settings', 'credentials', 'subagents', 'workspace',
]

/**
 * Report the runtime and the reachable services after settlement, then leave
 * the tree untouched.
 *
 * @param ctx - the profile's Cordis context.
 */
export function apply(ctx: Context): void {
  void (async () => {
    await ctx.get('loader')?.await()
    const reached: string[] = []
    const missing: string[] = []
    for (const key of WANTED) {
      const present = (ctx as unknown as { get(name: string): unknown }).get(key) !== undefined
      ;(present ? reached : missing).push(key)
    }
    process.stderr.write(`TUI-PROBE: mounted on node ${process.version}\n`)
    process.stderr.write(`TUI-PROBE: reached (${reached.length}): ${reached.join(', ')}\n`)
    process.stderr.write(`TUI-PROBE: missing (${missing.length}): ${missing.join(', ')}\n`)
    process.stderr.write(`TUI-PROBE: stdin tty=${process.stdin.isTTY === true} stdout tty=${process.stdout.isTTY === true}\n`)
  })()
}
