/** Turns replayed through the runner's output, row by row as a terminal without synchronized output may show them. */
import { EventEmitter } from 'node:events'
import React from 'react'
import { render } from 'ink'
import xterm from '@xterm/headless'
import { afterEach, describe, expect, it } from 'vitest'
import { App, type AppProps } from '@dsh-tui/ui/app.tsx'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { appendTranscript, emptyTranscript } from '@dsh-tui/ui/transcript.ts'
import type { Row } from '@dsh-tui/ui'
import { frameOutput, scrolling } from '../src/output.ts'
import { LiveBlocks } from '../src/live.ts'
import { Printed } from '../src/printed.ts'
import { caretRow } from '../../../tests/caret.ts'

class Input extends EventEmitter {
  isTTY = true
  setEncoding() {}
  setRawMode() {}
  resume() {}
  pause() {}
  ref() {}
  unref() {}
  read() { return null }
}

class Output extends EventEmitter {
  isTTY = true
  writes: string[] = []
  constructor(public columns: number, public rows: number) { super() }
  write(chunk: string, callback?: () => void) { this.writes.push(chunk); callback?.(); return true }
}

const disposers: (() => void)[] = []
afterEach(() => { for (const dispose of disposers.splice(0).reverse()) dispose() })

const COLUMNS = 80
const ROWS = 24

/**
 * Stream an answer on a full screen and replay what reached the terminal.
 * @param wrapped - whether Ink writes through {@link frameOutput}.
 * @returns the most rows any render left blank part-way through, beyond the
 *   blank rows before and after it; the writes made; and the final scrollback.
 */
async function turn(wrapped: boolean): Promise<{ readonly deficit: number, readonly screens: number, readonly history: string[] }> {
  const terminal = new xterm.Terminal({ cols: COLUMNS, rows: ROWS, convertEol: true, allowProposedApi: true })
  disposers.push(() => terminal.dispose())
  const stdout = new Output(COLUMNS, ROWS)
  const output = frameOutput(stdout as unknown as NodeJS.WriteStream, stdout as unknown as NodeJS.WriteStream)
  const streams = wrapped ? output : { out: stdout as unknown as NodeJS.WriteStream, err: stdout as unknown as NodeJS.WriteStream }
  const history: Row[] = Array.from({ length: 40 }, (_, index) => ({ kind: 'notice', tone: 'info', text: `earlier ${index}` }))
  let state: AppProps = {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 4,
    committed: appendTranscript(emptyTranscript, history), live: [], pending: [], status: 'running', stopping: false,
    command: undefined, notice: undefined, interaction: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'output', copy: dictionaries.en,
    frame: 'round', quitting: false, context: undefined,
    onSubmit: () => {}, onCancel: () => {}, onInterrupt: () => {}, onAnswer: () => {},
  }
  const instance = render(<App {...state} />, {
    stdout: streams.out, stderr: streams.err, stdin: new Input() as unknown as NodeJS.ReadStream,
    patchConsole: false, exitOnCtrlC: false, interactive: true, incrementalRendering: true,
  })
  disposers.push(() => { instance.unmount(); instance.cleanup() })
  const filled = () => Array.from({ length: ROWS }, (_, row) =>
    caretRow(terminal.buffer.active.getLine(terminal.buffer.active.viewportY + row), terminal.cols))
    .filter(line => line.trim() !== '').length
  let consumed = 0
  let deficit = 0
  const settle = async (): Promise<void> => {
    await instance.waitUntilRenderFlush()
    const before = filled()
    const seen: number[] = []
    // A row at a time. Where a terminal that ignores mode 2026 may present
    // the screen. Every piece ends at a newline, so no escape is split.
    // Unwrapped writes still step over unchanged rows as the runner does, so
    // the two differ only in how a region is erased.
    for (const piece of stdout.writes.slice(consumed).map(write => wrapped ? write : scrolling(write)).flatMap(write => write.split(/(?<=\n)/))) {
      await new Promise<void>(resolve => terminal.write(piece, resolve))
      seen.push(filled())
    }
    consumed = stdout.writes.length
    if (seen.length > 0) deficit = Math.max(deficit, Math.min(before, filled()) - Math.min(...seen))
  }
  await settle()
  const blocks = new LiveBlocks()
  const printed = new Printed()
  let committed = state.committed
  const answer = ['First paragraph line that finishes.', '', 'Second paragraph, one line.', '', '- a list item', '- another', '', 'Done.'].join('\n')
  blocks.push({ type: 'block-start', index: 0, blockType: 'text' })
  for (const token of answer.match(/\s*\S+/g) ?? []) {
    blocks.push({ type: 'text-delta', index: 0, text: token })
    const { print, live } = printed.split(blocks.keyed())
    committed = appendTranscript(committed, print)
    state = { ...state, committed, live }
    instance.rerender(<App {...state} />)
    await settle()
  }
  committed = appendTranscript(committed, printed.reconcile([{ kind: 'assistant', text: answer }]))
  state = { ...state, committed, live: [], status: 'idle' }
  instance.rerender(<App {...state} />)
  await settle()
  const buffer = terminal.buffer.active
  return {
    deficit,
    screens: stdout.writes.length,
    history: Array.from({ length: buffer.length }, (_, row) => caretRow(buffer.getLine(row), terminal.cols)),
  }
}

