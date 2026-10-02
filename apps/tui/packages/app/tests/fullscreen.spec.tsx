/** Real Ink writes through the fullscreen stream, interpreted by a terminal emulator. */
import { EventEmitter } from 'node:events'
import React from 'react'
import { render } from 'ink'
import xterm from '@xterm/headless'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { App, type AppProps } from '@dsh-tui/ui/app.tsx'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { appendTranscript, emptyTranscript } from '@dsh-tui/ui/transcript.ts'
import type { Row } from '@dsh-tui/ui/rows.ts'
import { frameOutput } from '../src/output.ts'
import { Printed } from '../src/printed.ts'

class Input extends EventEmitter {
  isTTY = true
  raw = false
  private data: string | null = null
  setEncoding() {}
  setRawMode(value: boolean) { this.raw = value }
  resume() {}
  pause() {}
  ref() {}
  unref() {}
  read() { const value = this.data; this.data = null; return value }
  send(value: string) { this.data = value; this.emit('readable') }
}

class Output extends EventEmitter {
  isTTY = true
  chunks: string[] = []
  constructor(public columns: number, public rows: number) { super() }
  write(text: string, callback?: () => void) { this.chunks.push(text); callback?.(); return true }
}

const disposers: (() => Promise<void>)[] = []
let beforeExitListeners = 0
beforeEach(() => { beforeExitListeners = process.listenerCount('beforeExit') })
afterEach(async () => {
  for (const dispose of disposers.splice(0).reverse()) await dispose()
  expect(process.listenerCount('beforeExit')).toBe(beforeExitListeners)
})

