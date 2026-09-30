/**
 * A terminal hangup through a real `dsh --profile headless` process while a
 * model reply is still streaming: SIGHUP runs the bounded disposal and exits
 * 129, the session's buffered writes reach disk, and the managed subprocess
 * tree is stopped, while every write to the lost terminal fails.
 */

import { existsSync, readdirSync, readFileSync, realpathSync } from 'node:fs'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import type { Socket } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { execa } from 'execa'
import { describe, expect, it, onTestFinished, vi } from 'vitest'
import { resolveExampleLaunch } from '@deepseek-ai/dsh-loader-smoke'
import { PROCESS_SHUTDOWN_TIMEOUT_MS } from '../src/process-shutdown.ts'

const dshBinScript = fileURLToPath(new URL('../src/bin.ts', import.meta.url))
const tsconfigPath = fileURLToPath(new URL('../../../tsconfig.json', import.meta.url))
const probePlugin = pathToFileURL(fileURLToPath(new URL('./fixtures/hangup-probe.mjs', import.meta.url))).href
const bootTimeoutMs = 60_000

interface Tree { root: number; descendant: number }
interface Trigger { session: string; type: string; seq: number }
interface LoggedEvent { type: string; seq?: number; data?: unknown }

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ESRCH') return false
    throw error
  }
}

function readJson<T>(path: string): T {
  return JSON.parse(readFileSync(path, 'utf8')) as T
}

/** The follow-up hangup-probe.mjs sends the agent while the reply streams. */
const FOLLOW_UP = 'follow-up typed during the reply'

/** The start of a DeepSeek Messages reply: one text block, opened and never closed. */
const PARTIAL_REPLY = [
  { type: 'message_start', message: { id: 'msg_hangup', model: 'deepseek-v4-flash', usage: { input_tokens: 12, output_tokens: 1 } } },
  { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
  { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'Partial reply still streaming' } },
].map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')

/**
 * A model endpoint that accepts the request and, only when released, streams
 * {@link PARTIAL_REPLY} over a response it never ends.
 */
