/** Checkout resolution and launch inputs for the paired agent-loop evaluator. */
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, readFileSync, realpathSync, statSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { rosterOverlay, systemPromptOverlay, type Roster } from './composition.ts'
import { isEdit } from './metrics.ts'
import { usesAgentInstructions } from './scenarios.ts'

/**
 * A native `bake-rs` binary, a built TypeScript Bake checkout, or an installed
 * pi CLI. A native root is the checkout whose Rust workspace built the binary,
 * or a prebuilt binary's directory; a pi root is its package directory.
 * Bake overlays apply only to the TypeScript arm. Native settings use
 * settings.json; pi does not accept per-arm settings.
 */
export interface Arm {
  kind: 'bake' | 'pi' | 'native'
  root: string
  bin?: string
  version?: string
  /** SHA-256 of a native arm's binary, so a record names the exact build. */
  binarySha256?: string
  /**
   * A native arm's provenance: the commit and tracked-file state of the
   * checkout it was built from, read after the build, or a prebuilt binary
   * with neither.
   */
  revision?: { prebuilt: boolean; commit: string | null; dirty: boolean | null }
  extra: unknown[]
  settings: string
  /** Native persistence experiment; memory is the evaluator's default. */
  nativeSession?: NativeSessionMode
  llmDeepseek: boolean
  composition: unknown[]
}

export type NativeSessionMode = 'memory' | 'persistent' | 'recoverable'

