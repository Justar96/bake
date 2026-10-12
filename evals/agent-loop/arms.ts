/** Checkout resolution and launch inputs for the paired agent-loop evaluator. */
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, readFileSync, realpathSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { rosterOverlay, systemPromptOverlay, type Roster } from './composition.ts'
import { usesAgentInstructions } from './scenarios.ts'

/**
 * A built Bake checkout, an installed pi CLI, or a native `bake-rs` binary. A
 * pi root is its package directory; a native root is the checkout whose Rust
 * workspace built the binary, or the binary's directory outside a checkout.
 * Bake overlays and settings apply to neither.
 */
export interface Arm {
  kind: 'bake' | 'pi' | 'native'
  root: string
  bin?: string
  version?: string
  /** SHA-256 of a native arm's binary, so a record names the exact build. */
  binarySha256?: string
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

/** The repository root this evaluator runs from. */
const EVALUATOR_ROOT = resolve(import.meta.dir, '../..')

/** The release `bake-rs` that `cargo build --locked --release -p bake-cli` leaves in a checkout. */
export function nativeBinary(checkout: string): string {
  return join(checkout, 'rust/target/release', `bake-rs${process.platform === 'win32' ? '.exe' : ''}`)
}

/** The nearest directory at or above `start` holding a Rust workspace, if any. */
function checkoutOf(start: string): string | undefined {
  for (let dir = start; ; dir = dirname(dir)) {
    if (existsSync(join(dir, 'rust/Cargo.toml'))) return dir
    if (dirname(dir) === dir) return undefined
  }
}

/**
 * Resolve `native`, `native:<checkout>`, or `native:<binary>`. A checkout,
 * this evaluator's own by default, is built with Cargo's locked release
 * profile from its `rust/` directory, so its pinned toolchain applies; a file
 * is run as given. Cargo's output goes to stderr.
 */
function nativeArm(spec: string): Arm {
  const target = resolve(spec.slice('native'.length).replace(/^:/, '') || EVALUATOR_ROOT)
  if (!existsSync(target)) throw new Error(`EVAL_ARMS: native arm ${target} does not exist`)
  let bin: string
  if (statSync(target).isFile()) bin = realpathSync(target)
  else {
    if (!existsSync(join(target, 'rust/Cargo.toml'))) throw new Error(`EVAL_ARMS: ${target} has no rust/Cargo.toml; pass a checkout or a bake-rs binary`)
    const build = spawnSync('cargo', ['build', '--locked', '--release', '-p', 'bake-cli'], { cwd: join(target, 'rust'), stdio: ['ignore', 2, 2] })
    if (build.error !== undefined || build.status !== 0) throw new Error(`EVAL_ARMS: cargo build --locked --release in ${join(target, 'rust')} failed${build.error ? `: ${build.error.message}` : ''}`)
    bin = realpathSync(nativeBinary(target))
  }
  const probe = spawnSync(bin, ['--version'], { encoding: 'utf8', timeout: 10_000 })
  if (probe.status !== 0) throw new Error(`EVAL_ARMS: ${bin} --version failed; is it a bake-rs binary?`)
  return {
    kind: 'native', root: checkoutOf(dirname(bin)) ?? dirname(bin), bin, version: probe.stdout.trim(),
    binarySha256: createHash('sha256').update(readFileSync(bin)).digest('hex'),
    extra: [], settings: '{}', llmDeepseek: false, composition: [],
  }
}

/**
 * Resolve arms and read their compositions before any sample launches. `env`
 * supplies per-arm EVAL_SETTINGS/EVAL_EXTRA values; pi lookup uses the current
 * PATH, and a native checkout is built here.
 */
export function parseArms(spec: string, roster: Roster, env: NodeJS.ProcessEnv): Record<string, Arm> {
  return Object.fromEntries(spec.split(',').filter(Boolean).map((entry): [string, Arm] => {
    const [name, root] = entry.split('=')
    if (!name || !root) throw new Error(`EVAL_ARMS entry "${entry}" is not name=checkout`)
    if (root === 'pi' || root.startsWith('pi:')) return [name, piArm(root)]
    if (root === 'native' || root.startsWith('native:')) return [name, nativeArm(root)]
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

/**
 * The `bake-rs` print-mode launch for one native sample: Pi's JSON event
 * stream, no session file, the Bake arms' `danger-full-access` preset, and the
 * route's model and effort. The prompt follows `--`, so it is never an option.
 */
export function nativeCommand(bin: string, provider: string, model: string, effort: string, prompt: string): string[] {
  return [bin, '-p', '--mode', 'json', '--no-session', '--permission', 'danger-full-access',
    '--model', `${provider}/${model}`, '--thinking', effort, '--', prompt]
}
