/**
 * Paired live evaluation of the agent loop: runs the built headless CLI of two
 * or more checkouts on the same scenarios, interleaved, and records what each
 * sample cost. See evals/README.md for the procedure and the regression rule.
 *
 * EVAL_ARMS    name=checkout pairs, comma-separated; each checkout must be built.
 *              name=pi, or name=pi:<bin>, runs the installed pi coding agent
 *              instead, with its own prompt, tools, and DeepSeek route
 * EVAL_MODELS  model ids or set names (default: standard and extended sets); a
 *              cliproxyapi id from ~/.bake/settings.yaml, or a DeepSeek id
 *              (deepseek/<id>, or a bare deepseek-* id the gateway lacks);
 *              a trailing @<effort> replaces the default effort
 * EVAL_CASES   scenario names or set names, `standard` or `extended` (default: the standard suite)
 * EVAL_ROSTER  `headless` (default) runs the headless bundle's tool configs; `tui`
 *              applies the terminal standard preset's to every Bake arm
 * EVAL_TRIALS  trials per scenario (default 3)
 * EVAL_OUTPUT  raw output directory (default .preflight/evals/agent-loop/<time>)
 * EVAL_EXTRA_<ARM>  optional JSON array of extra overlay rows for one arm
 * EVAL_GATEWAY optional JSON file of { baseUrl, apiKey } that replaces the
 *              cliproxyapi upstream and key for every arm, such as pi's
 *              ~/.pi/agent/cliproxyapi.json; DeepSeek routes are unaffected
 * EVAL_SETTINGS_<ARM>  optional JSON object deep-merged into one arm's settings.yaml;
 *              the strings $PROVIDER and $MODEL become the route under test
 */
import { spawn } from 'node:child_process'
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, writeFileSync, copyFileSync, rmSync, existsSync, realpathSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { tmpdir } from 'node:os'
import { createHash } from 'node:crypto'
import YAML from 'yaml'
import { reconcile } from './accounting.ts'
import { compositionCheck, ROSTERS, rosterOverlay, systemPromptOf, systemPromptOverlay, type Roster } from './composition.ts'
import {
  backgroundStarts, compactions, editCheckSplits, excessRequests, normalizeBake, normalizePi, orientationCalls, ranCheck,
  requestsOverFloor, routingDecisions, runawayAbort, shellEdits, verifiedBeforeFinal,
} from './metrics.ts'
import {
  CASE_SETS, capMsFor, checkCommandFor, CONTEXT_WINDOWS, fixture, maxRequestsFor, piCompactionFor, prompts, REQUEST_FLOORS, requestCaps,
  requestFloorFor, STANDARD_CASES, usesAgentInstructions, validate, wallClockCaps,
} from './scenarios.ts'
import { DEEPSEEK_ANTHROPIC_BASE_URL, deepseekProfile } from '../../packages/llm/llm-pi-ai/tests/deepseek-profile.ts'
import { parseCredentialsDocument } from '../../packages/credentials/credentials-local/src/index.ts'

const out = resolve(process.env.EVAL_OUTPUT ?? join('.preflight/evals/agent-loop', new Date().toISOString().replace(/[:.]/g, '-')))
mkdirSync(out, { recursive: true })
/**
 * Whether a checkout's base bundle still mounts the retired `llm-deepseek`
 * adapter, which owns `deepseek-official` there. Such an arm configures the
 * route through that adapter's settings section, since an `llm-pi-ai` route of
 * the same id would collide with it.
 */
function shipsLlmDeepseek(root: string): boolean {
  const patch = join(root, 'packages/bundle/base/cordis.patch.yml')
  return existsSync(patch) && /name:\s*['"]?@deepseek-ai\/dsh-llm-deepseek['"]?\s*$/m.test(readFileSync(patch, 'utf8'))
}
/**
 * One arm: a built Bake checkout, or the pi coding agent. A pi arm's `root` is
 * its package directory and `bin` the CLI it runs; overlays and settings are
 * Bake's and do not apply to it.
 */
/** Deep-merges an arm's settings into the route's, so one route key can change without replacing the route; arrays replace. */
function merge(base: Record<string, unknown>, extra: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = { ...base }
  for (const [key, value] of Object.entries(extra)) {
    const prior = out[key]
    const plain = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)
    out[key] = plain(prior) && plain(value) ? merge(prior, value) : value
  }
  return out
}

