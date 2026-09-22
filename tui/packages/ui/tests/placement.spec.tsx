/** Real Ink writes replayed in a terminal emulator, including cursor movement and scrolling. */
import { EventEmitter } from 'node:events'
import React from 'react'
import { render } from 'ink'
import xterm from '@xterm/headless'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { appendTranscript, emptyTranscript } from '../src/transcript.ts'
import type { Row } from '../src/rows.ts'

class Input extends EventEmitter {
  isTTY = true
  private data: string | null = null
  setEncoding() {}
  setRawMode() {}
  resume() {}
  pause() {}
  ref() {}
  unref() {}
  read() { const data = this.data; this.data = null; return data }
  write(data: string) { this.data = data; this.emit('readable') }
}

class Output extends EventEmitter {
  isTTY = true
  chunks: string[] = []
  constructor(public columns: number, public rows: number) { super() }
  write(chunk: string, callback?: () => void) { this.chunks.push(chunk); callback?.(); return true }
}

const disposers: (() => void)[] = []
afterEach(() => { for (const dispose of disposers.splice(0).reverse()) dispose() })

async function mount(columns: number, rows: number) {
  const terminal = new xterm.Terminal({ cols: columns, rows, convertEol: true, allowProposedApi: true })
  disposers.push(() => terminal.dispose())
  const stdout = new Output(columns, rows)
  const stdin = new Input()
  let state: AppProps = {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'screen', copy: dictionaries.en,
    frame: 'round', quitting: false, context: undefined,
    onSubmit: () => {}, onCancel: () => {}, onInterrupt: () => {}, onAnswer: () => {},
  }
  const instance = render(<App {...state} />, {
    stdout: stdout as unknown as NodeJS.WriteStream, stdin: stdin as unknown as NodeJS.ReadStream,
    stderr: stdout as unknown as NodeJS.WriteStream, patchConsole: false, exitOnCtrlC: false, interactive: true,
    // As the runner renders: only changed lines are rewritten.
    incrementalRendering: true,
  })
  disposers.push(() => { instance.unmount(); instance.cleanup() })
  let consumed = 0
  async function screen(): Promise<string[]> {
    await instance.waitUntilRenderFlush()
    const bytes = stdout.chunks.slice(consumed).join('')
    consumed = stdout.chunks.length
    if (bytes !== '') await new Promise<void>(resolve => terminal.write(bytes, resolve))
    return Array.from({ length: stdout.rows }, (_, row) => terminal.buffer.active.getLine(terminal.buffer.active.viewportY + row)?.translateToString(true) ?? '')
  }
  return {
    stdin, stdout, screen,
    async history(): Promise<string[]> {
      await screen()
      return Array.from({ length: terminal.buffer.active.length }, (_, row) => terminal.buffer.active.getLine(row)?.translateToString(true) ?? '')
    },
    async resize(columns: number, rows: number): Promise<string[]> {
      terminal.resize(columns, rows)
      stdout.columns = columns
      stdout.rows = rows
      stdout.emit('resize')
      // A narrowing repaints over two frames; read the screen once it settles.
      let written = -1
      while (written !== stdout.chunks.length) {
        written = stdout.chunks.length
        await screen()
        await new Promise(resolve => setTimeout(resolve, 50))
      }
      return screen()
    },
    async update(patch: Partial<AppProps>): Promise<string[]> {
      state = { ...state, ...patch }
      instance.rerender(<App {...state} />)
      return screen()
    },
  }
}

const inputRow = (screen: readonly string[]): number => screen.findIndex(line => line.includes('> ') && line.includes('▌'))
/** Last screen row containing `text`: the newest line the input should follow. */
const lastRow = (screen: readonly string[], text: string): number => screen.findLastIndex(line => line.includes(text))

/**
 * Assert the Claude Code shape: newest line, one blank, the turn header while
 * one runs or its summary after, the framed input, and the status line as the frame's footer.
 */
