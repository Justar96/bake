/** Checkout resolution and launch inputs for the paired agent-loop evaluator. */
import { existsSync, readFileSync, realpathSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { rosterOverlay, systemPromptOverlay, type Roster } from './composition.ts'
import { usesAgentInstructions } from './scenarios.ts'

/**
 * A built Bake checkout or installed pi CLI. A pi root is its package directory;
 * Bake overlays and settings do not apply to pi.
 */
export interface Arm {
  kind: 'bake' | 'pi'
  root: string
  bin?: string
  version?: string
  extra: unknown[]
  settings: string
  llmDeepseek: boolean
  composition: unknown[]
}

/** The retired adapter owns its route in older checkouts; adding the same route through llm-pi-ai would collide. */
function shipsLlmDeepseek(root: string): boolean {
  const patch = join(root, 'packages/bundle/base/cordis.patch.yml')
  return existsSync(patch) && /name:\s*['"]?@deepseek-ai\/dsh-llm-deepseek['"]?\s*$/m.test(readFileSync(patch, 'utf8'))
}

/** Resolve `pi` or `pi:<bin>` to the CLI and its package directory. */
function piArm(spec: string): Arm {
  const named = spec.slice('pi'.length).replace(/^:/, '') || Bun.which('pi')
  if (!named) throw new Error('EVAL_ARMS: pi is not on PATH; pass name=pi:<path to the pi CLI>')
  const bin = realpathSync(named)
  let root = dirname(bin)
  while (!existsSync(join(root, 'package.json')) && dirname(root) !== root) root = dirname(root)
  const version = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version as string
  return { kind: 'pi', root, bin, version, extra: [], settings: '{}', llmDeepseek: false, composition: [] }
}

/**
 * Resolve arms and read their compositions before any sample launches. `env`
 * supplies per-arm EVAL_SETTINGS/EVAL_EXTRA values; pi lookup uses the current PATH.
 */
export function parseArms(spec: string, roster: Roster, env: NodeJS.ProcessEnv): Record<string, Arm> {
  return Object.fromEntries(spec.split(',').filter(Boolean).map((entry): [string, Arm] => {
    const [name, root] = entry.split('=')
    if (!name || !root) throw new Error(`EVAL_ARMS entry "${entry}" is not name=checkout`)
    if (root === 'pi' || root.startsWith('pi:')) return [name, piArm(root)]
    if (!existsSync(join(root, 'apps/cli/lib/bin.js'))) throw new Error(`arm ${name}: ${root} has no built apps/cli/lib/bin.js; run bun run build there`)
    const settings = env[`EVAL_SETTINGS_${name.toUpperCase()}`] ?? '{}'
    JSON.parse(settings)
    return [name, {
      kind: 'bake', root: resolve(root), extra: JSON.parse(env[`EVAL_EXTRA_${name.toUpperCase()}`] ?? '[]'), settings, llmDeepseek: shipsLlmDeepseek(root),
      composition: [systemPromptOverlay(root), ...rosterOverlay(root, roster)],
    }]
  }))
}

/** Cordis rows for one Bake sample, retaining the arm's whole-row configuration. */
export function bakeOverlay(arm: Pick<Arm, 'composition' | 'extra'>, scenario: string,
  paths: { sessions: string; staleWriter: string }): unknown[] {
  return [
    ...arm.composition,
    { id: 'session-title-llm', disabled: true },
    // Only the instructions scenario admits its own AGENTS.md into the context.
    ...usesAgentInstructions(scenario) ? [] : [{ id: 'agent-instructions', disabled: true }],
    { id: 'session-persistence-jsonl', config: { root: paths.sessions, compression: 'none' } },
    ...(scenario === 'stale_edit' ? [{ insert: [{ id: 'token-evaluation-external-writer', name: paths.staleWriter }] }] : []),
    ...arm.extra,
  ]
}

/** The built Node entry point and arguments consumed by a Bake sample. */
export function bakeCommand(root: string, patch: string, prompt: string): string[] {
  return ['node', join(root, 'apps/cli/lib/bin.js'), 'headless', '--patch', patch, '--json', prompt]
}
