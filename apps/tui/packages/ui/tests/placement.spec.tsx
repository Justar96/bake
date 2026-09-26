/** Real Ink writes replayed in a terminal emulator, including cursor movement and scrolling. */
import { EventEmitter } from 'node:events'
import React from 'react'
import { render } from 'ink'
import xterm from '@xterm/headless'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { appendTranscript, emptyTranscript } from '../src/transcript.ts'
import type { Row, ToolCallRow } from '../src/rows.ts'
import { SPINNER_REST, THINKING_ROWS } from '../src/activity.ts'

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

/** Ink's `clearTerminal`, which starts a replay of history. */
const CLEAR_TERMINAL = '\u001b[2J\u001b[3J\u001b[H'
/** Cursor Next Line, which the runner writes as a scrolling newline (`scrolling` in its output). */
const NEXT_LINE = '\u001b[E'

/**
 * Mount the app in a terminal, placed as the runner places it.
 *
 * The runner's output (`packages/app/src/output.ts`) starts the frame on the
 * bottom row, under whatever the shell printed, and returns there after each
 * screen clear, and steps over unchanged rows with a newline that scrolls.
 * The streams here are Ink's own, so the terminal is given the same moves;
 * `packages/app/tests/output.spec.tsx` checks the runner's.
 */
