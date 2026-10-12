/**
 * Runs the live evaluator's native arm keylessly: `run.ts` launches the built
 * debug `bake-rs` for two arms, through its capturing proxy, against a local
 * OpenAI Responses server configured as the `cliproxyapi` route of a private
 * Bake home. The server scripts a read, an edit sent with the check, and a
 * final answer, so each sample is a full print-mode turn with tool calls; the
 * script then checks the metrics the evaluator extracted against what the
 * server sent. It measures nothing about a real model.
 *
 * The live evaluator stops its agents through POSIX process groups, so this
 * check is skipped on Windows. The script owns its temporary root, the
 * server's port, and the evaluator process. A sample's `bake-rs` belongs to
 * the evaluator; if the evaluator
 * must be killed, stopping the server and the evaluator's proxy ends any
 * model request the agent still waits on, and retries are off.
 */
import { deepStrictEqual } from 'node:assert'
import { spawn } from 'node:child_process'
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const root = resolve(import.meta.dir, '../..')
const binary = join(root, 'rust/target/debug', `bake-rs${process.platform === 'win32' ? '.exe' : ''}`)
const RUN = join(import.meta.dir, 'run.ts')
const MODEL = 'native-smoke-model'
const KEY = 'native-smoke-dummy-key'
const CASES = ['ordinary_edit', 'stale_edit'] as const
/** Two samples of three requests each, well inside the scenarios' own caps. */
const DEADLINE_MS = 120_000

/** Per-request usage the fake server reports; request `n` (1-based) reports these times `n`. */
const USAGE = { input: 100, cached: 40, output: 10 }

type Json = any

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

/**
 * The scripted turn, chosen from the request alone: no tool output yet asks for
 * a read; the read's output asks for the edit with the check in the same
 * response; the edit's and check's outputs get the final answer.
 */
function reply(body: Json): string {
  const outputs = (body.input as Json[]).filter(item => item.type === 'function_call_output').length
  if (outputs === 0) return stream('resp_read', 1, [call('read', 'read', { path: 'src/money.js' })])
  if (outputs === 1) {
    return stream('resp_edit', 2, [
      call('edit', 'edit', { path: 'src/money.js', edits: [{ oldText: 'Math.floor(value * 100) / 100', newText: 'Math.round(value * 100) / 100' }] }),
      call('check', 'bash', { command: 'node test.cjs' }),
    ])
  }
  return stream('resp_final', 3, [{ type: 'message', id: 'msg_final', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: 'Fixed roundMoney; node test.cjs passes.', annotations: [] }] }])
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

