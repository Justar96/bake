/**
 * Real composition: boots `dsh --profile desktop` through the Loader in a
 * child process, drives the bridge over its stdio carrier, and points the
 * real DeepSeek adapter at a scripted mock server.
 */

import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { mkdtemp, realpath, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createInterface } from 'node:readline'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it } from 'vitest'
import { resolveExampleLaunch } from '@deepseek-ai/dsh-loader-smoke'
import { startMockLlmServer, type MockLlmBehavior, type MockLlmServer } from '@deepseek-ai/dsh-llm-mock-server'
import type { CoreMessage, HarnessMessage, PermissionTier } from '../src/protocol.ts'

const dshBinScript = fileURLToPath(new URL('../../../../apps/cli/src/bin.ts', import.meta.url))
const TEST_TIMEOUT_MS = 120_000
const WAIT_MS = 60_000
const TRACE_ID = 'c0ffee00'.repeat(4)
const PARENT_SPAN = 'feedface'.repeat(2)

/** A running bridge process plus everything it has said so far. */
interface Bridge {
  readonly messages: HarnessMessage[]
  send(message: CoreMessage): void
  waitFor<T extends HarnessMessage['type']>(
    type: T,
    predicate?: (message: Extract<HarnessMessage, { type: T }>) => boolean,
  ): Promise<Extract<HarnessMessage, { type: T }>>
  stderr(): string
  close(): Promise<number | null>
}

const cleanups: (() => Promise<unknown>)[] = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
})

async function startBridge(
  sequence: MockLlmBehavior[],
  tool?: { name: string; arguments: string },
): Promise<{ bridge: Bridge; server: MockLlmServer; workspace: string }> {
  const server = await startMockLlmServer({
    sequence,
    repeatLast: true,
    successText: 'Bridge reply complete.',
    chunkSize: 4,
    ...tool === undefined ? {} : { toolName: tool.name, toolArguments: tool.arguments },
  })
  cleanups.push(() => server.close())
  const root = await realpath(await mkdtemp(join(tmpdir(), 'dsh-desktop-bridge-')))
  cleanups.push(() => rm(root, { recursive: true, force: true }))
  const workspace = join(root, 'workspace')
  const patchPath = join(root, 'test.patch.yml')
  await writeFile(patchPath, [
    '- id: llm-deepseek',
    '  config:',
    '    protocol: chat-completions',
    `    baseURL: ${server.baseURL}/v1`,
    // The title request would consume a scripted response in racing order.
    '- id: session-title-llm',
    '  disabled: true',
    '',
  ].join('\n'))
  const { mkdir } = await import('node:fs/promises')
  await mkdir(workspace)

  // Built lib only: profile plugins always load from `lib/`, and a source-mode
  // entry would evaluate a second copy of each package it imports directly.
  const launch = resolveExampleLaunch({
    srcBin: dshBinScript,
    mode: 'lib',
    configArgs: ['--profile', 'desktop', '--patch', patchPath],
    env: {
      DSH_HOME: join(root, 'home'),
      DSH_AGENTS_HOME: join(root, 'agents'),
      DEEPSEEK_API_KEY: 'mock-key',
      DSH_TELEMETRY_DISABLED: '1',
    },
  })
  const child: ChildProcessWithoutNullStreams = spawn(launch.command, launch.args, {
    cwd: workspace,
    env: { ...process.env, ...launch.env },
    stdio: ['pipe', 'pipe', 'pipe'],
  })
  let stderr = ''
  child.stderr.on('data', (chunk: Buffer) => { stderr += chunk.toString() })
  const messages: HarnessMessage[] = []
  const waiters = new Set<() => void>()
  createInterface({ input: child.stdout }).on('line', (line) => {
    messages.push(JSON.parse(line) as HarnessMessage)
    for (const waiter of waiters) waiter()
  })
  const exited = new Promise<number | null>(resolve => child.on('exit', resolve))
  cleanups.push(async () => {
    if (child.exitCode === null) child.kill('SIGKILL')
    await exited
  })

  const bridge: Bridge = {
    messages,
    send: (message) => { child.stdin.write(`${JSON.stringify(message)}\n`) },
    waitFor: (type, predicate) => new Promise((resolve, reject) => {
      const check = (): boolean => {
        const found = messages.find((m): m is Extract<HarnessMessage, { type: typeof type }> =>
          m.type === type && (predicate === undefined || predicate(m as never)))
        if (found === undefined) return false
        waiters.delete(check)
        clearTimeout(timer)
        resolve(found)
        return true
      }
      const timer = setTimeout(() => {
        waiters.delete(check)
        reject(new Error(`timed out waiting for ${type}; got ${messages.map(m => m.type).join(', ')}\nlast: ${JSON.stringify(messages.filter(m => m.type !== 'telemetry.spans').slice(-3))}\nstderr:\n${stderr}`))
      }, WAIT_MS)
      if (!check()) waiters.add(check)
    }),
    stderr: () => stderr,
    close: async () => {
      bridge.send({ v: 1, type: 'shutdown' })
      return exited
    },
  }
  return { bridge, server, workspace }
}

async function initialized(bridge: Bridge, workspace: string, permission: PermissionTier): Promise<void> {
  await bridge.waitFor('ready')
  bridge.send({ v: 1, type: 'init', workspace, permission })
  await bridge.waitFor('initialized')
}

