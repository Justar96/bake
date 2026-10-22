/**
 * Subagent model selection and task routing through a real `dsh --profile
 * headless` process: the user's `subagent-model-selection` setting reaches the
 * one-shot profile's `subagent` tool, and a delegation that names no route asks
 * the configured router for one. The model and the router are local endpoints.
 */

import { readdirSync, readFileSync, realpathSync } from 'node:fs'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execa } from 'execa'
import { describe, expect, it, onTestFinished } from 'vitest'
import { LOADER_SMOKE_TEST_TIMEOUT_MS, resolveExampleLaunch } from 'bake-loader-smoke'
import { deepseekEndpointSettings } from './fixtures/deepseek-endpoint.ts'

const dshBinScript = fileURLToPath(new URL('../src/bin.ts', import.meta.url))
const tsconfigPath = fileURLToPath(new URL('../../../tsconfig.json', import.meta.url))
const MODEL = 'deepseek-flash'
const PARENT_TASK = 'PARENT-TASK: delegate the lookup.'
const CHILD_PROMPT = 'CHILD-PROMPT: report the answer.'
const CHILD_REPLY = 'Child reply complete.'
const FINAL_REPLY = 'Parent reply complete.'
const ROUTER_TOKEN = 'test-router-token'

interface ToolSchema {
  readonly name: string
  readonly input_schema?: { properties?: Record<string, unknown> }
}

interface CapturedRequest {
  readonly body: { messages?: unknown; tools?: readonly ToolSchema[] }
}

/** Serve a local HTTP endpoint for this test and close it when the test ends. */
async function listen(handle: (request: IncomingMessage, body: string, response: ServerResponse) => void): Promise<string> {
  const server = createServer((request, response) => {
    const parts: Buffer[] = []
    request.on('data', (part: Buffer) => parts.push(part))
    request.on('end', () => { handle(request, Buffer.concat(parts).toString(), response) })
  })
  await new Promise<void>((resolve) => { server.listen(0, '127.0.0.1', resolve) })
  onTestFinished(async () => {
    server.closeAllConnections()
    await new Promise<void>((resolve) => { server.close(() => { resolve() }) })
  })
  const address = server.address()
  if (address === null || typeof address === 'string') throw new Error('endpoint has no port')
  return `http://127.0.0.1:${address.port}`
}

/** One Anthropic Messages stream: a text reply, or a single `subagent` call. */
function stream(content: { text: string } | { delegate: Record<string, unknown> }): string {
  const start = 'text' in content
    ? { type: 'text', text: '' }
    : { type: 'tool_use', id: 'toolu_delegate', name: 'subagent', input: {} }
  const delta = 'text' in content
    ? { type: 'text_delta', text: content.text }
    : { type: 'input_json_delta', partial_json: JSON.stringify(content.delegate) }
  const block = [
    { type: 'content_block_start', index: 0, content_block: start },
    { type: 'content_block_delta', index: 0, delta },
  ]
  const events = [
    { type: 'message_start', message: { id: 'msg_1', model: MODEL, usage: { input_tokens: 12, output_tokens: 1 } } },
    ...block,
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: 'text' in content ? 'end_turn' : 'tool_use' }, usage: { output_tokens: 5 } },
    { type: 'message_stop' },
  ]
  return events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')
}

/**
 * A DeepSeek Messages endpoint that plays both Agents: the parent delegates
 * once and then answers, and the child answers its prompt.
 */
async function deepseekEndpoint(): Promise<{ url: string; requests: CapturedRequest[] }> {
  const requests: CapturedRequest[] = []
  const url = await listen((request, body, response) => {
    // The Anthropic SDK may mark its beta surface with `?beta=true`.
    if (request.method !== 'POST' || !new URL(request.url ?? '', 'http://endpoint').pathname.endsWith('/messages')) {
      response.writeHead(404, { 'content-type': 'application/json' }).end('{}')
      return
    }
    const parsed = JSON.parse(body) as CapturedRequest['body']
    requests.push({ body: parsed })
    const messages = JSON.stringify(parsed.messages)
    const reply = messages.includes('"tool_result"')
      ? stream({ text: FINAL_REPLY })
      : messages.includes(PARENT_TASK)
        ? stream({ delegate: { description: 'Look it up', prompt: CHILD_PROMPT, run_in_background: false } })
        : stream({ text: CHILD_REPLY })
    response.writeHead(200, { 'content-type': 'text/event-stream' }).end(reply)
  })
  return { url, requests }
}

/** A task router that always chooses the one allowed route. */
async function routerEndpoint(): Promise<{ url: string; selections: { authorization?: string; body: Record<string, unknown> }[] }> {
  const selections: { authorization?: string; body: Record<string, unknown> }[] = []
  const url = await listen((request, body, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/bake/select') {
      response.writeHead(404, { 'content-type': 'application/json' }).end('{}')
      return
    }
    selections.push({
      ...request.headers.authorization === undefined ? {} : { authorization: request.headers.authorization },
      body: JSON.parse(body) as Record<string, unknown>,
    })
    response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({
      provider: 'deepseek-official', model: MODEL, reasoning_effort: 'high',
      reason: 'The only allowed route.', matched: true, fallback: false,
    }))
  })
  return { url, selections }
}

/**
 * A workspace with an isolated home, its settings document pointing the
 * DeepSeek route at the endpoint, and the test-only patch.
 */
