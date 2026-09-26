/** PTY failure paths release a live child and distinguish crashes from slow output. */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { afterEach, expect, it, spyOn } from 'bun:test'
import { Terminal } from '../performance/terminal.ts'
import { dictionaries } from '../../ui/src/copy.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })
const metricsModule = resolve(import.meta.dirname, '../performance/metrics.mjs')

async function terminal(code: string, signal?: AbortSignal, capturePtyExit?: (fail: () => void) => void) {
  const root = await mkdtemp(join(tmpdir(), 'dsh-tui-perf-driver-'))
  cleanup.push(() => rm(root, { recursive: true, force: true }))
  const metrics = join(root, 'metrics.json')
  const spawn = Bun.spawn
  const intercepted = capturePtyExit === undefined ? undefined : spyOn(Bun, 'spawn').mockImplementation(((command: string[], options: NonNullable<Parameters<typeof Bun.spawn>[1]>) => {
    const child = spawn(command, options)
    const terminalOptions = options.terminal as Bun.TerminalOptions
    capturePtyExit(() => terminalOptions.exit!(child.terminal!, 1, null))
    return child
  }) as typeof Bun.spawn)
  let tty: Terminal
  try {
    tty = new Terminal([process.env.DSH_TUI_TEST_NODE ?? 'node', '--expose-gc', '--import', metricsModule, '-e', code], root, metrics,
      { ...process.env, DSH_TUI_PERF_METRICS: metrics }, signal)
  } finally { intercepted?.mockRestore() }
  cleanup.push(() => tty.close())
  return { tty, metrics }
}

function interactiveExit(exit: string): string {
  return `
    process.stdin.setRawMode(true)
    process.stdin.resume()
    let interrupts = 0
    process.stdin.on('data', input => {
      if (!input.includes(3)) return
      if (++interrupts === 1) process.stdout.write(${JSON.stringify(dictionaries.en.quit)})
      else { ${exit} }
    })
    process.stdout.write('READY')
  `
}

it.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')('accepts native PTY shutdown after Node exits successfully', async () => {
  const { tty } = await terminal(interactiveExit('process.exit(0)'))
  await tty.wait('fixture ready', () => tty.clean.includes('READY'))
  await tty.quit()
  await tty.close()
  expect(() => process.kill(tty.pid, 0)).toThrow()
})

it.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')('rejects a PTY read failure while Node remains alive', async () => {
  let failRead: (() => void) | undefined
  const { tty } = await terminal('process.stdout.write("READY"); setInterval(() => {}, 1000)', undefined, fail => { failRead = fail })
  await tty.wait('fixture ready', () => tty.clean.includes('READY'))
  failRead!()
  await expect(tty.wait('next output', () => false)).rejects.toThrow('PTY read failed')
  await tty.close()
  expect(() => process.kill(tty.pid, 0)).toThrow()
})

for (const [outcome, exit, message] of [
  ['nonzero status', 'process.exit(7)', '"code":7'],
  ['fatal signal', 'process.kill(process.pid, "SIGKILL")', 'SIGKILL'],
] as const) {
  it.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')(`rejects ${outcome} after the quit handshake`, async () => {
    const { tty } = await terminal(interactiveExit(exit))
    await tty.wait('fixture ready', () => tty.clean.includes('READY'))
    await expect(tty.quit()).rejects.toThrow(message)
  })
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

it.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')('retains split input registration after history evicts it from the captured tail', async () => {
  const { tty } = await terminal(`
    process.stdin.setRawMode(true)
    process.stdin.once('data', () => process.stdout.write('h' + 'x'.repeat(150_000) + 'READY'))
    process.stdout.write('\\x1b[?2004')
    setInterval(() => {}, 1000)
  `)
  await tty.wait('registration prefix', () => tty.text.includes('\x1b[?2004'))
  expect(tty.inputReady).toBe(false)
  tty.send('continue')
  await tty.wait('complete history', () => tty.clean.includes('READY'))
  expect(tty.text).not.toContain('\x1b[?2004h')
  expect(tty.inputReady).toBe(true)
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