describe('dsh --profile desktop', () => {
  it('streams a reply, reports usage, and continues the desktop trace', async () => {
    const { bridge, workspace } = await startBridge(['success'])
    await initialized(bridge, workspace, 'normal')
    bridge.send({ v: 1, type: 'user.message', text: 'say hello', trace: `00-${TRACE_ID}-${PARENT_SPAN}-01` })

    const started = await bridge.waitFor('session.started')
    expect(started).toMatchObject({ cwd: workspace, resumed: false })
    const done = await bridge.waitFor('turn.done')
    expect(done).toMatchObject({ session_id: started.session_id, reason: 'completed' })

    const text = bridge.messages
      .filter((m): m is Extract<HarnessMessage, { type: 'message.delta' }> => m.type === 'message.delta' && m.channel === 'text')
      .map(m => m.text)
      .join('')
    expect(text).toBe('Bridge reply complete.')
    expect(bridge.messages.some(m => m.type === 'message.done' && m.outcome === 'committed')).toBe(true)
    expect(bridge.messages.some(m => m.type === 'usage')).toBe(true)

    const spans = (await bridge.waitFor('telemetry.spans', m => m.spans.some(s => s.name === 'bake.turn'))).spans
    const turn = spans.find(s => s.name === 'bake.turn')
    expect(turn).toMatchObject({ traceId: TRACE_ID, parentSpanId: PARENT_SPAN, status: 'ok' })
    const llm = bridge.messages
      .flatMap(m => m.type === 'telemetry.spans' ? m.spans : [])
      .find(s => s.name === 'llm.request')
    expect(llm).toMatchObject({ traceId: TRACE_ID })
    expect(typeof llm?.attrs.ttft_ms).toBe('number')

    expect(await bridge.close()).toBe(0)
  }, TEST_TIMEOUT_MS)

  it('asks before a shell command in normal mode and runs it once approved', async () => {
    const { bridge, workspace } = await startBridge(
      ['tool_call_success', 'success'],
      { name: 'bash', arguments: JSON.stringify({ command: 'echo BRIDGE_TOOL_OK', description: 'Print a marker' }) },
    )
    await initialized(bridge, workspace, 'normal')
    bridge.send({ v: 1, type: 'user.message', text: 'run it' })

    const request = await bridge.waitFor('approval.request')
    expect(request).toMatchObject({
      tool: 'bash',
      summary: 'echo BRIDGE_TOOL_OK',
      grant: { key: 'shell:echo', label: '`echo` commands' },
    })
    bridge.send({ v: 1, type: 'approval.result', request_id: request.request_id, decision: 'approve_once' })

    const finished = await bridge.waitFor('tool.finished')
    expect(finished.status).toBe('completed')
    expect(finished.output).toContain('BRIDGE_TOOL_OK')
    await bridge.waitFor('turn.done')
    const spans = bridge.messages.flatMap(m => m.type === 'telemetry.spans' ? m.spans : [])
    expect(spans.map(s => s.name)).toEqual(expect.arrayContaining(['tool.pipeline', 'approval.wait', 'tool.execute']))
    expect(await bridge.close()).toBe(0)
  }, TEST_TIMEOUT_MS)

  it('never runs a denied command', async () => {
    const marker = 'denied-marker'
    const { bridge, workspace } = await startBridge(
      ['tool_call_success', 'success'],
      { name: 'bash', arguments: JSON.stringify({ command: `touch ${marker}`, description: 'Create a marker file' }) },
    )
    await initialized(bridge, workspace, 'normal')
    bridge.send({ v: 1, type: 'user.message', text: 'make a file' })
    const request = await bridge.waitFor('approval.request')
    bridge.send({ v: 1, type: 'approval.result', request_id: request.request_id, decision: 'deny' })
    const finished = await bridge.waitFor('tool.finished')
    expect(finished.status).toBe('error')
    await bridge.waitFor('turn.done')
    const { access } = await import('node:fs/promises')
    await expect(access(join(workspace, marker))).rejects.toThrow()
    expect(await bridge.close()).toBe(0)
  }, TEST_TIMEOUT_MS)

  it('runs commands without asking in full-access mode', async () => {
    const { bridge, workspace } = await startBridge(
      ['tool_call_success', 'success'],
      { name: 'bash', arguments: JSON.stringify({ command: 'echo FULL_ACCESS_OK', description: 'Print a marker' }) },
    )
    await initialized(bridge, workspace, 'full-access')
    bridge.send({ v: 1, type: 'user.message', text: 'run it' })
    const finished = await bridge.waitFor('tool.finished')
    expect(finished.output).toContain('FULL_ACCESS_OK')
    expect(bridge.messages.some(m => m.type === 'approval.request')).toBe(false)
    expect(await bridge.close()).toBe(0)
  }, TEST_TIMEOUT_MS)

  it('refuses a workspace that is not its working directory', async () => {
    const { bridge } = await startBridge(['success'])
    await bridge.waitFor('ready')
    bridge.send({ v: 1, type: 'init', workspace: '/somewhere/else', permission: 'normal' })
    const error = await bridge.waitFor('error')
    expect(error.message).toContain('differs from the harness working directory')
    expect(await bridge.close()).toBe(0)
  }, TEST_TIMEOUT_MS)
})