/** Exercise the evaluator's native arm; throws if a metric disagrees with what the server sent. */
async function smokeNativeArm(): Promise<void> {
  const temp = mkdtempSync(join(tmpdir(), 'bake-native-arm-smoke-'))
  const received: { path: string; authorization: string | null; model: unknown }[] = []
  const server = Bun.serve({ port: 0, hostname: '127.0.0.1',
    async fetch(request) {
      const body = await request.json() as Json
      received.push({ path: new URL(request.url).pathname, authorization: request.headers.get('authorization'), model: body.model })
      return new Response(reply(body), { headers: { 'content-type': 'text/event-stream' } })
    },
  })
  try {
    const home = join(temp, 'home')
    mkdirSync(join(home, '.bake'), { recursive: true })
    writeFileSync(join(home, '.bake/settings.yaml'), settings(server.port!))
    writeFileSync(join(home, '.bake/.credentials.yaml'), `version: 1\nrefs:\n  CLIPROXYAPI_API_KEY: ${KEY}\n`, { mode: 0o600 })
    for (const dir of ['tmp', 'out']) mkdirSync(join(temp, dir))
    const out = join(temp, 'out')
    // A built environment: no ambient key or home reaches the evaluator or its agents.
    const env = {
      PATH: process.env.PATH ?? '', HOME: home, TMPDIR: join(temp, 'tmp'),
      EVAL_ARMS: `base=native:${binary},candidate=native:${binary}`, EVAL_MODELS: MODEL, EVAL_CASES: CASES.join(','),
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
    if (code !== 0) throw new Error(`run.ts exited ${code}\n${stdout}\n${stderr}`)

    const outputs = Object.fromEntries(readdirSync(out).map(name => [name, readFileSync(join(out, name), 'utf8')]))
    for (const [name, text] of Object.entries(outputs)) if (text.includes(KEY)) throw new Error(`${name} holds the route key`)
    const design = JSON.parse(outputs['design.json']!)
    for (const arm of ['base', 'candidate']) {
      const revision = design.revisions[arm]
      if (revision.agent !== 'native' || revision.binarySha256?.length !== 64 || !/^bake-rs /.test(revision.version)) {
        throw new Error(`design.json revision for ${arm}: ${JSON.stringify(revision)}`)
      }
    }
    deepStrictEqual(received.map(request => [request.path, request.authorization, request.model]),
      Array.from({ length: CASES.length * 2 * 3 }, () => ['/v1/responses', `Bearer ${KEY}`, MODEL]), 'requests the server received')

    // Request n reports n times the base usage; uncached input excludes the cached part.
    const usage = { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, totalTokens: 0 }
    for (const n of [1, 2, 3]) {
      usage.inputTokens += (USAGE.input - USAGE.cached) * n
      usage.outputTokens += USAGE.output * n
      usage.cacheReadTokens += USAGE.cached * n
      usage.totalTokens += (USAGE.input + USAGE.output) * n
    }
    for (const scenario of CASES) {
      for (const arm of ['base', 'candidate']) {
        const label = `${MODEL}.${scenario}.0.${arm}`
        const sample = JSON.parse(outputs[`${label}.json`] ?? 'null')
        if (sample === null) throw new Error(`${label}: no sample`)
        const observed = {
          agent: sample.agent, provider: sample.provider, api: sample.api, effort: sample.effort, success: sample.success, code: sample.code,
          requests: sample.requests, steps: sample.steps, toolCalls: sample.toolCalls, byTool: sample.byTool, toolErrors: sample.toolErrors,
          usage: sample.usage, usageComplete: sample.usageComplete, accountingMatches: sample.accounting.matches,
          excessRequests: sample.excessRequests, editCheckSplits: sample.editCheckSplits, orientationCalls: sample.orientationCalls,
          ranCheck: sample.ranCheck, verifiedBeforeFinal: sample.verifiedBeforeFinal, backgroundStarts: sample.backgroundStarts,
          compactions: sample.compactions, composition: sample.composition, roster: sample.roster, routingDecisions: sample.routingDecisions,
          injectedStale: sample.injectedStale, final: sample.final, hasSystemPrompt: typeof sample.systemPrompt === 'string' && sample.systemPrompt.length > 0,
        }
        try {
          deepStrictEqual(observed, {
            agent: 'native', provider: 'cliproxyapi', api: 'openai-responses', effort: 'medium', success: true, code: 0,
            requests: 3, steps: 3, toolCalls: 3, byTool: { read: 1, edit: 1, bash: 1 }, toolErrors: 0,
            usage, usageComplete: true, accountingMatches: true,
            excessRequests: 0, editCheckSplits: 0, orientationCalls: 0,
            ranCheck: true, verifiedBeforeFinal: true, backgroundStarts: 0,
            compactions: { summaries: 0, errors: 0 }, composition: null, roster: null, routingDecisions: [],
            injectedStale: scenario === 'stale_edit', final: 'Fixed roundMoney; node test.cjs passes.', hasSystemPrompt: true,
          })
        } catch (error) {
          throw new Error(`${label}: ${(error as Error).message}\nstderr: ${sample.stderr}`, { cause: error })
        }
        console.log(`pass native arm ${label}`)
      }
    }
    console.log(JSON.stringify({ schema: 'bake/native-arm-smoke', version: 1, platform: process.platform, arch: process.arch,
      samples: CASES.length * 2, requests: received.length, passed: true }))
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
