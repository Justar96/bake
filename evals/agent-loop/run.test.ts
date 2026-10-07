/**
 * Keyless launch contract of the live evaluator: runs the real `run.ts` over
 * fake Bake checkouts and a fake pi CLI whose entry points are the recorder in
 * tests/fixtures, and pins what each sample launched: argv, working directory,
 * environment, overlay, settings, pi agent files, and the stale writer.
 *
 * The recorders send no request and change no fixture file, so every sample
 * fails and carries no usage, composition check, or system prompt. These tests
 * pin configuration only; they say nothing about measured results.
 */
import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { type ChildProcess, spawn } from 'node:child_process'
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'
import { pathToFileURL } from 'node:url'
import YAML from 'yaml'
import { prompts } from './scenarios.ts'

const RUN = join(import.meta.dir, 'run.ts')
const RECORDER = join(import.meta.dir, 'tests/fixtures/launch-recorder.mjs')
const CLIPROXY_KEY = 'eval-contract-dummy-cliproxyapi'
const AMBIENT_DEEPSEEK_KEY = 'eval-contract-dummy-ambient-deepseek'
/** Below the 30 s lane, leaving room to reap a stuck evaluator and its agents before the hook times out. */
const RUN_DEADLINE_MS = 15_000

type Json = any
interface Capture {
  script: string; args: string[]; cwd: string; env: Record<string, string | null>; secretNames: string[]
  patch: string | null; extension: string | null; hook: string | null
  bakeSettings: string | null; bakeCredentials: string | null; piModels: string | null; piSettings: string | null
}
interface Evaluation {
  code: number | null; signal: string | null; stdout: string; stderr: string
  captures: Capture[]; outputs: Record<string, string>
}

// Hand-authored compositions, small enough to read; each arm's differs so a
// mixed-up checkout shows.
const write = (path: string, text: string) => { mkdirSync(join(path, '..'), { recursive: true }); writeFileSync(path, text) }
const CHECKOUTS = {
  base: {
    'packages/bundle/base/cordis.patch.yml': '- id: system-prompt\n  config:\n    personaPrefix: ""\n- id: spill-policy\n  config:\n    maxInlineBytes: 50000\n',
    'packages/bundle/headless/cordis.patch.yml': '- id: system-prompt\n  config:\n    includeHarnessIdentity: false\n    personaSuffix: "Fixture cwd {{cwd}}."\n    personaPrefix: Headless fixture persona.\n',
    'packages/preset/agent-presets/presets/standard/agent.cordis.yml': '- id: persona\n  config:\n    prefix: Base fixture persona.\n'
      + '- id: tool-fs\n  config:\n    readMaxBytes: 111\n- id: tool-fs-search\n  config:\n    maxChars: 222\n- id: tool-result-pruner\n  config:\n    maxChars: 333\n',
    'apps/tui/packages/app/cordis.built.patch.yml': '- id: spill-policy\n  config:\n    maxInlineBytes: 444\n',
  },
  // No terminal patch and no pruner row: the tui roster falls back to the base bundle's spill cap and skips the pruner.
  cand: {
    'packages/bundle/base/cordis.patch.yml': '- id: system-prompt\n  config:\n    personaPrefix: ""\n- id: spill-policy\n  config:\n    maxInlineBytes: 555\n',
    'packages/bundle/headless/cordis.patch.yml': '- id: system-prompt\n  config:\n    includeHarnessIdentity: false\n    personaSuffix: "Candidate cwd {{cwd}}."\n',
    'packages/preset/agent-presets/presets/standard/agent.cordis.yml': '- id: persona\n  config:\n    prefix: Candidate fixture persona.\n'
      + '- id: tool-fs\n  config:\n    readMaxBytes: 666\n- id: tool-fs-search\n  config:\n    maxChars: 777\n',
  },
} as const
const COMPOSITION: Record<'base' | 'cand', Record<'headless' | 'tui', Json[]>> = {
  base: {
    headless: [{ id: 'system-prompt', config: { includeHarnessIdentity: false, personaSuffix: 'Fixture cwd {{cwd}}.', personaPrefix: 'Base fixture persona.' } }],
    tui: [
      { id: 'system-prompt', config: { includeHarnessIdentity: false, personaSuffix: 'Fixture cwd {{cwd}}.', personaPrefix: 'Base fixture persona.' } },
      { id: 'tool-fs', config: { readMaxBytes: 111 } }, { id: 'tool-fs-search', config: { maxChars: 222 } },
      { id: 'tool-result-pruner', config: { maxChars: 333 } }, { id: 'spill-policy', config: { maxInlineBytes: 444 } },
    ],
  },
  cand: {
    headless: [{ id: 'system-prompt', config: { includeHarnessIdentity: false, personaSuffix: 'Candidate cwd {{cwd}}.', personaPrefix: 'Candidate fixture persona.' } }],
    tui: [
      { id: 'system-prompt', config: { includeHarnessIdentity: false, personaSuffix: 'Candidate cwd {{cwd}}.', personaPrefix: 'Candidate fixture persona.' } },
      { id: 'tool-fs', config: { readMaxBytes: 666 } }, { id: 'tool-fs-search', config: { maxChars: 777 } },
      { id: 'spill-policy', config: { maxInlineBytes: 555 } },
    ],
  },
}
const HOME_SETTINGS = `llm-pi-ai:
  providers:
    cliproxyapi:
      api: openai-responses
      baseURL: http://gateway.invalid/v1
      apiKeyEnv: CLIPROXYAPI_API_KEY
      compat:
        sendSessionAffinityHeaders: true
        keepThis: true
      modelOverrides:
        fixture-model:
          maxTokens: 1
      models:
        - id: fixture-model
          name: Fixture Model
          contextWindow: 64000
          reasoningEfforts:
            low: minimal
            high: deep
        - id: other-model
`
const HOME_CREDENTIALS = `version: 1\nrefs:\n  CLIPROXYAPI_API_KEY: ${CLIPROXY_KEY}\n`
const EXTRA_BASE = [{ id: 'fixture-extra', config: { marker: 'base-only' } }]
const SETTINGS_BASE = '{"llm-pi-ai":{"providers":{"$PROVIDER":{"strictTools":true}}},"fixture-section":{"model":"$MODEL"}}'

