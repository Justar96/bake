/**
 * Runs the live evaluator's native arm keylessly: `run.ts` launches the built
 * debug `bake-rs` for two arms, through its capturing proxy, against a local
 * OpenAI Responses server configured as the `cliproxyapi` route of a private
 * Bake home. The server scripts a read, an edit sent with the check, and a
 * final answer, so each sample is a full print-mode turn with tool calls; the
 * script then checks the metrics the evaluator extracted against what the
 * server sent. Before it sends a `stale_edit` edit, the server checks that the
 * evaluator's injected comment is already in the file, so a late injection
 * fails the smoke. Two controls follow: a run whose read never names the
 * target, so nothing is injected and the samples must fail, and a run that
 * reads and edits in one response, which the evaluator must mark as a raced
 * injection rather than score. A last control puts an `AGENTS.md` above the
 * workspace, which the evaluator must refuse before any request. It measures
 * nothing about a real model.
 *
 * The live evaluator stops its agents through POSIX process groups, so this
 * check is skipped on Windows. The script owns its temporary root, the
 * server's port, and the evaluator processes. A sample's `bake-rs` belongs to
 * the evaluator; if the evaluator must be killed, stopping the server and the
 * evaluator's proxy ends any model request the agent still waits on, and
 * retries are off.
 */
import { deepStrictEqual } from 'node:assert'
import { spawn } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { prompts } from './scenarios.ts'

const root = resolve(import.meta.dir, '../..')
const binary = join(root, 'rust/target/debug', `bake-rs${process.platform === 'win32' ? '.exe' : ''}`)
const RUN = join(import.meta.dir, 'run.ts')
const MODEL = 'native-smoke-model'
const KEY = 'native-smoke-dummy-key'
const MARKER = 'EXTERNAL_CHANGE_KEEP'
/** Each evaluation has at most four samples of three requests, well inside the scenarios' own caps. */
const DEADLINE_MS = 120_000

/** Per-request usage the fake server reports; request `n` (1-based) of a sample reports these times `n`. */
const USAGE = { input: 100, cached: 40, output: 10 }

type Json = any
/**
 * How the server scripts a turn: `normal` reads the target, then edits it with
 * the check; `never` reads `test.cjs` instead, so no injection happens;
 * `raced` sends the read, edit, and check in one response.
 */
type Mode = 'normal' | 'never' | 'raced'

/** One Responses SSE stream: `response.created`, the items, and `response.completed` with usage. */
function stream(id: string, n: number, items: Json[]): string {
  const events: Json[] = [{ type: 'response.created', response: { id, status: 'in_progress' } }]
  items.forEach((item, index) => {
    if (item.type === 'function_call') {
      events.push({ type: 'response.output_item.added', output_index: index, item: { ...item, arguments: '' } })
      events.push({ type: 'response.function_call_arguments.delta', output_index: index, item_id: item.id, delta: item.arguments })
    } else {
      events.push({ type: 'response.output_item.added', output_index: index, item: { ...item, content: [] } })
      events.push({ type: 'response.output_text.delta', output_index: index, item_id: item.id, content_index: 0, delta: item.content[0].text })
    }
    events.push({ type: 'response.output_item.done', output_index: index, item })
  })
  const input = USAGE.input * n
  const output = USAGE.output * n
  events.push({ type: 'response.completed', response: { id, status: 'completed', usage: {
    input_tokens: input, output_tokens: output, total_tokens: input + output, input_tokens_details: { cached_tokens: USAGE.cached * n },
  } } })
  return events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')
}

const call = (id: string, name: string, args: Json): Json => ({ type: 'function_call', id: `fc_${id}`, call_id: `call_${id}`, name, arguments: JSON.stringify(args), status: 'completed' })
const READ = call('read', 'read', { path: 'src/money.js' })
const EDIT = call('edit', 'edit', { path: 'src/money.js', edits: [{ oldText: 'Math.floor(value * 100) / 100', newText: 'Math.round(value * 100) / 100' }] })
const CHECK = call('check', 'bash', { command: 'node test.cjs' })
const FINAL_TEXT = 'Fixed roundMoney; node test.cjs passes.'
const FINAL = { type: 'message', id: 'msg_final', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: FINAL_TEXT, annotations: [] }] }