function expectInputUnder(screen: readonly string[], text: string): void {
  const newest = lastRow(screen, text)
  const input = inputRow(screen)
  expect(newest, screen.join('\n')).toBeGreaterThanOrEqual(0)
  expect(screen[newest + 1], screen.join('\n')).toBe('')
  const top = screen.findIndex((line, index) => index > newest && line.startsWith('╭'))
  expect(screen[top]).toMatch(/^╭─+╮$/)
  const header = screen.slice(newest + 2, top)
  // The reasoning ticker while there is one, then the turn header resting on
  // the frame: nothing else between, and nothing under the header.
  expect(header.length, screen.join('\n')).toBeLessThanOrEqual(2)
  // While a turn runs, the header, which yields to the output on a short
  // terminal; once it ends, the same row holds the turn's summary.
  if (header.length > 0) {
    const status = screen[input + 2]!
    expect(header.at(-1)).toMatch(/^ {2}Working/.test(status) ? /^✻ \S+…/ : /^[✓■✗] /)
  }
  expect(input, screen.join('\n')).toBe(top + 1)
  expect(screen[input + 1]).toMatch(/^╰─+╯$/)
  expect(screen[input + 2]).toMatch(/^ {2}(Ready|Working)/)
}

describe('composer placement', () => {
  it.each([[24, 2], [24, 3], [39, 5], [40, 3], [80, 5]])('keeps a usable composer at %ix%i', async (columns, rows) => {
    const ui = await mount(columns, rows)
    expect(inputRow(await ui.screen())).toBeGreaterThanOrEqual(0)
    ui.stdin.write('hello')
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('hello▌'))
    const frame = await ui.update({ status: 'running', live: [{ kind: 'assistant', text: 'Output\n'.repeat(20) }] })
    expect(frame.join('\n')).toContain('hello▌')
    expect(stdoutClears(ui.stdout)).toBe(false)
  })

  it.each([[40, 4], [80, 6]])('keeps the input visible on a short %ix%i terminal', async (columns, rows) => {
    const ui = await mount(columns, rows)
    expect(inputRow(await ui.screen())).toBeGreaterThanOrEqual(0)
    ui.stdin.write('\u001b[200~first\nsecond\nthird\nfourth\u001b[201~')
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('fourth▌'))
    const screen = await ui.screen()
    // The prompt row has scrolled out of the one-row window, so the caret
    // row carries `^` rather than the prompt marker.
    const input = screen.findIndex(line => line.includes('fourth▌'))
    expect(input).toBeGreaterThanOrEqual(0)
    expect(screen[input + 1]).toMatch(/^╰─+╯$/)
    expect(screen[rows - 1]).toBe('')
    await expect(snapshotOf(screen)).toMatchFileSnapshot(`./expected/composer-short.${columns}x${rows}.txt`)
  })

  it('opens directly under the session heading, with the status line beneath the frame', async () => {
    const ui = await mount(80, 24)
    const screen = await ui.screen()
    expectInputUnder(screen, 'Session: screen')
    expect(inputRow(screen)).toBe(3)
    expect(screen[5]).toBe('  Ready  model  /workspace')
  })

  it.each([[80, 24], [40, 10], [120, 40]])('follows the newest line through streaming and commits at %ix%i', async (columns, rows) => {
    const ui = await mount(columns, rows)
    await ui.screen()
    let committed = emptyTranscript
    for (let turn = 0; turn < 3; turn++) {
      committed = appendTranscript(committed, [{ kind: 'user', text: `Prompt ${turn}` }])
      expectInputUnder(await ui.update({ committed, status: 'running' }), `Prompt ${turn}`)
      for (const count of [1, 4, 18]) {
        const text = Array.from({ length: count }, (_, index) => `Response ${turn} line ${index}`).join('\n')
        const frame = await ui.update({ live: [{ kind: 'assistant', text }] })
        expectInputUnder(frame, `Response ${turn} line ${count - 1}`)
        // The status line and Ink's cursor row stay on screen below the input.
        expect(inputRow(frame)).toBeLessThanOrEqual(rows - 4)
      }
      committed = appendTranscript(committed, [{ kind: 'assistant', text: `Response ${turn} final` }])
      expectInputUnder(await ui.update({ committed, live: [], status: 'idle' }), `Response ${turn} final`)
    }
    expect(stdoutClears(ui.stdout)).toBe(false)
  })

  it('starts short live output directly below history, with the input right after it', async () => {
    const ui = await mount(80, 24)
    await ui.screen()
    const frame = await ui.update({ status: 'running', live: [{ kind: 'assistant', text: 'First response' }] })
    expect(frame.findIndex(line => line.includes('First response'))).toBe(2)
    // Blank, the turn header, then the frame.
    expect(frame[4]).toMatch(/^✻ \S+…  writing$/)
    expect(inputRow(frame)).toBe(6)
  })

  it('rests at the terminal bottom once history fills the screen', async () => {
    const ui = await mount(80, 24)
    await ui.screen()
    const committed = appendTranscript(emptyTranscript,
      Array.from({ length: 30 }, (_, index) => ({ kind: 'user' as const, text: `Prompt ${index}` })))
    const screen = await ui.update({ committed })
    expectInputUnder(screen, 'Prompt 29')
    // Frame, status line, and Ink's cursor row below it.
    expect(inputRow(screen)).toBe(24 - 4)
    expect(screen[23]).toBe('')
    expect(stdoutClears(ui.stdout)).toBe(false)
  })

  it('holds the input at the bottom while an answer prints line by line', async () => {
    // The controller prints each finished line to history and keeps only the
    // line still arriving live; on a full screen the terminal scrolls history
    // by what printed, and the composer does not move.
    const ui = await mount(80, 24)
    await ui.screen()
    let committed = appendTranscript(emptyTranscript, [
      ...Array.from({ length: 30 }, (_, index) => ({ kind: 'user' as const, text: `Prompt ${index}` })),
      { kind: 'tool-call' as const, callId: 'c1', tool: 'bash', input: 'ls' },
      { kind: 'tool-result' as const, callId: 'c1', ok: true, text: 'one.ts\ntwo.ts' },
    ])
    let screen = await ui.update({ committed, status: 'running', live: [{ kind: 'reasoning', text: 'Thinking it over' }] })
    const resting = inputRow(screen)
    expect(resting).toBe(24 - 4)
    committed = appendTranscript(committed, [{ kind: 'reasoning', text: 'Thinking it over' }])
    for (let line = 0; line < 12; line++) {
      const partial: Row = { kind: 'assistant', text: `Streamed ${line} par`, ...line === 0 ? {} : { continued: true } }
      screen = await ui.update({ committed, live: [partial] })
      expect(inputRow(screen), screen.join('\n')).toBe(resting)
      committed = appendTranscript(committed, [{ kind: 'assistant', text: `Streamed ${line} paragraph, long enough to wrap onto a second row at eighty columns, ending FIN-${line}.`, ...line === 0 ? {} : { continued: true } }])
      screen = await ui.update({ committed, live: [{ kind: 'assistant', text: '', continued: true }] })
      expect(inputRow(screen), screen.join('\n')).toBe(resting)
      expectInputUnder(screen, `FIN-${line}.`)
    }
    expect(stdoutClears(ui.stdout)).toBe(false)
  })

  it('keeps the caret visible when editing the start and middle of a wrapped draft', async () => {
    const ui = await mount(40, 10)
    await ui.screen()
    ui.stdin.write(`\u001b[200~START ${'你好 word '.repeat(50)} END\u001b[201~`)
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('END▌'))
    ui.stdin.write('\u001b[H')
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('▌START'))
    await expect(snapshotOf(await ui.screen())).toMatchFileSnapshot('./expected/composer-wrapped-home.txt')
    for (let index = 0; index < 100; index++) ui.stdin.write('\u001b[C')
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('▌'))
    expect((await ui.screen()).join('\n')).not.toContain('END')
    ui.stdin.write('\u001b[F')
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('END▌'))
    expect(stdoutClears(ui.stdout)).toBe(false)
  })

  it('keeps one clean copy of history after streaming, commits, and resize', async () => {
    const ui = await mount(80, 24)
    await ui.screen()
    let committed = emptyTranscript
    for (let index = 0; index < 12; index++) {
      committed = appendTranscript(committed, [{ kind: 'user', text: `Prompt ${index} unique` }])
      await ui.update({ committed, status: 'running' })
      await ui.update({ live: [{ kind: 'assistant', text: `Answer ${index} partial` }] })
      const text = `Answer ${index} final`
      await ui.update({ live: [{ kind: 'assistant', text }] })
      committed = appendTranscript(committed, [{ kind: 'assistant', text }])
      await ui.update({ committed, live: [], status: 'idle' })
    }
    expect(stdoutClears(ui.stdout)).toBe(false)
    for (const [columns, rows] of [[80, 24], [40, 10], [120, 40]]) {
      await ui.resize(columns!, rows!)
      const history = (await ui.history()).join('\n')
      for (let index = 0; index < 12; index++) {
        expect(history.split(`Prompt ${index} unique`), `${columns}x${rows}:\n${history}`).toHaveLength(2)
        expect(history.split(`Answer ${index} final`)).toHaveLength(2)
      }
      expect(history).not.toContain('partial')
      expect(history.split('▌')).toHaveLength(2)
    }
  })

  it('repaints without leftover frame rows when the terminal narrows', async () => {
    // A reflowing terminal re-wraps the full-width borders of the last frame,
    // so Ink's line-counted erase misses rows of it on every narrowing.
    const ui = await mount(80, 24)
    await ui.screen()
    const committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'Prompt one' }, { kind: 'assistant', text: 'Answer one' }])
    await ui.update({ committed })
    for (const [columns, rows] of [[60, 24], [45, 24], [100, 24], [40, 10]] as const) {
      const screen = await ui.resize(columns, rows)
      expect(screen.filter(line => line.startsWith('╭')), `${columns}x${rows}:\n${screen.join('\n')}`).toHaveLength(1)
      expectInputUnder(screen, 'Answer one')
    }
    const history = (await ui.history()).join('\n')
    expect(history.split('Prompt one')).toHaveLength(2)
    expect(history.split('▌')).toHaveLength(2)
  })

  it('follows wrapped history and survives terminal resizing', async () => {
    const ui = await mount(80, 24)
    await ui.screen()
    const committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: `${'Read 你好 👩🏽‍💻 '.repeat(30)}TAIL` }])
    expectInputUnder(await ui.update({ committed, status: 'running' }), 'TAIL')
    for (const [columns, rows] of [[40, 10], [120, 40]] as const) expectInputUnder(await ui.resize(columns, rows), 'TAIL')
    // Ink repaints on resize; ordinary streaming must not trigger another clear.
    const resized = ui.stdout.chunks.length
    const frame = await ui.update({ live: [{ kind: 'assistant', text: `${'Wrapped answer '.repeat(50)}FINISH` }] })
    expectInputUnder(frame, 'FINISH')
    expect(ui.stdout.chunks.slice(resized).join('')).not.toContain('\u001b[2J')
  })

  it('opens menus, notices, pending input and quit feedback between the newest line and the input', async () => {
    const ui = await mount(80, 24)
    const initial = await ui.screen()
    const anchor = inputRow(initial)
    await ui.update({ status: 'running', completion: { loading: false, error: undefined, entries:
      Array.from({ length: 12 }, (_, index) => ({ name: `command${index}`, description: 'Long description '.repeat(20), kind: 'command' as const })) } })
    ui.stdin.write('/')
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('/command0'))
    let screen = await ui.screen()
    // The blank opens the stack under the heading; the list sits on the frame.
    expect(screen[1]).toBe('')
    expect(lastRow(screen, 'more')).toBe(inputRow(screen) - 2)
    expect(screen[inputRow(screen) + 2]).toMatch(/^ {2}Working/)
    for (const notice of ['Short notice', 'Long notice\n'.repeat(30)]) {
      screen = await ui.update({ notice, pending: [{ id: 'queued', target: 'next-step', text: 'Next instruction' }] })
      expect(lastRow(screen, 'Session: screen')).toBeLessThan(lastRow(screen, 'Next instruction'))
      expect(lastRow(screen, 'notice')).toBeLessThan(inputRow(screen))
      expect(inputRow(screen)).toBeLessThanOrEqual(24 - 4)
    }
    await ui.update({ notice: undefined })
    ui.stdin.write('\u001b')
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).not.toContain('/command0'))
    screen = await ui.update({ pending: [], quitting: true, status: 'idle' })
    expect(lastRow(screen, dictionaries.en.quit)).toBe(inputRow(screen) - 2)
    // Everything closed, the input returns under the heading and the one row
    // the finished turn's summary holds.
    screen = await ui.update({ quitting: false })
    expect(inputRow(screen)).toBe(anchor + 1)
    expect(screen[anchor - 1]).toMatch(/^✓ /)
    expect(stdoutClears(ui.stdout)).toBe(false)
  })
})

/**
 * A screen as a file snapshot, without the empty rows under the frame: the
 * tests assert those rows directly, and a file ending in blank lines fails
 * the repository's whitespace check.
 */
function snapshotOf(screen: readonly string[]): string {
  return screen.join('\n').trimEnd() + '\n'
}

function stdoutClears(stdout: Output): boolean {
  return /\u001b\[2J/.test(stdout.chunks.join(''))
}
