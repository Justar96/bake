/** Terminal modes are restored through normal exit and Cordis's fatal-release path. */
import { EventEmitter } from 'node:events'
import { afterEach, expect, it, vi } from 'vitest'
import { run, type TuiIo } from '../src/runner.ts'
import { harness } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

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

it.each(['quit', 'dispose'] as const)('restores Ink modes and drains the agent on %s', async mode => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const input = new Input()
  const output = new Output()
  const error = new Output()
  const exit = vi.fn()
  // These streams implement exactly the terminal methods Ink consumes.
  const io = { in: input, out: output, err: error, exit } as unknown as TuiIo
  const finished = run(fixture.ctx, { locale: 'en', composerFrame: 'auto', completionLimit: 8, resultLines: 8, attachmentMaxBytes: 1048576, attachmentLimit: 8, doubleInterruptMs: 500, credentialRefs: [] }, io)
  cleanup.push(async () => { await fixture.ctx.fiber.dispose(); await finished })
  await Promise.race([finished, vi.waitFor(() => expect(output.text).toContain('Ready'))])
  expect(input.isRaw).toBe(true)
  expect(output.text).toContain('\u001b[?2004h')
  if (mode === 'quit') {
    input.write('\u0003')
    await vi.waitFor(() => expect(output.text).toContain('Press Ctrl-C again'))
    input.write('\u0003')
  } else {
    // The launcher's installFailLoud release hook awaits this same operation.
    await fixture.ctx.fiber.dispose()
  }
  await finished
  expect(input.isRaw).toBe(false)
  expect(output.text).toContain('\u001b[?2004l')
  expect(input.listenerCount('readable')).toBe(0)
  if (mode === 'quit') expect(exit).toHaveBeenCalledExactlyOnceWith(0)
  else expect(exit).not.toHaveBeenCalled()
})

it.each(['stdin', 'stdout'])('refuses piped %s before acquiring terminal modes', async stream => {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const input = new Input()
  input.isTTY = stream !== 'stdin'
  const output = new Output()
  output.isTTY = stream !== 'stdout'
  await expect(run(fixture.ctx, { locale: 'en', composerFrame: 'auto', completionLimit: 8, resultLines: 8, attachmentMaxBytes: 1048576, attachmentLimit: 8, doubleInterruptMs: 500, credentialRefs: [] }, {
    in: input, out: output, err: output, exit: vi.fn(),
  } as unknown as TuiIo)).rejects.toThrow('interactive terminal')
  expect(input.isRaw).toBe(false)
  expect(output.frames).toEqual([])
})