/** The scripted turn, chosen from the request alone by how many tool outputs it carries. */
function reply(body: Json, mode: Mode): { text: string; sendsEdit: boolean } {
  const outputs = (body.input as Json[]).filter(item => item.type === 'function_call_output').length
  if (mode === 'raced') return outputs === 0 ? { text: stream('resp_all', 1, [READ, EDIT, CHECK]), sendsEdit: true } : { text: stream('resp_final', 2, [FINAL]), sendsEdit: false }
  if (outputs === 0) return { text: stream('resp_read', 1, [mode === 'never' ? call('read', 'read', { path: 'test.cjs' }) : READ]), sendsEdit: false }
  if (outputs === 1) return { text: stream('resp_edit', 2, [EDIT, CHECK]), sendsEdit: true }
  return { text: stream('resp_final', 3, [FINAL]), sendsEdit: false }
}

const settings = (port: number) => `llm-pi-ai:
  providers:
    cliproxyapi:
      displayName: CLIProxyAPI
      apiKeyEnv: CLIPROXYAPI_API_KEY
      api: openai-responses
      baseURL: http://127.0.0.1:${port}/v1
      models:
        - id: ${MODEL}
          name: Native Smoke Model
          contextWindow: 64000
          maxTokens: 4096
          reasoningEfforts: {low: low, medium: medium, high: high}
agent-default-model: {provider: cliproxyapi, model: ${MODEL}, reasoningEffort: medium}
`

/** The usage a sample of `requests` scripted responses reports in total. */
function usageOf(requests: number) {
  const usage = { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, totalTokens: 0 }
  for (let n = 1; n <= requests; n++) {
    usage.inputTokens += (USAGE.input - USAGE.cached) * n
    usage.outputTokens += USAGE.output * n
    usage.cacheReadTokens += USAGE.cached * n
    usage.totalTokens += (USAGE.input + USAGE.output) * n
  }
  return usage
}