/** `composition` holds a Bake arm's overlay rows read from its own checkout: its system prompt and the roster's tool configs. */
interface Arm { kind: 'bake' | 'pi'; root: string; bin?: string; version?: string; extra: unknown[]; settings: string; llmDeepseek: boolean; composition: unknown[] }
/** Resolve `pi` or `pi:<bin>` to the CLI and the package directory that ships it. */
function piArm(spec: string): Arm {
  const named = spec.slice('pi'.length).replace(/^:/, '') || Bun.which('pi')
  if (!named) throw new Error('EVAL_ARMS: pi is not on PATH; pass name=pi:<path to the pi CLI>')
  const bin = realpathSync(named)
  let root = dirname(bin)
  while (!existsSync(join(root, 'package.json')) && dirname(root) !== root) root = dirname(root)
  const version = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version as string
  return { kind: 'pi', root, bin, version, extra: [], settings: '{}', llmDeepseek: false, composition: [] }
}
const roster = (process.env.EVAL_ROSTER ?? 'headless') as Roster
if (!ROSTERS.includes(roster)) throw new Error(`EVAL_ROSTER must be one of ${ROSTERS.join(', ')}`)
const ARMS: Record<string, Arm> = Object.fromEntries((process.env.EVAL_ARMS ?? '').split(',').filter(Boolean).map(entry => {
  const [name, root] = entry.split('=')
  if (!name || !root) throw new Error(`EVAL_ARMS entry "${entry}" is not name=checkout`)
  if (root === 'pi' || root.startsWith('pi:')) return [name, piArm(root)]
  if (!existsSync(join(root, 'apps/cli/lib/bin.js'))) throw new Error(`arm ${name}: ${root} has no built apps/cli/lib/bin.js; run bun run build there`)
  const settings = process.env[`EVAL_SETTINGS_${name.toUpperCase()}`] ?? '{}'
  JSON.parse(settings)
  return [name, {
    kind: 'bake', root: resolve(root), extra: JSON.parse(process.env[`EVAL_EXTRA_${name.toUpperCase()}`] ?? '[]'), settings, llmDeepseek: shipsLlmDeepseek(root),
    composition: [systemPromptOverlay(root), ...rosterOverlay(root, roster)],
  }]
}))
const armNames = Object.keys(ARMS)
if (armNames.length < 2) throw new Error('EVAL_ARMS needs at least two name=checkout pairs, for example base=../bake-v0.2.0,candidate=.')
/** Named model sets; EVAL_MODELS may list set names, model ids, or both. */
const MODEL_SETS: Record<string, string[]> = {
  standard: ['gemini-3.8-flash-medium', 'gpt-6.1-sol', 'claude-sonnet-5-5'],
  extended: ['claude-opus-5-5', 'deepseek/deepseek-flash', 'deepseek/deepseek-v4-pro'],
}
const models = [...new Set((process.env.EVAL_MODELS ?? 'standard,extended').split(',').filter(Boolean)
  .flatMap(entry => MODEL_SETS[entry] ?? [entry]))]
const cases = [...new Set((process.env.EVAL_CASES ?? STANDARD_CASES.join(',')).split(',').filter(Boolean)
  .flatMap(entry => CASE_SETS[entry] ?? [entry]))]
for (const scenario of cases) if (prompts[scenario] === undefined) throw new Error(`EVAL_CASES names unknown scenario ${scenario}`)
const trials = Number(process.env.EVAL_TRIALS ?? 3)
const settings = YAML.parse(readFileSync(join(process.env.HOME!, '.bake/settings.yaml'), 'utf8'))
const original = settings['llm-pi-ai'].providers.cliproxyapi
const credentialFile = join(process.env.HOME!, '.bake/.credentials.yaml')
/**
 * An optional replacement gateway for cliproxyapi models. The proxy swaps each
 * arm's credential for this one on the way out, so both arms reach the same
 * upstream over the same wire with the same account.
 */
const override = process.env.EVAL_GATEWAY === undefined ? undefined
  : JSON.parse(readFileSync(process.env.EVAL_GATEWAY, 'utf8')) as { baseUrl: string; apiKey: string }
const gatewayBase = (api: string, configured: string) => override === undefined ? configured.replace(/\/$/, '')
  : new URL(override.baseUrl).origin + (api === 'anthropic-messages' ? '' : '/v1')

/**
 * One model's route through the capturing proxy: the provider the agent selects,
 * the wire format whose usage the proxy reads, the upstream it forwards to, and
 * the settings that point the provider at the proxy.
 */
interface Route {
  /** Bare model id, used in labels and records. */
  model: string
  provider: string
  api: string
  upstreamBase: string
  /** The requested effort; DeepSeek has no `medium`, so it runs at its `high` default. */
  effort: string
  /** Settings for one arm; `llmDeepseek` is whether that arm's build still ships the retired DeepSeek adapter. */
  settings(proxyBase: string, llmDeepseek: boolean): Record<string, unknown>
  /** The same model for a pi arm: its wire, upstream, credential, and `models.json` model entry. */
  pi(arm: Arm): PiRoute
}
interface PiRoute { api: string; upstreamBase: string; keyName: string; model: Record<string, unknown> }

/** Every pi thinking level, so a model entry maps the levels it lacks to null. */
const PI_LEVELS = ['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']
const piLevels = (efforts: Record<string, string | null> | undefined) =>
  Object.fromEntries(PI_LEVELS.map(level => [level, efforts?.[level] ?? null]))
