/** Terminal modes are restored through normal exit and Cordis's fatal-release path. */
import { EventEmitter } from 'node:events'
import { afterEach, expect, it, vi } from 'vitest'
import { run, type TuiIo } from '../src/runner.ts'
import { SessionNavigation } from '../src/navigation.ts'
import { harness } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => {
  try { for (const dispose of cleanup.splice(0).reverse()) await dispose() }
  finally { vi.unstubAllEnvs() }
})

class Input extends EventEmitter {
  isTTY = true
  isRaw = false
  private data: string | null = null
  setEncoding() {}
  setRawMode(value: boolean) { this.isRaw = value }
  resume() {}
  pause() {}
  ref() {}
  unref() {}
  read() { const data = this.data; this.data = null; return data }
  write(data: string) { this.data = data; this.emit('readable') }
}
class Output extends EventEmitter {
  isTTY = true
  columns = 100
  rows = 30
  frames: string[] = []
  write(chunk: string, callback?: () => void) { this.frames.push(chunk); callback?.(); return true }
  get text() { return this.frames.join('') }
}

it.each([
  ['inline', 'quit'], ['inline', 'dispose'], ['fullscreen', 'quit'], ['fullscreen', 'dispose'],
  ['fullscreen', 'screen-reader'],
] as const)('restores %s Ink modes and drains the agent (%s)', async (screen, mode) => {
  vi.stubEnv('INK_SCREEN_READER', mode === 'screen-reader' ? 'true' : 'false')
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const input = new Input()
  const output = new Output()
  const error = new Output()
  const exit = vi.fn()
  // These streams implement exactly the terminal methods Ink consumes.
  const io = { in: input, out: output, err: error, exit } as unknown as TuiIo
  const finished = run(fixture.ctx, { screen, composerFrame: 'auto', completionLimit: 8, resultLines: 8, attachmentMaxBytes: 1048576, attachmentLimit: 8, doubleInterruptMs: 500, credentialRefs: [] }, io)
  cleanup.push(async () => { await fixture.ctx.fiber.dispose(); await finished })
  await Promise.race([finished, vi.waitFor(() => expect(output.text).toContain('Session: '))])
  expect(input.isRaw).toBe(true)
  expect(output.text).toContain('\u001b[?2004h')
  if (mode === 'quit') {
    input.write('\u0003')
    await vi.waitFor(() => expect(output.text).toContain('Press Ctrl-C again'), { timeout: 10_000 })
    // Another key ends the prompt, so the next Ctrl-C asks again. It does not quit.
    const asked = output.text.split('Press Ctrl-C again').length
    const beforeDismiss = output.frames.length
    input.write('x')
    await vi.waitFor(() => expect(output.frames.length).toBeGreaterThan(beforeDismiss), { timeout: 10_000 })
    input.write('\u0003')
    await vi.waitFor(() => expect(output.text.split('Press Ctrl-C again').length).toBeGreaterThan(asked), { timeout: 10_000 })
    expect(exit).not.toHaveBeenCalled()
    input.write('\u0003')
  } else {
    // The launcher's installFailLoud release hook awaits this same operation.
    await fixture.ctx.fiber.dispose()
  }
  await finished
  expect(input.isRaw).toBe(false)
  expect(output.text).toContain('\u001b[?2004l')
  expect(output.text.includes('\u001b[?1049h')).toBe(screen === 'fullscreen' && mode !== 'screen-reader')
  expect(output.text.includes('\u001b[?1049l')).toBe(screen === 'fullscreen' && mode !== 'screen-reader')
  expect(input.listenerCount('readable')).toBe(0)
  if (mode === 'quit') expect(exit).toHaveBeenCalledExactlyOnceWith(0)
  else expect(exit).not.toHaveBeenCalled()
})

it('leaves terminal modes untouched when fullscreen startup fails', async () => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const input = new Input()
  const output = new Output()
  const error = new Output()
  const exit = vi.fn()
  await expect(run(fixture.ctx, { screen: 'fullscreen', resume: 'missing-session', composerFrame: 'auto', completionLimit: 8, resultLines: 8, attachmentMaxBytes: 1048576, attachmentLimit: 8, doubleInterruptMs: 500, credentialRefs: [] }, {
    in: input, out: output, err: error, exit,
  } as unknown as TuiIo)).rejects.toThrow('missing-session')
  expect(input.isRaw).toBe(false)
  expect(input.listenerCount('readable')).toBe(0)
  expect(output.frames).toEqual([])
  // Reported immediately when caught, not deferred behind the caller's own catch.
  expect(error.text).toContain('missing-session')
  expect(exit).not.toHaveBeenCalled()
})

it.each(['stdin', 'stdout'])('refuses piped %s before acquiring terminal modes', async stream => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const input = new Input()
  input.isTTY = stream !== 'stdin'
  const output = new Output()
  output.isTTY = stream !== 'stdout'
  await expect(run(fixture.ctx, { screen: 'fullscreen', composerFrame: 'auto', completionLimit: 8, resultLines: 8, attachmentMaxBytes: 1048576, attachmentLimit: 8, doubleInterruptMs: 500, credentialRefs: [] }, {
    in: input, out: output, err: output, exit: vi.fn(),
  } as unknown as TuiIo)).rejects.toThrow('interactive terminal')
  expect(input.isRaw).toBe(false)
  expect(output.frames).toEqual([])
})

it('root fiber disposal awaits the drains owned by run()\'s own finally', async () => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const input = new Input()
  const output = new Output()
  const error = new Output()
  const exit = vi.fn()
  const io = { in: input, out: output, err: error, exit } as unknown as TuiIo
  const order: string[] = []
  const gate = Promise.withResolvers<void>()
  const original = SessionNavigation.prototype.drain
  const drainSpy = vi.spyOn(SessionNavigation.prototype, 'drain').mockImplementation(async function (this: SessionNavigation) {
    order.push('drain-start')
    await gate.promise
    order.push('drain-end')
    return original.call(this)
  })
  cleanup.push(async () => { drainSpy.mockRestore() })
  const finished = run(fixture.ctx, { screen: 'inline', composerFrame: 'auto', completionLimit: 8, resultLines: 8, attachmentMaxBytes: 1048576, attachmentLimit: 8, doubleInterruptMs: 500, credentialRefs: [] }, io)
  cleanup.push(async () => { gate.resolve(); await fixture.ctx.fiber.dispose(); await finished })
  await Promise.race([finished, vi.waitFor(() => expect(output.text).toContain('Session: '))])
  // Cordis's own root disposal, not run()'s explicit `await stop()`: proves the
  // effect's disposer blocks the caller on the same drains `finally` runs.
  const disposal = fixture.ctx.fiber.dispose()
  disposal.then(() => order.push('disposed'))
  // The mocked drain is gated, so this settling would prove the drain was
  // skipped rather than awaited; flushing every pending microtask and one
  // macrotask turn gives a stuck chain every chance to resolve regardless.
  await new Promise(resolve => setImmediate(resolve))
  expect(order).toEqual(['drain-start'])
  gate.resolve()
  await disposal
  await finished
  expect(order).toEqual(['drain-start', 'drain-end', 'disposed'])
})