async function mount(overrides: Partial<AppProps> = {}, columns = 60, rows = 18) {
  const input = new Input()
  const stdout = new Output(columns, rows)
  const output = frameOutput(stdout as unknown as NodeJS.WriteStream, stdout as unknown as NodeJS.WriteStream, false, 'fullscreen')
  const terminal = new xterm.Terminal({ cols: columns, rows, convertEol: true, allowProposedApi: true })
  let ui: ReturnType<typeof render> | undefined
  let exited: ReturnType<ReturnType<typeof render>['waitUntilExit']> | undefined
  disposers.push(async () => {
    ui?.cleanup()
    await exited
    output.flush()
    terminal.dispose()
  })
  await new Promise<void>(resolve => terminal.write('shell before Bake\r\n$ ', resolve))
  const shellCursor = { x: terminal.buffer.active.cursorX, y: terminal.buffer.active.cursorY }
  let state: AppProps = {
    screen: 'fullscreen', frame: 'round', copy: dictionaries.en,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'fullscreen', quitting: false, context: undefined,
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 4,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
  ui = render(<App {...state} />, {
    stdout: output.out, stderr: output.err, stdin: input as unknown as NodeJS.ReadStream,
    interactive: true, alternateScreen: true, incrementalRendering: true, patchConsole: false, exitOnCtrlC: false, isScreenReaderEnabled: false,
  })
  exited = ui.waitUntilExit()
  let consumed = 0
  const capture = async (): Promise<string[]> => {
    await ui!.waitUntilRenderFlush()
    output.flush()
    const text = stdout.chunks.slice(consumed).join('')
    consumed = stdout.chunks.length
    if (text !== '') await new Promise<void>(resolve => terminal.write(text, resolve))
    const buffer = terminal.buffer.active
    return Array.from({ length: stdout.rows }, (_, index) => buffer.getLine(buffer.viewportY + index)?.translateToString(true) ?? '')
  }
  const check = async (assertion: (lines: readonly string[]) => void): Promise<string[]> => {
    let lines: string[] = []
    await vi.waitFor(async () => { lines = await capture(); assertion(lines) })
    return lines
  }
  await check(lines => expect(lines.join('\n')).toContain('▌'))
  return {
    input, stdout, terminal, capture, check, shellCursor,
    update(patch: Partial<AppProps>) { state = { ...state, ...patch }; ui!.rerender(<App {...state} />) },
    resize(width: number, height: number) {
      terminal.resize(width, height)
      stdout.columns = width; stdout.rows = height; stdout.emit('resize')
    },
    async close() { ui!.cleanup(); await exited; return capture() },
  }
}

const history = (count: number) => appendTranscript(emptyTranscript,
  Array.from({ length: count }, (_, index): Row => ({ kind: 'notice', tone: 'info', text: `History ${index}` })))
const caret = (lines: readonly string[]) => lines.findIndex(line => line.includes('▌'))
const snapshot = (lines: readonly string[]) => `${lines.map((line, index) => `${String(index).padStart(2, '0')} |${line}|`).join('\n')}\n`

it.each(['en'] as const)('pages history without moving the composer, holds appends, and resumes following (%s)', async locale => {
  let committed = history(80)
  const copy = dictionaries[locale]
  const view = await mount({ committed, copy })
  const initial = await view.check(lines => expect(lines.join('\n')).toContain('History 79'))
  expect(view.terminal.buffer.active.type).toBe('alternate')
  expect(caret(initial)).toBe(15)
  expect(initial.at(-1)).toBe('  model  /workspace')
  await expect(snapshot(initial)).toMatchFileSnapshot(`./expected/fullscreen.${locale}.txt`)
  view.input.send('saved draft')
  await view.check(lines => expect(lines.join('\n')).toContain('saved draft▌'))
  view.input.send('\x1b[5~')
  const paused = await view.check(lines => expect(lines.join('\n')).toContain(copy.transcriptPaused))
  expect(caret(paused)).toBe(15)
  expect(paused[0]).not.toBe(initial[0])
  committed = appendTranscript(committed, [{ kind: 'notice', tone: 'info', text: 'new output' }])
  view.update({ committed, live: [{ kind: 'assistant', text: 'streaming response' }], status: 'running' })
  await view.check(lines => { expect(lines[0]).toBe(paused[0]); expect(lines.join('\n')).toContain('saved draft▌') })
  view.input.send('\x1b[1;5F')
  await view.check(lines => {
    expect(lines.join('\n')).toContain('streaming response')
    expect(lines.join('\n')).toContain(copy.transcriptScroll)
    expect(caret(lines)).toBe(15)
  })
  view.input.send('\x1b[1;5H')
  await view.check(lines => expect(lines.join('\n')).toContain(`${copy.session}: fullscreen`))
  view.input.send('\x1b[6~')
  await view.check(lines => expect(lines.join('\n')).not.toContain(`${copy.session}: fullscreen`))
})

const wheel = (direction: 'up' | 'down', column = 5, row = 5) => `\x1b[<${direction === 'up' ? 64 : 65};${column};${row}M`

it('scrolls by wheel rows without typing reports into the draft, and resumes following at the bottom', async () => {
  const copy = dictionaries.en
  const view = await mount({ committed: history(80) })
  const initial = await view.check(lines => expect(lines.join('\n')).toContain('History 79'))
  view.input.send('draft')
  await view.check(lines => expect(lines.join('\n')).toContain('draft▌'))
  view.input.send(wheel('up'))
  const scrolled = await view.check(lines => {
    expect(lines.join('\n')).toContain(copy.transcriptPaused)
    expect(lines.join('\n')).toContain(copy.transcriptLatest)
  })
  // One notch is three rows, not a page: the view moved by exactly that.
  expect(scrolled.slice(3, 10)).toEqual(initial.slice(0, 7))
  expect(caret(scrolled)).toBe(15)
  // A Shift-modified notch and a click's press and release are reports too.
  view.input.send('\x1b[<68;5;5M')
  view.input.send('\x1b[<0;5;5M')
  view.input.send('\x1b[<0;5;5m')
  await view.check(lines => expect(lines.slice(6, 10)).toEqual(initial.slice(0, 4)))
  view.input.send(wheel('down'))
  view.input.send(wheel('down'))
  await view.check(lines => {
    expect(lines.join('\n')).toContain('History 79')
    expect(lines.join('\n')).toContain(copy.transcriptScroll)
    expect(lines.join('\n')).toContain('draft▌')
    expect(lines.join('\n')).not.toContain('[<')
  })
})

it('keeps bracketed paste owned while a fullscreen sheet is open', async () => {
  const view = await mount({ todos: [{ text: 'Task', status: 'pending' }] })
  view.input.send('\x14')
  await view.check(lines => expect(lines.join('\n')).toContain(dictionaries.en.sheetClose))
  view.input.send('\x1b[200~ignored while the sheet is open\x1b[201~')
  await view.check(lines => {
    expect(lines.join('\n')).not.toContain('ignored while the sheet is open')
    expect(lines.join('\n')).not.toContain('[200~')
  })
  view.input.send('\x1b')
  await view.check(lines => expect(lines.join('\n')).not.toContain(dictionaries.en.sheetClose))
  view.input.send('\x1b[200~accepted after the sheet closes\x1b[201~')
  await view.check(lines => expect(lines.join('\n')).toContain('accepted after the sheet closes▌'))
  expect(view.stdout.chunks.join('')).not.toContain('\x1b[?2004l')
})

it.each(['en'] as const)('offers the way back to the latest output and marks output that arrived below (%s)', async locale => {
  const copy = dictionaries[locale]
  let committed = history(80)
  const view = await mount({ committed, copy })
  await view.check(lines => expect(lines.join('\n')).toContain('History 79'))
  view.input.send('\x1b[5~')
  const paused = await view.check(lines => {
    expect(lines.join('\n')).toContain(copy.transcriptPaused)
    expect(lines.join('\n')).toContain(copy.transcriptLatest)
  })
  // A rerender without new output leaves the offer as it is.
  view.update({ notice: 'unrelated' })
  await view.check(lines => { expect(lines[0]).toBe(paused[0]); expect(lines.join('\n')).toContain(copy.transcriptLatest) })
  view.update({ live: [{ kind: 'assistant', text: 'streaming below' }], status: 'running' })
  const unseen = await view.check(lines => {
    expect(lines[0]).toBe(paused[0])
    expect(lines.join('\n')).toContain(copy.transcriptUnseen)
    expect(lines.join('\n')).not.toContain('streaming below')
  })
  const row = unseen.findIndex(line => line.includes(copy.transcriptUnseen))
  // A click on another row does nothing; on the offer it follows output again.
  view.input.send('\x1b[<0;5;1M')
  await view.check(lines => expect(lines.join('\n')).toContain(copy.transcriptUnseen))
  view.input.send(`\x1b[<0;5;${row + 1}M`)
  await view.check(lines => {
    expect(lines.join('\n')).toContain('streaming below')
    expect(lines.join('\n')).toContain(copy.transcriptScroll)
  })
  // Reading history again starts from what is now on screen.
  committed = appendTranscript(committed, [{ kind: 'notice', tone: 'info', text: 'settled' }])
  view.update({ committed, live: [], status: 'idle' })
  await view.check(lines => expect(lines.join('\n')).toContain('settled'))
  view.input.send(wheel('up'))
  await view.check(lines => {
    expect(lines.join('\n')).toContain(copy.transcriptLatest)
    expect(lines.join('\n')).not.toContain(copy.transcriptUnseen)
  })
})

it('keeps the same passage visible when a paused paragraph rewraps repeatedly', async () => {
  const text = Array.from({ length: 500 }, (_, index) => `word${String(index).padStart(3, '0')}`).join(' ')
  const view = await mount({ committed: appendTranscript(emptyTranscript, [{ kind: 'assistant', text }]) })
  view.input.send('\x1b[5~')
  const paused = await view.check(lines => expect(lines.join('\n')).toContain(dictionaries.en.transcriptPaused))
  const word = paused[0]!.match(/word\d+/)![0]
  for (const width of [37, 81, 60]) {
    view.resize(width, 18)
    await view.check(lines => expect(lines[0]).toContain(word))
  }
  await view.check(lines => expect(lines[0]).toBe(paused[0]))
})

it.each(['committed', 'live'] as const)('keeps a paused table row through column and stacked layouts (%s)', async mode => {
  let text = '| Item | Count | Description |\n| :--- | ---: | :---: |\n'
    + Array.from({ length: 40 }, (_, index) => `| row_${String(index).padStart(2, '0')} | ${index + 100} | entry_${index} with enough detail to wrap in a smaller cell |`).join('\n')
  let committed = mode === 'committed' ? appendTranscript(emptyTranscript, [{ kind: 'assistant', text }]) : emptyTranscript
  const view = await mount({ committed, live: mode === 'live' ? [{ kind: 'assistant', text }] : [],
    status: mode === 'live' ? 'running' : 'idle' }, 80, 18)
  await view.check(lines => expect(lines.join('\n')).toContain('row_39'))
  view.input.send('\x1b[5~')
  const paused = await view.check(lines => expect(lines.join('\n')).toContain(dictionaries.en.transcriptPaused))
  const item = paused[0]!.match(/row_\d+/)?.[0]
  expect(item, paused.join('\n')).toBeDefined()
  expect(paused[0]).toContain('\u2502')
  if (mode === 'live') {
    text += '\n| a_new_item_that_widens_the_first_column | 999 | A longer item makes the preceding descriptions wrap. |'
    view.update({ live: [{ kind: 'assistant', text }] })
    await view.check(lines => expect(lines[0]).toContain(item))
  }
  view.resize(24, 18)
  await view.check(lines => {
    expect(lines[0]).toContain(`Item: ${item}`)
    expect(caret(lines)).toBe(15)
  })
  if (mode === 'live') {
    text += '\n| row_40 | 140 | entry_40 |'
    view.update({ live: [{ kind: 'assistant', text }] })
    await view.check(lines => expect(lines[0]).toContain(`Item: ${item}`))
    const printed = new Printed()
    text += '\n\nTable complete.'
    const split = printed.split([{ key: 0, row: { kind: 'assistant', text } }])
    committed = appendTranscript(committed, split.print)
    view.update({ committed, live: split.live })
    await view.check(lines => expect(lines[0]).toContain(`Item: ${item}`))
    committed = appendTranscript(committed, printed.reconcile([{ kind: 'assistant', text }]))
    view.update({ committed, live: [], status: 'idle' })
    await view.check(lines => expect(lines[0]).toContain(`Item: ${item}`))
  }
  view.resize(80, 18)
  await view.check(lines => {
    expect(lines[0]).toContain(item)
    expect(lines[0]).toContain('\u2502')
    expect(caret(lines)).toBe(15)
  })
})

it('keeps a paused live passage through chunks arriving during and after resize', async () => {
  let text = Array.from({ length: 500 }, (_, index) => `word${String(index).padStart(3, '0')}`).join(' ')
  const view = await mount({ live: [{ kind: 'assistant', text }], status: 'running' })
  view.input.send('\x1b[5~')
  const paused = await view.check(lines => expect(lines.join('\n')).toContain(dictionaries.en.transcriptPaused))
  const word = paused[0]!.match(/word\d+/)![0]
  view.resize(37, 18)
  text += ' more text'
  view.update({ live: [{ kind: 'assistant', text }] })
  await view.check(lines => expect(lines[0]).toContain(word))
  view.resize(81, 18)
  await view.check(lines => expect(lines[0]).toContain(word))
  text += ' and more text'
  view.update({ live: [{ kind: 'assistant', text }] })
  await view.check(lines => expect(lines[0]).toContain(word))
})

it('holds paused live text through settled fragments and the final commit', async () => {
  const printed = new Printed()
  let committed = history(40)
  let text = `\`\`\`text\n${Array.from({ length: 50 }, (_, index) => `stream_${index}`).join('\n')}`
  const view = await mount({ committed, live: [{ kind: 'assistant', text }], status: 'running' })
  view.input.send('\x1b[5~')
  const paused = await view.check(lines => expect(lines.join('\n')).toContain(dictionaries.en.transcriptPaused))
  expect(paused[0]).toContain('stream_')
  for (const suffix of ['\n```\n\nNext paragraph', '\n\nAnother paragraph', ' completed.']) {
    text += suffix
    const split = printed.split([{ key: 0, row: { kind: 'assistant', text } }])
    committed = appendTranscript(committed, split.print)
    view.update({ committed, live: split.live })
    await view.check(lines => expect(lines[0]).toBe(paused[0]))
  }
  committed = appendTranscript(committed, printed.reconcile([{ kind: 'assistant', text }]))
  view.update({ committed, live: [], status: 'idle' })
  await view.check(lines => expect(lines[0]).toBe(paused[0]))
  view.input.send('\x1b[1;5F')
  await view.check(lines => expect(lines.join('\n')).toContain('Another paragraph completed.'))
})

it('reflows history, keeps a reading anchor, and preserves input on tiny terminals', async () => {
  const view = await mount({ committed: history(80) })
  view.input.send('\x1b[5~')
  const paused = await view.check(lines => expect(lines.join('\n')).toContain(dictionaries.en.transcriptPaused))
  view.resize(72,24)
  await view.check(lines => { expect(lines[0]).toBe(paused[0]); expect(caret(lines)).toBe(21) })
  view.input.send('\x1b[1;5F')
  await view.check(lines => expect(lines.join('\n')).toContain('History 79'))
  for (const [columns, rows] of [[40,8], [20,4], [12,1], [80,24]] as const) {
    view.resize(columns,rows)
    view.input.send('x')
    await view.check(lines => {
      expect(caret(lines)).toBeGreaterThanOrEqual(0)
      expect(lines.filter(line => line.includes('▌'))).toHaveLength(1)
      if (rows > 4) expect(lines.at(-1)).toMatch(/^ {2}model/)
    })
  }
})

it('windows a large multiline answer and reflows a wrapped Unicode paragraph', async () => {
  const code = Array.from({ length: 2000 }, (_, index) => `code_${index}`).join('\n')
  const view = await mount({ committed: appendTranscript(emptyTranscript, [{ kind: 'assistant', text: `\`\`\`text\n${code}\n\`\`\`` }]) })
  await view.check(lines => expect(lines.join('\n')).toContain('code_1999'))
  expect(view.stdout.chunks.join('')).not.toContain('code_0')
  view.input.send('\x1b[5~')
  await view.check(lines => expect(lines.join('\n')).toContain('code_1988'))
  view.update({ sessionId: 'unicode', committed: appendTranscript(emptyTranscript, [{ kind: 'assistant', text: `${'你好 **world** 🌟 '.repeat(50)}DONE` }]) })
  await view.check(lines => expect(lines.join('\n')).toContain('DONE'))
  view.resize(30,12)
  const narrowed = await view.check(lines => { expect(lines.join('\n')).toContain('DONE'); expect(caret(lines)).toBe(9) })
  await expect(snapshot(narrowed)).toMatchFileSnapshot('./expected/fullscreen-unicode.txt')
})

it('presents only visited history and does not reread the prefix on appends', async () => {
  const presented = new Set<number>()
  let reads = 0
  const rows = new Proxy(Array.from({ length: 10_000 }, (_, index): Row => ({
    kind: 'assistant', get text() { presented.add(index); return `Answer ${index}` },
  })), {
    get(target, property, receiver) {
      if (typeof property === 'string' && /^\d+$/.test(property)) reads++
      return Reflect.get(target, property, receiver)
    },
  })
  const committed = appendTranscript(emptyTranscript, rows)
  const view = await mount({ committed })
  await view.check(lines => expect(lines.join('\n')).toContain('Answer 9999'))
  expect(presented.size).toBeLessThan(20)
  expect(presented.has(0)).toBe(false)
  reads = 0
  presented.clear()
  view.update({ live: [{ kind: 'assistant', text: 'New live answer' }] })
  await view.check(lines => expect(lines.join('\n')).toContain('New live answer'))
  view.update({ committed: appendTranscript(committed, [{ kind: 'assistant', text: 'New committed answer' }]), live: [] })
  await view.check(lines => expect(lines.join('\n')).toContain('New committed answer'))
  expect(reads).toBe(0)
  expect(presented.size).toBe(0)
  view.input.send('\x1b[1;5H')
  await view.check(lines => expect(lines.join('\n')).toContain('Answer 0'))
  expect(presented.has(0)).toBe(true)
  expect(presented.size).toBeLessThan(20)
})

it('gives sheets and approvals their keys and retains the parent draft after inspection', async () => {
  const answer = vi.fn()
  const view = await mount({ committed: history(50), onAnswer: answer, todos: Array.from({ length: 20 }, (_, i) => ({ text: `Task ${i}`, status: 'pending' })) })
  view.input.send('draft')
  await view.check(lines => expect(lines.join('\n')).toContain('draft▌'))
  view.input.send('\x14')
  await view.check(lines => expect(lines.join('\n')).toContain(dictionaries.en.sheetClose))
  view.input.send('\x1b[6~')
  await view.check(lines => expect(lines.join('\n')).toContain('Task 2'))
  view.input.send(wheel('down'))
  await view.check(lines => expect(lines.join('\n')).toContain('Task 5'))
  view.input.send('\x1b')
  await view.check(lines => { expect(lines.join('\n')).not.toContain(dictionaries.en.sheetClose); expect(lines.join('\n')).toContain('draft▌') })
  view.update({ inspection: { sessionId: 'child', label: 'Child', committed: history(100), live: [], status: 'idle', model: 'mock/model' } })
  await view.check(lines => expect(lines.join('\n')).toContain('History 99'))
  view.input.send('\x1b[1;5H')
  await view.check(lines => expect(lines.join('\n')).toContain('fullscreen > child'))
  view.update({ inspection: undefined })
  await view.check(lines => expect(lines.join('\n')).toContain('draft▌'))
  view.update({ interaction: { id: 42, kind: 'approval', tool: 'bash', reason: 'Run this command?' } })
  await view.check(lines => expect(lines.join('\n')).toContain('Approval required: bash'))
  view.input.send('\x1b[5~')
  view.input.send('y')
  await vi.waitFor(() => expect(answer).toHaveBeenCalledExactlyOnceWith(42, 'allowed-once'))
  view.update({ interaction: undefined })
  await view.check(lines => expect(lines.join('\n')).toContain('draft▌'))
})

it('restores the primary screen, its cursor, paste, and raw mode on exit', async () => {
  const view = await mount({ committed: history(80) })
  view.resize(40,12)
  await view.check(lines => expect(caret(lines)).toBe(9))
  await view.close()
  expect(view.terminal.buffer.active.type).toBe('normal')
  expect(view.terminal.buffer.active.getLine(0)?.translateToString(true)).toBe('shell before Bake')
  expect({ x: view.terminal.buffer.active.cursorX, y: view.terminal.buffer.active.cursorY }).toEqual(view.shellCursor)
  expect(view.input.raw).toBe(false)
  const bytes = view.stdout.chunks.join('')
  expect(bytes).toContain('\x1b[?1049l')
  expect(bytes).toContain('\x1b[?2004l')
  // The mouse is reported only while the alternate buffer is shown.
  expect(bytes.indexOf('\x1b[?1000h\x1b[?1006h')).toBeGreaterThan(bytes.indexOf('\x1b[?1049h'))
  expect(bytes.lastIndexOf('\x1b[?1006l\x1b[?1000l')).toBe(bytes.lastIndexOf('\x1b[?1049l') - '\x1b[?1006l\x1b[?1000l'.length)
})
