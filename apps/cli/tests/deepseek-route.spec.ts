/**
 * What the shipped DeepSeek route sends, through a real `dsh --profile
 * headless` process. The base bundle serves `deepseek-official` from
 * `llm-pi-ai` over DeepSeek's Anthropic-format Messages endpoint; a user
 * settings section points it at a local endpoint, as a user would, without
 * replacing the shipped profile. A resumed Session replays its conversation
 * into the next request.
 */

import { readdirSync, readFileSync, realpathSync } from 'node:fs'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer, type IncomingHttpHeaders, type IncomingMessage, type ServerResponse } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execa } from 'execa'
import { describe, expect, it, onTestFinished } from 'vitest'
import { LOADER_SMOKE_TEST_TIMEOUT_MS, resolveExampleLaunch } from '@deepseek-ai/dsh-loader-smoke'
import { deepseekEndpointSettings } from './fixtures/deepseek-endpoint.ts'

const dshBinScript = fileURLToPath(new URL('../src/bin.ts', import.meta.url))
const tsconfigPath = fileURLToPath(new URL('../../../tsconfig.json', import.meta.url))
const MODEL = 'deepseek-flash'
const API_KEY = 'keyless-deepseek-route'
const REPLY = 'Endpoint reply complete.'

interface CapturedRequest {
  readonly path: string
  readonly headers: IncomingHttpHeaders
  readonly body: Record<string, unknown>
}

/** A DeepSeek Messages endpoint that records each request and answers with one text block. */
async function deepseekEndpoint(): Promise<{ url: string; requests: CapturedRequest[] }> {
  const requests: CapturedRequest[] = []
  const events = [
    {
      type: 'message_start',
      message: {
        id: 'msg_1', type: 'message', role: 'assistant', model: MODEL, content: [],
        stop_reason: null, stop_sequence: null, usage: { input_tokens: 12, output_tokens: 1 },
      },
    },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: REPLY } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 5 } },
    { type: 'message_stop' },
  ]
  const sse = events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')
  const handle = async (request: IncomingMessage, response: ServerResponse): Promise<void> => {
    const parts: Buffer[] = []
    for await (const part of request as AsyncIterable<Buffer>) parts.push(part)
    const path = request.url ?? ''
    if (request.method !== 'POST' || !new URL(path, 'http://endpoint').pathname.endsWith('/messages')) {
      response.writeHead(404, { 'content-type': 'application/json' }).end('{}')
      return
    }
    requests.push({
      path,
      headers: request.headers,
      body: JSON.parse(Buffer.concat(parts).toString()) as Record<string, unknown>,
    })
    response.writeHead(200, { 'content-type': 'text/event-stream' }).end(sse)
  }
  const server = createServer((request, response) => {
    void handle(request, response).catch((error: unknown) => { response.destroy(error as Error) })
  })
  await new Promise<void>((resolve) => { server.listen(0, '127.0.0.1', resolve) })
  onTestFinished(async () => {
    server.closeAllConnections()
    await new Promise<void>((resolve) => { server.close(() => { resolve() }) })
  })
  const address = server.address()
  if (address === null || typeof address === 'string') throw new Error('DeepSeek endpoint has no port')
  return { url: `http://127.0.0.1:${address.port}`, requests }
}

/** A workspace, an isolated home whose settings point the route at the endpoint, and the test-only patch. */
async function workspace(endpoint: string): Promise<{ cwd: string; patch: string }> {
  const cwd = realpathSync(await mkdtemp(join(tmpdir(), 'dsh-deepseek-route-')))
  onTestFinished(() => rm(cwd, { recursive: true, force: true, maxRetries: 3 }))
  await mkdir(join(cwd, '.dsh'))
  await writeFile(join(cwd, '.dsh', 'settings.yaml'), deepseekEndpointSettings(endpoint))
  const patch = join(cwd, 'endpoint.patch.yml')
  await writeFile(patch, [
    // No provider is the default; the run names the shipped route.
    '- id: agent-default-model',
    '  config:',
    '    provider: deepseek-official',
    `    model: ${MODEL}`,
    '- id: session-persistence-jsonl',
    '  config:',
    "    root: !!js dshHomePath('sessions')",
    '    compression: none',
    // The title request is a second model call racing the one-shot exit.
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
      DEEPSEEK_API_KEY: API_KEY,
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
  expect(outcome.stdout).toContain(REPLY)
  return outcome.stdout
}

/** The id of the one Session log in the home, read from its header. */
function sessionId(cwd: string): string {
  const root = join(cwd, '.dsh', 'sessions')
  const logs = readdirSync(root, { recursive: true, encoding: 'utf8' }).filter(path => path.endsWith('.jsonl'))
  expect(logs).toHaveLength(1)
  const header = JSON.parse(readFileSync(join(root, logs[0] as string), 'utf8').split('\n')[0] as string) as { id?: string }
  if (header.id === undefined) throw new Error('the Session log must open with its header')
  return header.id
}

describe('the shipped DeepSeek route in the headless profile', () => {
  it('posts an Anthropic-format Messages request with DeepSeek thinking and no Bake extensions', async () => {
    const endpoint = await deepseekEndpoint()
    const { cwd, patch } = await workspace(endpoint.url)

    await headless(cwd, ['--patch', patch, 'say hello'])

    expect(endpoint.requests).toHaveLength(1)
    const [request] = endpoint.requests
    // The Anthropic SDK may mark its beta surface with `?beta=true`.
    expect(new URL(request?.path ?? '', endpoint.url).pathname).toBe('/v1/messages')
    expect(request?.headers['x-api-key']).toBe(API_KEY)
    expect(request?.body['model']).toBe(MODEL)
    // DeepSeek reads `thinking.type` as enabled or disabled beside an effort.
    expect(request?.body['thinking']).toEqual({ type: 'enabled' })
    expect(request?.body['output_config']).toEqual({ effort: 'high' })
    expect(request?.body).not.toHaveProperty('dsh_session_log')
    expect(request?.body).not.toHaveProperty('dsh_plugin_packages')
    expect(JSON.stringify(request?.body['messages'])).toContain('say hello')
  }, LOADER_SMOKE_TEST_TIMEOUT_MS)

  it('replays a resumed conversation into the next request', async () => {
    const endpoint = await deepseekEndpoint()
    const { cwd, patch } = await workspace(endpoint.url)

    await headless(cwd, ['--patch', patch, 'first task'])
    const before = endpoint.requests.length
    await headless(cwd, ['--patch', patch, '--resume', sessionId(cwd), 'second task'])

    const resumed = endpoint.requests.slice(before)
    expect(resumed).toHaveLength(1)
    const messages = JSON.stringify(resumed[0]?.body['messages'])
    expect(messages).toContain('first task')
    expect(messages).toContain(REPLY)
    expect(messages).toContain('second task')
  }, LOADER_SMOKE_TEST_TIMEOUT_MS * 2)
})
