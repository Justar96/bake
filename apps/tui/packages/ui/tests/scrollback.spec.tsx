/** Real Ink replay with a writable barrier controlling terminal backpressure. */
import { EventEmitter } from 'node:events'
import { Writable } from 'node:stream'
import React from 'react'
import { render } from 'ink'
import { afterEach, expect, it, vi } from 'vitest'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { appendTranscript, emptyTranscript, type Transcript } from '../src/transcript.ts'
import type { Row } from '../src/rows.ts'

class Input extends EventEmitter {
  isTTY = true
  raw = false
  private data: string | null = null
  setEncoding() {}
  setRawMode(raw: boolean) { this.raw = raw }
  resume() {}
  pause() {}
  ref() {}
  unref() {}
  read() { const data = this.data; this.data = null; return data }
  send(data: string) { this.data = data; this.emit('readable') }
}

class Output extends Writable {
  isTTY = true
  columns = 80
  rows = 24
  chunks: string[] = []
  private blocked = true
  private historyStarted = false
  private callback: (() => void) | undefined
  get waiting(): boolean { return this.callback !== undefined }
  override _write(chunk: Buffer, _encoding: BufferEncoding, callback: () => void): void {
    this.chunks.push(chunk.toString())
    this.historyStarted ||= chunk.includes('history-')
    // Ink uses an empty write callback to observe delivery of preceding output.
    if (chunk.length === 0 && this.blocked && this.historyStarted) this.callback = callback
    else callback()
  }
  release(): void {
    this.blocked = false
    const callback = this.callback
    this.callback = undefined
    callback?.()
  }
}

function props(committed: Transcript, overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'replay-parent', copy: dictionaries.en,
    frame: 'round', quitting: false, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

const history = (count: number): Transcript => appendTranscript(emptyTranscript,
  Array.from({ length: count }, (_, index): Row => ({ kind: 'assistant', text: `history-${index}-end` })))
const markers = (text: string): string[] => text.match(/history-\d+-end/g) ?? []
const owned: { ui: ReturnType<typeof render>; output: Output }[] = []
afterEach(async () => {
  for (const { ui, output } of owned.splice(0).reverse()) {
    ui.unmount()
    output.release()
    await ui.waitUntilExit()
    ui.cleanup()
    output.destroy()
  }
})

function mount(state: AppProps) {
  const input = new Input()
  const output = new Output()
  const ui = render(<App {...state} />, {
    stdin: input as unknown as NodeJS.ReadStream, stdout: output as unknown as NodeJS.WriteStream,
    stderr: output as unknown as NodeJS.WriteStream, patchConsole: false, exitOnCtrlC: false, interactive: true,
  })
  owned.push({ ui, output })
  return { ui, input, output }
}

it('waits for output before admitting more history, accepting input and ordered appends meanwhile', async () => {
  const state = props(history(800))
  const { ui, input, output } = mount(state)
  await vi.waitFor(() => expect(output.waiting).toBe(true))
  const first = markers(output.chunks.join(''))
  expect(first.length).toBeGreaterThan(0)
  expect(first.length).toBeLessThan(800)
  input.send('draft\r')
  await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalled())
  const committed = appendTranscript(state.committed, [{ kind: 'assistant', text: 'history-800-end' }])
  ui.rerender(<App {...state} committed={committed} />)
  expect(markers(output.chunks.join(''))).toEqual(first)
  output.release()
  await vi.waitFor(() => expect(output.chunks.join('')).toContain('history-800-end'))
  expect(markers(output.chunks.join(''))).toEqual(Array.from({ length: 801 }, (_, index) => `history-${index}-end`))
})

it('bounds one large multiline answer across flushes without dropping or repeating lines', async () => {
  const lines = Array.from({ length: 1000 }, (_, index) => `history-${index}-end`)
  const { output } = mount(props(appendTranscript(emptyTranscript, [{ kind: 'assistant', text: ['```text', ...lines, '```'].join('\n') }])))
  await vi.waitFor(() => expect(output.waiting).toBe(true))
  expect(markers(output.chunks.join('')).length).toBeLessThan(1000)
  output.release()
  await vi.waitFor(() => expect(output.chunks.join('')).toContain(lines.at(-1)!))
  expect(markers(output.chunks.join(''))).toEqual(lines)
  expect(Math.max(...output.chunks.map(chunk => markers(chunk).length))).toBeLessThanOrEqual(512)
})

it('cancels pending admission on exit and restores raw mode', async () => {
  const { ui, input, output } = mount(props(history(800)))
  await vi.waitFor(() => expect(output.waiting).toBe(true))
  const first = markers(output.chunks.join(''))
  ui.unmount()
  output.release()
  await ui.waitUntilExit()
  await ui.waitUntilRenderFlush()
  expect(input.raw).toBe(false)
  expect(markers(output.chunks.join(''))).toEqual(first)
})

it('restarts parent replay after child inspection and retains the composer draft', async () => {
  const state = props(history(800))
  const { ui, input, output } = mount(state)
  await vi.waitFor(() => expect(output.waiting).toBe(true))
  input.send('saved draft')
  ui.rerender(<App {...state} inspection={{ sessionId: 'replay-child', label: 'Child',
    committed: appendTranscript(emptyTranscript, [{ kind: 'assistant', text: 'child-complete' }]),
    live: [], status: 'idle', model: 'mock/model' }} />)
  output.release()
  await vi.waitFor(() => expect(output.chunks.join('')).toContain('child-complete'))
  expect(output.chunks.join('')).not.toContain('history-799-end')
  output.chunks.length = 0
  ui.rerender(<App {...state} />)
  await vi.waitFor(() => expect(output.chunks.join('')).toContain('history-799-end'))
  expect(output.chunks.join('')).toContain('saved draft')
  // Inspection causes a terminal repaint; the final replay must still contain
  // every parent row in order, even when the first prefix is repainted too.
  expect(markers(output.chunks.join('')).slice(-800)).toEqual(Array.from({ length: 800 }, (_, index) => `history-${index}-end`))
})

it('abandons a pending session on navigation and completes replay after a resize', async () => {
  const state = props(history(800))
  const { ui, output } = mount(state)
  await vi.waitFor(() => expect(output.waiting).toBe(true))
  ui.rerender(<App {...state} sessionId="replacement" committed={appendTranscript(emptyTranscript,
    Array.from({ length: 800 }, (_, index): Row => ({ kind: 'assistant', text: `replacement-${index}-end` })))} />)
  output.columns = 40
  output.rows = 10
  output.emit('resize')
  output.release()
  await vi.waitFor(() => expect(output.chunks.join('')).toContain('replacement-799-end'))
  const text = output.chunks.join('')
  expect(text).not.toContain('history-799-end')
  const replacement = text.match(/replacement-\d+-end/g) ?? []
  expect(replacement.slice(-800)).toEqual(Array.from({ length: 800 }, (_, index) => `replacement-${index}-end`))
})