/** A pi `models.json` model; the 8192-token output cap matches the Bake arms'. */
const piModel = (id: string, fields: Record<string, any>) => ({
  id, name: fields.name ?? id, reasoning: true, input: fields.input ?? ['text'],
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: fields.contextWindow ?? 200_000, maxTokens: 8192,
  thinkingLevelMap: fields.thinkingLevelMap ?? piLevels(fields.reasoningEfforts), ...fields.compat ? { compat: fields.compat } : {},
})
/** pi's own DeepSeek model, from the catalog the arm's pi-ai ships, so the pi arm keeps pi's wire and compat settings. */
function piDeepseek(arm: Arm, id: string): PiRoute {
  const candidates = [join(arm.root, 'node_modules/@earendil-works/pi-ai'), join(arm.root, '../pi-ai')]
  const catalog = candidates.map(dir => join(dir, 'dist/providers/data/deepseek.json')).find(existsSync)
  if (catalog === undefined) throw new Error(`pi arm at ${arm.root}: no pi-ai DeepSeek catalog`)
  const entry = Object.values(JSON.parse(readFileSync(catalog, 'utf8')) as Record<string, Record<string, any>>)
    .flatMap(byId => Object.values(byId)).find(model => model.id === id)
  if (entry === undefined) throw new Error(`pi's DeepSeek catalog has no ${id}`)
  return { api: entry.api, upstreamBase: String(entry.baseUrl).replace(/\/$/, ''), keyName: 'DEEPSEEK_API_KEY', model: piModel(id, entry) }
}

/** Resolve an EVAL_MODELS entry to its route; a gateway listing wins for a bare id. */
function resolveRoute(spec: string): Route {
  const [entry, effortOverride] = spec.split('@') as [string, string | undefined]
  const route = routeOf(entry)
  return effortOverride === undefined ? route : withEffort(route, effortOverride)
}
/** Replace a route's effort in both arms' configuration. */
function withEffort(route: Route, effort: string): Route {
  return {
    ...route, effort,
    settings: (proxyBase, llmDeepseek) => {
      const settings = route.settings(proxyBase, llmDeepseek)
      return { ...settings, 'agent-default-model': { ...settings['agent-default-model'] as object, reasoningEffort: effort } }
    },
  }
}
function routeOf(entry: string): Route {
  const [prefix, rest] = entry.includes('/') ? [entry.slice(0, entry.indexOf('/')), entry.slice(entry.indexOf('/') + 1)] : [undefined, entry]
  const gateway = original.models.find((m: any) => m.id === rest)
  if (prefix === 'deepseek' || (prefix === undefined && gateway === undefined && rest.startsWith('deepseek-'))) {
    // The shipped llm-pi-ai deepseek-official route over Anthropic-format
    // Messages, narrowed to the model under test. A settings `models` list
    // replaces the shipped one, so the entry repeats the shipped model with
    // only its output cap lowered. An arm built before that route existed
    // still owns deepseek-official through llm-deepseek and gets its section.
    const shipped = deepseekProfile().models?.find(m => m.id === rest)
    if (shipped === undefined) throw new Error(`EVAL_MODELS entry "${entry}": deepseek-official ships no model ${rest}`)
    return {
      model: rest, provider: 'deepseek-official', api: 'anthropic-messages', upstreamBase: DEEPSEEK_ANTHROPIC_BASE_URL, effort: 'high',
      settings: (proxyBase, llmDeepseek) => ({
        ...llmDeepseek
          ? { 'llm-deepseek': { baseURL: proxyBase, maxTokens: 8192, retryPolicy: { mode: 'normal', maxRetries: 0 } } }
          : { 'llm-pi-ai': { providers: { 'deepseek-official': {
            baseURL: proxyBase, models: [{ ...shipped, maxTokens: 8192 }], retryPolicy: { mode: 'normal', maxRetries: 0 },
          } } } },
        'agent-default-model': { provider: 'deepseek-official', model: rest, reasoningEffort: 'high' },
      }),
      pi: arm => piDeepseek(arm, rest),
    }
  }
  if (prefix !== undefined && prefix !== 'cliproxyapi') throw new Error(`EVAL_MODELS entry "${entry}": unknown provider ${prefix}; use cliproxyapi/<id> or deepseek/<id>`)
  if (gateway === undefined) throw new Error(`Missing configured model ${rest}`)
  const api = gateway.api ?? original.api
  return {
    model: rest, provider: 'cliproxyapi', api, upstreamBase: gatewayBase(api, gateway.baseURL ?? original.baseURL), effort: 'medium',
    settings: (proxyBase) => {
      const route = structuredClone(original)
      route.baseURL = proxyBase
      route.models = [{ ...gateway, baseURL: proxyBase, maxTokens: 8192 }]
      if (api === 'openai-responses' && route.compat !== undefined) delete route.compat.sendSessionAffinityHeaders
      route.retryPolicy = { mode: 'normal', maxRetries: 0 }
      delete route.modelOverrides
      return {
        'llm-pi-ai': { providers: { cliproxyapi: route } },
        'agent-default-model': { provider: 'cliproxyapi', model: rest, reasoningEffort: 'medium' },
      }
    },
    pi: () => ({ api, upstreamBase: gatewayBase(api, gateway.baseURL ?? original.baseURL), keyName: original.apiKeyEnv ?? 'CLIPROXYAPI_API_KEY', model: piModel(rest, gateway) }),
  }
}
const routes = models.map(resolveRoute)
/** Scenarios that need a Bake tool pi does not ship; a pi arm skips them, so they have no pair. */
const piUnsupported = (scenario: string) => scenario.startsWith('delegation')
/**
 * A credential a pi arm passes to pi: the environment first, then the reference
 * of that name in ~/.bake/.credentials.yaml, which a Bake arm reads itself.
 */
