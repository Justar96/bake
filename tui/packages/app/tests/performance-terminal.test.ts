/** PTY failure paths release a live child and distinguish crashes from slow output. */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { afterEach, expect, it } from 'bun:test'
import { Terminal } from '../performance/terminal.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })
const metricsModule = resolve(import.meta.dirname, '../performance/metrics.mjs')

async function terminal(code: string, signal?: AbortSignal) {
  const root = await mkdtemp(join(tmpdir(), 'dsh-tui-perf-driver-'))
  cleanup.push(() => rm(root, { recursive: true, force: true }))
  const metrics = join(root, 'metrics.json')
  const tty = new Terminal([process.env.DSH_TUI_TEST_NODE ?? 'node', '--expose-gc', '--import', metricsModule, '-e', code], root, metrics,
    { ...process.env, DSH_TUI_PERF_METRICS: metrics }, signal)
  cleanup.push(() => tty.close())
  return { tty, metrics }
}

it.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')('runs Node in a native TTY and samples its live heap', async () => {
  const { tty } = await terminal('console.log("NODE_TTY", process.stdout.isTTY, process.stdout.columns, process.stdout.rows, process.versions.bun); setInterval(() => {}, 1000)')
  await tty.wait('Node TTY ready', () => tty.clean.includes('NODE_TTY true 120 40 undefined'))
  const first = await tty.sample()
  const second = await tty.sample()
  expect(first.sequence).toBe(1)
  expect(second.sequence).toBe(2)
  expect(second.afterGc.heapUsed).toBeGreaterThan(0)
})

it.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')('reports the actual failed Node exit', async () => {
  const { tty } = await terminal('process.exit(7)')
  await expect(tty.wait('ready', () => tty.clean.includes('NEVER_READY'))).rejects.toThrow('Node exited {"code":7')
})

it.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')('bounds a wait and kills the measured process before cleanup finishes', async () => {
  const { tty } = await terminal('process.stdout.write("WAITING"); setInterval(() => {}, 1000)')
  await tty.wait('fixture ready', () => tty.clean.includes('WAITING'))
  const pid = tty.pid
  await expect(tty.wait('missing output', () => false, 20)).rejects.toThrow('timed out')
  await tty.close()
  expect(() => process.kill(pid, 0)).toThrow()
})

it.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')('reports a fatal Node signal separately from its exit code', async () => {
  const { tty } = await terminal('process.kill(process.pid, "SIGKILL")')
  await expect(tty.wait('ready after signal', () => false)).rejects.toThrow('SIGKILL')
})

it.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')('cancels an observation and drains its live process', async () => {
  const aborter = new AbortController()
  const { tty } = await terminal('process.stdout.write("WAITING"); setInterval(() => {}, 1000)', aborter.signal)
  await tty.wait('fixture ready', () => tty.clean.includes('WAITING'))
  const pid = tty.pid
  const waiting = tty.wait('missing output', () => false)
  aborter.abort(new Error('Canceled observation'))
  await expect(waiting).rejects.toThrow('Canceled observation')
  await tty.close()
  expect(() => process.kill(pid, 0)).toThrow()
})