export function nativeSessionMode(value: string | undefined): NativeSessionMode {
  if (value === undefined) return 'memory'
  if (value === 'memory' || value === 'persistent' || value === 'recoverable') return value
  throw new Error('EVAL_NATIVE_SESSION must be memory, persistent, or recoverable')
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

/**
 * The `bake-rs` executable a Cargo build reported, from its
 * `--message-format=json-render-diagnostics` stdout. Cargo names the path
 * wherever `CARGO_TARGET_DIR` or `build.target-dir` put it, so this never
 * guesses a path that could hold an older build.
 */
export function builtExecutable(stdout: string): string | undefined {
  let found: string | undefined
  for (const line of stdout.split('\n')) {
    if (!line.startsWith('{')) continue
    let message: any
    try { message = JSON.parse(line) } catch { continue }
    if (message.reason === 'compiler-artifact' && message.target?.name === 'bake-rs'
      && message.target.kind?.includes('bin') && typeof message.executable === 'string') found = message.executable
  }
  return found
}

/**
 * Resolve a native arm's checkout or binary; an empty path is this
 * evaluator's own checkout. A checkout is built with Cargo's locked release
 * profile from its `rust/` directory, so its pinned toolchain applies, and
 * the arm runs the executable Cargo reports; its commit and tracked-file
 * state are read right after the build. A file is a prebuilt binary run as
 * given: no commit is credited to it, only its digest. Cargo's progress goes
 * to stderr.
 */
function nativeArm(path: string): Arm {
  const target = resolve(path || EVALUATOR_ROOT)
  if (!existsSync(target)) throw new Error(`EVAL_ARMS: native arm ${target} does not exist`)
  const prebuilt = statSync(target).isFile()
  let bin: string
  let revision: NonNullable<Arm['revision']>
  if (prebuilt) {
    bin = realpathSync(target)
    revision = { prebuilt: true, commit: null, dirty: null }
  } else {
    if (!existsSync(join(target, 'rust/Cargo.toml'))) {
      const typescript = existsSync(join(target, 'apps/cli/lib/bin.js')) ? `; name=ts:${path} runs its TypeScript agent` : ''
      throw new Error(`EVAL_ARMS: ${target} has no rust/Cargo.toml; pass a checkout or a bake-rs binary${typescript}`)
    }
    const build = spawnSync('cargo', ['build', '--locked', '--release', '-p', 'bake-cli', '--message-format=json-render-diagnostics'], {
      cwd: join(target, 'rust'), stdio: ['ignore', 'pipe', 2], encoding: 'utf8', maxBuffer: 256 * 1024 * 1024,
    })
    if (build.error !== undefined || build.status !== 0) throw new Error(`EVAL_ARMS: cargo build --locked --release in ${join(target, 'rust')} failed${build.error ? `: ${build.error.message}` : ''}`)
    const executable = builtExecutable(build.stdout)
    if (executable === undefined) throw new Error(`EVAL_ARMS: cargo build in ${join(target, 'rust')} reported no bake-rs executable`)
    bin = realpathSync(executable)
    const vcs = (...args: string[]) => spawnSync('git', ['-C', target, ...args], { encoding: 'utf8' })
    const head = vcs('rev-parse', 'HEAD')
    revision = head.status === 0
      ? { prebuilt: false, commit: head.stdout.trim(), dirty: vcs('status', '--porcelain', '--untracked-files=no').stdout.trim() !== '' }
      : { prebuilt: false, commit: null, dirty: null }
  }
  const probe = spawnSync(bin, ['--version'], { encoding: 'utf8', timeout: 10_000 })
  if (probe.status !== 0) throw new Error(`EVAL_ARMS: ${bin} --version failed; is it a bake-rs binary?`)
  return {
    kind: 'native', root: prebuilt ? dirname(bin) : target, bin, version: probe.stdout.trim(),
    binarySha256: createHash('sha256').update(readFileSync(bin)).digest('hex'), revision,
    extra: [], settings: '{}', llmDeepseek: false, composition: [],
  }
}

/**
 * Resolve arms and read their compositions before any sample launches. A bare
 * checkout or binary, `native`, or `native:<path>` is the native `bake-rs`;
 * `ts:<checkout>` is the frozen TypeScript agent of a built checkout; `pi` or
 * `pi:<bin>` is pi. `env` supplies per-arm values: EVAL_SETTINGS for a native
 * or TypeScript arm, EVAL_NATIVE_SESSION for a native arm, and EVAL_EXTRA for
 * a TypeScript arm; one an arm would ignore stops the run. pi lookup uses the
 * current PATH, and a native checkout is built here.
 */
export function parseArms(spec: string, roster: Roster, env: NodeJS.ProcessEnv): Record<string, Arm> {
  return Object.fromEntries(spec.split(',').filter(Boolean).map((entry): [string, Arm] => {
    const [name, root] = entry.split('=')
    if (!name || !root) throw new Error(`EVAL_ARMS entry "${entry}" is not name=checkout`)
    const settingsKey = `EVAL_SETTINGS_${name.toUpperCase()}`
    const extraKey = `EVAL_EXTRA_${name.toUpperCase()}`
    const sessionKey = `EVAL_NATIVE_SESSION_${name.toUpperCase()}`
    const pi = root === 'pi' || root.startsWith('pi:')
    const typescript = root.startsWith('ts:')
    // An override the arm would ignore must not pass for a measured setting.
    const ignored = [...pi ? [settingsKey] : [], ...typescript ? [] : [extraKey], ...pi || typescript ? [sessionKey] : []]
      .filter(key => env[key] !== undefined)
    if (ignored.length > 0) throw new Error(`EVAL_ARMS: arm ${name} would not apply ${ignored.join(' and ')}`)
    if (pi) return [name, piArm(root)]
    const settings = env[settingsKey] ?? '{}'
    parseSettings(settings)
    if (!typescript) {
      const path = root === 'native' ? '' : root.startsWith('native:') ? root.slice('native:'.length) : root
      return [name, { ...nativeArm(path), settings, nativeSession: nativeSessionMode(env[sessionKey]) }]
    }
    const checkout = root.slice('ts:'.length)
    if (!existsSync(join(checkout, 'apps/cli/lib/bin.js'))) throw new Error(`arm ${name}: ${checkout} has no built apps/cli/lib/bin.js; run bun run build there`)
    return [name, {
      kind: 'bake', root: resolve(checkout), extra: JSON.parse(env[extraKey] ?? '[]'), settings, llmDeepseek: shipsLlmDeepseek(checkout),
      composition: [systemPromptOverlay(checkout), ...rosterOverlay(checkout, roster)],
    }]
  }))
}

const plainObject = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value)

function parseSettings(text: string): Record<string, unknown> {
  const value: unknown = JSON.parse(text)
  if (!plainObject(value)) throw new Error('EVAL_SETTINGS must be a JSON object')
  return value
}