async function mount(columns: number, rows: number, overrides: Partial<AppProps> = {}) {
  const terminal = new xterm.Terminal({ cols: columns, rows, convertEol: true, allowProposedApi: true })
  disposers.push(() => terminal.dispose())
  await new Promise<void>(resolve => terminal.write(`$ bake\n\u001b[${rows}B`, resolve))
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
    ...overrides,
  }
  const instance = render(<App {...state} />, {
    stdout: stdout as unknown as NodeJS.WriteStream, stdin: stdin as unknown as NodeJS.ReadStream,
    stderr: stdout as unknown as NodeJS.WriteStream, patchConsole: false, exitOnCtrlC: false, interactive: true,
    // As the runner renders. Only changed lines are rewritten.
    incrementalRendering: true,
  })
  disposers.push(() => { instance.unmount(); instance.cleanup() })
  let consumed = 0
  async function screen(): Promise<string[]> {
    await instance.waitUntilRenderFlush()
    const bytes = stdout.chunks.slice(consumed).join('').replaceAll(CLEAR_TERMINAL, `${CLEAR_TERMINAL}\u001b[${stdout.rows}B`)
      .replaceAll(NEXT_LINE, '\r\n')
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
/** Last screen row containing `text`. The newest line the input should follow. */
const lastRow = (screen: readonly string[], text: string): number => screen.findLastIndex(line => line.includes(text))

/**
 * Assert the resting shape. The newest line, blank rows, the thinking window
 * while one streams, the header — blank, naming the running turn, or holding
 * its summary — the bare rule, the input under it, the base rule, and the
 * status line on the terminal's last rows, over Ink's cursor row.
 * @returns the blank rows between the newest line and what rests on the
 *   input. One, and more while the frame holds rows something above the
 *   input gave up.
 */
function expectInputUnder(screen: readonly string[], text: string): number {
  const newest = lastRow(screen, text)
  const input = inputRow(screen)
  const dump = screen.join('\n')
  expect(newest, dump).toBeGreaterThanOrEqual(0)
  expect(screen[newest + 1], dump).toBe('')
  const rule = input - 1
  expect(screen[rule], dump).toMatch(/^─+$/)
  const header = rule - 1
  expect(header, dump).toBeGreaterThan(newest + 1)
  expect(screen[header], dump).toMatch(new RegExp(`^((> |${SPINNER_REST} )\\S+….*|[✓■✗] .*|)$`))
  // The thinking window while there is one, one blank row above the header.
  // nothing else between.
  let bottom = header
  if (screen[header - 1] === '' && screen[header - 2] !== undefined && header - 2 > newest && screen[header - 2] !== '') bottom = header - 1
  let first = bottom
  while (first > newest + 1 && screen[first - 1] !== '') first--
  expect(screen.slice(newest + 1, first).every(line => line === ''), dump).toBe(true)
  expect(bottom - first, dump).toBeLessThanOrEqual(THINKING_ROWS)
  expect(screen[input]!.startsWith('> '), dump).toBe(true)
  expect(screen[input + 1], dump).toMatch(/^─+$/)
  expect(screen[input + 2], dump).toMatch(/^ {2}Model: /)
  expect(input + 3, dump).toBe(screen.length - 1)
  return first - newest - 1
}

describe('composer placement', () => {
  it('refits a running step to the new size when the terminal is resized mid-step', async () => {
    const calls: ToolCallRow[] = Array.from({ length: 10 }, (_, index) => ({
      kind: 'tool-call', callId: `c${index}`, tool: 'read', input: `src/file${index}.ts`,
      ...index === 9 ? {} : { result: { ok: true, text: Array.from({ length: 20 }, (_, line) => `line ${line} of ${index}`).join('\n') } },
    }))
    const ui = await mount(120, 50, { status: 'running', live: [{ kind: 'tool-group', calls }] })
    for (const [columns, rows] of [[120, 50], [50, 14], [30, 9], [90, 30]] as const) {
      const screen = columns === 120 ? await ui.screen() : await ui.resize(columns, rows)
      const dump = screen.join('\n')
      const input = inputRow(screen)
      expect(screen[input - 2], dump).toMatch(new RegExp(`^${SPINNER_REST} \\S+…`))
      expect(screen.some(line => /^● read 10/.test(line)), dump).toBe(true)
      // Nine rows leave the live region one. The step's head, which says the
      // most. Every larger size also keeps the newest call.
      expect(screen.some(line => line.includes('Read(src/file9.ts)')), dump).toBe(rows > 9)
    }
  })

  it.each([[100, 30], [80, 24], [60, 16], [40, 12], [30, 10], [160, 60], [220, 20]])('keeps the header over a running step taller than its window at %ix%i', async (columns, rows) => {
    const ui = await mount(columns, rows, { status: 'running' })
    const calls: ToolCallRow[] = []
    for (let index = 0; index < 12; index++) {
      const call = { kind: 'tool-call', callId: `c${index}`, tool: index % 3 === 0 ? 'edit' : 'read', input: `src/file${index}.ts` } as const
      for (const done of [false, true]) {
        calls[index] = done ? { ...call, result: { ok: true, text: Array.from({ length: 20 }, (_, line) => `line ${line} of ${index}`).join('\n') } } : call
        const screen = await ui.update({ live: [{ kind: 'tool-group', calls: [...calls] }] })
        const dump = screen.join('\n')
        const input = inputRow(screen)
        expect(screen[input - 2], dump).toMatch(new RegExp(`^${SPINNER_REST} \\S+…`))
        // The step's head stays too. The window folds detail, never the line
        // that says what the step is doing.
        expect(screen.some(line => /^● \S+ \d/.test(line)), dump).toBe(true)
      }
    }
  })

  it.each([[80, 24], [40, 10], [24, 3]])('prints the welcome once without displacing input at %ix%i', async (columns, rows) => {
    const ui = await mount(columns, rows, { version: '1.2.3' })
    const first = await ui.screen()
    const input = inputRow(first)
    expect(input).toBeGreaterThanOrEqual(0)
    expect((await ui.history()).join('\n')).toContain('BAKE  v1.2.3')
    const committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'First prompt' }])
    expect(inputRow(await ui.update({ committed, status: 'running' }))).toBe(input)
    await ui.update({ status: 'idle' })
    const history = (await ui.history()).join('\n')
    expect(history.split('BAKE  v1.2.3')).toHaveLength(2)
    expect(history).toContain('First prompt')
    expect(stdoutClears(ui.stdout)).toBe(false)
    const resized = await ui.resize(columns + 10, rows + 4)
    expect(inputRow(resized)).toBeGreaterThanOrEqual(0)
    expect((await ui.history()).join('\n').split('BAKE  v1.2.3')).toHaveLength(2)
  })

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
    // row carries `^`. The prompt marker is no longer on that row.
    const input = screen.findIndex(line => line.includes('fourth▌'))
    expect(input).toBeGreaterThanOrEqual(0)
    // The header yields last. The rule over the input yields just before it,
    // so that rule is present whenever the height allows it.
    if (rows > 4) expect(screen[input - 1]).toMatch(/^─+$/)
    else expect(screen[input - 1]).not.toMatch(/─/)
    expect(screen[rows - 1]).toBe('')
    await expect(snapshotOf(screen)).toMatchFileSnapshot(`./expected/composer-short.${columns}x${rows}.txt`)
  })

  it('opens on the bottom rows under the session heading, below what the shell printed', async () => {
    const ui = await mount(80, 24)
    const screen = await ui.screen()
    // The frame grew up from the bottom row, scrolling the shell's line away.
    expect((await ui.history())[0]).toBe('$ bake')
    expect(screen.slice(0, lastRow(screen, 'Session: screen')).every(line => line === '')).toBe(true)
    expect(expectInputUnder(screen, 'Session: screen')).toBe(1)
    expect(inputRow(screen)).toBe(24 - 4)
    expect(screen[22]).toBe('  Model: model  /workspace')
  })

  it.each([[80, 24], [40, 10], [120, 40]])('holds the input on the bottom row through streaming and commits at %ix%i', async (columns, rows) => {
    const ui = await mount(columns, rows)
    await ui.screen()
    let committed = emptyTranscript
    for (let turn = 0; turn < 3; turn++) {
      committed = appendTranscript(committed, [{ kind: 'user', text: `Prompt ${turn}` }])
      expectInputUnder(await ui.update({ committed, status: 'running' }), `Prompt ${turn}`)
      for (const count of [1, 4, 18]) {
        const text = Array.from({ length: count }, (_, index) => `Response ${turn} line ${index}`).join('\n')
        expectInputUnder(await ui.update({ live: [{ kind: 'assistant', text }] }), `Response ${turn} line ${count - 1}`)
      }
      // One committed row replaces an answer the window drew at full height.
      // The frame keeps the rows that answer gave up, so the composer does not rise.
      committed = appendTranscript(committed, [{ kind: 'assistant', text: `Response ${turn} final` }])
      expectInputUnder(await ui.update({ committed, live: [], status: 'idle' }), `Response ${turn} final`)
    }
    expect(stdoutClears(ui.stdout)).toBe(false)
  })

  it('starts short live output directly below history, with the controls resting on the bottom', async () => {
    const ui = await mount(80, 24)
    await ui.screen()
    const frame = await ui.update({ status: 'running', live: [{ kind: 'assistant', text: 'First response' }] })
    const heading = lastRow(frame, 'Session: screen')
    expect(lastRow(frame, 'First response')).toBe(heading + 2)
    // Blank, then the header naming the turn, sitting on the rule over the input.
    expect(frame[heading + 4]).toMatch(new RegExp(`^${SPINNER_REST} \\S+…  writing$`))
    expect(frame[heading + 5]).toMatch(/^─+$/)
    expect(inputRow(frame)).toBe(heading + 6)
    expect(expectInputUnder(frame, 'First response')).toBe(1)
  })

  it('rests at the terminal bottom once history fills the screen', async () => {
    const ui = await mount(80, 24)
    await ui.screen()
    const committed = appendTranscript(emptyTranscript,
      Array.from({ length: 30 }, (_, index) => ({ kind: 'user' as const, text: `Prompt ${index}` })))
    await ui.update({ committed })
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('Prompt 29'))
    const screen = await ui.screen()
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
      expect(expectInputUnder(screen, `FIN-${line}.`)).toBe(1)
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
    // A reflowing terminal re-wraps the full-width surface rows of the last
    // frame, so Ink's line-counted erase misses rows of it on every narrowing.
    const ui = await mount(80, 24)
    await ui.screen()
    const committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'Prompt one' }, { kind: 'assistant', text: 'Answer one' }])
    await ui.update({ committed })
    for (const [columns, rows] of [[60, 24], [45, 24], [100, 24], [40, 10]] as const) {
      const screen = await ui.resize(columns, rows)
      const dump = `${columns}x${rows}:\n${screen.join('\n')}`
      expect(screen.filter(line => line.includes('Model: ')), dump).toHaveLength(1)
      expect(screen.filter(line => line.includes('▌')), dump).toHaveLength(1)
      expectInputUnder(screen, 'Answer one')
    }
    const history = (await ui.history()).join('\n')
    expect(history.split('Prompt one')).toHaveLength(2)
    expect(history.split('▌')).toHaveLength(2)
  })

  it('keeps a wrapped draft regular and inside its surface through resizes', async () => {
    const ui = await mount(80, 24)
    await ui.screen()
    await ui.update({ committed: appendTranscript(emptyTranscript, [{ kind: 'assistant', text: 'Answer one' }]) })
    const words = `START ${'alpha 你好 beta '.repeat(10)}/a/very/long/path/that/does/not/fit/anywhere.ts\tEND`
    ui.stdin.write(`\u001b[200~${words}\u001b[201~`)
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('END▌'))
    for (const [columns, rows] of [[60, 24], [45, 16], [39, 12], [100, 30], [41, 10], [80, 24]] as const) {
      const screen = await ui.resize(columns, rows)
      const dump = `${columns}x${rows}:\n${screen.join('\n')}`
      expect(screen.filter(line => line.includes('▌')), dump).toHaveLength(1)
      // Between the rule and the base rule, the draft's text starts at the
      // prompt column, and no row it wraps onto opens with a stray space.
      const caret = screen.findIndex(line => line.includes('▌'))
      let start = caret
      while (start > 0 && !screen[start - 1]!.startsWith('─')) start--
      expect(screen[caret + 1], dump).toMatch(/^─+$/)
      expect(screen[caret + 2], dump).toMatch(/^ {2}Model: /)
      const draft = screen.slice(start, caret + 1)
      expect(draft.length, dump).toBeGreaterThan(1)
      for (const line of draft) expect(line, dump).toMatch(/^(> |\^ | {2})\S/)
    }
    // A tab reaches the terminal as spaces, the width the layout measured.
    expect(ui.stdout.chunks.join('')).not.toContain('\t')
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
    // The blank opens the stack under the heading; the list sits on the header.
    expect(screen[lastRow(screen, 'Session: screen') + 1]).toBe('')
    expect(lastRow(screen, 'more')).toBe(inputRow(screen) - 3)
    expect(screen[inputRow(screen) + 2]).toMatch(/^ {2}Model: /)
    expect(inputRow(screen)).toBe(anchor)
    for (const notice of ['Short notice', 'Long notice\n'.repeat(30)]) {
      screen = await ui.update({ notice, pending: [{ id: 'queued', target: 'next-step', text: 'Next instruction' }] })
      expect(lastRow(screen, 'Session: screen')).toBeLessThan(lastRow(screen, 'Next instruction'))
      expect(lastRow(screen, 'notice')).toBeLessThan(inputRow(screen))
      expect(inputRow(screen)).toBe(anchor)
    }
    await ui.update({ notice: undefined })
    ui.stdin.write('\u001b')
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).not.toContain('/command0'))
    screen = await ui.update({ pending: [], quitting: true, status: 'idle' })
    expect(lastRow(screen, dictionaries.en.quit)).toBe(inputRow(screen) - 3)
    expect(inputRow(screen)).toBe(anchor)
    // Everything closed, the input stays where it was, under the header that
    // holds the finished turn's summary, and the rows the panels gave up are
    // blank until printed history takes them.
    screen = await ui.update({ quitting: false })
    expect(inputRow(screen)).toBe(anchor)
    expect(screen[anchor - 2]).toMatch(/^✓ /)
    expect(expectInputUnder(screen, 'Session: screen')).toBeGreaterThan(1)
    const committed = appendTranscript(emptyTranscript,
      Array.from({ length: 30 }, (_, index) => ({ kind: 'user' as const, text: `Prompt ${index}` })))
    screen = await ui.update({ committed })
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('Prompt 29'))
    screen = await ui.screen()
    expect(inputRow(screen)).toBe(anchor)
    expect(expectInputUnder(screen, 'Prompt 29')).toBe(1)
    expect(stdoutClears(ui.stdout)).toBe(false)
  })

  it('keeps compaction progress above the draft without moving the composer off short screens', async () => {
    const ui = await mount(80, 8)
    ui.stdin.write('keep this draft')
    await vi.waitFor(async () => expect((await ui.screen()).join('\n')).toContain('keep this draft▌'))
    for (const [compactPhase, label] of [
      ['preparing', dictionaries.en.compactPreparing],
      ['summarizing', dictionaries.en.compactSummarizing],
      ['saving', dictionaries.en.compactSaving],
    ] as const) {
      const screen = await ui.update({ command: '/compact', compactPhase })
      expect(screen.join('\n')).toContain('Compacting history…')
      expect(screen.join('\n')).toContain(label)
      // The progress is the turn's to say; the status row still names the model.
      expect(screen[inputRow(screen) + 2]).toMatch(/^ {2}Model: /)
      expect(screen.join('\n')).toContain('keep this draft▌')
      expect(lastRow(screen, 'Compacting history…')).toBeLessThan(inputRow(screen))
    }
    expect(stdoutClears(ui.stdout)).toBe(false)
    const short = await ui.resize(40, 4)
    expect(short.join('\n')).toContain('keep this draft▌')
    const resized = ui.stdout.chunks.length
    expect((await ui.update({ compactPhase: 'summarizing' })).join('\n')).toContain('keep this draft▌')
    expect(ui.stdout.chunks.slice(resized).join('')).not.toContain('\u001b[2J')
  })
})

