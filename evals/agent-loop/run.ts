/**
 * Paired live evaluation of the agent loop: runs the built headless CLI of two
 * or more checkouts on the same scenarios, interleaved, and records what each
 * sample cost. See evals/README.md for the procedure and the regression rule.
 *
 * EVAL_ARMS    name=checkout pairs, comma-separated; each checkout must be built
 * EVAL_MODELS  cliproxyapi model ids from ~/.bake/settings.yaml
 * EVAL_CASES   scenario names (default: the standard suite)
 * EVAL_TRIALS  trials per scenario (default 3)
 * EVAL_OUTPUT  raw output directory (default .preflight/evals/agent-loop/<time>)
 * EVAL_EXTRA_<ARM>  optional JSON array of extra overlay rows for one arm
 */
import { spawn } from 'node:child_process'
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, copyFileSync, rmSync, existsSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { tmpdir } from 'node:os'
import { createHash } from 'node:crypto'
import YAML from 'yaml'
import { reconcile } from './accounting.ts'

const out = resolve(process.env.EVAL_OUTPUT ?? join('.preflight/evals/agent-loop', new Date().toISOString().replace(/[:.]/g, '-')))
mkdirSync(out, { recursive: true })
const ARMS: Record<string, { root: string; extra: unknown[] }> = Object.fromEntries((process.env.EVAL_ARMS ?? '').split(',').filter(Boolean).map(entry => {
  const [name, root] = entry.split('=')
  if (!name || !root) throw new Error(`EVAL_ARMS entry "${entry}" is not name=checkout`)
  if (!existsSync(join(root, 'apps/cli/lib/bin.js'))) throw new Error(`arm ${name}: ${root} has no built apps/cli/lib/bin.js; run bun run build there`)
  return [name, { root: resolve(root), extra: JSON.parse(process.env[`EVAL_EXTRA_${name.toUpperCase()}`] ?? '[]') }]
}))
const armNames = Object.keys(ARMS)
if (armNames.length < 2) throw new Error('EVAL_ARMS needs at least two name=checkout pairs, for example base=../bake-v0.2.0,candidate=.')
const variants = Object.fromEntries(armNames.map(name => [name, ARMS[name]!.root])) as Record<string, string>
const models = (process.env.EVAL_MODELS ?? 'gemini-3.8-flash-medium,gpt-6.1-sol,claude-sonnet-5-5').split(',')
/** The standard suite every recorded version runs; duplicate_recovery is an opt-in stress case. */
const STANDARD_CASES = ['no_tools', 'ordinary_edit', 'path_discovery', 'stale_edit', 'unprompted_edit', 'multi_site_edit', 'multi_file_edit', 'shell_then_edit', 'workflow_script']
const cases = (process.env.EVAL_CASES ?? STANDARD_CASES.join(',')).split(',')
const trials = Number(process.env.EVAL_TRIALS ?? 3)
const settings = YAML.parse(readFileSync(join(process.env.HOME!, '.bake/settings.yaml'), 'utf8'))
const original = settings['llm-pi-ai'].providers.cliproxyapi
const credentialFile = join(process.env.HOME!, '.bake/.credentials.yaml')
const summaries: any[] = []
const capMs = 180_000
const maxRequestsFor = (scenario: string) => scenario === 'workflow_script' ? 40 : 14
const maxLogicalTokens = Number(process.env.EVAL_MAX_TOKENS ?? 5_000_000)
const hash = (text: string) => createHash('sha256').update(text).digest('hex')
const baseCode = 'function roundMoney(value) {\n  return Math.floor(value * 100) / 100;\n}\nmodule.exports = { roundMoney };\n'
const fixedTests = (file: string) => `const assert = require('node:assert/strict');\nconst { roundMoney } = require('./${file}');\nfor (const [input, expected] of [[1.236,1.24],[2.994,2.99],[0.019,0.02],[-1.236,-1.24],[0,0]]) assert.equal(roundMoney(input), expected);\nconsole.log('FIXTURE_PASS');\n`
const natural = ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs, or make commits. Run node test.cjs to verify and give a brief final result.'
const common = ' Work only inside this fixture. Do not delegate, install packages, use the network, modify test.cjs, or make commits. Use the read and edit tools for source-file changes. Run node test.cjs to verify and give a brief final result.'
const prompts: Record<string, string> = {
  no_tools: 'Reply exactly TOKEN_CONTROL_OK. Do not call any tools.',
  ordinary_edit: 'Fix roundMoney in src/money.js: it should round to the nearest cent, matching Math.round(value * 100) / 100. The tests in test.cjs describe the required behavior.' + common,
  path_discovery: 'Fix roundMoney so it rounds to the nearest cent, matching Math.round(value * 100) / 100. An old ticket points to src/money.js, but the module may have moved. Locate the current implementation and use test.cjs to verify.' + common,
  stale_edit: 'Read src/money.js with the read tool, then fix roundMoney to round to the nearest cent, matching Math.round(value * 100) / 100. Another writer may update this file while you work; preserve any added comments.' + common,
  unprompted_edit: 'Fix roundMoney in src/money.js: it should round to the nearest cent, matching Math.round(value * 100) / 100.' + natural,
  multi_site_edit: 'In src/config.js set DEFAULT_PORT to 8080, DEFAULT_HOST to \'0.0.0.0\', and RETRIES to 3.' + natural,
  multi_file_edit: 'Fix two bugs: roundMoney in src/money.js must round to the nearest cent (Math.round(value * 100) / 100), and formatMoney in src/format.js must show two decimals with a leading $ sign.' + natural,
  shell_then_edit: 'Read src/money.js, then run node scripts/stamp.cjs (it adds a build header to the file that must be kept), then fix roundMoney to round to the nearest cent, matching Math.round(value * 100) / 100.' + natural,
  workflow_script: 'Use the workflow tool for this task. Write a workflow script that uses parallel() to run two subagents: one reads notes/a.txt and the other reads notes/b.txt, and each returns only that file\'s single line of text. The script returns { a, b }. Then write summary.txt in this directory with the a line followed by the b line, one per line. Work only inside this fixture. Do not install packages, use the network, or make commits. Give a brief final result.',
  duplicate_recovery: 'Exercise a guarded-edit recovery case. Before reading or running any other tool, attempt the same edit of src/money.js three times: replace "Math.floor(value * 100) / 100" with "Math.round(value * 100) / 100". Make these three edit calls consecutive, even if a call is refused. Then recover from any refusal and finish the correction.' + common,
}
function persona(root: string): string {
  const source = readFileSync(join(root, 'packages/preset/agent-presets/presets/standard/agent.cordis.yml'), 'utf8')
  const start = source.indexOf('    prefix: |-\n') + '    prefix: |-\n'.length
  return source.slice(start).split('\n- id:')[0]!.split('\n').map(line => line.startsWith('      ') ? line.slice(6) : line).join('\n').trim()
}
const configCode = "const DEFAULT_PORT = 3000\nconst DEFAULT_HOST = 'localhost'\nconst RETRIES = 1\nfunction url() {\n  return `http://${DEFAULT_HOST}:${DEFAULT_PORT}`\n}\nmodule.exports = { DEFAULT_PORT, DEFAULT_HOST, RETRIES, url }\n"
const configTests = "const assert = require('node:assert/strict');\nconst c = require('./src/config.js');\nassert.equal(c.DEFAULT_PORT, 8080); assert.equal(c.DEFAULT_HOST, '0.0.0.0'); assert.equal(c.RETRIES, 3);\nassert.equal(c.url(), 'http://0.0.0.0:8080');\nconsole.log('FIXTURE_PASS');\n"
const formatCode = "function formatMoney(value) {\n  return value.toFixed(1)\n}\nmodule.exports = { formatMoney }\n"
const multiTests = fixedTests('src/money.js').replace("console.log('FIXTURE_PASS');", "const { formatMoney } = require('./src/format.js');\nassert.equal(formatMoney(3), '$3.00'); assert.equal(formatMoney(2.5), '$2.50');\nconsole.log('FIXTURE_PASS');")
const stampScript = "const fs = require('node:fs');\nconst p = 'src/money.js';\nconst s = fs.readFileSync(p, 'utf8');\nif (!s.startsWith('// build: 42')) fs.writeFileSync(p, '// build: 42\\n' + s);\n"
const WORKFLOW_LINES = { a: 'ALPHA-7F3Q', b: 'BRAVO-2K9X' }
/** The test file each scenario validates against, unchanged by the agent. */
function testsFor(scenario: string, file: string): string {
  if (scenario === 'multi_site_edit') return configTests
  if (scenario === 'multi_file_edit') return multiTests
  return fixedTests(file)
}
function fixture(root: string, scenario: string) {
  const workspace = join(root, 'workspace')
  mkdirSync(workspace)
  const file = scenario === 'path_discovery' ? 'packages/billing/money.js' : scenario === 'multi_site_edit' ? 'src/config.js' : 'src/money.js'
  if (scenario === 'multi_site_edit') {
    mkdirSync(join(workspace, 'src'), { recursive: true })
    writeFileSync(join(workspace, file), configCode)
    writeFileSync(join(workspace, 'test.cjs'), configTests)
    return { workspace, file }
  }
  if (scenario === 'multi_file_edit') {
    mkdirSync(join(workspace, 'src'), { recursive: true })
    writeFileSync(join(workspace, 'src/money.js'), baseCode)
    writeFileSync(join(workspace, 'src/format.js'), formatCode)
    writeFileSync(join(workspace, 'test.cjs'), multiTests)
    return { workspace, file }
  }
  if (scenario === 'workflow_script') {
    mkdirSync(join(workspace, 'notes'), { recursive: true })
    writeFileSync(join(workspace, 'notes/a.txt'), `${WORKFLOW_LINES.a}\n`)
    writeFileSync(join(workspace, 'notes/b.txt'), `${WORKFLOW_LINES.b}\n`)
    return { workspace, file: 'summary.txt' }
  }
  if (scenario === 'shell_then_edit') {
    mkdirSync(join(workspace, 'scripts'), { recursive: true })
    writeFileSync(join(workspace, 'scripts/stamp.cjs'), stampScript)
  }
  if (scenario !== 'no_tools') {
    mkdirSync(join(workspace, file, '..'), { recursive: true })
    writeFileSync(join(workspace, file), baseCode)
    writeFileSync(join(workspace, 'test.cjs'), fixedTests(file))
    if (scenario === 'path_discovery') {
      for (let i = 0; i < 40; i++) {
        mkdirSync(join(workspace, `packages/utility${i}`), { recursive: true })
        writeFileSync(join(workspace, `packages/utility${i}/index.js`), `module.exports = { fixtureNumber: ${i} };\n`)
      }
    }
  }
  return { workspace, file }
}
function wireUsage(data: any): any | undefined {
  const usage = data?.response?.usage ?? data?.message?.usage ?? data?.usage
  if (usage == null) return undefined
  return { event: data.type ?? 'usage', ...usage }
}
async function run(model: string, scenario: string, trial: number, variant: string) {
  const label = `${model}.${scenario}.${trial}.${variant}`
  const savedPath = join(out, `${label}.json`)
  if (existsSync(savedPath)) { const saved = JSON.parse(readFileSync(savedPath, 'utf8')); summaries.push(saved); return saved }
  const modelConfig = original.models.find((m: any) => m.id === model)
  if (!modelConfig) throw new Error(`Missing configured model ${model}`)
  const api = modelConfig.api ?? original.api
  const upstreamBase = (modelConfig.baseURL ?? original.baseURL).replace(/\/$/, '')
  const root = mkdtempSync(join(tmpdir(), 'bake-token-eval-'))
  const { workspace, file } = fixture(root, scenario)
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
      }
      wire.push(rec)
      if (scenario === 'no_tools' && trial === 0 && wire.length === 1) writeFileSync(join(out, `${label}.request.json`), body)
      if (scenario === 'ordinary_edit' && trial === 0) writeFileSync(join(out, `${label}.request-${wire.length}.json`), body)
      console.log(JSON.stringify({ progress: label, request: wire.length, bytes: rec.requestBytes }))
      if (wire.length > maxRequestsFor(scenario)) { terminate('request_limit'); return new Response('Evaluation request limit', { status: 429 }) }
      const headers = new Headers(request.headers)
      for (const key of ['host', 'content-length', 'connection', 'accept-encoding']) headers.delete(key)
      try {
        const response = await fetch(upstreamBase + suffix, { method: request.method, headers, body, signal: timeout.signal })
        rec.status = response.status
        if (!response.ok || response.body === null) {
          const text = await response.text()
          rec.error = text.slice(0, 2000)
          return new Response(text, { status: response.status, headers: { 'content-type': response.headers.get('content-type') ?? 'application/json' } })
        }
        let buffered = ''
        const decoder = new TextDecoder()
        const inspect = (chunk: Uint8Array) => {
          buffered += decoder.decode(chunk, { stream: true })
          const lines = buffered.split('\n'); buffered = lines.pop()!
          for (const line of lines) {
            if (!line.startsWith('data: ')) continue
            try { const data = JSON.parse(line.slice(6)); const usage = wireUsage(data); if (usage !== undefined) rec.usage.push(usage) } catch {}
          }
        }
        const stream = response.body.pipeThrough(new TransformStream({ transform(chunk, controller) { inspect(chunk); controller.enqueue(chunk) } }))
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
    const route = structuredClone(original)
    route.baseURL = `${server.url.origin}/upstream`
    route.models = [{ ...modelConfig, baseURL: route.baseURL, maxTokens: 8192 }]
    if (api === 'openai-responses' && route.compat !== undefined) delete route.compat.sendSessionAffinityHeaders
    route.retryPolicy = { mode: 'normal', maxRetries: 0 }
    delete route.modelOverrides
    writeFileSync(join(home, 'settings.yaml'), YAML.stringify({
      'llm-pi-ai': { providers: { cliproxyapi: route } },
      'agent-default-model': { provider: 'cliproxyapi', model, reasoningEffort: 'medium' },
      permission: { defaultPreset: 'danger-full-access' },
    }), { mode: 0o600 })
    copyFileSync(credentialFile, join(home, '.credentials.yaml'))
    const injectionPath = join(root, 'injection.json')
    const hookPath = join(root, 'stale-writer.mjs')
    writeFileSync(hookPath, `import { readFileSync, writeFileSync } from 'node:fs';\nimport { resolve } from 'node:path';\nexport const name = 'token-evaluation-external-writer';\nexport function apply(ctx) {\n let changed = false;\n ctx.on('tools/result', (exec, result) => {\n if (changed || exec.name !== 'read' || result.isError || resolve(${JSON.stringify(workspace)}, exec.arguments?.file_path ?? '') !== ${JSON.stringify(join(workspace, file))}) return;\n changed = true;\n writeFileSync(${JSON.stringify(join(workspace, file))}, '// EXTERNAL_CHANGE_KEEP\\n' + readFileSync(${JSON.stringify(join(workspace, file))}, 'utf8'));\n writeFileSync(${JSON.stringify(injectionPath)}, JSON.stringify({ injected: true }));\n });\n}\n`)
    const overlay = [
      { id: 'system-prompt', config: { personaPrefix: persona(variants[variant]) } },
      { id: 'session-title-llm', disabled: true },
      { id: 'agent-instructions', disabled: true },
      { id: 'session-persistence-jsonl', config: { root: join(root, 'sessions'), compression: 'none' } },
      ...(scenario === 'stale_edit' ? [{ insert: [{ id: 'token-evaluation-external-writer', name: hookPath }] }] : []),
      ...ARMS[variant]!.extra,
    ]
    const patch = join(root, 'evaluation.patch.json'); writeFileSync(patch, JSON.stringify(overlay))
    const env = { ...process.env, DSH_HOME: home, DSH_AGENTS_HOME: join(root, 'agents'), NO_COLOR: '1' }
    delete env.DEEPSEEK_API_KEY
    child = spawn('node', [join(variants[variant], 'apps/cli/lib/bin.js'), 'headless', '--patch', patch, '--json', prompts[scenario]!], {
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
    deadline = setTimeout(() => terminate('wall_clock_limit'), capMs)
    ;({ code, signal } = await new Promise<{ code: number | null; signal: string | null }>((resolve, reject) => {
      child!.once('error', reject)
      child!.once('close', (code, signal) => resolve({ code, signal }))
    }))
    const final = events.findLast(event => event.type === 'final')?.text ?? ''
    const steps = events.filter(event => event.type === 'status' && event.phase === 'step_end')
    const calls = events.filter(event => event.type === 'tool_call')
    const results = events.filter(event => event.type === 'tool_result')
    const byTool: Record<string, number> = {}
    for (const call of calls) byTool[call.tool] = (byTool[call.tool] ?? 0) + 1
    const usage = { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, totalTokens: 0 }
    let usageComplete = steps.length > 0 && steps.every(step => step.usage !== undefined)
    for (const step of steps) if (step.usage !== undefined) for (const key of Object.keys(usage)) usage[key as keyof typeof usage] += step.usage[key] ?? 0
    if (wire.some(request => request.status !== 200 || request.usage.length === 0)) usageComplete = false
    let validated = scenario === 'no_tools' && final.trim() === 'TOKEN_CONTROL_OK' && calls.length === 0
    let source = ''
    let testsUnchanged: boolean | null = null
    let testExit: number | null = null
    if (scenario === 'workflow_script') {
      source = existsSync(join(workspace, file)) ? readFileSync(join(workspace, file), 'utf8') : ''
      validated = code === 0 && (byTool.workflow ?? 0) > 0
        && source.trim().split(/\r?\n/).map(line => line.trim()).join('\n') === `${WORKFLOW_LINES.a}\n${WORKFLOW_LINES.b}`
    } else if (scenario !== 'no_tools') {
      source = readFileSync(join(workspace, file), 'utf8')
      testsUnchanged = readFileSync(join(workspace, 'test.cjs'), 'utf8') === testsFor(scenario, file)
      const validation = Bun.spawnSync(['node', 'test.cjs'], { cwd: workspace, stdout: 'pipe', stderr: 'pipe', timeout: 5000 })
      testExit = validation.exitCode
      validated = validation.exitCode === 0 && testsUnchanged
        && (scenario !== 'stale_edit' || (existsSync(injectionPath) && source.includes('EXTERNAL_CHANGE_KEEP')))
        && (scenario !== 'shell_then_edit' || source.startsWith('// build: 42'))
    }
    const toolErrors = results.filter(result => result.status === 'error').map(result => result.result)
    const summary = {
      label, model, api, scenario, trial, variant, code, signal, abortCause: abortCause ?? null,
      success: code === 0 && validated, validated, testsUnchanged, testExit,
      injectedStale: scenario === 'stale_edit' && existsSync(injectionPath),
      elapsedMs: Math.round(performance.now() - started), requests: wire.length, steps: steps.length,
      toolCalls: calls.length, byTool, toolErrors: toolErrors.length,
      duplicateRefusals: toolErrors.filter(text => /identical to one already refused|repeats one already refused/.test(text)).length,
      guardRefusals: toolErrors.filter(text => /has not been read|changed since it was read/.test(text)).length,
      shellEdits: calls.filter(call => call.tool === 'bash' && /python3?\b[\s\S]*(write_text|\.write\(|open\([^)]*['"]w)|sed\s+-[a-zA-Z]*i|perl\s+-[a-zA-Z]*i|>\s*src\//.test(call.input?.command ?? '')).length,
      toolHelpCalls: byTool.tool_help ?? 0,
      workflowCalls: byTool.workflow ?? 0,
      workflowErrors: results.filter(result => result.status === 'error' && calls.some(call => call.callId === result.callId && call.tool === 'workflow')).length,
      multiEditCalls: calls.filter(call => call.tool === 'edit' && Array.isArray(call.input?.edits)).length,
      editedLineEchoes: results.filter(result => String(result.result).includes('The edited lines now read')).length,
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
    console.log(JSON.stringify({ label, success: summary.success, requests: wire.length, tools: calls.length, errors: toolErrors.length, tokens: summary.logicalTotalTokens, uncached: usage.inputTokens, cached: usage.cacheReadTokens, complete: usageComplete, seconds: Math.round(summary.elapsedMs / 1000) }))
    return summary
  } finally {
    clearTimeout(deadline); clearTimeout(killTimer)
    timeout.abort()
    await server.stop(true)
    rmSync(root, { recursive: true, force: true })
  }
}

/** The checkout's commit and whether its tracked files differ from it. */
function revisionOf(root: string) {
  const git = (...args: string[]) => Bun.spawnSync(['git', '-C', root, ...args]).stdout.toString().trim()
  return { root, commit: git('rev-parse', 'HEAD'), dirty: git('status', '--porcelain', '--untracked-files=no') !== '' }
}
writeFileSync(join(out, 'design.json'), JSON.stringify({
  arms: Object.fromEntries(armNames.map(name => [name, ARMS[name]])),
  revisions: Object.fromEntries(armNames.map(name => [name, revisionOf(ARMS[name]!.root)])),
  startedAt: new Date().toISOString(),
  node: Bun.spawnSync(['node', '--version']).stdout.toString().trim(), models, cases, trials,
  effort: 'medium', maxOutputTokens: 8192, capMs, maxRequests: { default: 14, workflow_script: 40 }, maxLogicalTokens,
  retryPolicy: { mode: 'normal', maxRetries: 0 },
  composition: 'Built headless CLI, with each revision standard-preset persona; host tools retained. Session title and ambient AGENTS disabled equally.',
  cache: 'Fresh process/home/workspace/session per sample; provider cache is observed, not assumed cold. Pair order alternates by trial and scenario.',
  endpoint: 'Agent exits; external Node test passes without test modification; injected external comment is preserved.',
  tokenAccounting: 'Provider total_tokens is authoritative. Gemini via Responses may report reasoning outside output_tokens; recorded separately and reconciled against raw SSE. Anthropic totals are the sum of input, cache reads/writes, and output.',
  exclusions: 'No price inference; no inference about gateway retries hidden from the wire; no cross-model token-count comparability claim.',
}, null, 2) + '\n')
for (const model of models) {
  let modelBlocked = false
  for (let trial = 0; trial < trials && !modelBlocked; trial++) {
    for (const [caseIndex, scenario] of cases.entries()) {
      const shift = (trial + caseIndex) % armNames.length
      const order = [...armNames.slice(shift), ...armNames.slice(0, shift)]
      const pair = []
      for (const variant of order) {
        if (summaries.reduce((sum, sample) => sum + sample.logicalTotalTokens, 0) > maxLogicalTokens) throw new Error('Evaluation token limit reached; partial results retained')
        pair.push(await run(model, scenario, trial, variant))
      }
      if (scenario === 'no_tools' && pair.every(sample => !sample.success && (sample.requests === 0 || sample.rawProviderUsage.some((request: any) => request.status !== 200)))) {
        console.log(JSON.stringify({ model, blocked: 'both control samples failed before a usable response; remaining cells skipped' }))
        modelBlocked = true; break
      }
    }
  }
}
console.log(JSON.stringify({ completed: summaries.length, successful: summaries.filter(sample => sample.success).length, tokens: summaries.reduce((sum, sample) => sum + sample.logicalTotalTokens, 0) }))