describe('frame output', () => {
  it('removes styles but reverse video for NO_COLOR while preserving terminal controls and write order', () => {
    const stdout = new Output(80, 24)
    const stderr = new Output(80, 24)
    const output = frameOutput(stdout as unknown as NodeJS.WriteStream, stderr as unknown as NodeJS.WriteStream, false)
    const modes = '\x1b[?2026h\x1b[?25l\x1b[?2004h'
    output.out.write(modes + '\x1b[38;2;96;165;250mfile.ts\x1b[39m\n')
    output.err.write('\x1b[31merror\x1b[0m\n')
    output.out.write('\x1b[2A\x1b[1mbold\x1b[22m\x1b[7m \x1b[27m\x1b[?2026l')
    output.flush()
    // Reverse video, which draws the caret, is no colour and stays.
    expect(stdout.writes).toEqual(['\x1b[24B' + modes + 'file.ts\n', '\x1b[2Abold\x1b[7m \x1b[27m\x1b[?2026l'])
    expect(stderr.writes).toEqual(['error\n'])
  })

  it('never shows the controls erased while a streamed answer prints', async () => {
    const raw = await turn(false)
    const framed = await turn(true)
    // Without frameOutput, each printed line erases at least the rule, the
    // composer, and the status line before drawing them again.
    expect(raw.deficit).toBeGreaterThanOrEqual(3)
    // Drawn over, at most the row being written is ever blank.
    expect(framed.deficit).toBeLessThanOrEqual(1)
    // One write per render, and the same scrollback at the end, under the
    // blank rows the framed session started below.
    expect(framed.screens).toBeLessThan(raw.screens)
    expect(framed.history.slice(framed.history.findIndex(line => line !== ''))).toEqual(raw.history)
  })
})

/**
 * Where a write erases to the end of a row after drawing on it. The cursor
 * rests on a full-width row's last cell until the next character wraps it,
 * and a terminal that does not defer the wrap, as Warp does not, erases that
 * cell. A row is drawn from its first column after a newline, a return, or a
 * move to a column or position.
 * @param bytes - what reached the terminal.
 * @returns the text drawn before each such erase.
 */