const credentialRefs = existsSync(credentialFile) ? parseCredentialsDocument(readFileSync(credentialFile, 'utf8'), credentialFile).refs : new Map<string, string>()
function credential(name: string): string {
  const value = process.env[name] ?? credentialRefs.get(name)
  if (!value) throw new Error(`pi arm: ${name} is neither set nor a reference in ${credentialFile}`)
  return value
}
for (const route of routes) if (routes.filter(other => other.model === route.model).length > 1) throw new Error(`EVAL_MODELS names ${route.model} through two providers`)
const summaries: any[] = []
const maxLogicalTokens = Number(process.env.EVAL_MAX_TOKENS ?? 5_000_000)
const hash = (text: string) => createHash('sha256').update(text).digest('hex')
/** Every event the run's session logs recorded, for routing decisions and compactions. */
function sessionEvents(dir: string): { type?: string; data?: any }[] {
  if (!existsSync(dir)) return []
  return readdirSync(dir, { recursive: true, encoding: 'utf8' }).filter(path => path.endsWith('.jsonl'))
    .flatMap(path => readFileSync(join(dir, path), 'utf8').split('\n').filter(Boolean))
    .map(line => JSON.parse(line) as { type?: string; data?: any })
}
/**
 * Point the route's model entry at a smaller context window, so a scenario
 * reaches compaction. Returns false where the arm's route has no `llm-pi-ai`
 * model entry to carry it (the retired `llm-deepseek` adapter).
 */