/** Exercise the evaluator's native arm; throws if a metric disagrees with what the server sent. */
async function smokeNativeArm(): Promise<void> {
  const temp = mkdtempSync(join(tmpdir(), 'bake-native-arm-smoke-'))
  const samplesDir = join(temp, 'tmp')
  let mode: Mode = 'normal'
  const received: { path: string; authorization: string | null; model: unknown }[] = []
  /** Whether each `stale_edit` edit the server sent found the injected comment already in place. */
  const injectedBeforeEdit: boolean[] = []
  const server = Bun.serve({ port: 0, hostname: '127.0.0.1',
    async fetch(request) {
      const body = await request.json() as Json
      received.push({ path: new URL(request.url).pathname, authorization: request.headers.get('authorization'), model: body.model })
      const { text, sendsEdit } = reply(body, mode)
      if (sendsEdit && JSON.stringify(body.input).includes(JSON.stringify(prompts.stale_edit).slice(1, 40))) {
        // Samples run one at a time, so the evaluator's sample root holds exactly one workspace.
        const targets = readdirSync(samplesDir).filter(name => name.startsWith('bake-token-eval-'))
          .map(name => join(samplesDir, name, 'workspace/src/money.js')).filter(path => existsSync(path))
        if (targets.length !== 1) throw new Error(`expected one sample workspace, found ${targets.length}`)
        injectedBeforeEdit.push(readFileSync(targets[0]!, 'utf8').includes(MARKER))
      }
      return new Response(text, { headers: { 'content-type': 'text/event-stream' } })
    },
  })
  try {
    const home = join(temp, 'home')
    mkdirSync(join(home, '.bake'), { recursive: true })
    writeFileSync(join(home, '.bake/settings.yaml'), settings(server.port!))
    writeFileSync(join(home, '.bake/.credentials.yaml'), `version: 1\nrefs:\n  CLIPROXYAPI_API_KEY: ${KEY}\n`, { mode: 0o600 })
    mkdirSync(samplesDir)

    /** Run the evaluator once in `mode`; resolves with its exit code and output. */
    const launch = async (next: Mode, cases: string[], out: string) => {
      mode = next
      // A built environment: no ambient key or home reaches the evaluator or its agents.
      const env = {
        PATH: process.env.PATH ?? '', HOME: home, TMPDIR: samplesDir,
        EVAL_ARMS: `base=native:${binary},candidate=native:${binary}`, EVAL_MODELS: MODEL, EVAL_CASES: cases.join(','),
        EVAL_TRIALS: '1', EVAL_OUTPUT: out,
      }
      const child = spawn(process.execPath, [RUN], { cwd: temp, env, stdio: ['ignore', 'pipe', 'pipe'] })
      let stdout = ''
      let stderr = ''
      child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk })
      child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk })
      const timer = setTimeout(() => child.kill('SIGKILL'), DEADLINE_MS)
      const code = await new Promise<number | null>((resolveClose, reject) => {
        child.once('error', reject)
        child.once('close', resolveClose)
      }).finally(() => clearTimeout(timer))
      return { code, stdout, stderr }
    }
    /** One evaluator run that must succeed; returns its raw outputs, after checking that none holds the key. */
    const evaluate = async (next: Mode, cases: string[]): Promise<Record<string, string>> => {
      const out = join(temp, `out-${next}`)
      const { code, stdout, stderr } = await launch(next, cases, out)
      if (code !== 0) throw new Error(`run.ts (${next}) exited ${code}\n${stdout}\n${stderr}`)
      const outputs = Object.fromEntries(readdirSync(out).map(name => [name, readFileSync(join(out, name), 'utf8')]))
      for (const [name, text] of Object.entries(outputs)) if (text.includes(KEY)) throw new Error(`${name} holds the route key`)
      return outputs
    }
    const sampleOf = (outputs: Record<string, string>, scenario: string, arm: string) => {
      const label = `${MODEL}.${scenario}.0.${arm}`
      const sample = JSON.parse(outputs[`${label}.json`] ?? 'null')
      if (sample === null) throw new Error(`${label}: no sample`)
      return { label, sample }
    }
    const check = (label: string, sample: Json, observed: Json, expected: Json) => {
      try { deepStrictEqual(observed, expected) } catch (error) {
        throw new Error(`${label}: ${(error as Error).message}\nstderr: ${sample.stderr}`, { cause: error })
      }
      console.log(`pass native arm ${label}`)
    }

    const outputs = await evaluate('normal', ['ordinary_edit', 'stale_edit'])
    const design = JSON.parse(outputs['design.json']!)
    for (const arm of ['base', 'candidate']) {
      const revision = design.revisions[arm]
      // A binary passed by path is prebuilt: it has a digest and version but no commit.
      if (revision.agent !== 'native' || revision.prebuilt !== true || revision.commit !== null
        || revision.binarySha256?.length !== 64 || !/^bake-rs /.test(revision.version)) {
        throw new Error(`design.json revision for ${arm}: ${JSON.stringify(revision)}`)
      }
    }
    deepStrictEqual(received.map(request => [request.path, request.authorization, request.model]),
      Array.from({ length: 2 * 2 * 3 }, () => ['/v1/responses', `Bearer ${KEY}`, MODEL]), 'requests the server received')
    deepStrictEqual(injectedBeforeEdit, [true, true], 'the stale comment was in place before each stale_edit edit was sent')
    for (const scenario of ['ordinary_edit', 'stale_edit']) {
      for (const arm of ['base', 'candidate']) {
        const { label, sample } = sampleOf(outputs, scenario, arm)
        check(label, sample, {
          agent: sample.agent, provider: sample.provider, api: sample.api, effort: sample.effort, success: sample.success, code: sample.code,
          invalid: sample.invalid, evaluatorErrors: sample.evaluatorErrors, staleInjection: sample.staleInjection,
          requests: sample.requests, steps: sample.steps, toolCalls: sample.toolCalls, byTool: sample.byTool, toolErrors: sample.toolErrors,
          usage: sample.usage, usageComplete: sample.usageComplete, accountingMatches: sample.accounting.matches,
          excessRequests: sample.excessRequests, editCheckSplits: sample.editCheckSplits, orientationCalls: sample.orientationCalls,
          ranCheck: sample.ranCheck, verifiedBeforeFinal: sample.verifiedBeforeFinal, backgroundStarts: sample.backgroundStarts,
          compactions: sample.compactions, composition: sample.composition, roster: sample.roster, routingDecisions: sample.routingDecisions,
          injectedStale: sample.injectedStale, final: sample.final, hasSystemPrompt: typeof sample.systemPrompt === 'string' && sample.systemPrompt.length > 0,
        }, {
          agent: 'native', provider: 'cliproxyapi', api: 'openai-responses', effort: 'medium', success: true, code: 0,
          invalid: null, evaluatorErrors: [], staleInjection: scenario === 'stale_edit' ? { injected: true, raced: false } : null,
          requests: 3, steps: 3, toolCalls: 3, byTool: { read: 1, edit: 1, bash: 1 }, toolErrors: 0,
          usage: usageOf(3), usageComplete: true, accountingMatches: true,
          excessRequests: 0, editCheckSplits: 0, orientationCalls: 0,
          ranCheck: true, verifiedBeforeFinal: true, backgroundStarts: 0,
          compactions: { summaries: 0, errors: 0 }, composition: null, roster: null, routingDecisions: [],
          injectedStale: scenario === 'stale_edit', final: FINAL_TEXT, hasSystemPrompt: true,
        })
      }
    }

    // Control: with no injection the edit finds no comment, and the evaluator must fail the sample as the agent's.
    injectedBeforeEdit.length = 0
    const never = await evaluate('never', ['stale_edit'])
    deepStrictEqual(injectedBeforeEdit, [false, false], 'the never control sent its edits with no comment in place')
    for (const arm of ['base', 'candidate']) {
      const { label, sample } = sampleOf(never, 'stale_edit', arm)
      check(`${label} (no injection)`, sample,
        { success: sample.success, code: sample.code, invalid: sample.invalid, injectedStale: sample.injectedStale, staleInjection: sample.staleInjection, testExit: sample.testExit },
        { success: false, code: 0, invalid: null, injectedStale: false, staleInjection: { injected: false, raced: false }, testExit: 0 })
    }

    // Control: a read and edit in one response race the evaluator's write, so the sample is invalid rather than scored.
    const raced = await evaluate('raced', ['stale_edit'])
    for (const arm of ['base', 'candidate']) {
      const { label, sample } = sampleOf(raced, 'stale_edit', arm)
      check(`${label} (raced injection)`, sample,
        { success: sample.success, invalid: sample.invalid, staleInjection: sample.staleInjection, requests: sample.requests },
        { success: false, invalid: 'stale_injection_raced', staleInjection: { injected: true, raced: true }, requests: 2 })
    }

    // Control: an AGENTS.md above the workspace would reach bake-rs's prompt, so the evaluator must refuse before launching it.
    // The evaluator names the real path, which differs where the temporary directory is a symbolic link (macOS).
    const stray = join(realpathSync(samplesDir), 'AGENTS.md')
    writeFileSync(stray, 'stray instructions\n')
    const before = received.length
    const refused = await launch('normal', ['ordinary_edit'], join(temp, 'out-stray'))
    rmSync(stray)
    if (refused.code === 0 || !refused.stderr.includes(`context files outside the fixture: ${stray}`) || received.length !== before) {
      throw new Error(`a stray context file was not refused: exit ${refused.code}, ${received.length - before} requests\n${refused.stderr}`)
    }
    console.log('pass native arm stray context file refused')
    console.log(JSON.stringify({ schema: 'bake/native-arm-smoke', version: 2, platform: process.platform, arch: process.arch,
      samples: 8, requests: received.length, passed: true }))
  } finally {
    await server.stop(true)
    rmSync(temp, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
  }
}

if (import.meta.main && process.platform === 'win32') console.log('skip native arm smoke: the live evaluator needs POSIX process groups')
else if (import.meta.main) {
  try { await smokeNativeArm() } catch (error) {
    console.error(`native arm smoke failed: ${(error as Error).message}`)
    process.exitCode = 1
  }
}