/** Merge nested objects; arrays and scalar values replace the preceding value. */
export function mergeSettings(base: Record<string, unknown>, extra: Record<string, unknown>): Record<string, unknown> {
  const result = { ...base }
  for (const [key, value] of Object.entries(extra)) {
    const prior = result[key]
    Object.defineProperty(result, key, { value: plainObject(prior) && plainObject(value) ? mergeSettings(prior, value) : value, enumerable: true, configurable: true, writable: true })
  }
  return result
}

/**
 * Native settings.json for a sample. Per-arm settings cannot change the
 * evaluator's route, retry policy, permission preset, or forced compaction
 * policy; conflicting keys throw before any sample process starts.
 */
export function nativeSettings(text: string, provider: string, model: string,
  compaction?: { reserveTokens: number; keepRecentTokens: number }): Record<string, unknown> {
  const extra = parseSettings(text.replaceAll('$PROVIDER', provider).replaceAll('$MODEL', model))
  for (const key of ['defaultProvider', 'defaultModel', 'defaultThinkingLevel', 'enabledModels', 'modelThinkingLevels',
    'cliproxyapi', 'llm-pi-ai', 'agent-default-model', 'permission', 'retry', 'sessionDir']) {
    if (Object.hasOwn(extra, key)) throw new Error(`native EVAL_SETTINGS cannot set evaluator-controlled ${key}`)
  }
  const fixed: Record<string, unknown> = { retry: { enabled: false } }
  if (compaction !== undefined) {
    const policy: Record<string, unknown> = { enabled: true, ...compaction }
    if (Object.hasOwn(extra, 'compaction')) {
      if (!plainObject(extra.compaction) || Object.entries(extra.compaction).some(([key, value]) => policy[key] !== value)) {
        throw new Error('native EVAL_SETTINGS cannot change the scenario compaction policy')
      }
    }
    fixed.compaction = policy
  }
  return mergeSettings(extra, fixed)
}

/**
 * The tools a native sample's settings ask the model to be offered: the plain
 * names of `defaultTools`, or only `codemode` when `codemode.mode` is `only`,
 * which presents the others through it. Undefined when `defaultTools` is unset
 * or uses `+`/`-` modifiers on bake-rs's own defaults. bake-rs, as pi does,
 * drops a name it does not register without a word, so a binary built before a
 * tool existed would measure an arm without it.
 */