function withContextWindow(settings: Record<string, any>, provider: string, contextWindow: number): boolean {
  const models = settings['llm-pi-ai']?.providers?.[provider]?.models
  if (!Array.isArray(models) || models.length === 0) return false
  for (const model of models) model.contextWindow = contextWindow
  return true
}
function wireUsage(data: any): any | undefined {
  const usage = data?.response?.usage ?? data?.message?.usage ?? data?.usage
  if (usage == null) return undefined
  return { event: data.type ?? 'usage', ...usage }
}
async function run(route: Route, scenario: string, trial: number, variant: string) {
  const arm = ARMS[variant]!
  const piRoute = arm.kind === 'pi' ? route.pi(arm) : undefined
  const { model } = route
  const { api, upstreamBase } = piRoute ?? route
  const label = `${model}.${scenario}.${trial}.${variant}`
  const savedPath = join(out, `${label}.json`)
  if (existsSync(savedPath)) { const saved = JSON.parse(readFileSync(savedPath, 'utf8')); summaries.push(saved); return saved }
  const root = mkdtempSync(join(tmpdir(), 'bake-token-eval-'))
  const built = fixture(root, scenario)
  const { workspace, file } = built
  const contextWindow = CONTEXT_WINDOWS[scenario]
  // The rendered system prompt of the agent's first request: the composition this sample measured.
  let systemPrompt: string | null = null
  const home = join(root, 'home'); mkdirSync(home, { mode: 0o700 })
  const wire: any[] = []
  const events: any[] = []
  let child: ReturnType<typeof spawn> | undefined
  let abortCause: string | undefined
  const timeout = new AbortController()
  let killTimer: ReturnType<typeof setTimeout> | undefined
  const terminate = (cause: string) => {
    if (abortCause !== undefined) return
    abortCause = cause
    timeout.abort()
    if (child?.pid !== undefined) {
      try { process.kill(-child.pid, 'SIGTERM') } catch {}
      killTimer = setTimeout(() => { try { process.kill(-child!.pid!, 'SIGKILL') } catch {} }, 5000)
    }
  }
  const server = Bun.serve({ port: 0, hostname: '127.0.0.1', idleTimeout: 255,
    async fetch(request) {
      const body = await request.text()
      let payload: any
      try { payload = JSON.parse(body) } catch { return new Response('Expected JSON', { status: 400 }) }
      const requestPath = new URL(request.url).pathname
      const suffix = requestPath.replace(/^\/upstream/, '')
      const rec: any = {
        index: wire.length, path: suffix, api, model: payload.model,
        requestBytes: Buffer.byteLength(body), systemChars: JSON.stringify(payload.system ?? payload.instructions ?? '').length,
        toolSchemaChars: JSON.stringify(payload.tools ?? []).length,
        inputChars: JSON.stringify(payload.input ?? payload.messages ?? []).length,
        requestHash: hash(body), usage: [],
        // Non-secret routing headers, so a gateway that treats two agents differently shows why.
        anthropicBeta: request.headers.get('anthropic-beta'), userAgent: request.headers.get('user-agent'),
      }
      wire.push(rec)
      if (wire.length === 1) systemPrompt = systemPromptOf(payload)
      if (scenario === 'no_tools' && trial === 0 && wire.length === 1) writeFileSync(join(out, `${label}.request.json`), body)
      if (scenario === 'ordinary_edit' && trial === 0) writeFileSync(join(out, `${label}.request-${wire.length}.json`), body)
      console.log(JSON.stringify({ progress: label, request: wire.length, bytes: rec.requestBytes }))
      if (wire.length > maxRequestsFor(scenario)) { terminate('request_limit'); return new Response('Evaluation request limit', { status: 429 }) }
      const headers = new Headers(request.headers)
      for (const key of ['host', 'content-length', 'connection', 'accept-encoding']) headers.delete(key)
      if (override !== undefined && route.provider === 'cliproxyapi') {
        if (headers.has('x-api-key')) headers.set('x-api-key', override.apiKey)
        if (headers.has('authorization')) headers.set('authorization', `Bearer ${override.apiKey}`)
      }
      try {
        // Stream timing, in ms from the proxy receiving the request, separates a gateway that stops
        // sending from a model that streams slowly: a stall shows events that stop well before the cap.
        const started = performance.now()
        const since = () => Math.round(performance.now() - started)
        rec.stream = { headersMs: null, firstByteMs: null, lastByteMs: null, endMs: null, events: 0, lastEvent: null }
        const response = await fetch(upstreamBase + suffix, { method: request.method, headers, body, signal: timeout.signal })
        rec.status = response.status
        rec.stream.headersMs = since()
        if (!response.ok || response.body === null) {
          const text = await response.text()
          rec.error = text.slice(0, 2000)
          return new Response(text, { status: response.status, headers: { 'content-type': response.headers.get('content-type') ?? 'application/json' } })
        }
        let buffered = ''
        const decoder = new TextDecoder()
        // The streaming tool call's name and argument size, with its head and tail, so a call whose
        // arguments run on until the cap shows what the model kept writing.
        const call = (name: unknown) => { if (typeof name === 'string') rec.stream.tool = { name, argsChars: 0, head: '', tail: '' } }
        const args = (delta: unknown) => {
          if (typeof delta !== 'string' || rec.stream.tool === undefined) return
          const tool = rec.stream.tool
          tool.argsChars += delta.length
          if (tool.head.length < 400) tool.head = (tool.head + delta).slice(0, 400)
          tool.tail = (tool.tail + delta).slice(-400)
        }
        const toolStream = (data: any) => {
          if (data?.type === 'response.output_item.added' && data.item?.type === 'function_call') call(data.item.name)
          else if (data?.type === 'response.function_call_arguments.delta') args(data.delta)
          else if (data?.type === 'content_block_start' && data.content_block?.type === 'tool_use') call(data.content_block.name)
          else if (data?.type === 'content_block_delta' && data.delta?.type === 'input_json_delta') args(data.delta.partial_json)
          else for (const toolCall of data?.choices?.[0]?.delta?.tool_calls ?? []) { call(toolCall.function?.name); args(toolCall.function?.arguments) }
        }
        const inspect = (chunk: Uint8Array) => {
          rec.stream.firstByteMs ??= since()
          rec.stream.lastByteMs = since()
          buffered += decoder.decode(chunk, { stream: true })
          const lines = buffered.split('\n'); buffered = lines.pop()!
          for (const line of lines) {
            if (line.startsWith('event: ')) rec.stream.lastEvent = line.slice(7).trim()
            if (!line.startsWith('data: ')) continue
            rec.stream.events++
            try { const data = JSON.parse(line.slice(6)); if (typeof data?.type === 'string') rec.stream.lastEvent = data.type; toolStream(data); const usage = wireUsage(data); if (usage !== undefined) rec.usage.push(usage) } catch {}
          }
        }
        const stream = response.body.pipeThrough(new TransformStream({ transform(chunk, controller) { inspect(chunk); controller.enqueue(chunk) }, flush() { rec.stream.endMs = since() } }))
        return new Response(stream, { status: response.status, headers: { 'content-type': response.headers.get('content-type') ?? 'text/event-stream', 'cache-control': 'no-cache' } })
      } catch (error) { rec.error = String(error); return new Response('Evaluation transport failure', { status: 502 }) }
    },
  })
  const started = performance.now()
  let deadline: ReturnType<typeof setTimeout> | undefined
  let stderr = ''
  let code: number | null = null
  let signal: string | null = null
  try {
    const injectionPath = join(root, 'injection.json')
    const hookPath = join(root, 'stale-writer.mjs')
    // The stale writer, as a Bake plugin or a pi extension: after the first
    // successful read of the target file, prepend a comment the fix must keep.
    const writeOnce = `if (changed) return;\n changed = true;\n writeFileSync(${JSON.stringify(join(workspace, file))}, '// EXTERNAL_CHANGE_KEEP\\n' + readFileSync(${JSON.stringify(join(workspace, file))}, 'utf8'));\n writeFileSync(${JSON.stringify(injectionPath)}, JSON.stringify({ injected: true }));`
    const target = (path: string) => `resolve(${JSON.stringify(workspace)}, ${path} ?? '') === ${JSON.stringify(join(workspace, file))}`
    let command: string[]
    // Whether this sample's route carries the scenario's smaller context window.
    let contextWindowApplied = false
    const env: Record<string, string | undefined> = { ...process.env, NO_COLOR: '1' }
    delete env.DEEPSEEK_API_KEY
    if (piRoute !== undefined) {
      const agentDir = join(root, 'pi-agent'); mkdirSync(agentDir, { mode: 0o700 })
      writeFileSync(join(agentDir, 'models.json'), JSON.stringify({ providers: { eval: {
        baseUrl: `${server.url.origin}/upstream`, api: piRoute.api, apiKey: '$EVAL_API_KEY',
        models: [contextWindow === undefined ? piRoute.model : { ...piRoute.model, contextWindow }],
      } } }), { mode: 0o600 })
      // Bake arms run with retries off; pi's agent-level retry would hide failed requests from the pair.
      // A forced window also gets compaction settings that mirror Bake's policy at that window.
      writeFileSync(join(agentDir, 'settings.json'), JSON.stringify({
        quietStartup: true, retry: { enabled: false }, ...contextWindow === undefined ? {} : { compaction: piCompactionFor(contextWindow) },
      }))
      writeFileSync(hookPath, `import { readFileSync, writeFileSync } from 'node:fs';\nimport { resolve } from 'node:path';\nexport default function (pi) {\n let changed = false;\n pi.on('tool_result', (event) => {\n if (event.toolName !== 'read' || event.isError || !(${target('event.input?.path')})) return;\n ${writeOnce}\n });\n}\n`)
      contextWindowApplied = contextWindow !== undefined
      Object.assign(env, { PI_CODING_AGENT_DIR: agentDir, PI_OFFLINE: '1', EVAL_API_KEY: credential(piRoute.keyName) })
      command = [arm.bin!, '--mode', 'json', '--no-session', '--no-extensions', '--no-skills', '--no-prompt-templates', '--no-themes',
        ...usesAgentInstructions(scenario) ? [] : ['--no-context-files'],
        '--model', `eval/${model}`, '--thinking', route.effort, ...(scenario === 'stale_edit' ? ['-e', hookPath] : []), prompts[scenario]!]
    } else {
      const routeSettings = route.settings(`${server.url.origin}/upstream`, arm.llmDeepseek)
      if (contextWindow !== undefined) contextWindowApplied = withContextWindow(routeSettings, route.provider, contextWindow)
      writeFileSync(join(home, 'settings.yaml'), YAML.stringify({
        ...merge(routeSettings, JSON.parse(arm.settings.replaceAll('$PROVIDER', route.provider).replaceAll('$MODEL', model))),
        permission: { defaultPreset: 'danger-full-access' },
      }), { mode: 0o600 })
      copyFileSync(credentialFile, join(home, '.credentials.yaml'))
      writeFileSync(hookPath, `import { readFileSync, writeFileSync } from 'node:fs';\nimport { resolve } from 'node:path';\nexport const name = 'token-evaluation-external-writer';\nexport function apply(ctx) {\n let changed = false;\n ctx.on('tools/result', (exec, result) => {\n if (exec.name !== 'read' || result.isError || !(${target('exec.arguments?.file_path')})) return;\n ${writeOnce}\n });\n}\n`)
      const overlay = [
        ...arm.composition,
        { id: 'session-title-llm', disabled: true },
        // Ambient AGENTS stay out, except where the scenario's own AGENTS.md is the point.
        ...usesAgentInstructions(scenario) ? [] : [{ id: 'agent-instructions', disabled: true }],
        { id: 'session-persistence-jsonl', config: { root: join(root, 'sessions'), compression: 'none' } },
        ...(scenario === 'stale_edit' ? [{ insert: [{ id: 'token-evaluation-external-writer', name: hookPath }] }] : []),
        ...arm.extra,
      ]
      const patch = join(root, 'evaluation.patch.json'); writeFileSync(patch, JSON.stringify(overlay))
      Object.assign(env, { BAKE_HOME: home, DSH_HOME: home, BAKE_AGENTS_HOME: join(root, 'agents'), DSH_AGENTS_HOME: join(root, 'agents') })
      command = ['node', join(arm.root, 'apps/cli/lib/bin.js'), 'headless', '--patch', patch, '--json', prompts[scenario]!]
    }
    child = spawn(command[0]!, command.slice(1), {
      cwd: workspace, env, detached: true, stdio: ['ignore', 'pipe', 'pipe'],
    })
    let pending = ''
    child.stdout!.setEncoding('utf8')
    child.stdout!.on('data', chunk => {
      pending += chunk
      const lines = pending.split('\n'); pending = lines.pop()!
      for (const line of lines) {
        try { events.push(JSON.parse(line)) } catch { if (line.trim()) events.push({ type: 'unparsed', text: line.slice(0, 1000) }) }
      }
    })
    child.stderr!.setEncoding('utf8')
    child.stderr!.on('data', chunk => { stderr += chunk })
    deadline = setTimeout(() => terminate('wall_clock_limit'), capMsFor(scenario))
    ;({ code, signal } = await new Promise<{ code: number | null; signal: string | null }>((resolve, reject) => {
      child!.once('error', reject)
      child!.once('close', (code, signal) => resolve({ code, signal }))
    }))
    const { final, steps, calls, results } = (piRoute === undefined ? normalizeBake : normalizePi)(events)
    const byTool: Record<string, number> = {}
    for (const call of calls) byTool[call.tool] = (byTool[call.tool] ?? 0) + 1
    const usage = { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, totalTokens: 0 }
    let usageComplete = steps.length > 0 && steps.every(step => step.usage !== undefined)
    for (const step of steps) if (step.usage !== undefined) for (const key of Object.keys(usage)) usage[key as keyof typeof usage] += step.usage[key] ?? 0
    if (wire.some(request => request.status !== 200 || request.usage.length === 0)) usageComplete = false
    const { validated, source, testsUnchanged, testExit, fixturesUnchanged } = validate(scenario, built, {
      code, final, toolCalls: calls.length, subagentCalls: byTool.subagent ?? 0, injectionPath,
    })
    const check = checkCommandFor(scenario)
    const logged = piRoute === undefined ? sessionEvents(join(root, 'sessions')) : []
    const workspaces = [workspace, realpathSync(workspace)]
    const toolErrors = results.filter(result => result.status === 'error').map(result => result.result)
    const summary = {
      label, model, provider: piRoute === undefined ? route.provider : 'eval', agent: arm.kind, api, effort: route.effort, scenario, trial, variant, code, signal, abortCause: abortCause ?? null,
      success: code === 0 && validated, validated, testsUnchanged, testExit, fixturesUnchanged,
      injectedStale: scenario === 'stale_edit' && existsSync(injectionPath),
      elapsedMs: Math.round(performance.now() - started), requests: wire.length, steps: steps.length,
      toolCalls: calls.length, byTool, toolErrors: toolErrors.length,
      duplicateRefusals: toolErrors.filter(text => /identical to one already refused|repeats one already refused/.test(text)).length,
      guardRefusals: toolErrors.filter(text => /has not been read|changed since it was read/.test(text)).length,
      shellEdits: shellEdits(calls),
      toolHelpCalls: byTool.tool_help ?? 0,
      multiEditCalls: calls.filter(call => call.tool === 'edit' && Array.isArray(call.input?.edits)).length,
      editedLineEchoes: results.filter(result => String(result.result).includes('The edited lines now read')).length,
      subagentCalls: byTool.subagent ?? 0,
      routingDecisions: routingDecisions(logged),
      roster: arm.kind === 'pi' ? null : roster,
      // The rendered prompt and whether it is the shipped composition; pi brings its own prompt, so it is not checked.
      systemPrompt,
      composition: arm.kind === 'pi' || systemPrompt === null ? null : compositionCheck(systemPrompt, workspaces),
      requestFloor: requestFloorFor(scenario),
      excessRequests: excessRequests(wire.length, requestFloorFor(scenario)),
      requestsOverFloor: requestsOverFloor(wire.length, requestFloorFor(scenario)),
      editCheckSplits: editCheckSplits(calls, check),
      orientationCalls: orientationCalls(calls, workspaces),
      ranCheck: ranCheck(calls, check),
      verifiedBeforeFinal: verifiedBeforeFinal(calls, check),
      backgroundStarts: backgroundStarts(calls),
      contextWindow: contextWindowApplied ? contextWindow : null,
      compactions: piRoute === undefined ? compactions(logged) : null,
      runawayAbort: runawayAbort([stderr, final, ...events.filter(event => event.type === 'unparsed').map(event => event.text)]),
      cache: (() => {
        const logical = usage.inputTokens + usage.cacheReadTokens + usage.cacheWriteTokens
        const perStep = steps.map(step => step.usage === undefined ? null : ({ read: step.usage.cacheReadTokens ?? 0, write: step.usage.cacheWriteTokens ?? 0, uncached: step.usage.inputTokens ?? 0 }))
        return {
          hitRatio: logical === 0 ? null : usage.cacheReadTokens / logical,
          firstRequestRead: perStep[0]?.read ?? null,
          stepsWithHit: perStep.filter(step => step !== null && step.read > 0).length,
          steps: perStep.length,
          perStep,
        }
      })(),
      usageComplete, usage, accounting: reconcile(api, wire, usage), stepUsage: steps.map(step => step.usage ?? null),
      logicalInputTokens: usage.inputTokens + usage.cacheReadTokens + usage.cacheWriteTokens,
      logicalTotalTokens: usage.totalTokens,
      newInputTokens: usage.inputTokens + usage.cacheWriteTokens,
      initialUsage: steps[0]?.usage ?? null,
      rawProviderUsage: wire.map(request => ({ index: request.index, status: request.status, usage: request.usage })),
      requestMetrics: wire.map(({ usage, ...rest }) => rest),
      final, sourceHash: source === '' ? null : hash(source),
      toolErrorMessages: toolErrors, stderr: stderr.slice(-5000),
    }
    writeFileSync(savedPath, JSON.stringify(summary, null, 2) + '\n')
    writeFileSync(join(out, `${label}.events.jsonl`), events.map(event => JSON.stringify(event)).join('\n') + '\n')
    summaries.push(summary)
    writeFileSync(join(out, 'results.json'), JSON.stringify(summaries, null, 2) + '\n')
    if (summary.composition?.ok === false) console.log(JSON.stringify({ label, composition: summary.composition }))
    console.log(JSON.stringify({ label, success: summary.success, requests: wire.length, tools: calls.length, errors: toolErrors.length, tokens: summary.logicalTotalTokens, uncached: usage.inputTokens, cached: usage.cacheReadTokens, complete: usageComplete, seconds: Math.round(summary.elapsedMs / 1000) }))
    return summary
  } finally {
    clearTimeout(deadline); clearTimeout(killTimer)
    timeout.abort()
    await server.stop(true)
    rmSync(root, { recursive: true, force: true })
  }
}

