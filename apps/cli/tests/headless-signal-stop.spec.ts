/**
 * A real `dsh --profile headless --json` process stopped by SIGTERM or SIGINT
 * while its model request is pending: it exits with the signal's code, reports
 * the stop rather than the Session that disposal closed, leaves the log saved
 * through the disposed turn's end, and the `--resume` hint it prints continues
 * the Session in a later process.
 */

import { readdirSync, readFileSync, realpathSync } from 'node:fs'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import type { Socket } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execa } from 'execa'
import { describe, expect, it, onTestFinished } from 'vitest'
import { resolveExampleLaunch } from 'bake-loader-smoke'
import { PROCESS_SHUTDOWN_TIMEOUT_MS } from '../src/process-shutdown.ts'
import { deepseekEndpointSettings } from './fixtures/deepseek-endpoint.ts'

const dshBinScript = fileURLToPath(new URL('../src/bin.ts', import.meta.url))
const tsconfigPath = fileURLToPath(new URL('../../../tsconfig.json', import.meta.url))
const bootTimeoutMs = 60_000

/** A complete DeepSeek Messages reply carrying one text block. */
const ANSWER = [
  { type: 'message_start', message: { id: 'msg_resumed', model: 'deepseek-flash', usage: { input_tokens: 12, output_tokens: 1 } } },
  { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
  { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'Resumed after the stop' } },
  { type: 'content_block_stop', index: 0 },
  { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 4 } },
  { type: 'message_stop' },
].map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')

/** A model endpoint that holds every request open until `answer()`, then answers each new one. */
async function model(): Promise<{ url: string; requested: Promise<void>; answer(): void }> {
  let markRequested!: () => void
  const requested = new Promise<void>((resolve) => { markRequested = resolve })
  let answering = false
  const sockets = new Set<Socket>()
  const server = createServer((request, response) => {
    request.resume()
    request.on('end', () => {
      markRequested()
      if (!answering) return
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      response.end(ANSWER)
    })
  })
  server.on('connection', (socket) => {
    sockets.add(socket)
    socket.once('close', () => { sockets.delete(socket) })
  })
  await new Promise<void>((resolve) => { server.listen(0, '127.0.0.1', resolve) })
  onTestFinished(async () => {
    for (const socket of sockets) socket.destroy()
    await new Promise<void>((resolve) => { server.close(() => { resolve() }) })
  })
  const address = server.address()
  if (address === null || typeof address === 'string') throw new Error('model server has no port')
  return { url: `http://127.0.0.1:${address.port}`, requested, answer: () => { answering = true } }
}

describe.skipIf(process.platform === 'win32')('a headless run stopped by a signal (real headless profile)', () => {
  it.each([['SIGTERM', 0], ['SIGINT', 130]] as const)('%s: exits %i, reports the stop, and resumes', async (signal, code) => {
    const cwd = realpathSync(await mkdtemp(join(tmpdir(), 'dsh-signal-stop-')))
    onTestFinished(() => rm(cwd, { recursive: true, force: true, maxRetries: 3 }))
    const endpoint = await model()
    await mkdir(join(cwd, '.dsh'))
    await writeFile(join(cwd, '.dsh', 'settings.yaml'), deepseekEndpointSettings(endpoint.url))
    const patch = join(cwd, 'stop.patch.yml')
    await writeFile(patch, [
      '- id: agent-default-model',
      '  config:',
      '    provider: deepseek-official',
      '    model: deepseek-flash',
      '- id: session-persistence-jsonl',
      '  config:',
      "    root: !!js dshHomePath('sessions')",
      '    compression: none',
      '- id: session-title-llm',
      '  disabled: true',
      '',
    ].join('\n'))
    const run = (args: string[]) => {
      const launch = resolveExampleLaunch({
        srcBin: dshBinScript,
        configArgs: ['--profile', 'headless', '--patch', patch, '--json', ...args],
        tsconfigPath,
        env: {
          DSH_HOME: join(cwd, '.dsh'),
          DSH_AGENTS_HOME: join(cwd, '.agents'),
          DSH_TELEMETRY_DISABLED: '1',
          DEEPSEEK_API_KEY: 'keyless-signal-stop',
        },
      })
      const child = execa(launch.command, launch.args, {
        cwd, env: launch.env, stdin: 'ignore', reject: false,
        timeout: bootTimeoutMs + PROCESS_SHUTDOWN_TIMEOUT_MS, killSignal: 'SIGKILL',
      })
      onTestFinished(async () => { child.kill('SIGKILL'); await child })
      return child
    }

    const first = run(['wait for the model'])
    await endpoint.requested
    first.kill(signal)
    const stopped = await first
    expect(stopped.timedOut).toBe(false)
    expect(stopped.exitCode).toBe(code)
    const stderr = stopped.stderr.split('\n').filter(line => line.startsWith('dsh: '))
    expect(stderr).toEqual([expect.stringMatching(/^dsh: stopped before the task finished; continue it with --resume session-[0-9a-f-]+$/)])
    const id = /--resume (session-[0-9a-f-]+)/.exec(stderr[0] ?? '')![1]!
    const events = stopped.stdout.split('\n').filter(line => line !== '').map(line => JSON.parse(line) as { type: string; sessionId?: string })
    expect(events[0]).toMatchObject({ type: 'session', sessionId: id })
    expect(events.map(event => event.type)).not.toContain('error')
    expect(events.map(event => event.type)).not.toContain('final')
    // Disposal closed the Session, which drained its log through the disposed turn's end.
    const sessions = join(cwd, '.dsh', 'sessions')
    const log = readdirSync(sessions, { recursive: true, encoding: 'utf8' }).find(path => path.includes(id) && path.endsWith('.jsonl'))
    const logged = readFileSync(join(sessions, log!), 'utf8').split('\n').filter(line => line !== '').map(line => JSON.parse(line) as { type: string; data?: unknown })
    expect(logged.findLast(event => event.type === 'turn/end')?.data).toMatchObject({ reason: { kind: 'aborted', reason: { kind: 'disposed' } } })

    endpoint.answer()
    const resumed = await run(['--resume', id, 'carry on'])
    expect(resumed.exitCode).toBe(0)
    expect(resumed.stdout.split('\n').filter(line => line !== '').map(line => JSON.parse(line) as unknown).at(-1))
      .toEqual({ type: 'final', text: 'Resumed after the stop' })
  }, 2 * bootTimeoutMs + 30_000)
})