/**
 * A screen as a file snapshot, without the empty rows under the frame. The
 * tests assert those rows directly, and a file ending in blank lines fails
 * the repository's whitespace check.
 */
function snapshotOf(screen: readonly string[]): string {
  return screen.join('\n').trimEnd() + '\n'
}

function stdoutClears(stdout: Output): boolean {
  return /\u001b\[2J/.test(stdout.chunks.join(''))
}


it.each([[80, 24], [40, 10], [40, 4]])('bounds child branches and inspection at %i by %i', async (columns, rows) => {
  const terminal = await mount(columns!, rows!)
  const subagents = Array.from({ length: 12 }, (_, index) => ({ id: `child-${index}`, label: `Review child ${index}`,
    state: 'working' as const, detail: 'Continuable', inspectable: true }))
  let screen = await terminal.update({ subagents })
  expect(inputRow(screen), screen.join('\n')).toBeGreaterThanOrEqual(0)
  if (rows! >= 10) expect(inputRow(screen), screen.join('\n')).toBe(rows! - 4)
  screen = await terminal.update({ inspection: { sessionId: 'child-0', label: 'Review child 0',
    committed: appendTranscript(emptyTranscript, [{ kind: 'assistant', text: 'Child history' }]),
    live: [{ kind: 'assistant', text: 'Child is still reviewing' }], status: 'running', model: 'mock/child' } })
  expect(inputRow(screen), screen.join('\n')).toBeGreaterThanOrEqual(0)
  if (rows! >= 10) expect(inputRow(screen), screen.join('\n')).toBe(rows! - 4)
  screen = await terminal.update({ inspection: undefined })
  expect(inputRow(screen), screen.join('\n')).toBeGreaterThanOrEqual(0)
  if (rows! >= 10) expect(inputRow(screen), screen.join('\n')).toBe(rows! - 4)
  expect(screen.join('\n')).not.toContain('Session navigation')
})