export function requestedTools(settings: Record<string, unknown>): string[] | undefined {
  const entries = settings.defaultTools
  if (!Array.isArray(entries) || entries.length === 0 || !entries.every(entry => typeof entry === 'string' && !/^[+-]/.test(entry))) return undefined
  const codemode = plainObject(settings.codemode) ? settings.codemode : {}
  return entries.includes('codemode') && codemode.mode === 'only' ? ['codemode'] : [...new Set(entries as string[])]
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

/** The permission presets a native arm can run under (`EVAL_NATIVE_PERMISSION`). */
export const NATIVE_PERMISSIONS = ['danger-full-access', 'workspace-write'] as const
export type NativePermission = typeof NATIVE_PERMISSIONS[number]

/**
 * The `bake-rs` print-mode launch for one native sample: Pi's JSON event
 * stream, no session file by default, the permission preset (the TypeScript
 * arms' `danger-full-access` unless the run asks for the default
 * `workspace-write`), and the route's model and effort. Persistent
 * experiments supply a private file and fixed run limits. The prompt follows
 * `--`, so it is never an option.
 */
export function nativeCommand(bin: string, provider: string, model: string, effort: string, prompt: string,
  permission: NativePermission = 'danger-full-access',
  session?: { mode: Exclude<NativeSessionMode, 'memory'>; path: string; maxModelCalls: number; timeoutMs: number }): string[] {
  const args = session === undefined ? ['--no-session'] : ['--session', session.path,
    '--max-model-calls', String(session.maxModelCalls), '--run-timeout-ms', String(session.timeoutMs),
    ...session.mode === 'recoverable' ? ['--recoverable'] : []]
  return [bin, '-p', '--mode', 'json', ...args, '--permission', permission,
    '--model', `${provider}/${model}`, '--thinking', effort, '--', prompt]
}

/** What a native arm's stale writer did: whether it wrote, and whether the write raced an edit. */
export interface StaleInjection { injected: boolean; raced: boolean }

/**
 * The stale writer for a native arm, which has no extensions: it reads
 * `bake-rs`'s `--mode json` events as the evaluator receives them and, after
 * the first successful `read` of `target`, prepends the comment the fix must
 * keep and writes `injectionPath`, once.
 *
 * The write lands after `bake-rs` has already moved on, so it is ordered only
 * against later model responses. An edit in the same response as that read,
 * whether its `tool_execution_start` comes before or after the write, may run
 * before the write lands; such an injection is `raced`, and the sample says
 * nothing about the agent's handling of the external change.
 * @param workspace - the directory relative `read` paths resolve against.
 * @param target - the absolute path of the file to change.
 * @param injectionPath - the marker the evaluator's predicate checks.
 */
export function nativeStaleWriter(workspace: string, target: string, injectionPath: string): { onEvent(event: any): void; state: StaleInjection } {
  const state: StaleInjection = { injected: false, raced: false }
  // pi's convention: each assistant `message_end` is one response, and the calls after it are its own.
  let step = -1
  let injectedStep: number | undefined
  const reads = new Map<string, number>()
  const editSteps = new Set<number>()
  return {
    state,
    onEvent(event) {
      if (event?.type === 'message_end' && event.message?.role === 'assistant') step++
      else if (event?.type === 'tool_execution_start') {
        if (event.toolName === 'read' && typeof event.args?.path === 'string' && resolve(workspace, event.args.path) === target) reads.set(event.toolCallId, step)
        else if (isEdit({ tool: event.toolName, input: event.args, callId: event.toolCallId, step })) {
          editSteps.add(step)
          if (injectedStep === step) state.raced = true
        }
      } else if (event?.type === 'tool_execution_end' && event.toolName === 'read' && event.isError !== true && !state.injected) {
        const readStep = reads.get(event.toolCallId)
        if (readStep === undefined) return
        writeFileSync(target, '// EXTERNAL_CHANGE_KEEP\n' + readFileSync(target, 'utf8'))
        writeFileSync(injectionPath, JSON.stringify({ injected: true }))
        state.injected = true
        injectedStep = readStep
        if (editSteps.has(readStep)) state.raced = true
      }
    },
  }
}

/** The context file names `bake-rs` loads from a directory, after Pi's, in its order. */
export const NATIVE_CONTEXT_FILES = ['AGENTS.override.md', 'AGENTS.md', 'AGENTS.MD']

/**
 * Context files and skill directories a native sample would load but the
 * scenario does not own. `bake-rs` reads `~/.agents/AGENTS.md` and
 * `~/.agents/skills` under the user's home, one context file from each
 * directory between the filesystem root and its working directory, and the
 * `.agents/skills` of each directory from its working directory up to the git
 * root, or to the filesystem root outside a repository. The workspace's own
 * context file is left out when the scenario provides its own instructions,
 * and its own `.agents/skills` when the scenario provides skills.
 * @returns the files found, first match per directory, then the skill directories.
 */
export function strayContextFiles(workspace: string, userHome: string, ownsWorkspaceFile: boolean, ownsWorkspaceSkills = false): string[] {
  const start = realpathSync(workspace)
  const contextDirs = [join(userHome, '.agents')]
  const skillDirs = [join(userHome, '.agents', 'skills')]
  let collectingSkills = true
  for (let dir = start; ; dir = dirname(dir)) {
    if (!(ownsWorkspaceFile && dir === start)) contextDirs.push(dir)
    if (collectingSkills && !(ownsWorkspaceSkills && dir === start)) skillDirs.push(join(dir, '.agents', 'skills'))
    if (existsSync(join(dir, '.git'))) collectingSkills = false
    if (dirname(dir) === dir) break
  }
  const files = contextDirs.flatMap(dir => {
    const found = NATIVE_CONTEXT_FILES.map(name => join(dir, name)).find(path => existsSync(path) && statSync(path).isFile())
    return found === undefined ? [] : [found]
  })
  return [...files, ...skillDirs.filter(dir => existsSync(dir))]
}