/** Run a short host command to completion, awaiting `close` even when the spawn fails. */
async function host(command: string, args: string[]): Promise<{ code: number | null; stdout: string }> {
  const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'ignore'], timeout: 5_000, killSignal: 'SIGKILL' })
  let stdout = ''
  let failure: Error | undefined
  child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk })
  child.once('error', error => { failure = error })
  const code = await new Promise<number | null>(resolve => child.once('close', resolve))
  if (failure) throw failure
  if (code === null) throw new Error(`${command} did not exit normally`)
  return { code, stdout }
}

/** Poll a condition until it holds, failing after `limitMs`; a bound on waiting, never a readiness delay. */
async function until(condition: () => boolean | Promise<boolean>, what: string, limitMs = 5_000, signal?: AbortSignal): Promise<void> {
  const end = Date.now() + limitMs
  while (!await condition()) {
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`)
    await sleep(10, undefined, { signal })
  }
}

const exists = (pid: number) => { try { process.kill(pid, 0); return true } catch (error) { return (error as NodeJS.ErrnoException).code === 'EPERM' } }

/**
 * Stop a stuck evaluator together with the agents it launched. Each agent runs
 * detached in its own process group, so killing the evaluator alone would leave
 * them running. The evaluator is frozen first: a stopped parent can neither
 * reap its children, so their PIDs cannot be recycled, nor launch new ones, so
 * its direct children listed by `ps` are exactly the agents it owns. Their
 * groups are killed, then the evaluator, and teardown waits until every one of
 * those groups is gone.
 */
async function reap(child: ChildProcess, closed: Promise<unknown>, root: string, inspect = host): Promise<void> {
  const pid = child.pid!
  let agents: number[] = []
  let failure: unknown
  try {
    if (!child.kill('SIGSTOP')) return
    await until(async () => {
      const { code, stdout } = await inspect('ps', ['-o', 'stat=', '-p', String(pid)])
      if (code !== 0 && exists(pid)) throw new Error(`ps could not observe evaluator ${pid}`)
      return !/^[^TZ]/.test(stdout.trim())
    }, `evaluator ${pid} to stop`)
    const { code, stdout } = await inspect('ps', ['-A', '-o', 'pid=', '-o', 'ppid='])
    if (code !== 0) throw new Error('ps could not list the evaluator children')
    agents = stdout.trim().split('\n').map(line => line.trim().split(/\s+/).map(Number)).filter(([, ppid]) => ppid === pid).map(([agent]) => agent!)
    // A child caught between fork and setsid is not yet a group leader, so it is also signalled by PID.
    for (const agent of agents) for (const target of [-agent, agent]) { try { process.kill(target, 'SIGKILL') } catch {} }
  } catch (error) {
    failure = error
  } finally {
    child.kill('SIGKILL')
    await closed
  }
  if (failure !== undefined) {
    // Once the evaluator exits, parent ids are lost. Only these private recorder
    // entry points identify its detached groups; no shared executable is matched.
    const scripts = [...new Set([root, realpathSync(root)])].flatMap(prefix =>
      ['base/apps/cli/lib/bin.js', 'cand/apps/cli/lib/bin.js', 'pi/bin/pi'].map(path => join(prefix, path)))
    const { code, stdout } = await host('ps', ['-A', '-o', 'pid=', '-o', 'pgid=', '-o', 'args='])
    if (code !== 0) throw new Error('ps could not identify private recorder groups', { cause: failure })
    for (const line of stdout.split('\n')) {
      const match = /^\s*(\d+)\s+(\d+)\s+(.*)$/.exec(line)
      if (match === null) continue
      const [, candidate, group, command = ''] = match
      if (candidate !== group || !scripts.some(path => `${command} `.includes(` ${path} `))) continue
      const agent = Number(candidate)
      agents.push(agent)
      try { process.kill(-agent, 'SIGKILL') } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error
      }
    }
  }
  await until(() => agents.every(agent => !exists(-agent) && !exists(agent)), `agents ${agents.join(', ')} to exit`)
  if (failure !== undefined) throw failure
}

/** Resolves when a run should be abandoned; the default is the fixed deadline. */
type Deadline = (owned: { root: string; evaluator: number }, signal: AbortSignal) => Promise<void>
const RUN_DEADLINE: Deadline = (_, signal) => sleep(RUN_DEADLINE_MS, undefined, { signal })

/**
 * Run the evaluator once in a private root and return everything it and its
 * agents left, read before the root is removed. The child gets a built
 * environment, never this process's, so no ambient credential or home reaches it.
 * A run past its deadline is reaped with its agents before the root goes.
 */
async function evaluate(options: { roster: 'headless' | 'tui'; models: string; cases: string[]; env?: Record<string, string> | undefined; deadline?: Deadline; inspect?: typeof host }): Promise<Evaluation & { root: string; realRoot: string }> {
  const root = mkdtempSync(join(tmpdir(), 'bake-eval-contract-'))
  try {
    const realRoot = realpathSync(root)
    for (const [arm, files] of Object.entries(CHECKOUTS)) {
      for (const [path, text] of Object.entries(files)) write(join(root, arm, path), text)
      write(join(root, arm, 'package.json'), '{"private":true,"type":"module"}\n')
      mkdirSync(join(root, arm, 'apps/cli/lib'), { recursive: true })
      copyFileSync(RECORDER, join(root, arm, 'apps/cli/lib/bin.js'))
    }
    write(join(root, 'pi/package.json'), '{"name":"fake-pi","version":"0.0.0-contract","type":"module"}\n')
    write(join(root, 'pi/bin/pi'), '#!/usr/bin/env node\n' + readFileSync(RECORDER, 'utf8'))
    chmodSync(join(root, 'pi/bin/pi'), 0o755)
    write(join(root, 'home/.bake/settings.yaml'), HOME_SETTINGS)
    write(join(root, 'home/.bake/.credentials.yaml'), HOME_CREDENTIALS)
    for (const dir of ['tmp', 'capture', 'out']) mkdirSync(join(root, dir))
    const env: Record<string, string> = {
      PATH: process.env.PATH ?? '', HOME: join(root, 'home'), TMPDIR: join(root, 'tmp'),
      EVAL_ARMS: `base=${join(root, 'base')},cand=${join(root, 'cand')},pi=pi:${join(root, 'pi/bin/pi')}`,
      EVAL_MODELS: options.models, EVAL_CASES: options.cases.join(','), EVAL_ROSTER: options.roster, EVAL_TRIALS: '1',
      EVAL_OUTPUT: join(root, 'out'), EVAL_CONTRACT_CAPTURE: join(root, 'capture'),
      // Planted, not ambient: the evaluator must keep it from every agent.
      DEEPSEEK_API_KEY: AMBIENT_DEEPSEEK_KEY,
      ...options.env,
    }
    const child = spawn(process.execPath, [RUN], { cwd: root, env, stdio: ['ignore', 'pipe', 'pipe'] })
    let stdout = ''
    let stderr = ''
    let failure: Error | undefined
    child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk })
    child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk })
    child.once('error', error => { failure = error })
    // `close` follows `error` when the spawn itself fails, so this always settles.
    const closed = new Promise<{ code: number | null; signal: string | null }>(resolve => child.once('close', (code, signal) => resolve({ code, signal })))
    const stop = new AbortController()
    let deadlineFailure: unknown
    const expired = child.pid === undefined
      ? Promise.resolve(false)
      : Promise.resolve().then(() => (options.deadline ?? RUN_DEADLINE)({ root, evaluator: child.pid! }, stop.signal)).then(() => true, error => {
        if (stop.signal.aborted) return false
        deadlineFailure = error
        return true
      })
    const timedOut = await Promise.race([closed.then(() => false), expired])
    stop.abort()
    await expired
    if (timedOut) await reap(child, closed, root, options.inspect)
    const { code, signal } = await closed
    if (failure) throw failure
    if (deadlineFailure !== undefined) throw deadlineFailure
    if (timedOut) throw new Error(`run.ts did not finish before its deadline\n${stdout}\n${stderr}`)
    const captures = readdirSync(join(root, 'capture')).filter(name => name.endsWith('.json')).map(name => JSON.parse(readFileSync(join(root, 'capture', name), 'utf8')) as Capture)
    const outputs = Object.fromEntries(readdirSync(join(root, 'out')).map(name => [name, readFileSync(join(root, 'out', name), 'utf8')]))
    return { code, signal, stdout, stderr, captures, outputs, root, realRoot }
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}

/**
 * Replace this run's private root (both spellings, since a macOS temporary
 * directory realpaths from /var to /private/var), then the one sample root the
 * evaluator made under it. Throws if a capture names more than one sample root.
 */
function normalizer(root: string, realRoot: string) {
  const roots = [...new Set([realRoot, root])].sort((a, b) => b.length - a.length)
  const base = (text: string) => roots.reduce((value, prefix) => value.replaceAll(prefix, '<ROOT>'), text)
  return (capture: Capture): Capture => {
    const samples = new Set(base(JSON.stringify(capture)).match(/<ROOT>\/tmp\/bake-token-eval-[A-Za-z0-9]+/g))
    if (samples.size !== 1) throw new Error(`expected one sample root, found ${[...samples].join(', ')}`)
    const [sample] = samples
    return JSON.parse(base(JSON.stringify(capture)).replaceAll(sample!, '<SAMPLE>'))
  }
}

/** The proxy origin each sample's route points at, replaced after checking it is one loopback origin. */
function proxied(text: string): string {
  const origins = new Set(text.match(/http:\/\/127\.0\.0\.1:\d+/g))
  expect(origins.size).toBe(1)
  return text.replaceAll([...origins][0]!, '<PROXY>')
}

const scenarioOf = (prompt: string | undefined) => Object.keys(prompts).find(name => prompts[name] === prompt)
const armOf = (capture: Capture) => capture.script === '<ROOT>/pi/bin/pi' ? 'pi' : capture.script.match(/^<ROOT>\/(base|cand)\/apps\/cli\/lib\/bin\.js$/)?.[1]

const BAKE_HOOK = `import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
export const name = 'token-evaluation-external-writer';
export function apply(ctx) {
 let changed = false;
 ctx.on('tools/result', (exec, result) => {
 if (exec.name !== 'read' || result.isError || !(resolve("<SAMPLE>/workspace", exec.arguments?.file_path ?? '') === "<SAMPLE>/workspace/src/money.js")) return;
 if (changed) return;
 changed = true;
 writeFileSync("<SAMPLE>/workspace/src/money.js", '// EXTERNAL_CHANGE_KEEP\\n' + readFileSync("<SAMPLE>/workspace/src/money.js", 'utf8'));
 writeFileSync("<SAMPLE>/injection.json", JSON.stringify({ injected: true }));
 });
}
`
const PI_HOOK = `import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
export default function (pi) {
 let changed = false;
 pi.on('tool_result', (event) => {
 if (event.toolName !== 'read' || event.isError || !(resolve("<SAMPLE>/workspace", event.input?.path ?? '') === "<SAMPLE>/workspace/src/money.js")) return;
 if (changed) return;
 changed = true;
 writeFileSync("<SAMPLE>/workspace/src/money.js", '// EXTERNAL_CHANGE_KEEP\\n' + readFileSync("<SAMPLE>/workspace/src/money.js", 'utf8'));
 writeFileSync("<SAMPLE>/injection.json", JSON.stringify({ injected: true }));
 });
}
`

/** The overlay a Bake arm's sample launches with. */
function expectedPatch(arm: 'base' | 'cand', roster: 'headless' | 'tui', scenario: string, extra: Json[] = []): Json[] {
  return [
    ...COMPOSITION[arm][roster],
    { id: 'session-title-llm', disabled: true },
    ...scenario === 'instructions_file' ? [] : [{ id: 'agent-instructions', disabled: true }],
    { id: 'session-persistence-jsonl', config: { root: '<SAMPLE>/sessions', compression: 'none' } },
    ...scenario === 'stale_edit' ? [{ insert: [{ id: 'token-evaluation-external-writer', name: '<SAMPLE>/stale-writer.mjs' }] }] : [],
    ...extra,
  ]
}
/** The settings.yaml a Bake arm's sample reads: the gateway route narrowed to the model and pointed at the proxy. */
function expectedBakeSettings(effort: string, scenario: string, overrides: Json = {}): Json {
  const provider = {
    api: 'openai-responses', baseURL: '<PROXY>/upstream', apiKeyEnv: 'CLIPROXYAPI_API_KEY', compat: { keepThis: true },
    models: [{ id: 'fixture-model', name: 'Fixture Model', contextWindow: scenario === 'long_session' ? 16_000 : 64_000, reasoningEfforts: { low: 'minimal', high: 'deep' }, baseURL: '<PROXY>/upstream', maxTokens: 8192 }],
    retryPolicy: { mode: 'normal', maxRetries: 0 },
    ...overrides.provider,
  }
  return {
    'llm-pi-ai': { providers: { cliproxyapi: provider } },
    'agent-default-model': { provider: 'cliproxyapi', model: 'fixture-model', reasoningEffort: effort },
    ...overrides.sections,
    permission: { defaultPreset: 'danger-full-access' },
  }
}
function expectedPiArgs(effort: string, scenario: string): string[] {
  return [
    '--mode', 'json', '--no-session', '--no-extensions', '--no-skills', '--no-prompt-templates', '--no-themes',
    ...scenario === 'instructions_file' ? [] : ['--no-context-files'],
    '--model', 'eval/fixture-model', '--thinking', effort,
    ...scenario === 'stale_edit' ? ['-e', '<SAMPLE>/stale-writer.mjs'] : [],
    prompts[scenario]!,
  ]
}
function expectedPiModels(scenario: string): Json {
  return { providers: { eval: {
    baseUrl: '<PROXY>/upstream', api: 'openai-responses', apiKey: '$EVAL_API_KEY',
    models: [{
      id: 'fixture-model', name: 'Fixture Model', reasoning: true, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: scenario === 'long_session' ? 16_000 : 64_000, maxTokens: 8192,
      thinkingLevelMap: { off: null, minimal: null, low: 'minimal', medium: null, high: 'deep', xhigh: null, max: null },
    }],
  } } }
}
const expectedPiSettings = (scenario: string): Json => ({
  quietStartup: true, retry: { enabled: false },
  ...scenario === 'long_session' ? { compaction: { reserveTokens: 3200, keepRecentTokens: 2560 } } : {},
})

/** Every capture of one roster run, normalized and keyed by `<arm>.<scenario>`. */
function byLaunch(run: Awaited<ReturnType<typeof evaluate>>): Map<string, Capture> {
  const normalize = normalizer(run.root, run.realRoot)
  const launches = new Map<string, Capture>()
  for (const raw of run.captures) {
    const capture = normalize(raw)
    const key = `${armOf(capture)}.${scenarioOf(capture.args.at(-1))}`
    expect(launches.has(key)).toBe(false)
    launches.set(key, capture)
  }
  return launches
}

// The evaluator stops agents by process group and the fake pi CLI runs through its shebang; both are POSIX-only.
const posix = describe.skipIf(process.platform === 'win32')

function contract(roster: 'headless' | 'tui', setup: { models: string; effort: string; cases: string[]; order: string[]; env?: Record<string, string>; baseExtra?: Json[]; baseSettings?: Json }) {
  posix(`EVAL_ROSTER=${roster}`, () => {
    let run: Awaited<ReturnType<typeof evaluate>>
    let launches: Map<string, Capture>
    beforeAll(async () => {
      run = await evaluate({ roster, models: setup.models, cases: setup.cases, env: setup.env })
      launches = byLaunch(run)
    }, 30_000)

    test('the evaluator completes every sample, none successful, without a request', () => {
      expect({ code: run.code, signal: run.signal }).toEqual({ code: 0, signal: null })
      expect(JSON.parse(run.stdout.trim().split('\n').at(-1)!)).toEqual({ completed: setup.order.length, successful: 0, tokens: 0 })
      const results = JSON.parse(run.outputs['results.json']!) as Json[]
      expect(results.map(sample => sample.label)).toEqual(setup.order)
      for (const sample of results) {
        const pi = sample.variant === 'pi'
        expect(sample).toMatchObject({
          model: 'fixture-model', provider: pi ? 'eval' : 'cliproxyapi', agent: pi ? 'pi' : 'bake', api: 'openai-responses', effort: setup.effort, trial: 0,
          code: 0, signal: null, abortCause: null, success: false, requests: 0, usageComplete: false, systemPrompt: null, composition: null,
          injectedStale: false, roster: pi ? null : roster, contextWindow: sample.scenario === 'long_session' ? 16_000 : null,
        })
      }
      expect(launches.size).toBe(setup.order.length)
    })

    test('Bake arms launch the built CLI headless with their own overlay and route settings', () => {
      for (const arm of ['base', 'cand'] as const) {
        for (const scenario of setup.cases) {
          const capture = launches.get(`${arm}.${scenario}`)!
          expect(capture.args).toEqual(['headless', '--patch', '<SAMPLE>/evaluation.patch.json', '--json', prompts[scenario]!])
          expect(capture.cwd).toBe('<SAMPLE>/workspace')
          expect(capture.env).toEqual({
            NO_COLOR: '1', BAKE_HOME: '<SAMPLE>/home', DSH_HOME: '<SAMPLE>/home', BAKE_AGENTS_HOME: '<SAMPLE>/agents', DSH_AGENTS_HOME: '<SAMPLE>/agents',
            PI_CODING_AGENT_DIR: null, PI_OFFLINE: null, EVAL_API_KEY: null,
          })
          expect(capture.secretNames).toEqual([])
          const own = arm === 'base'
          expect(JSON.parse(capture.patch!)).toEqual(expectedPatch(arm, roster, scenario, own ? setup.baseExtra : []))
          expect(YAML.parse(proxied(capture.bakeSettings!))).toEqual(expectedBakeSettings(setup.effort, scenario, own ? setup.baseSettings : undefined))
          expect(capture.bakeCredentials).toBe(HOME_CREDENTIALS)
          if (scenario === 'stale_edit') expect(capture.hook).toBe(BAKE_HOOK)
        }
      }
    })

    test('the pi arm launches with its own agent directory, the dummy key, and no Bake overlay', () => {
      for (const scenario of setup.cases) {
        const capture = launches.get(`pi.${scenario}`)
        if (scenario.startsWith('delegation')) { expect(capture).toBeUndefined(); continue }
        expect(capture!.args).toEqual(expectedPiArgs(setup.effort, scenario))
        expect(capture!.cwd).toBe('<SAMPLE>/workspace')
        expect(capture!.env).toEqual({
          NO_COLOR: '1', BAKE_HOME: null, DSH_HOME: null, BAKE_AGENTS_HOME: null, DSH_AGENTS_HOME: null,
          PI_CODING_AGENT_DIR: '<SAMPLE>/pi-agent', PI_OFFLINE: '1', EVAL_API_KEY: CLIPROXY_KEY,
        })
        expect(capture!.secretNames).toEqual(['EVAL_API_KEY'])
        expect(capture!.patch).toBeNull()
        expect(capture!.bakeSettings).toBeNull()
        expect(JSON.parse(proxied(capture!.piModels!))).toEqual(expectedPiModels(scenario))
        expect(JSON.parse(capture!.piSettings!)).toEqual(expectedPiSettings(scenario))
        expect(capture!.extension).toBe(scenario === 'stale_edit' ? PI_HOOK : null)
      }
    })

    test('design.json records each arm as resolved, with per-arm extras on their own arm', () => {
      const design = JSON.parse(run.outputs['design.json']!)
      const at = (path: string) => join(run.root, path)
      expect(design.arms).toEqual({
        base: { kind: 'bake', root: at('base'), extra: setup.baseExtra ?? [], settings: setup.env?.EVAL_SETTINGS_BASE ?? '{}', llmDeepseek: false, composition: COMPOSITION.base[roster] },
        cand: { kind: 'bake', root: at('cand'), extra: [], settings: '{}', llmDeepseek: false, composition: COMPOSITION.cand[roster] },
        pi: { kind: 'pi', root: join(run.realRoot, 'pi'), bin: join(run.realRoot, 'pi/bin/pi'), version: '0.0.0-contract', extra: [], settings: '{}', llmDeepseek: false, composition: [] },
      })
      expect(design).toMatchObject({
        roster, models: ['fixture-model'], cases: setup.cases, trials: 1, gateway: null,
        routes: { 'fixture-model': { provider: 'cliproxyapi', api: 'openai-responses', effort: setup.effort } },
        contextWindows: { long_session: 16_000 }, piCompaction: { long_session: { reserveTokens: 3200, keepRecentTokens: 2560 } },
      })
      expect(design.revisions.pi).toEqual({ root: join(run.realRoot, 'pi'), agent: 'pi', version: '0.0.0-contract', commit: null, dirty: false })
    })

    test('no dummy credential reaches the raw output or the evaluator log', () => {
      const written = [run.stdout, run.stderr, ...Object.values(run.outputs)].join('\n')
      for (const secret of [CLIPROXY_KEY, AMBIENT_DEEPSEEK_KEY]) expect(written).not.toContain(secret)
      for (const capture of launches.values()) expect(JSON.stringify(capture)).not.toContain(AMBIENT_DEEPSEEK_KEY)
    })
  })
}

posix('a run past its deadline', () => {
  test.each(['hang observed', 'readiness failed', 'child lookup failed'])('is reaped with its hung agent when %s', async (reason) => {
    let owned: { root: string; evaluator: number; agent: number } | undefined
    // The deadline is the observed hang, not a delay: it fires once the first agent announces itself.
    const deadline: Deadline = async ({ root, evaluator }, signal) => {
      const hung = () => readdirSync(join(root, 'capture')).find(name => name.endsWith('.hung'))
      await until(() => hung() !== undefined, 'the recorder to hang', RUN_DEADLINE_MS, signal)
      owned = { root, evaluator, agent: Number.parseInt(hung()!) }
      if (reason === 'readiness failed') throw new Error('recorder readiness failed')
    }
    const inspect: typeof host = (command, args) => {
      if (reason === 'child lookup failed' && args.includes('ppid=')) throw new Error('child lookup failed')
      return host(command, args)
    }
    await expect(evaluate({ roster: 'headless', models: 'fixture-model', cases: ['ordinary_edit'], env: { EVAL_CONTRACT_HANG: '1' }, deadline, inspect }))
      .rejects.toThrow(reason === 'hang observed' ? 'run.ts did not finish before its deadline'
        : reason === 'readiness failed' ? 'recorder readiness failed' : 'child lookup failed')
    expect(owned).toBeDefined()
    const { root, evaluator, agent } = owned!
    expect({ evaluator: exists(evaluator), agent: exists(agent), group: exists(-agent), root: existsSync(root) })
      .toEqual({ evaluator: false, agent: false, group: false, root: false })
    expect((await host('ps', ['-A', '-o', 'args='])).stdout).not.toContain(root)
  }, 30_000)
})

contract('headless', {
  models: 'fixture-model', effort: 'medium',
  cases: ['ordinary_edit', 'stale_edit', 'instructions_file', 'long_session', 'delegation'],
  // Arm order rotates by scenario; pi has no delegation sample.
  order: [
    'ordinary_edit.0.base', 'ordinary_edit.0.cand', 'ordinary_edit.0.pi',
    'stale_edit.0.cand', 'stale_edit.0.pi', 'stale_edit.0.base',
    'instructions_file.0.pi', 'instructions_file.0.base', 'instructions_file.0.cand',
    'long_session.0.base', 'long_session.0.cand', 'long_session.0.pi',
    'delegation.0.cand', 'delegation.0.base',
  ].map(label => `fixture-model.${label}`),
})

contract('tui', {
  models: 'fixture-model@high', effort: 'high',
  cases: ['ordinary_edit', 'stale_edit', 'instructions_file', 'long_session'],
  order: [
    'ordinary_edit.0.base', 'ordinary_edit.0.cand', 'ordinary_edit.0.pi',
    'stale_edit.0.cand', 'stale_edit.0.pi', 'stale_edit.0.base',
    'instructions_file.0.pi', 'instructions_file.0.base', 'instructions_file.0.cand',
    'long_session.0.base', 'long_session.0.cand', 'long_session.0.pi',
  ].map(label => `fixture-model.${label}`),
  env: { EVAL_EXTRA_BASE: JSON.stringify(EXTRA_BASE), EVAL_SETTINGS_BASE: SETTINGS_BASE },
  baseExtra: EXTRA_BASE,
  baseSettings: { provider: { strictTools: true }, sections: { 'fixture-section': { model: 'fixture-model' } } },
})

describe('the stale writer', () => {
  const scratch: string[] = []
  afterAll(() => { for (const dir of scratch) rmSync(dir, { recursive: true, force: true }) })

  // The pinned sources are loaded and driven here, since the recorders never run them.
  for (const [kind, source] of [['Bake plugin', BAKE_HOOK], ['pi extension', PI_HOOK]] as const) {
    test(`as a ${kind}, prepends the kept comment once, after the first read of the target`, async () => {
      const dir = mkdtempSync(join(tmpdir(), 'bake-eval-writer-'))
      scratch.push(dir)
      write(join(dir, 'workspace/src/money.js'), 'original\n')
      write(join(dir, 'writer.mjs'), source.replaceAll('<SAMPLE>', dir))
      const handlers: Record<string, (...args: Json[]) => void> = {}
      const host = { on: (event: string, handler: (...args: Json[]) => void) => { handlers[event] = handler } }
      const module = await import(pathToFileURL(join(dir, 'writer.mjs')).href)
      const read = (path: string, isError = false) => kind === 'Bake plugin'
        ? handlers['tools/result']!({ name: 'read', arguments: { file_path: path } }, { isError })
        : handlers.tool_result!({ toolName: 'read', input: { path }, isError })
      if (kind === 'Bake plugin') { expect(module.name).toBe('token-evaluation-external-writer'); module.apply(host) } else module.default(host)
      read('test.cjs')
      read('src/money.js', true)
      expect(readFileSync(join(dir, 'workspace/src/money.js'), 'utf8')).toBe('original\n')
      read('src/money.js')
      read(join(dir, 'workspace/src/money.js'))
      expect(readFileSync(join(dir, 'workspace/src/money.js'), 'utf8')).toBe('// EXTERNAL_CHANGE_KEEP\noriginal\n')
      expect(JSON.parse(readFileSync(join(dir, 'injection.json'), 'utf8'))).toEqual({ injected: true })
    })
  }
})
