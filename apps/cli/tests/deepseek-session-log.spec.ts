/**
 * What official DeepSeek requests carry, through a real `dsh --profile
 * headless` process whose DeepSeek adapter keeps its default Messages protocol
 * and reaches a local endpoint through `DEEPSEEK_BASE_URL`. Bake does not
 * send the Session log to DeepSeek: by default no request carries
 * `dsh_session_log`. Logs that an earlier opt-in wrote, with their
 * `session-log-deepseek/delivery-accepted` events, still resume and replay.
 */

import { readdirSync, readFileSync, realpathSync } from 'node:fs'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execa } from 'execa'
import { describe, expect, it, onTestFinished } from 'vitest'
import { LOADER_SMOKE_TEST_TIMEOUT_MS, resolveExampleLaunch } from '@deepseek-ai/dsh-loader-smoke'
import type { DeepSeekSessionLogExtension } from '@deepseek-ai/dsh-session-log-deepseek'

const dshBinScript = fileURLToPath(new URL('../src/bin.ts', import.meta.url))
const tsconfigPath = fileURLToPath(new URL('../../../tsconfig.json', import.meta.url))
const REPLY = 'Endpoint reply complete.'
const DELIVERY_ACCEPTED = 'session-log-deepseek/delivery-accepted'

interface CapturedRequest {
  readonly path: string
  readonly body: Record<string, unknown>
}

interface LogLine {
  readonly type?: string
  readonly id?: string
  readonly data?: unknown
}

/** A DeepSeek Messages endpoint that records each request body and answers with one text block. */
async function deepseekEndpoint(): Promise<{ url: string; requests: CapturedRequest[] }> {
  const requests: CapturedRequest[] = []
  const events = [
    { type: 'message_start', message: { id: 'msg_1', model: 'deepseek-v4-flash', usage: { input_tokens: 12, output_tokens: 1 } } },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: REPLY } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 5 } },
    { type: 'message_stop' },
  ]
  const sse = events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')
  const handle = async (request: IncomingMessage, response: ServerResponse): Promise<void> => {
    const parts: Buffer[] = []
    for await (const part of request as AsyncIterable<Buffer>) parts.push(part)
    const path = request.url ?? ''
    if (request.method !== 'POST' || !path.endsWith('/messages')) {
      response.writeHead(404, { 'content-type': 'application/json' }).end('{}')
      return
    }
    requests.push({ path, body: JSON.parse(Buffer.concat(parts).toString()) as Record<string, unknown> })
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

/** A workspace, isolated home, and the test-only patch every run shares. */
async function workspace(): Promise<{ cwd: string; patch: string; optIn: string }> {
  const cwd = realpathSync(await mkdtemp(join(tmpdir(), 'dsh-session-log-')))
  onTestFinished(() => rm(cwd, { recursive: true, force: true, maxRetries: 3 }))
  const patch = join(cwd, 'endpoint.patch.yml')
  await writeFile(patch, [
    // No provider is the default; the run names the one the endpoint serves.
    '- id: agent-default-model',
    '  config:',
    '    provider: deepseek-official',
    '    model: deepseek-v4-flash',
    '- id: session-persistence-jsonl',
    '  config:',
    "    root: !!js dshHomePath('sessions')",
    '    compression: none',
    // The title request is a second model call racing the one-shot exit.
    '- id: session-title-llm',
    '  disabled: true',
    '',
  ].join('\n'))
  const optIn = join(cwd, 'session-log-opt-in.patch.yml')
  await writeFile(optIn, '- id: session-log-deepseek\n  config:\n    enabled: true\n')
  return { cwd, patch, optIn }
}

/** Run one headless task to completion against the endpoint. */
async function headless(cwd: string, endpoint: string, args: readonly string[]): Promise<string> {
  const launch = resolveExampleLaunch({
    srcBin: dshBinScript,
    configArgs: ['--profile', 'headless', ...args],
    tsconfigPath,
    env: {
      DSH_HOME: join(cwd, '.dsh'),
      DSH_AGENTS_HOME: join(cwd, '.agents'),
      DSH_TELEMETRY_OTLP_URL: undefined,
      DSH_TELEMETRY_MODE: undefined,
      DEEPSEEK_API_KEY: 'keyless-session-log',
      DEEPSEEK_BASE_URL: endpoint,
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

/** The one Session log in the home, header first. */
function sessionLog(cwd: string): LogLine[] {
  const root = join(cwd, '.dsh', 'sessions')
  const logs = readdirSync(root, { recursive: true, encoding: 'utf8' }).filter(path => path.endsWith('.jsonl'))
  expect(logs).toHaveLength(1)
  return readFileSync(join(root, logs[0] as string), 'utf8').split('\n').filter(line => line !== '')
    .map(line => JSON.parse(line) as LogLine)
}

function acceptances(log: readonly LogLine[]): LogLine[] {
  return log.filter(line => line.type === DELIVERY_ACCEPTED)
}

describe('official DeepSeek requests from the shipped headless profile', () => {
  it('carry no dsh_session_log by default', async () => {
    const endpoint = await deepseekEndpoint()
    const { cwd, patch } = await workspace()

    await headless(cwd, endpoint.url, ['--patch', patch, 'say hello'])

    expect(endpoint.requests).not.toHaveLength(0)
    for (const request of endpoint.requests) {
      // Request extensions still run: the package inventory is the positive control.
      expect(request.body).toHaveProperty('dsh_plugin_packages')
      expect(request.body).not.toHaveProperty('dsh_session_log')
      expect(JSON.stringify(request.body)).toContain('say hello')
    }
    expect(acceptances(sessionLog(cwd))).toEqual([])
  }, LOADER_SMOKE_TEST_TIMEOUT_MS)

  it('resume and replay a log an earlier opt-in uploaded, without uploading it again', async () => {
    const endpoint = await deepseekEndpoint()
    const { cwd, patch, optIn } = await workspace()

    // The earlier behavior, now an explicit opt-in: the log rides the request.
    await headless(cwd, endpoint.url, ['--patch', patch, '--patch', optIn, 'first task'])
    const uploaded = endpoint.requests.map(request => request.body['dsh_session_log'] as DeepSeekSessionLogExtension | undefined)
    expect(uploaded[0]).toMatchObject({ version: 1, afterSeq: -1 })
    expect(JSON.stringify(uploaded[0]?.events)).toContain('first task')
    const recorded = sessionLog(cwd)
    const sessionId = recorded[0]?.id
    if (sessionId === undefined) throw new Error('the Session log must open with its header')
    expect(acceptances(recorded)).not.toHaveLength(0)

    const before = endpoint.requests.length
    await headless(cwd, endpoint.url, ['--patch', patch, '--resume', sessionId, 'second task'])

    const resumed = endpoint.requests.slice(before)
    expect(resumed).not.toHaveLength(0)
    for (const request of resumed) {
      expect(request.body).not.toHaveProperty('dsh_session_log')
      // The restored conversation replays into the next model request.
      expect(JSON.stringify(request.body['messages'])).toContain('first task')
      expect(JSON.stringify(request.body['messages'])).toContain(REPLY)
      expect(JSON.stringify(request.body['messages'])).toContain('second task')
    }
    const log = sessionLog(cwd)
    expect(log.slice(0, recorded.length)).toEqual(recorded)
    expect(acceptances(log)).toEqual(acceptances(recorded))
  }, LOADER_SMOKE_TEST_TIMEOUT_MS * 2)
})