async function heldModel(): Promise<{ url: string; requested: Promise<void>; release(): void }> {
  let markRequested!: () => void
  let release!: () => void
  const requested = new Promise<void>((resolve) => { markRequested = resolve })
  const released = new Promise<void>((resolve) => { release = resolve })
  const sockets = new Set<Socket>()
  const server = createServer((request, response) => {
    request.resume()
    request.on('end', () => {
      markRequested()
      void released.then(() => {
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.write(PARTIAL_REPLY)
      })
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
  if (address === null || typeof address === 'string') throw new Error('held model server has no port')
  return { url: `http://127.0.0.1:${address.port}`, requested, release }
}

async function hangUp(graceMs: number) {
  const cwd = realpathSync(await mkdtemp(join(tmpdir(), 'dsh-hangup-')))
  onTestFinished(() => rm(cwd, { recursive: true, force: true, maxRetries: 3 }))
  const model = await heldModel()
  const patch = join(cwd, 'hangup.patch.yml')
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
    // Keep the agent's request the only one `requested` can observe.
    '- id: session-title-llm',
    '  disabled: true',
    '- insert:',
    '    - id: hangup-probe',
    `      name: '${probePlugin}'`,
    '',
  ].join('\n'))
  const launch = resolveExampleLaunch({
    srcBin: dshBinScript,
    configArgs: ['--profile', 'headless', '--patch', patch, 'wait for the model'],
    tsconfigPath,
    env: {
      DSH_HOME: join(cwd, '.dsh'),
      DSH_AGENTS_HOME: join(cwd, '.agents'),
      DSH_TELEMETRY_DISABLED: '1',
      DEEPSEEK_API_KEY: 'keyless-hangup',
      DEEPSEEK_BASE_URL: model.url,
      DSH_TEST_HANGUP_DIR: cwd,
      DSH_TEST_HANGUP_GRACE_MS: String(graceMs),
    },
  })
  const child = execa(launch.command, launch.args, {
    cwd,
    env: launch.env,
    stdin: 'ignore',
    reject: false,
    timeout: bootTimeoutMs + PROCESS_SHUTDOWN_TIMEOUT_MS,
    killSignal: 'SIGKILL',
  })
  let reaped = false
  // Runs before the directory's removal, and stops a tree that started even if a later step failed.
  // A tree seen gone is left alone, so a reused pid is never signalled.
  onTestFinished(async () => {
    child.kill('SIGKILL')
    await child
    if (reaped || !existsSync(join(cwd, 'tree.json'))) return
    const { root, descendant } = readJson<Tree>(join(cwd, 'tree.json'))
    for (const pid of [root, descendant]) {
      try { process.kill(pid, 'SIGKILL') } catch { /* already stopped */ }
    }
  })
  let early = ''
  child.stderr?.on('data', (chunk: Buffer) => { early += chunk.toString() })
  let exited = false
  void child.then(() => { exited = true })
  await vi.waitFor(() => {
    if (exited) throw new Error(`dsh exited before the hangup:\n${early}`)
    if (!existsSync(join(cwd, 'tree.json'))) throw new Error('managed tree not started')
  }, { timeout: bootTimeoutMs, interval: 20 })
  const tree = readJson<Tree>(join(cwd, 'tree.json'))
  await model.requested
  // The terminal is gone: every later write to stdout or stderr fails with EPIPE.
  child.stdout?.destroy()
  child.stderr?.destroy()
  await writeFile(join(cwd, 'hang-up'), '')
  model.release()
  const outcome = await child
  const trigger = readJson<Trigger>(join(cwd, 'trigger.json'))
  const sessions = join(cwd, '.dsh', 'sessions')
  const log = readdirSync(sessions, { recursive: true, encoding: 'utf8' })
    .find(path => path.includes(trigger.session) && path.endsWith('.jsonl'))
  const events = log === undefined
    ? []
    : readFileSync(join(sessions, log), 'utf8').split('\n').filter(line => line !== '')
      .map(line => JSON.parse(line) as LoggedEvent)
  await vi.waitFor(() => {
    expect([alive(tree.root), alive(tree.descendant)]).toEqual([false, false])
  }, { timeout: 5_000, interval: 25 })
  reaped = true
  return { outcome, trigger, events }
}

describe.skipIf(process.platform === 'win32')('terminal hangup (real headless profile)', () => {
  it('disposes, flushes the session, stops managed subprocesses, and exits 129 despite a repeated SIGHUP', async () => {
    const { outcome, trigger, events } = await hangUp(100)

    expect(outcome.exitCode).toBe(129)
    expect(outcome.signal).toBeUndefined()
    // Appended in the task that raised the signal while the reply was still in flight, so only
    // the disposal's flush can have written it. The second SIGHUP joined that disposal; forcing
    // exit would have lost it.
    expect(trigger.type).toBe('agent/inbox/spliced')
    const recorded = events.find(event => event.type === trigger.type && event.seq === trigger.seq)
    expect(JSON.stringify(recorded?.data)).toContain(FOLLOW_UP)
    // The process was interrupted while the model stream was open. The agent
    // must finish the step and turn before persistence closes; otherwise a
    // resumed session has to synthesize an interrupted tail and the durable log
    // does not describe what actually happened before the hangup.
    const stepEnd = events.find(event => event.type === 'step/end')
    expect(stepEnd).toBeDefined()
    const turnEnd = events.find(event => event.type === 'turn/end')
    expect(turnEnd?.data).toMatchObject({ reason: { kind: 'aborted', reason: { kind: 'disposed' } } })
    expect(stepEnd?.seq).toBeLessThan(turnEnd?.seq ?? Number.POSITIVE_INFINITY)
  }, bootTimeoutMs + 30_000)

  it('still stops managed subprocesses when disposal outlasts the shutdown bound', async () => {
    // TERM is ignored and the grace outlasts the bound, so only the exit phase can SIGKILL the tree.
    const { outcome } = await hangUp(120_000)

    expect(outcome.exitCode).toBe(129)
    expect(outcome.signal).toBeUndefined()
  }, bootTimeoutMs + 30_000)
})