function erasedAfterText(bytes: string): string[] {
  const found: string[] = []
  let drawn = ''
  for (const token of bytes.match(/\u001B\[[\d;?]*[\x40-\x7e]|\u001B[^[]|[^\u001B]/gu) ?? []) {
    if (token === '\n' || token === '\r' || /^\u001B\[[\d;]*[GEFHf]$/u.test(token)) drawn = ''
    else if (token === '\u001B[K' || token === '\u001B[0K') { if (drawn !== '') found.push(drawn); drawn = '' }
    else if (!token.startsWith('\u001B')) drawn += token
  }
  return found
}

describe('the right edge', () => {
  it('never erases a row after drawing it, so the hint and the goal keep their last cells', async () => {
    const stdout = new Output(COLUMNS, ROWS)
    const output = frameOutput(stdout as unknown as NodeJS.WriteStream, stdout as unknown as NodeJS.WriteStream)
    let state: AppProps = {
      files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
      completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 4,
      committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
      command: undefined, notice: undefined, interaction: undefined,
      model: 'mock/model', cwd: '/workspace', sessionId: 'edge', copy: dictionaries.en,
      frame: 'round', quitting: false, context: undefined,
      onSubmit: () => {}, onCancel: () => {}, onInterrupt: () => {}, onAnswer: () => {},
    }
    const instance = render(<App {...state} />, {
      stdout: output.out, stderr: output.err, stdin: new Input() as unknown as NodeJS.ReadStream,
      patchConsole: false, exitOnCtrlC: false, interactive: true, incrementalRendering: true,
    })
    disposers.push(() => { instance.unmount(); instance.cleanup() })
    const update = async (patch: Partial<AppProps>): Promise<void> => {
      state = { ...state, ...patch }
      instance.rerender(<App {...state} />)
      await instance.waitUntilRenderFlush()
      await new Promise(resolve => setImmediate(resolve))
    }
    await update({})
    // Each of these rewrites the header, whose goal ends in the last column, and the composer.
    await update({ goal: { objective: 'Ship it', phase: 'paused', armed: false, rounds: 3, maxRounds: 256 } })
    await update({ status: 'running' })
    await update({ notice: 'A notice above the composer' })
    await update({ status: 'idle', notice: undefined })
    const bytes = stdout.writes.join('')
    // Ink erases after each row it rewrites; every one reaching the terminal is cleared first.
    expect(bytes).toMatch(/\u001B\[1G\u001B\[K/u)
    expect(erasedAfterText(bytes)).toEqual([])
  })
})

/**
 * Mount the app through {@link frameOutput} in a terminal whose shell has
 * printed one line.
 * @returns a props update that reads the settled screen, and a resize.
 */
async function session(columns: number, rows: number) {
  const terminal = new xterm.Terminal({ cols: columns, rows, convertEol: true, allowProposedApi: true })
  disposers.push(() => terminal.dispose())
  await new Promise<void>(resolve => terminal.write('$ bake\n', resolve))
  const stdout = new Output(columns, rows)
  const output = frameOutput(stdout as unknown as NodeJS.WriteStream, stdout as unknown as NodeJS.WriteStream)
  let state: AppProps = {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 4,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'bottom', copy: dictionaries.en,
    frame: 'round', quitting: false, context: undefined,
    onSubmit: () => {}, onCancel: () => {}, onInterrupt: () => {}, onAnswer: () => {},
  }
  const instance = render(<App {...state} />, {
    stdout: output.out, stderr: output.err, stdin: new Input() as unknown as NodeJS.ReadStream,
    patchConsole: false, exitOnCtrlC: false, interactive: true, incrementalRendering: true,
  })
  disposers.push(() => { instance.unmount(); instance.cleanup() })
  let consumed = 0
  const screen = async (): Promise<string[]> => {
    await instance.waitUntilRenderFlush()
    await new Promise(resolve => setImmediate(resolve))
    const bytes = stdout.writes.slice(consumed).join('')
    consumed = stdout.writes.length
    if (bytes !== '') await new Promise<void>(resolve => terminal.write(bytes, resolve))
    const buffer = terminal.buffer.active
    return Array.from({ length: stdout.rows }, (_, row) => caretRow(buffer.getLine(buffer.viewportY + row), terminal.cols))
  }
  return {
    screen,
    state: () => state,
    async update(patch: Partial<AppProps>): Promise<string[]> {
      state = { ...state, ...patch }
      instance.rerender(<App {...state} />)
      return screen()
    },
    async resize(width: number, height: number): Promise<string[]> {
      terminal.resize(width, height)
      stdout.columns = width
      stdout.rows = height
      stdout.emit('resize')
      let written = -1
      while (written !== stdout.writes.length) {
        written = stdout.writes.length
        await screen()
        await new Promise(resolve => setTimeout(resolve, 50))
      }
      return screen()
    },
    async history(): Promise<string[]> {
      await screen()
      const buffer = terminal.buffer.active
      return Array.from({ length: buffer.length }, (_, row) => caretRow(buffer.getLine(row), terminal.cols))
    },
  }
}

const inputRow = (screen: readonly string[]): number => screen.findIndex(line => line.startsWith('> ') || line.startsWith('^ '))

describe('bottom anchoring', () => {
  it('releases a closed code block into complete scrollback while the response is still streaming', async () => {
    const ui = await session(COLUMNS, ROWS)
    const blocks = new LiveBlocks()
    const printed = new Printed()
    const code = Array.from({ length: 30 }, (_, index) => `console.log("code-line-${String(index).padStart(2, '0')}")`)
    const open = `\`\`\`js\n${code.join('\n')}\n`
    const closing = '```\n'
    const feed = async (text: string): Promise<string[]> => {
      blocks.push({ type: 'text-delta', index: 0, text })
      const { print, live } = printed.split(blocks.keyed())
      return ui.update({ status: 'running', committed: appendTranscript(ui.state().committed, print), live })
    }
    const preview = await feed(open)
    expect(preview.join('\n')).not.toContain(code[0])
    expect(preview.join('\n')).toContain(code.at(-1))
    const closed = await feed(closing)
    expect(inputRow(closed)).toBe(ROWS - 4)
    const history = await ui.history()
    for (const line of code) expect(history.filter(row => row.trim() === line)).toHaveLength(1)
    expect(history.join('\n')).not.toContain('```')
    const committed = appendTranscript(ui.state().committed,
      printed.reconcile([{ kind: 'assistant', text: open + closing }]))
    const done = await ui.update({ committed, live: [], status: 'idle' })
    expect(inputRow(done)).toBe(ROWS - 4)
    const final = await ui.history()
    for (const line of code) expect(final.filter(row => row.trim() === line)).toHaveLength(1)
  })

  it('rests the composer on the bottom row from the first frame, through a turn and its panels', async () => {
    const ui = await session(COLUMNS, ROWS)
    // The base rule, the status line, and Ink's cursor row under it.
    const bottom = ROWS - 4
    const rows: number[] = []
    const record = (screen: readonly string[]): void => { rows.push(inputRow(screen)) }
    const first = await ui.screen()
    record(first)
    // Above the input, bottom to top. The upper rule, the header, and the
    // gap that opens the chrome. The session heading is the row above that.
    expect(first.findLastIndex(line => line.includes('Session: bottom'))).toBe(bottom - 4)
    let committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'Fix the loader.' }])
    record(await ui.update({ committed, status: 'running', live: [{ kind: 'reasoning', text: 'Where is the home read?\nIn startup, then again in the launcher.' }] }))
    record(await ui.update({ pending: [
      { id: 'p1', target: 'next-turn', text: 'Then thread the home.' }, { id: 'p2', target: 'next-turn', text: 'And test it.' },
    ] }))
    record(await ui.update({ notice: 'Model set for the next turn' }))
    // Rows given up with nothing printed to take them.
    record(await ui.update({ notice: undefined }))
    record(await ui.update({ pending: [{ id: 'p2', target: 'next-turn', text: 'And test it.' }] }))
    // Reasoning commits as its preview while the answer starts.
    committed = appendTranscript(committed, [{ kind: 'reasoning', text: 'Where is the home read?\nIn startup, then again in the launcher.' }])
    record(await ui.update({ committed, live: [{ kind: 'assistant', text: 'Reading it' }] }))
    record(await ui.update({ live: [{ kind: 'assistant', text: 'Reading it now.' }, { kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'grep -n DSH_HOME' }] }))
    committed = appendTranscript(committed, [
      { kind: 'assistant', text: 'Reading it now.' },
      { kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'grep -n DSH_HOME' },
      { kind: 'tool-result', callId: 'c1', ok: true, text: 'startup.ts:4\nlauncher.ts:9' },
    ])
    record(await ui.update({ committed, live: [], pending: [] }))
    committed = appendTranscript(committed, [{ kind: 'assistant', text: 'Both reads found.' }])
    const idle = await ui.update({ committed, status: 'idle' })
    record(idle)
    expect(rows, idle.join('\n')).toEqual(rows.map(() => bottom))
    // The shell's line and the session heading are each in history once.
    const history = (await ui.history()).join('\n')
    expect(history.split('$ bake')).toHaveLength(2)
    expect(history.split('Session: bottom')).toHaveLength(2)
  })

  it('rewraps a live answer on both shrink and grow without moving the composer', async () => {
    const ui = await session(160, 24)
    const answer = `${'alpha '.repeat(18)}DONE`
    const committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'Prompt' }])
    const wide = await ui.update({ committed, status: 'running', live: [{ kind: 'assistant', text: answer }] })
    expect(wide.filter(line => line.includes('alpha'))).toHaveLength(1)
    const narrow = await ui.resize(40, 24)
    expect(narrow.filter(line => line.includes('alpha')).length, narrow.join('\n')).toBeGreaterThan(1)
    expect(narrow.join('\n')).toContain('DONE')
    expect(inputRow(narrow)).toBe(24 - 4)
    const grown = await ui.resize(120, 24)
    expect(grown.filter(line => line.includes('alpha'))).toHaveLength(1)
    expect(inputRow(grown)).toBe(24 - 4)
    const done = await ui.update({ committed: appendTranscript(committed, [{ kind: 'assistant', text: answer }]), live: [], status: 'idle' })
    expect(inputRow(done)).toBe(24 - 4)
    expect((await ui.history()).join('\n').split('DONE')).toHaveLength(2)
  })

  it('returns the composer to the bottom row when a resize replays history', async () => {
    const ui = await session(COLUMNS, ROWS)
    await ui.update({ committed: appendTranscript(emptyTranscript, [{ kind: 'user', text: 'One prompt' }, { kind: 'assistant', text: 'One answer' }]) })
    for (const [columns, rows] of [[60, 24], [60, 30], [40, 12], [80, 24]] as const) {
      const screen = await ui.resize(columns, rows)
      const dump = `${columns}x${rows}:\n${screen.join('\n')}`
      expect(inputRow(screen), dump).toBe(rows - 4)
      // One copy of the input. The replay left no rows of the old frame behind.
      expect(screen.filter(line => line.includes('▌')), dump).toHaveLength(1)
      expect(screen.findLastIndex(line => line.includes('One answer')), dump).toBeGreaterThan(0)
    }
    const history = (await ui.history()).join('\n')
    expect(history.split('One answer')).toHaveLength(2)
  })
})