/** The checkout's commit and whether its tracked files differ from it; a pi arm records its package version instead. */
function revisionOf(arm: Arm) {
  if (arm.kind === 'pi') return { root: arm.root, agent: 'pi', version: arm.version, commit: null, dirty: false }
  const { root } = arm
  const git = (...args: string[]) => Bun.spawnSync(['git', '-C', root, ...args]).stdout.toString().trim()
  return { root, commit: git('rev-parse', 'HEAD'), dirty: git('status', '--porcelain', '--untracked-files=no') !== '' }
}
writeFileSync(join(out, 'design.json'), JSON.stringify({
  arms: Object.fromEntries(armNames.map(name => [name, ARMS[name]])),
  revisions: Object.fromEntries(armNames.map(name => [name, revisionOf(ARMS[name]!)])),
  startedAt: new Date().toISOString(),
  node: Bun.spawnSync(['node', '--version']).stdout.toString().trim(), models: routes.map(route => route.model), cases, trials, roster,
  routes: Object.fromEntries(routes.map(route => [route.model, { provider: route.provider, api: route.api, effort: route.effort }])),
  effort: 'medium where the model offers it; DeepSeek runs at high, its default, having no medium', maxOutputTokens: 8192, capMs: wallClockCaps().default, capMsByScenario: wallClockCaps(), maxRequests: requestCaps(), requestFloors: REQUEST_FLOORS, contextWindows: CONTEXT_WINDOWS, maxLogicalTokens,
  // pi's compaction settings for each forced window, mirroring Bake's default threshold and retained tail; pi's defaults would compact before every request.
  piCompaction: Object.fromEntries(Object.entries(CONTEXT_WINDOWS).map(([scenario, contextWindow]) => [scenario, piCompactionFor(contextWindow)])),
  retryPolicy: { mode: 'normal', maxRetries: 0 },
  gateway: override === undefined ? null : new URL(override.baseUrl).origin,
  composition: 'Built headless CLI with each revision\'s own headless system-prompt config (harness opener off, working-directory suffix) and its standard-preset persona prefix; host tools retained.'
    + (roster === 'tui' ? ' Roster tui: the standard preset\'s tool-fs, tool-fs-search, and tool-result-pruner configs and the terminal spill-policy inline cap; ask_user_question stays out, having no one to answer it headless.' : ' Roster headless: the headless bundle\'s own tool configs.')
    + ' Session title and ambient AGENTS disabled equally; instructions_file keeps agent-instructions for its own AGENTS.md.'
    + (armNames.some(name => ARMS[name]!.kind === 'pi') ? ' A pi arm runs pi --mode json with its own system prompt and default tools, no session, extensions, skills, prompt templates, or context files, agent-level retry off, and pi\'s own wire for each model; it skips scenarios that need a Bake-only tool. Where a scenario forces a context window, pi gets the same window and compaction settings that mirror Bake\'s policy at it (piCompaction).' : ''),
  cache: 'Fresh process/home/workspace/session per sample; provider cache is observed, not assumed cold. Pair order alternates by trial and scenario.',
  endpoint: 'Agent exits; external Node test passes without test modification; injected external comment is preserved.',
  tokenAccounting: 'Provider total_tokens is authoritative. Gemini via Responses may report reasoning outside output_tokens; recorded separately and reconciled against raw SSE. Anthropic-format totals, DeepSeek\'s included, are the sum of input, cache reads/writes, and output.',
  exclusions: 'No price inference; no inference about gateway retries hidden from the wire; no cross-model token-count comparability claim.',
}, null, 2) + '\n')
for (const route of routes) {
  const { model } = route
  let modelBlocked = false
  for (let trial = 0; trial < trials && !modelBlocked; trial++) {
    for (const [caseIndex, scenario] of cases.entries()) {
      const shift = (trial + caseIndex) % armNames.length
      const order = [...armNames.slice(shift), ...armNames.slice(0, shift)]
      const pair = []
      for (const variant of order) {
        if (ARMS[variant]!.kind === 'pi' && piUnsupported(scenario)) continue
        if (summaries.reduce((sum, sample) => sum + sample.logicalTotalTokens, 0) > maxLogicalTokens) throw new Error('Evaluation token limit reached; partial results retained')
        const sample = await run(route, scenario, trial, variant)
        pair.push(sample)
        // A Bake prompt that is not the shipped composition invalidates every sample after it; stop before spending more.
        if (sample.composition?.ok === false) throw new Error(`${sample.label}: the rendered system prompt is not the shipped composition (${JSON.stringify(sample.composition)}); partial results retained`)
      }
      if (scenario === 'no_tools' && pair.every(sample => !sample.success && (sample.requests === 0 || sample.rawProviderUsage.some((request: any) => request.status !== 200)))) {
        console.log(JSON.stringify({ model, blocked: 'both control samples failed before a usable response; remaining cells skipped' }))
        modelBlocked = true; break
      }
    }
  }
}
console.log(JSON.stringify({ completed: summaries.length, successful: summaries.filter(sample => sample.success).length, tokens: summaries.reduce((sum, sample) => sum + sample.logicalTotalTokens, 0) }))