async function workspace(endpoint: string, settings = ''): Promise<{ cwd: string; patch: string }> {
  const cwd = realpathSync(await mkdtemp(join(tmpdir(), 'dsh-headless-selection-')))
  onTestFinished(() => rm(cwd, { recursive: true, force: true, maxRetries: 3 }))
  await mkdir(join(cwd, '.dsh'))
  await writeFile(join(cwd, '.dsh', 'settings.yaml'), deepseekEndpointSettings(endpoint) + settings)
  const patch = join(cwd, 'endpoint.patch.yml')
  await writeFile(patch, [
    '- id: agent-default-model',
    '  config:',
    '    provider: deepseek-official',
    `    model: ${MODEL}`,
    '- id: session-persistence-jsonl',
    '  config:',
    "    root: !!js dshHomePath('sessions')",
    '    compression: none',
    // The title request is another model call racing the one-shot exit.
    '- id: session-title-llm',
    '  disabled: true',
    '',
  ].join('\n'))
  return { cwd, patch }
}

/** Run one headless task to completion against the endpoint. */
async function headless(cwd: string, args: readonly string[]): Promise<string> {
  const launch = resolveExampleLaunch({
    srcBin: dshBinScript,
    configArgs: ['--profile', 'headless', ...args],
    tsconfigPath,
    env: {
      DSH_HOME: join(cwd, '.dsh'),
      DSH_AGENTS_HOME: join(cwd, '.agents'),
      DSH_TELEMETRY_OTLP_URL: undefined,
      DSH_TELEMETRY_MODE: undefined,
      DEEPSEEK_API_KEY: 'keyless-model-selection',
      ING_API_TOKEN: ROUTER_TOKEN,
    },
  })
  const outcome = await execa(launch.command, launch.args, {
    cwd,
    env: launch.env,
    stdin: 'ignore',
    reject: false,
    timeout: LOADER_SMOKE_TEST_TIMEOUT_MS - 5_000,
    killSignal: 'SIGKILL',
  })
  expect(outcome.exitCode, outcome.stderr).toBe(0)
  return outcome.stdout
}

/** Every event the home's Session logs recorded, the child's included. */
function loggedEvents(cwd: string): { type?: string; data?: Record<string, unknown> }[] {
  const root = join(cwd, '.dsh', 'sessions')
  return readdirSync(root, { recursive: true, encoding: 'utf8' }).filter(path => path.endsWith('.jsonl'))
    .flatMap(path => readFileSync(join(root, path), 'utf8').split('\n').filter(line => line !== ''))
    .map(line => JSON.parse(line) as { type?: string; data?: Record<string, unknown> })
}

/** The `subagent` definition and tool names of a request. */
function delegationTools(request: CapturedRequest | undefined): { routeFields: string[]; names: string[] } {
  const tools = request?.body.tools ?? []
  const properties = tools.find(tool => tool.name === 'subagent')?.input_schema?.properties ?? {}
  return {
    routeFields: ['provider', 'model', 'reasoning_effort'].filter(field => field in properties),
    names: tools.map(tool => tool.name),
  }
}

describe('subagent model selection in the shipped headless profile', () => {
  it('leaves the subagent tool without route fields while the setting is off', async () => {
    const endpoint = await deepseekEndpoint()
    const { cwd, patch } = await workspace(endpoint.url)

    await headless(cwd, ['--patch', patch, 'say hello'])

    const tools = delegationTools(endpoint.requests[0])
    expect(tools.names).toContain('subagent')
    expect(tools.names).not.toContain('list_subagent_models')
    expect(tools.routeFields).toEqual([])
    expect(loggedEvents(cwd).map(event => event.type)).not.toContain('subagent/model-selection-policy')
  }, LOADER_SMOKE_TEST_TIMEOUT_MS)

  it('offers the allowed routes and asks the router when a delegation names none', async () => {
    const endpoint = await deepseekEndpoint()
    const router = await routerEndpoint()
    const { cwd, patch } = await workspace(endpoint.url, [
      'subagent-model-selection:',
      '  enabled: true',
      '  allowedModels:',
      '    - provider: deepseek-official',
      `      model: ${MODEL}`,
      '  router:',
      '    enabled: true',
      `    url: ${router.url}`,
      '',
    ].join('\n'))

    const stdout = await headless(cwd, ['--patch', patch, PARENT_TASK])

    expect(stdout).toContain(FINAL_REPLY)
    const tools = delegationTools(endpoint.requests[0])
    expect(tools.names).toContain('list_subagent_models')
    expect(tools.routeFields).toEqual(['provider', 'model', 'reasoning_effort'])
    // The child ran on the routed model and answered the delegated prompt.
    expect(endpoint.requests.some(request => JSON.stringify(request.body.messages).includes(CHILD_PROMPT)
      && !JSON.stringify(request.body.messages).includes(PARENT_TASK))).toBe(true)

    expect(router.selections).toHaveLength(1)
    expect(router.selections[0]?.authorization).toBe(`Bearer ${ROUTER_TOKEN}`)
    expect(router.selections[0]?.body['task']).toContain(CHILD_PROMPT)
    expect(router.selections[0]?.body['allowed_models']).toEqual([
      expect.objectContaining({ provider: 'deepseek-official', model: MODEL }),
    ])

    const events = loggedEvents(cwd)
    expect(events.find(event => event.type === 'subagent/model-selection-policy')?.data)
      .toEqual({ allowedModels: [{ provider: 'deepseek-official', model: MODEL }] })
    expect(events.find(event => event.type === 'subagent/routing-decision')?.data).toMatchObject({
      source: 'auto',
      route: { provider: 'deepseek-official', model: MODEL },
      router: { reason: 'The only allowed route.', fallback: false },
    })
  }, LOADER_SMOKE_TEST_TIMEOUT_MS)
})
