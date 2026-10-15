#!/usr/bin/env bun
/**
 * Drive the built Rust preview, and live `bake-rs` sessions against a fake
 * streaming model, in a real PTY and check their return to the shell.
 */
import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import { mkdir, mkdtemp, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { parseArgs } from 'node:util'
import xterm from '@xterm/headless'
import { Unicode11Addon } from '@xterm/addon-unicode11'

const root = resolve(import.meta.dir, '..')
const { values } = parseArgs({ options: { binary: { type: 'string' }, filter: { type: 'string' } } })
const binary = resolve(values.binary ?? join(root, 'rust/target/debug/bake-rs'))

/** A live session's command line and the isolated environment it runs in. */
interface Live { readonly args: readonly string[]; readonly env: Readonly<Record<string, string>> }

class Preview {
  private readonly terminal = new xterm.Terminal({ cols: 80, rows: 24, allowProposedApi: true })
  private readonly pty: Bun.Terminal
  private readonly initialModes: readonly number[]
  private readonly process: Bun.Subprocess
  private readonly exited: Promise<number>
  private parsed = Promise.resolve()
  /** Whether the emulator has parsed the start of a synchronized update but not its end. */
  private frameOpen = false
  private parsedTail = ''
  private code: number | undefined
  private streamStatus: number | undefined
  /** Everything the preview wrote, its last MiB, for sequences the screen does not show. */
  raw = ''

  constructor(cwd: string, readonly inline = false, readonly live?: Live) {
    // Emoji cases use Unicode 11 widths, matching modern terminal cell allocation.
    this.terminal.loadAddon(new Unicode11Addon())
    this.terminal.unicode.activeVersion = '11'
    const decoder = new TextDecoder()
    // The shell waits for our handshake before exec, preserving its PID. This
    // lets us read cooked modes before the native process can enable raw mode.
    const history = 'i=0; while [ "$i" -lt 40 ]; do printf "SHELL_HISTORY_%02d\\n" "$i"; i=$((i + 1)); done; '
    const shell = `read -r ready; ${inline ? history : ''}exec "$@"`
    const args = live?.args ?? (inline ? ['preview', '--inline'] : ['preview'])
    this.process = Bun.spawn(['sh', '-c', shell, 'rust-preview-pty', binary, ...args], {
      cwd,
      // SSH_TTY sends a copy to the terminal as OSC 52, which the driver
      // reads, instead of to this machine's clipboard.
      env: { PATH: process.env.PATH, TERM: 'xterm-256color', LANG: 'C.UTF-8', SSH_TTY: '/dev/pts/bake-pty', ...live?.env },
      terminal: {
        cols: 80, rows: 24,
        data: (_terminal, bytes) => {
          const text = decoder.decode(bytes, { stream: true })
          this.raw = (this.raw + text).slice(-1_048_576)
          this.parsed = this.parsed.then(() => new Promise<void>(done => this.terminal.write(text, () => {
            this.track(text)
            done()
          })))
        },
        exit: (_terminal, status) => { this.streamStatus = status },
      },
    })
    const pty = this.process.terminal
    assert(pty)
    this.pty = pty
    this.terminal.onData((data) => { this.pty.write(data) })
    this.initialModes = this.modes()
    this.pty.write('start\n')
    this.exited = this.process.exited.then((code) => { this.code = code; return code })
  }

  /**
   * Follows synchronized updates through parsed output. The headless emulator
   * shows a frame's cells as they arrive, while a supporting terminal holds
   * them until the update ends; a read split inside a frame would otherwise
   * expose its text before the caret moves. The kept tail is shorter than a
   * mode sequence, so one split across reads is still seen once whole.
   */
  private track(text: string): void {
    const seen = this.parsedTail + text
    const begin = seen.lastIndexOf('\x1b[?2026h')
    const end = seen.lastIndexOf('\x1b[?2026l')
    if (begin !== end) this.frameOpen = begin > end
    this.parsedTail = seen.slice(-7)
  }

  private modes(): readonly number[] {
    return [this.pty.inputFlags, this.pty.outputFlags, this.pty.localFlags, this.pty.controlFlags]
  }

  get screen(): string {
    const buffer = this.terminal.buffer.active
    return Array.from({ length: this.terminal.rows }, (_, row) => buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? '').join('\n')
  }

  get history(): string {
    const buffer = this.terminal.buffer.normal
    return Array.from({ length: buffer.length }, (_, row) => buffer.getLine(row)?.translateToString(true) ?? '').join('\n')
  }

  assertCursorAfter(text: string): void {
    const buffer = this.terminal.buffer.active
    const row = buffer.getLine(buffer.baseY + buffer.cursorY)
    assert(row, 'cursor row is absent')
    assert.equal(row.translateToString(true, 0, buffer.cursorX), `│ ❯ ${text}`, 'cursor is not at the draft caret')
  }

  assertShellHistory(): void {
    for (let index = 0; index < 40; index++) {
      const marker = `SHELL_HISTORY_${String(index).padStart(2, '0')}`
      assert.equal(this.history.split(marker).length - 1, 1, `shell row ${marker} was lost or duplicated`)
    }
    assert(!this.raw.includes('\x1b[?1049h'), 'inline preview acquired the alternate screen')
    assert(!this.raw.includes('\x1b[3J'), 'inline preview erased shell scrollback')
  }

  assertNoChromeResidue(exited = false): void {
    const lines = this.history.split('\n')
    const boxes = lines.filter(line => /^╭─.*╮$/u.test(line))
    assert.equal(boxes.length, exited ? 0 : 1, 'obsolete composer frames remain in terminal history')
    assert(!lines.some(line => line.includes('typing never reaches an agent')), 'obsolete inspector rows remain')
    const buffer = this.terminal.buffer.normal
    const scrollback = lines.slice(0, buffer.baseY).join('\n')
    assert(!scrollback.includes('❯') && !/^ {2}Agents .*Ctrl\+G$/mu.test(scrollback), 'live controls escaped into scrollback')
  }

  /** Waits until `condition` holds of a whole frame, or of any output when `framed` is false. */
  async wait(label: string, condition: () => boolean, framed = true): Promise<void> {
    const deadline = performance.now() + 30_000
    while (true) {
      await this.parsed
      if ((!framed || !this.frameOpen) && condition()) return
      if ((this.code !== undefined && this.streamStatus !== undefined) || performance.now() > deadline) {
        throw new Error(`${label}: ${this.code === undefined ? 'timed out' : `exited ${this.code}`}\n${this.screen}`)
      }
      await delay(5)
    }
  }

  async ready(): Promise<void> {
    await this.wait('Rust preview ready', () => this.terminal.buffer.active.type === (this.inline ? 'normal' : 'alternate')
      // The sample session's newest lines show on the first frame, and so does the composer.
      && (this.live !== undefined || this.screen.includes('rejects it before splitting.')) && this.screen.includes('❯ ')
      && this.raw.includes('\x1b[?2004h') && this.raw.includes('\x1b[?1000h\x1b[?1002h\x1b[?1006h'))
    assert.notDeepEqual(this.modes(), this.initialModes, 'preview did not acquire raw mode')
  }

  send(text: string): void { this.pty.write(text) }

  async resize(cols: number, rows: number): Promise<void> {
    const offset = this.raw.length
    this.terminal.resize(cols, rows)
    this.pty.resize(cols, rows)
    await this.wait('resize redraw', () => this.inline
      ? this.raw.length > offset && this.screen.split('\n').some(line => /^╭─.*╮$/u.test(line) && line.length === cols)
      : this.raw.slice(offset).includes('\x1b[2J'))
  }

  /**
   * Resizes twice while the preview is stopped, after typing `keys`, so it
   * wakes to input and a resize at once and must read both.
   */
  async resizeBurst(keys: string): Promise<void> {
    this.process.kill('SIGSTOP')
    const offset = this.raw.length
    try {
      // Hold every application thread while the emulator reflows twice. The
      // final kernel size equals the previous frame, but that frame is stale.
      await this.wait('preview stopped', () => /^State:\s+T/mu.test(readFileSync(`/proc/${this.process.pid}/status`, 'utf8')), false)
      this.send(keys)
      for (const [cols, rows] of [[40, 12], [80, 24]] as const) {
        this.terminal.resize(cols, rows)
        this.pty.resize(cols, rows)
      }
    } finally { this.process.kill('SIGCONT') }
    await this.wait('coalesced resize redraw', () => this.raw.slice(offset).includes(this.inline ? '╭─' : '\x1b[2J'))
  }

  async quit(signal?: 'SIGINT' | 'SIGTERM' | 'SIGHUP'): Promise<void> {
    if (signal === 'SIGTERM' && this.live === undefined) {
      this.send('\x07')
      await this.wait('agent list with hidden cursor', () => this.screen.includes('Fixed examples · nothing is running')
        && this.raw.lastIndexOf('\x1b[?25l') > this.raw.lastIndexOf('\x1b[?25h'))
    }
    if (signal === undefined) {
      // The first Ctrl+C only arms the quit; the second, inside its window, quits.
      this.send('\x03')
      await this.wait('quit armed', () => this.screen.includes('Press Ctrl-C again to quit'))
      assert.equal(this.code, undefined, 'one Ctrl+C quit the preview')
      this.send('\x03')
    } else this.process.kill(signal)
    await this.wait('preview process and PTY exit', () => this.code !== undefined && this.streamStatus !== undefined, false)
    // The wait read the parse queue before its last output arrived; the
    // stream has ended now, so this queue holds every byte the preview wrote.
    await this.parsed
    assert.equal(await this.exited, signal === 'SIGTERM' ? 143 : signal === 'SIGHUP' ? 129 : signal === 'SIGINT' ? 130 : 0)
    assert.equal(this.process.signalCode, null)
    // Linux reports EIO when the last slave closes, including a clean exit.
    assert(this.streamStatus === 0 || (process.platform === 'linux' && this.streamStatus === 1))
    assert.deepEqual(this.modes(), this.initialModes, 'terminal mode flags were not restored')
    assert.equal(this.terminal.buffer.active.type, 'normal', 'alternate screen was not released')
    assert(this.raw.lastIndexOf('\x1b[?2004l') > this.raw.lastIndexOf('\x1b[?2004h'), 'paste mode was not released')
    assert(this.raw.lastIndexOf('\x1b[?7h') > this.raw.lastIndexOf('\x1b[?7l'), 'autowrap was not restored')
    for (const mode of ['1000', '1002', '1006'])
      assert(this.raw.lastIndexOf(`\x1b[?${mode}l`) > this.raw.lastIndexOf(`\x1b[?${mode}h`), `mouse mode ${mode} was not released`)
    assert(this.raw.lastIndexOf('\x1b[?2026l') > this.raw.lastIndexOf('\x1b[?2026h'), 'synchronized output was left open')
    assert(this.raw.lastIndexOf('\x1b[?25h') > this.raw.lastIndexOf('\x1b[?25l'), 'cursor was not restored')
    assert.throws(() => process.kill(this.process.pid, 0), 'application remains alive after exit')
  }

  /** Waits for the process to exit by itself and returns its status, for a run that never takes the terminal. */
  async exit(): Promise<number> {
    const code = await Promise.race([this.exited, delay(30_000).then(() => 'timed out')])
    assert.equal(typeof code, 'number', 'the process did not exit')
    await this.wait('PTY drained', () => this.streamStatus !== undefined, false)
    await this.parsed
    return code as number
  }

  /** The preview's process id: the shell that `exec`s the binary. */
  get pid(): number { return this.process.pid }

  get unchangedModes(): boolean { return JSON.stringify(this.modes()) === JSON.stringify(this.initialModes) }

  /** Close the terminal under the preview, as an emulator window closing does, and expect the quiet hangup exit. */
  async hangUp(): Promise<void> {
    this.pty.close()
    const code = await Promise.race([this.exited, delay(5_000).then(() => 'timed out')])
    assert.equal(code, 129, 'a closed terminal did not end the preview as a hangup')
    assert.equal(this.process.signalCode, null)
  }

  async close(): Promise<void> {
    if (this.code === undefined) this.process.kill('SIGKILL')
    await this.exited
    this.pty.close()
    await this.parsed
    this.terminal.dispose()
  }

  async saveFailure(path: string): Promise<void> {
    await writeFile(path, this.raw)
    // ANSI alone cannot replay the emulator's resize/reflow operations.
    await writeFile(`${path}.screen.txt`, this.history)
  }
}

/** Run one scenario in a fresh working directory holding only `files`, which must be all it holds at the end. */
async function scenario(
  name: string, run: (preview: Preview) => Promise<void>, files: readonly string[] = [], inline = false,
): Promise<void> {
  if (values.filter !== undefined && !name.includes(values.filter)) {
    console.log(`SKIP Rust PTY: ${name} (filter ${values.filter})`)
    return
  }
  const cwd = await mkdtemp(join(tmpdir(), 'bake-rust-pty-'))
  let preview: Preview | undefined
  try {
    for (const file of files) {
      await mkdir(dirname(join(cwd, file)), { recursive: true })
      await writeFile(join(cwd, file), '')
    }
    preview = new Preview(cwd, inline)
    await preview.ready()
    await run(preview)
    const seeded = [...new Set(files.map(file => file.split('/')[0]))].sort()
    assert.deepEqual((await readdir(cwd)).sort(), seeded, 'preview wrote files into the working directory')
    console.log(`PASS Rust PTY: ${name}`)
  } catch (error) {
    if (preview !== undefined) {
      await mkdir(join(root, '.preflight'), { recursive: true })
      const transcript = join(root, '.preflight', `${basename(cwd)}.ansi`)
      await preview.saveFailure(transcript)
      console.error(`Rust PTY transcript: ${transcript}`)
    }
    throw error
  } finally {
    await preview?.close()
    await rm(cwd, { recursive: true })
  }
}

if (process.platform === 'win32') throw new Error('Rust PTY scenarios require POSIX; ConPTY qualification remains open')

await scenario('composer, agent inspection, paste, and resize', async (preview) => {
  preview.send('draftAB\x1b[D')
  await preview.wait('typed draft', () => preview.screen.includes('draftAB'))
  preview.send('\x07')
  await preview.wait('sample agent picker', () => preview.screen.includes('Fixed examples · nothing is running'))
  preview.send('\x1b[B\r')
  await preview.wait('read-only inspection', () => preview.screen.includes('typing never reaches an agent'))
  preview.send('forbidden\x1b')
  await preview.wait('return to composer', () => !preview.screen.includes('typing never reaches an agent') && preview.screen.includes('draftAB'))
  preview.send('X')
  await preview.wait('restored caret', () => preview.screen.includes('draftAXB'))
  assert(!preview.screen.includes('forbidden'), 'inspection accepted draft input')
  preview.send('\x1f')
  await preview.wait('undo after inspection', () => preview.screen.includes('draftAB') && !preview.screen.includes('draftAXB'))
  preview.send('\x1b[200~line one\r\nline two\x1b[201~')
  await preview.wait('multiline paste', () => preview.screen.includes('line one') && preview.screen.includes('line two'))
  await preview.resize(40, 12)
  await preview.wait('narrow draft', () => preview.screen.includes('line two'))
  await preview.resize(80, 24)
  await preview.wait('resized draft', () => preview.screen.includes('line one') && preview.screen.includes('line two'))
  preview.send('\x1f')
  await preview.wait('atomic paste undo', () => preview.screen.includes('draftAB') && !preview.screen.includes('line one'))
  // Readline keys: Ctrl+A, then Ctrl+E and Ctrl+W kill the word, and Ctrl+Y puts it back.
  preview.send('\x01X')
  await preview.wait('line start', () => preview.screen.includes('❯ XdraftAB'))
  preview.send('\x05 tail\x17')
  await preview.wait('word killed', () => preview.screen.includes('❯ XdraftAB ') && !preview.screen.includes('tail'))
  preview.send('\x19')
  await preview.wait('kill yanked', () => preview.screen.includes('❯ XdraftAB tail'))
  // Up on a one-row draft recalls the newest prompt; Down restores the draft.
  preview.send('\x1b[A')
  await preview.wait('prompt recalled', () => preview.screen.includes('❯ Run the parser tests') && !preview.screen.includes('XdraftAB'))
  preview.send('\x1b[B')
  await preview.wait('draft restored', () => preview.screen.includes('❯ XdraftAB tail'))
  // A paste of three lines or more collapses into one placeholder, which Backspace removes whole.
  preview.send('\x1b[200~one\ntwo\nthree\nfour\x1b[201~')
  await preview.wait('collapsed paste', () => preview.screen.includes('❯ XdraftAB tail[Pasted text #1 +3 lines]'))
  preview.send('\x7f')
  await preview.wait('placeholder erased', () => !preview.screen.includes('Pasted text') && preview.screen.includes('❯ XdraftAB tail'))
  // A pasted image path stages the image behind a placeholder, listed above the bar; Backspace unstages it.
  const imageDir = await mkdtemp(join(tmpdir(), 'bake-rust-image-'))
  try {
    const png = Buffer.concat([Buffer.from('89504e470d0a1a0a0000000d49484452', 'hex'), Buffer.from([0, 0, 0, 3, 0, 0, 0, 2]), Buffer.alloc(16)])
    await writeFile(join(imageDir, 'shot.png'), png)
    preview.send(`\x1b[200~${join(imageDir, 'shot.png')}\x1b[201~`)
    await preview.wait('image staged', () => preview.screen.includes('❯ XdraftAB tail[Image #2]')
      && preview.screen.includes('Staged attachments: 1') && preview.screen.includes('1. shot.png · image/png · 40 B · 3×2'))
    preview.send('\x7f')
    await preview.wait('image unstaged', () => !preview.screen.includes('Staged attachments') && !preview.screen.includes('[Image'))
  } finally { await rm(imageDir, { recursive: true }) }
  // A leading slash opens the command menu; Tab fills in the selected command and its usage replaces the menu.
  preview.send('\x01\x0b/go')
  await preview.wait('slash menu', () => /▸ \/goal +\[objective\|clear\]/u.test(preview.screen) && preview.screen.includes('Tab completes'))
  preview.send('\t')
  await preview.wait('command completed', () => preview.screen.includes('❯ /goal') && preview.screen.includes('/goal [objective|clear]  Set or view the goal'))
  preview.send('\r')
  await preview.wait('command refused', () => preview.screen.includes('Commands are not available in this preview'))
  await preview.quit()
})

await scenario('an @ mention lists workspace paths, completes one, and closes on Esc', async (preview) => {
  preview.send('read @src/')
  await preview.wait('file menu', () => /▸ @src\/lib\.rs/u.test(preview.screen) && preview.screen.includes('@src/main.rs')
    && preview.screen.includes('Tab completes'))
  preview.send('\x1b[B\t')
  await preview.wait('mention completed', () => preview.screen.includes('❯ read @src/main.rs') && !preview.screen.includes('@src/lib.rs'))
  // A bare name is ranked against the whole tree.
  preview.send('@READ')
  await preview.wait('ranked match', () => /▸ @README\.md/u.test(preview.screen))
  preview.send('\x1b')
  await preview.wait('menu closed', () => !preview.screen.includes('@README.md') && preview.screen.includes('❯ read @src/main.rs @READ'))
  await preview.quit()
}, ['src/main.rs', 'src/lib.rs', 'README.md'])

await scenario('transcript pages, jumps between prompts, and follows output again', async (preview) => {
  preview.send('keep')
  await preview.wait('following output', () => preview.screen.includes('PgUp scroll · Ctrl+↑ prompts') && preview.screen.includes('before splitting.'))
  preview.send('\x1b[5~')
  await preview.wait('reading history', () => /↓ \d+ lines below · Ctrl\+End/u.test(preview.screen) && !preview.screen.includes('before splitting.'))
  // Two prompts back from the newest lines is the code-mode turn: its program, its call sites, and what it returned.
  preview.send('\x1b[1;5F\x1b[1;5A')
  await preview.wait('parser turn at the top', () => /^> Run the parser tests/u.test(preview.screen))
  preview.send('\x1b[1;5A')
  await preview.wait('code-mode block', () => /^> Find TODO comments/u.test(preview.screen)
    && /(?:✓|\{\}) Codemode: Find TODOs +13 calls · 1 failed/u.test(preview.screen)
    && /╰ ✓ tools\.read ×12 .+11 done · 1 failed/u.test(preview.screen)
    && /✗ src\/m5\.ts +Permission denied/u.test(preview.screen))
  preview.send('\x1b[1;5H')
  await preview.wait('transcript start', () => /^ {2}Bake · Rust preview/mu.test(preview.screen))
  preview.send('\x1b[1;5B')
  await preview.wait('next prompt at the top', () => /^> Find where the session controller/u.test(preview.screen))
  preview.send('\x1b[1;5F')
  await preview.wait('following again', () => preview.screen.includes('PgUp scroll') && preview.screen.includes('before splitting.'))
  assert(preview.screen.includes('❯ keep'), 'transcript navigation changed the draft')
  // Wide enough, an edit's numbered diff goes side by side; narrow again, it is unified. Without truecolor
  // there is no box, so the gutter takes the cell before the diff.
  await preview.resize(120, 30)
  await preview.wait('side-by-side diff', () => /^ {2}[│ ] 42 [-▎] {3}if \(quote\) fields\.push\(rest\); +│ 42 [+▎] {3}if \(quote\) throw/mu.test(preview.screen))
  await preview.resize(80, 30)
  await preview.wait('unified diff', () => /^ {2}[│ ] 42 [+▎] {3}if \(quote\) throw/mu.test(preview.screen))
  await preview.quit()
})

await scenario('the wheel scrolls the transcript and the scrollbar takes clicks and drags', async (preview) => {
  // SGR reports: button 64 is the wheel toward older output, 65 toward newer; 0 the primary button.
  const sgr = (button: number, column: number, row: number, press = true) => `\x1b[<${button};${column};${row}${press ? 'M' : 'm'}`
  await preview.wait('scrollbar', () => preview.screen.split('\n').some(line => line.endsWith('┃') || line.endsWith('#')))
  preview.send(sgr(64, 10, 5).repeat(3))
  await preview.wait('wheel scrolled back', () => /↓ \d+ lines below · Ctrl\+End/u.test(preview.screen))
  // The pill sits on the row just above the status bar, and a press on it follows output.
  const lines = preview.screen.split('\n')
  const pillRow = lines.findIndex(line => line.includes(' lines below · Ctrl+End'))
  assert(lines[pillRow + 1]?.includes('no model'), 'the scroll pill is not just above the status bar')
  const pillLine = lines[pillRow] ?? ''
  const pillColumn = [...pillLine.slice(0, pillLine.indexOf('↓'))].length
  preview.send(sgr(0, pillColumn + 2, pillRow + 1) + sgr(0, pillColumn + 2, pillRow + 1, false))
  await preview.wait('pill pressed', () => preview.screen.includes('Wheel/PgUp scroll'))
  preview.send(sgr(64, 10, 5).repeat(3))
  await preview.wait('wheel scrolled back again', () => /↓ \d+ lines below · Ctrl\+End/u.test(preview.screen))
  preview.send(sgr(65, 10, 5).repeat(40))
  await preview.wait('wheel back to the newest line', () => preview.screen.includes('Wheel/PgUp scroll'))
  // A press at the head of the scrollbar's track goes to the start; the bar is the last column.
  preview.send(sgr(0, 80, 1) + sgr(0, 80, 1, false))
  await preview.wait('scrollbar press at the head', () => /^ {2}Bake · Rust preview/mu.test(preview.screen))
  // Dragged from the head past the foot, the transcript follows output again.
  preview.send(sgr(0, 80, 1) + sgr(32, 40, 30) + sgr(0, 40, 30, false))
  await preview.wait('scrollbar dragged to the foot', () => preview.screen.includes('Wheel/PgUp scroll'))
  // A press on the draft puts the caret on the cell it hit: typing lands before `world`.
  preview.send('hello world')
  await preview.wait('typed draft', () => preview.screen.includes('❯ hello world'))
  const draftLines = preview.screen.split('\n')
  const draftRow = draftLines.findIndex(line => line.includes('❯ hello world'))
  const draftLine = draftLines[draftRow] ?? ''
  const worldColumn = [...draftLine.slice(0, draftLine.indexOf('world'))].length
  preview.send(sgr(0, worldColumn + 1, draftRow + 1) + sgr(0, worldColumn + 1, draftRow + 1, false) + 'big ')
  await preview.wait('caret placed by a press', () => preview.screen.includes('❯ hello big world'))
  // A double click in the transcript selects a word and copies it on release, through OSC 52 here.
  const textLines = preview.screen.split('\n')
  const wordRow = textLines.findIndex(line => line.includes('before splitting.'))
  const wordLine = textLines[wordRow] ?? ''
  const wordColumn = [...wordLine.slice(0, wordLine.indexOf('splitting'))].length + 3
  const click = sgr(0, wordColumn, wordRow + 1) + sgr(0, wordColumn, wordRow + 1, false)
  preview.send(click + click)
  const copied = `\x1b]52;c;${Buffer.from('splitting').toString('base64')}\x07`
  await preview.wait('word copied', () => preview.raw.includes(copied) && preview.screen.includes('Copied'))
  await preview.wait('copy notice gone', () => !preview.screen.includes('Copied'))
  await preview.quit()
})

await scenario('sample activity is text that advances on its own, then compacts, and stops', async (preview) => {
  preview.send('\x14')
  await preview.wait('sample turn', () => /^ {2}\S+… {2}thinking · 0s +no model/mu.test(preview.screen) && preview.screen.includes('Esc interrupts')
    && preview.screen.includes('Enter steers the next step'))
  // The turn runs a code-mode script: its dot blinks on its own, shown and then a blank in place.
  await preview.wait('running script shown', () => /^ {2}(?:●|\{\}) Codemode: Read every manifest, then build/mu.test(preview.screen))
  await preview.wait('running script blinks', () => /^ {4,5}Codemode: Read every manifest, then build/mu.test(preview.screen))
  await preview.wait('running script shown again', () => /^ {2}(?:●|\{\}) Codemode: Read every manifest, then build/mu.test(preview.screen))
  // Its calls arrive on the clock, and the build runs last until the turn ends.
  await preview.wait('script calls arrive', () => /✗ packages\/goal\/package\.json +Permission denied/u.test(preview.screen)
    && /╰ [● ] tools\.bash +bun run build/u.test(preview.screen))
  // No key is pressed: the loop's own timer must redraw the elapsed time.
  await preview.wait('elapsed time advances', () => preview.screen.includes('thinking · 1s'))
  assert(!/[\u2800-\u28ff]/u.test(preview.screen), 'the activity drew a spinner glyph')
  preview.send('\x14')
  // The settled block outgrows the screen; its foot shows the build and what the program returned.
  await preview.wait('script settles with the turn', () => /╰ ✓ tools\.bash +bun run build +exit 0/u.test(preview.screen) && /return +\{ manifests: 12, exitCode: 0 \}/u.test(preview.screen))
  await preview.wait('sample compaction', () => preview.screen.includes('Compacting history…  preparing · 0s')
    && preview.screen.includes('Compacting… Enter queues · Esc cancels') && !preview.screen.includes('Esc interrupts'))
  preview.send('\x1b')
  // The compaction ends. The bar keeps the turn's outcome on its left and the
  // status, no model and then the directory, right-aligned on the same row.
  await preview.wait('sample stopped', () => /^ {2}✓ Completed {2}\d+s +no model {2}\S+$/mu.test(preview.screen)
    && !preview.screen.includes('Compacting'))
  await preview.quit()
})

await scenario('a prompt runs a fixture turn that streams, completes, takes a steer, and stops on Esc', async (preview) => {
  preview.send('hello fixture\r')
  // The prompt shows once the runtime commits it, and the draft is empty again.
  await preview.wait('turn started', () => /^> hello fixture +[│┃]$/mu.test(preview.screen)
    && preview.screen.includes('Esc interrupts') && preview.screen.includes('Enter steers the next step'))
  await preview.wait('turn completed', () => preview.screen.includes('no model or tool ran.')
    && /^ {2}✓ Completed {2}\d+s/mu.test(preview.screen) && preview.screen.includes('❯ Type a draft'))
  assert.equal(preview.screen.split('no model or tool ran.').length - 1, 1, 'the streamed answer was drawn twice')
  preview.send('again\r')
  await preview.wait('call running', () => preview.screen.includes('running bash'))
  // A prompt sent while the turn runs waits above the bar until Alt+↑ sends it.
  preview.send('later\r')
  await preview.wait('pending input', () => preview.screen.includes('Pending input')
    && preview.screen.includes('Next step: later') && preview.screen.includes('Alt+↑ sends it now'))
  preview.send('\x1b[1;3A')
  await preview.wait('pending sent', () => preview.screen.includes('Sent queued input now')
    && /^> later +[│┃]$/mu.test(preview.screen) && !preview.screen.includes('Pending input'))
  await preview.wait('steered turn running', () => preview.screen.includes('running bash'))
  preview.send('\x1b')
  await preview.wait('turn interrupted', () => /^ {2}■ Interrupted/mu.test(preview.screen)
    && /Bash: true +interrupted/u.test(preview.screen))
  await preview.quit()
})

await scenario('a resize while a turn streams redraws it once at the new width', async (preview) => {
  preview.send('resize me\r')
  await preview.wait('turn running', () => preview.screen.includes('Esc interrupts'))
  await preview.resize(60, 20)
  await preview.wait('turn completed narrow', () => preview.screen.includes('❯ Type a draft')
    && /^ {2}✓ Completed {2}\d+s/mu.test(preview.screen))
  assert.equal(preview.screen.split('\n').length, 20)
  assert(preview.screen.split('\n').every(line => [...line].length <= 60), `a row is wider than the terminal\n${preview.screen}`)
  assert.equal(preview.screen.split('> resize me').length - 1, 1, 'the prompt was drawn twice')
  await preview.resize(80, 24)
  await preview.wait('wide again', () => preview.screen.includes('> resize me'))
  await preview.quit()
})

// The sample session's running call pulses, so the next frame's write can
// see the hangup before the watcher does; the watcher has its own unit test.
// Either way the input thread is stuck in Crossterm and must not be joined.
await scenario('a terminal that closes while idle ends the preview quietly as a hangup', async (preview) => {
  await preview.hangUp()
})

await scenario('a terminal that closes during a turn ends the preview quietly as a hangup', async (preview) => {
  preview.send('hello\r')
  await preview.wait('turn running', () => preview.screen.includes('Esc interrupts'))
  await preview.hangUp()
})

if (process.platform === 'linux') {
  await scenario('coalesced resize invalidates stale screen', async (preview) => {
    preview.send('RESIZE_DRAFT')
    await preview.wait('draft before resize', () => preview.screen.includes('RESIZE_DRAFT'))
    await preview.resizeBurst('_TYPED')
    await preview.wait('draft after resize', () => preview.screen.includes('RESIZE_DRAFT_TYPED'))
    assert.equal(preview.screen.split('RESIZE_DRAFT').length - 1, 1, 'resize duplicated the draft')
    await preview.quit()
  })
} else console.log('SKIP Rust PTY: coalesced resize barrier needs Linux /proc process state')

for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP'] as const) {
  await scenario(`${signal} restores terminal`, async (preview) => { await preview.quit(signal) })
}

await scenario('fullscreen Unicode 11 draft keeps the terminal caret at grapheme boundaries', async (preview) => {
  const draft = '漢字🙂e\u0301'
  preview.send(draft)
  await preview.wait('fullscreen Unicode draft', () => preview.screen.includes(`❯ ${draft}`))
  preview.assertCursorAfter(draft)
  preview.send('\x1b[DZ')
  await preview.wait('fullscreen grapheme edit', () => preview.screen.includes('❯ 漢字🙂Ze\u0301'))
  preview.assertCursorAfter('漢字🙂Z')
  await preview.quit()
})

await scenario('inline preserves shell history, commits turns once, and keeps the Unicode caret', async (preview) => {
  preview.assertShellHistory()
  const draft = '漢字🙂e\u0301'
  preview.send(draft)
  await preview.wait('Unicode draft', () => preview.screen.includes(`❯ ${draft}`))
  preview.assertCursorAfter(draft)
  preview.send('\x1b[DZ')
  await preview.wait('grapheme caret edit', () => preview.screen.includes('❯ 漢字🙂Ze\u0301'))
  preview.assertCursorAfter('漢字🙂Z')
  preview.send('\x01\x0b\x1b[200~inline line one\ninline line two\x1b[201~')
  await preview.wait('inline multiline composer', () => preview.screen.includes('inline line one') && preview.screen.includes('inline line two'))
  preview.assertShellHistory()
  preview.send('\x1f')
  await preview.wait('inline multiline undo', () => !preview.screen.includes('inline line two'))
  preview.send('\x01\x0b')
  for (const prompt of ['INLINE_FIRST_UNIQUE', 'INLINE_SECOND_UNIQUE']) {
    preview.send(`${prompt}\r`)
    await preview.wait('inline turn running', () => preview.screen.includes('Esc interrupts'))
    if (prompt === 'INLINE_FIRST_UNIQUE') {
      // transcript::present opens a user row with one blank and its dashed rule.
      const lines = preview.history.split('\n')
      const sampleEnd = lines.findIndex(line => line.trim() === 'rejects it before splitting.')
      const userStart = lines.findIndex(line => line.trimEnd() === `> ${prompt}`)
      assert(sampleEnd >= 0 && userStart > sampleEnd, 'the committed sample-to-prompt boundary is missing')
      assert.deepEqual(lines.slice(sampleEnd + 1, userStart), ['', '-'.repeat(79)],
        'publishing a prompt inserted live-region padding into the transcript')
      await preview.resize(60, 20)
      preview.send('kept while streaming')
      await preview.wait('draft during streaming', () => preview.screen.includes('❯ kept while streaming'))
    }
    await preview.wait('inline turn completed', () => /^ {2}✓ Completed {2}\d+s/mu.test(preview.screen))
    preview.assertShellHistory()
    assert.equal(preview.history.split('\n').filter(line => line.trimEnd() === `> ${prompt}`).length, 1,
      'committed user prompt was lost or duplicated')
    if (prompt === 'INLINE_FIRST_UNIQUE') {
      preview.send('\x01\x0b')
      await preview.resize(80, 24)
      await preview.wait('empty draft after streaming', () => preview.screen.includes('❯ Type a draft'))
    }
  }
  preview.send('\x1b[5~')
  await preview.wait('inline reads history', () => /↓ \d+ lines below · Ctrl\+End/u.test(preview.screen))
  preview.send('\x1b[1;5F')
  await preview.wait('inline follows output again', () => preview.screen.includes('PgUp scroll') && !/↓ \d+ lines below/u.test(preview.screen))
  for (const prompt of ['INLINE_FIRST_UNIQUE', 'INLINE_SECOND_UNIQUE'])
    assert.equal(preview.history.split('\n').filter(line => line.trimEnd() === `> ${prompt}`).length, 1,
      'revisiting history duplicated a committed user prompt')
  assert.equal(preview.history.split("This reply comes from the preview's fixture runtime.").length - 1, 2, 'a settled fixture answer was lost or duplicated')
  preview.send('\x07')
  await preview.wait('inline agent picker', () => preview.screen.includes('Fixed examples · nothing is running'))
  preview.send('\x1b[B\r')
  await preview.wait('inline inspection', () => preview.screen.includes('typing never reaches an agent'))
  preview.send('forbidden\x1b')
  await preview.wait('inline composer restored', () => !preview.screen.includes('typing never reaches an agent') && preview.screen.includes('❯ Type a draft'))
  assert(!preview.history.includes('forbidden'), 'inspection accepted input')
  preview.assertShellHistory()
  preview.assertNoChromeResidue()
  preview.send('\x14')
  await preview.wait('inline sample running', () => preview.screen.includes('Esc interrupts'))
  await preview.wait('inline sample calls arrive', () => /╰ [● ] tools\.bash +bun run build/u.test(preview.screen))
  preview.send('\x14')
  await preview.wait('inline sample settles', () => preview.history.includes('return    { manifests: 12, exitCode: 0 }')
    || /return +\{ manifests: 12, exitCode: 0 \}/u.test(preview.history))
  await preview.wait('inline sample compaction', () => preview.screen.includes('Compacting history…'))
  preview.send('\x1b')
  await preview.wait('inline sample stops', () => !preview.screen.includes('Compacting') && /^ {2}✓ Completed/mu.test(preview.screen))
  assert.equal(preview.history.split('{ manifests: 12, exitCode: 0 }').length - 1, 1, 'sample settlement was lost or duplicated')
  preview.assertShellHistory()
  await preview.quit()
  preview.assertShellHistory()
  preview.assertNoChromeResidue(true)
}, [], true)

for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP'] as const) {
  await scenario(`inline ${signal} restores terminal and shell history`, async (preview) => {
    await preview.quit(signal)
    preview.assertShellHistory()
    preview.assertNoChromeResidue(true)
  }, [], true)
}

// Typing and a resize that reach the preview together must both be read; a
// reader that took only the resize left the keys unread until the next one.
await scenario('inline keys sent with a resize are read at once', async (preview) => {
  let draft = ''
  for (let round = 0; round < 40; round++) {
    draft += String.fromCharCode(97 + round % 26)
    preview.send(draft.at(-1) ?? '')
    await preview.resize(round % 2 === 0 ? 79 : 80, 24)
    await preview.wait(`key ${round} with a resize`, () => preview.screen.includes(`❯ ${draft} `))
  }
  await preview.quit()
}, [], true)

await scenario('inline terminal hangup while idle exits quietly', async (preview) => {
  await preview.hangUp()
}, [], true)

await scenario('inline terminal hangup while streaming exits quietly', async (preview) => {
  preview.send('INLINE_HANGUP\r')
  await preview.wait('inline turn before hangup', () => preview.screen.includes('Esc interrupts'))
  await preview.hangUp()
}, [], true)


if (process.platform === 'linux') {
  await scenario('inline coalesced resize keeps shell scrollback and live controls out of history', async (preview) => {
    preview.send('INLINE_BURST_UNIQUE\r')
    await preview.wait('inline stream before burst', () => preview.screen.includes('Esc interrupts'))
    preview.send('BURST_DRAFT')
    await preview.wait('inline draft before burst', () => preview.screen.includes('❯ BURST_DRAFT'))
    await preview.resizeBurst('_TYPED')
    await preview.wait('inline draft after burst', () => preview.screen.includes('❯ BURST_DRAFT_TYPED'))
    preview.assertCursorAfter('BURST_DRAFT_TYPED')
    preview.assertShellHistory()
    preview.assertNoChromeResidue()
    assert.equal(preview.history.split('\n').filter(line => line.trimEnd() === '> INLINE_BURST_UNIQUE').length, 1,
      'coalesced resize erased the already published prompt')
    await preview.wait('inline turn after burst', () => /^ {2}✓ Completed/mu.test(preview.screen))
    assert.equal(preview.history.split('\n').filter(line => line.trimEnd() === '> INLINE_BURST_UNIQUE').length, 1,
      `coalesced resize lost or duplicated committed prompt\n${preview.history.split('\n').filter(line => line.includes('BURST')).join('\n')}`)
    assert.equal(preview.history.split('BURST_DRAFT').length - 1, 1, 'coalesced resize duplicated live draft')
    preview.assertShellHistory()
    preview.assertNoChromeResidue()
    await preview.quit()
    preview.assertShellHistory()
    preview.assertNoChromeResidue(true)
  }, [], true)
} else console.log('SKIP Rust PTY: inline coalesced resize barrier needs Linux /proc process state')

/**
 * A fake OpenAI Responses model on a loopback port. It answers by the last
 * user prompt: `ECHO <words>` streams `Echo: <words>.`; `READ <path>` calls
 * `read`, then answers with the file's first line; `SLEEP` calls bash
 * `sleep 30`; `HOLD` streams `Holding` and finishes only on `release()` or
 * when the request is aborted; `STEER <words>` answers `Steered: <words>.`;
 * `WRITE <files…>` calls `write` once per file, in order, each asking to
 * escalate to workspace-write and all with the one call id `call_write`,
 * then answers with the last result's first line; `HOLDWRITE <file>` is
 * `WRITE` whose call is held until `release()`.
 */
/** CLIProxyAPI scenario text; the key is fake. */
const UNCONFIGURED = 'No model is ready yet. Run /login'
const FIELD_KEYS = 'Enter continue · Esc cancel'
const PROXY_KEY = 'sk-fake-pty-0123456789'

class FakeModel {
  readonly requests: { input: unknown[] }[] = []
  /** The Authorization header of each request, model lists included, in order. */
  readonly authorizations: string[] = []
  /** The model lists served, as a CLIProxyAPI lists them. */
  readonly modelLists: string[] = []
  /** Holds each model list until `release()`, as a slow proxy would. */
  holdModels = false
  /** Settles when a held model list is aborted by its client. */
  readonly listAborted = Promise.withResolvers<void>()
  private releases: (() => void)[] = []
  aborted = 0
  private readonly server: ReturnType<typeof Bun.serve>

  constructor() {
    this.server = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: request => this.answer(request) })
  }

  get port(): number { return this.server.port ?? 0 }

  /** Lets every held response finish. */
  release(): void {
    for (const done of this.releases.splice(0)) done()
  }

  get holding(): number { return this.releases.length }

  async stop(): Promise<void> { await this.server.stop(true) }

  /** A held response: its first events stream at once, the rest on `release()`; an abort ends it. */
  private held(request: Request, events: string[], head: number): Response {
    const encoder = new TextEncoder()
    let release: () => void = () => {}
    const released = new Promise<void>((done) => { release = done })
    this.releases.push(release)
    request.signal.addEventListener('abort', () => { this.aborted++; release() })
    return new Response(new ReadableStream({
      start: async (controller) => {
        controller.enqueue(encoder.encode(events.slice(0, head).join('')))
        await released
        if (request.signal.aborted) {
          controller.close()
          return
        }
        controller.enqueue(encoder.encode(events.slice(head).join('')))
        controller.close()
      },
    }), { headers: { 'content-type': 'text/event-stream' } })
  }

  private async answer(request: Request): Promise<Response> {
    this.authorizations.push(request.headers.get('authorization') ?? '')
    if (request.method === 'GET' && new URL(request.url).pathname === '/v1/models') {
      this.modelLists.push(request.url)
      const list = JSON.stringify({ data: [
        { id: 'gpt-loop', owned_by: 'openai' },
        { id: 'gpt-two', owned_by: 'openai', supported_reasoning_levels: [{ effort: 'low' }, { effort: 'high' }] },
      ] })
      if (!this.holdModels) return new Response(list, { headers: { 'content-type': 'application/json' } })
      let release: () => void = () => {}
      const released = new Promise<void>((done) => { release = done })
      this.releases.push(release)
      request.signal.addEventListener('abort', () => { this.listAborted.resolve(); release() })
      await released
      return new Response(list, { headers: { 'content-type': 'application/json' } })
    }
    const body = await request.json() as { input: { role?: string; type?: string; content?: { text?: string }[]; output?: string }[] }
    this.requests.push(body)
    const input = body.input
    const last = input.at(-1)
    const prompt = [...input].reverse().find(item => item.role === 'user')?.content?.map(part => part.text ?? '').join('') ?? ''
    const toolOutput = last?.type === 'function_call_output' ? String(last.output ?? '') : undefined
    const n = this.requests.length
    const write = /^(HOLD)?WRITE (.+)$/u.exec(prompt)
    if (write !== null) {
      const targets = write[2]
      assert(targets !== undefined)
      const files = targets.trim().split(/\s+/u)
      const since = input.length - 1 - [...input].reverse().findIndex(item => item.role === 'user')
      const done = input.slice(since).filter(item => item.type === 'function_call_output').length
      const file = files[done]
      if (file === undefined) return sse(text(n, `Wrote: ${(toolOutput ?? '').split('\n')[0]}`))
      const events = call(n, 'write', { path: file, content: `${file} body`, sandbox_permissions: 'workspace-write', justification: `create ${file}` }, 'call_write')
      return write[1] === undefined ? sse(events) : this.held(request, events, 1)
    }
    if (toolOutput !== undefined) {
      if (prompt.startsWith('READ ')) return sse(text(n, `Read it: ${toolOutput.split('\n')[0]}`))
      return sse(text(n, 'Slept.'))
    }
    if (prompt.startsWith('READ ')) return sse(call(n, 'read', { path: prompt.slice(5).trim() }))
    if (prompt.startsWith('SLEEP')) return sse(call(n, 'bash', { command: 'sleep 30' }))
    if (prompt.startsWith('STEER ')) return sse(text(n, `Steered: ${prompt.slice(6).trim()}.`))
    if (prompt.startsWith('HOLD')) {
      const events = text(n, 'Holding until released.')
      const encoder = new TextEncoder()
      let release: () => void = () => {}
      const released = new Promise<void>((done) => { release = done })
      this.releases.push(release)
      request.signal.addEventListener('abort', () => { this.aborted++; release() })
      return new Response(new ReadableStream({
        start: async (controller) => {
          // The first delta streams at once; the rest waits for the release.
          controller.enqueue(encoder.encode(events.slice(0, 5).join('')))
          await released
          if (request.signal.aborted) {
            controller.close()
            return
          }
          controller.enqueue(encoder.encode(events.slice(5).join('')))
          controller.close()
        },
      }), { headers: { 'content-type': 'text/event-stream' } })
    }
    return sse(text(n, `Echo: ${prompt.replace(/^ECHO\s*/u, '')}.`))
  }
}

function event(sequence: number, value: Record<string, unknown>): string {
  return `event: ${String(value.type)}\ndata: ${JSON.stringify({ ...value, sequence_number: sequence })}\n\n`
}

const usage = {
  input_tokens: 40,
  input_tokens_details: { cached_tokens: 0 },
  output_tokens: 8,
  output_tokens_details: { reasoning_tokens: 0 },
  total_tokens: 48,
}

/** A text response in SSE events: its first five open the message and stream `Holding`'s first word. */
function text(n: number, answer: string): string[] {
  const id = `msg_${n}`
  const [head, ...rest] = answer.split(' ')
  const tail = rest.length > 0 ? ` ${rest.join(' ')}` : ''
  const item = { id, type: 'message', status: 'completed', role: 'assistant', content: [{ type: 'output_text', annotations: [], text: answer }] }
  return [
    { type: 'response.created', response: { id: `resp_${n}`, object: 'response', created_at: 1_760_000_000, status: 'in_progress', model: 'loop-model', output: [] } },
    { type: 'response.output_item.added', output_index: 0, item: { id, type: 'message', status: 'in_progress', role: 'assistant', content: [] } },
    { type: 'response.content_part.added', item_id: id, output_index: 0, content_index: 0, part: { type: 'output_text', annotations: [], text: '' } },
    { type: 'response.output_text.delta', item_id: id, output_index: 0, content_index: 0, delta: head },
    { type: 'response.in_progress', response: { id: `resp_${n}`, object: 'response', created_at: 1_760_000_000, status: 'in_progress', model: 'loop-model', output: [] } },
    { type: 'response.output_text.delta', item_id: id, output_index: 0, content_index: 0, delta: tail },
    { type: 'response.output_text.done', item_id: id, output_index: 0, content_index: 0, text: answer },
    { type: 'response.content_part.done', item_id: id, output_index: 0, content_index: 0, part: { type: 'output_text', annotations: [], text: answer } },
    { type: 'response.output_item.done', output_index: 0, item },
    { type: 'response.completed', response: { id: `resp_${n}`, object: 'response', created_at: 1_760_000_000, status: 'completed', model: 'loop-model', output: [item], usage } },
  ].map((value, sequence) => event(sequence, value))
}

/** A response holding one tool call, with its call id `call_<n>` unless one is given. */
function call(n: number, name: string, args: Record<string, unknown>, callId = `call_${n}`): string[] {
  const id = `fc_${n}`
  const item = { id, type: 'function_call', status: 'completed', call_id: callId, name, arguments: JSON.stringify(args) }
  return [
    { type: 'response.created', response: { id: `resp_${n}`, object: 'response', created_at: 1_760_000_000, status: 'in_progress', model: 'loop-model', output: [] } },
    { type: 'response.output_item.added', output_index: 0, item: { ...item, status: 'in_progress', arguments: '' } },
    { type: 'response.function_call_arguments.done', item_id: id, output_index: 0, arguments: item.arguments },
    { type: 'response.output_item.done', output_index: 0, item },
    { type: 'response.completed', response: { id: `resp_${n}`, object: 'response', created_at: 1_760_000_000, status: 'completed', model: 'loop-model', output: [item], usage } },
  ].map((value, sequence) => event(sequence, value))
}

function sse(events: string[]): Response {
  return new Response(events.join(''), { headers: { 'content-type': 'text/event-stream' } })
}

/** An isolated Bake home, user home, and project for live sessions, all removed afterwards. */
interface LiveWorld {
  readonly root: string
  readonly home: string
  readonly project: string
  readonly model: FakeModel
  launch(args?: readonly string[], inline?: boolean): Preview
}

/** The session files the home holds, each as its parsed entries. */
async function sessionEntries(home: string): Promise<Record<string, unknown>[][]> {
  const found: Record<string, unknown>[][] = []
  const walk = async (dir: string): Promise<void> => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name)
      if (entry.isDirectory()) await walk(path)
      else if (entry.name.endsWith('.jsonl')) found.push(readFileSync(path, 'utf8').trim().split('\n').map(line => JSON.parse(line) as Record<string, unknown>))
    }
  }
  await walk(join(home, 'sessions')).catch(() => {})
  return found
}

function messageTexts(entries: Record<string, unknown>[], role: string): string[] {
  return entries.flatMap((entry) => {
    const message = entry.message as { role?: string; content?: string | { type: string; text?: string }[] } | undefined
    if (entry.type !== 'message' || message?.role !== role) return []
    const content = message.content
    return [typeof content === 'string' ? content : (content ?? []).map(part => part.text ?? '').join('')]
  })
}

/** Run one live scenario against a fresh fake model and isolated homes. */
async function liveScenario(name: string, run: (world: LiveWorld) => Promise<void>): Promise<void> {
  if (values.filter !== undefined && !name.includes(values.filter)) {
    console.log(`SKIP Rust PTY: ${name} (filter ${values.filter})`)
    return
  }
  const root = await mkdtemp(join(tmpdir(), 'bake-rust-live-'))
  const home = join(root, 'home')
  const project = join(root, 'project')
  const user = join(root, 'user')
  for (const dir of [home, project, user]) await mkdir(dir, { recursive: true })
  const model = new FakeModel()
  await writeFile(join(home, 'models.json'), JSON.stringify({
    providers: { loop: { baseUrl: `http://127.0.0.1:${model.port}/v1`, api: 'openai-responses', apiKey: 'test-key', models: [{ id: 'loop-model', name: 'Loop', contextWindow: 1000, input: ['text', 'image'] }] } },
  }))
  const launched: Preview[] = []
  const world: LiveWorld = {
    root, home, project, model,
    launch: (args = [], inline = false) => {
      const preview = new Preview(project, inline, { args: inline ? ['--inline', ...args] : args, env: { BAKE_HOME: home, HOME: user, USERPROFILE: user } })
      launched.push(preview)
      return preview
    },
  }
  try {
    await run(world)
    console.log(`PASS Rust PTY: ${name}`)
  } catch (error) {
    const last = launched.at(-1)
    if (last !== undefined) {
      await mkdir(join(root, '..', basename(root)), { recursive: true })
      await mkdir(join(join(import.meta.dir, '..'), '.preflight'), { recursive: true })
      const transcript = join(import.meta.dir, '..', '.preflight', `${basename(root)}.ansi`)
      await last.saveFailure(transcript)
      console.error(`Rust PTY transcript: ${transcript}`)
    }
    throw error
  } finally {
    for (const preview of launched) await preview.close()
    await model.stop()
    await rm(root, { recursive: true, force: true })
  }
}

await liveScenario('a live session streams an answer and a tool call, takes a second prompt, and resumes', async (world) => {
  await writeFile(join(world.project, 'notes.txt'), 'alpha line\nbeta line\n')
  const session = world.launch()
  await session.ready()
  await session.wait('model in the status line', () => session.screen.includes('loop-model'))
  session.send('READ notes.txt\r')
  await session.wait('tool call and answer', () => session.screen.includes('Read it:') && /notes\.txt/u.test(session.screen)
    && /^ {2}✓ Completed/mu.test(session.screen) && session.screen.includes('❯ Type a draft'))
  assert(/✓ Read: notes\.txt +2 lines/u.test(session.screen) && session.screen.includes('│ alpha line'), `the read card is missing\n${session.screen}`)
  session.send('ECHO second prompt\r')
  await session.wait('second answer', () => session.screen.includes('Echo: second prompt.'))
  assert.equal(session.screen.split('Echo: second prompt.').length - 1, 1, 'the streamed answer was drawn twice')
  // A pasted image path stages the image, and an idle prompt sends it with the text.
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==', 'base64')
  const imagePath = join(world.root, 'dot.png')
  await writeFile(imagePath, png)
  session.send('ECHO picture ')
  session.send(`\x1b[200~${imagePath}\x1b[201~`)
  await session.wait('image staged', () => session.screen.includes('❯ ECHO picture [Image #1]') && session.screen.includes('Staged attachments: 1'))
  const sentBefore = world.model.requests.length
  session.send('\r')
  await session.wait('image prompt answered', () => session.screen.includes('Echo: picture [Image #1].') && !session.screen.includes('Staged attachments'))
  assert(JSON.stringify(world.model.requests[sentBefore]?.input).includes('input_image'), 'the staged image was not sent')
  await session.quit()
  const [entries] = await sessionEntries(world.home)
  assert(entries, 'no session file was written')
  assert.deepEqual(messageTexts(entries, 'user'), ['READ notes.txt', 'ECHO second prompt', 'ECHO picture [Image #1]'])
  assert(messageTexts(entries, 'toolResult').some(text => text.includes('alpha line')), 'the tool result was not persisted')
  assert(messageTexts(entries, 'assistant').includes('Echo: second prompt.'))

  // Resumed, the transcript replays from the session file and the next request carries the history.
  const resumed = world.launch(['--continue'])
  await resumed.ready()
  await resumed.wait('replayed transcript', () => resumed.screen.includes('Echo: second prompt.') && resumed.screen.includes('> ECHO second prompt'))
  const before = world.model.requests.length
  resumed.send('ECHO third\r')
  await resumed.wait('resumed answer', () => resumed.screen.includes('Echo: third.'))
  const sent = JSON.stringify(world.model.requests[before]?.input)
  assert(sent.includes('READ notes.txt') && sent.includes('Echo: second prompt.'), 'the resumed request lost its history')
  await resumed.quit()
  const files = await sessionEntries(world.home)
  assert.equal(files.length, 1, '--continue started a new session file')
  assert.deepEqual(messageTexts(files[0] ?? [], 'user'), ['READ notes.txt', 'ECHO second prompt', 'ECHO picture [Image #1]', 'ECHO third'])
  assert.deepEqual(await readdir(world.project), ['notes.txt'], 'the session wrote into the project')
})

await liveScenario('inline live session steers a held turn, cancels another, and returns the unsent steer', async (world) => {
  const session = world.launch([], true)
  await session.ready()
  session.assertShellHistory()
  session.send('HOLD first\r')
  await session.wait('held stream', () => session.screen.includes('Holding') && session.screen.includes('Esc interrupts'))
  session.send('STEER left\r')
  await session.wait('steer waits for the next step', () => session.screen.includes('Next step: STEER left'))
  world.model.release()
  await session.wait('steer answered', () => session.history.includes('Steered: left.') && /^ {2}✓ Completed/mu.test(session.screen))
  assert(!session.screen.includes('Pending input'), 'the delivered steer stayed pending')
  session.send('HOLD second\r')
  await session.wait('second held stream', () => world.model.holding === 1 && session.screen.includes('Esc interrupts'))
  session.send('STEER keep me\r')
  await session.wait('second steer pending', () => session.screen.includes('Next step: STEER keep me'))
  session.send('\x1b')
  await session.wait('turn interrupted and steer returned', () => /^ {2}■ Interrupted/mu.test(session.screen)
    && session.screen.includes('❯ STEER keep me') && session.screen.includes('Unsent prompts are back in your draft'))
  assert.equal(world.model.aborted, 1, 'the held request was not aborted')
  // The returned draft is not sent again by itself.
  const requests = world.model.requests.length
  await delay(300)
  assert.equal(world.model.requests.length, requests, 'the cancelled steer reached the model')
  assert.equal(session.history.split('\n').filter(line => line.trimEnd() === '> HOLD first').length, 1, 'a committed prompt was lost or duplicated')
  session.assertShellHistory()
  await session.quit()
  session.assertShellHistory()
  session.assertNoChromeResidue(true)
  const [entries] = await sessionEntries(world.home)
  assert.deepEqual(messageTexts(entries ?? [], 'user'), ['HOLD first', 'STEER left', 'HOLD second'])
})

/** A process `ps` listed. */
type Listed = { parent: number; command: string }

/**
 * Each live process (not a zombie) as pid → parent pid and command
 * line. A listing that failed, or that misses this process, is an error, so
 * no check reads it as "nothing left".
 */
function processTable(): Map<number, Listed> {
  const listed = Bun.spawnSync(['ps', '-A', '-o', 'pid=,ppid=,stat=,args='])
  assert.equal(listed.exitCode, 0, `ps failed: ${listed.stderr.toString()}`)
  const listing = listed.stdout.toString()
  const table = new Map<number, Listed>()
  for (const line of listing.split('\n')) {
    const [, pid, parent, state, command] = /^\s*(\d+)\s+(\d+)\s+(\S+)\s*(.*)$/u.exec(line) ?? []
    if (pid === undefined || parent === undefined || state === undefined || state.startsWith('Z')) continue
    table.set(Number(pid), { parent: Number(parent), command: (command ?? '').trimEnd() })
  }
  assert(table.has(process.pid), 'ps listed no usable rows')
  return table
}

/** The live descendants of `root`, with their command lines. */
function descendants(root: number): Map<number, string> {
  const table = processTable()
  const found = new Map<number, string>()
  const queue = [root]
  for (let parent = queue.shift(); parent !== undefined; parent = queue.shift()) {
    for (const [pid, entry] of table) {
      if (entry.parent !== parent) continue
      queue.push(pid)
      found.set(pid, entry.command)
    }
  }
  return found
}

/** Which of `owned` (pid → command line) are still live: the same pid with the same command line. */
function survivors(owned: Map<number, string>): number[] {
  const table = processTable()
  return [...owned].filter(([pid, command]) => table.get(pid)?.command === command).map(([pid]) => pid)
}

await liveScenario('a live bash call cancelled by Esc fails its card and leaves no process', async (world) => {
  const session = world.launch()
  await session.ready()
  session.send('SLEEP\r')
  // The call's own process tree (its sandbox runner, shell, and `sleep 30`),
  // found under this session's process; other processes on the host with
  // the same command line are not this test's.
  let owned = new Map<number, string>()
  await session.wait('bash running', () => {
    if (!session.screen.includes('running bash') || !session.screen.includes('sleep 30')) return false
    owned = descendants(session.pid)
    return [...owned.values()].includes('sleep 30')
  })
  try {
    session.send('\x1b')
    await session.wait('bash interrupted', () => /^ {2}■ Interrupted/mu.test(session.screen))
    const left = survivors(owned)
    assert.deepEqual(left, [], `the cancelled bash call left its process: ${left.join(', ')}`)
    await session.quit()
  } finally {
    for (const pid of survivors(owned)) {
      try {
        process.kill(pid, 'SIGKILL')
      } catch (error) {
        // It exited after the listing, or with a parent killed before it.
        if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error
      }
    }
  }
})

for (const [signal, inline] of [['SIGTERM', false], ['SIGHUP', true], ['SIGINT', false]] as const) {
  await liveScenario(`${inline ? 'inline ' : ''}live ${signal} mid-turn shuts the session down and restores the terminal`, async (world) => {
    const session = world.launch([], inline)
    await session.ready()
    session.send('HOLD signal\r')
    await session.wait('held stream', () => session.screen.includes('Holding'))
    await session.quit(signal)
    if (inline) session.assertShellHistory()
    assert.equal(world.model.aborted, 1, 'shutdown did not abort the request')
    const [entries] = await sessionEntries(world.home)
    assert.deepEqual(messageTexts(entries ?? [], 'user'), ['HOLD signal'])
  })
}

await liveScenario('a live terminal hangup mid-turn exits quietly', async (world) => {
  const session = world.launch()
  await session.ready()
  session.send('HOLD hangup\r')
  await session.wait('held stream', () => session.screen.includes('Holding'))
  await session.hangUp()
})

await liveScenario('live command-line prompts run first, and a setup failure exits before the terminal changes', async (world) => {
  const session = world.launch(['ECHO from args', 'ECHO later arg'])
  await session.ready()
  await session.wait('both prompts answered', () => session.screen.includes('Echo: from args.') && session.screen.includes('Echo: later arg.')
    && /^ {2}✓ Completed/mu.test(session.screen))
  // A command bake-rs does not run is refused, and the draft is kept.
  session.send('/clone other\r')
  await session.wait('command refused', () => session.screen.includes('/clone is not available in bake-rs yet')
    && session.screen.includes('❯ /clone other'))
  await session.quit()

  const failed = world.launch(['--model', 'missing-model'])
  assert.equal(await failed.exit(), 1)
  assert(failed.raw.includes('missing-model'), `the error was not reported\n${failed.raw}`)
  assert(!failed.raw.includes('\x1b[?2004h') && !failed.raw.includes('\x1b[?1049h'), 'setup failure changed the terminal')
  assert(failed.unchangedModes, 'setup failure left terminal modes changed')

  // A model without a key opens the terminal, so /login can set one up; a
  // prompt is refused with its draft kept, and no session file is written.
  await writeFile(join(world.home, 'models.json'), JSON.stringify({
    providers: { loop: { baseUrl: `http://127.0.0.1:${world.model.port}/v1`, api: 'openai-responses', models: [{ id: 'loop-model' }] } },
  }))
  const before = (await sessionEntries(world.home)).length
  const keyless = world.launch(['--model', 'loop/loop-model'])
  await keyless.ready()
  await keyless.wait('unconfigured note', () => keyless.screen.includes(UNCONFIGURED))
  keyless.send('ECHO nope\r')
  await keyless.wait('prompt refused', () => keyless.screen.includes('No API key found') && keyless.screen.includes('❯ ECHO nope'))
  await keyless.quit()
  assert.equal((await sessionEntries(world.home)).length, before, 'a refused prompt wrote a session file')
})

/** Every tool result the home's sessions hold, as text. */
async function toolResults(home: string): Promise<string[]> {
  return (await sessionEntries(home)).flatMap(entries => messageTexts(entries, 'toolResult'))
}

const ALLOWED_PROMPT = 'Approval prompts: enabled.'
const REFUSED_PROMPT = 'Approval prompts: disabled.'

const CHOOSE_FIRST = 'Press Y first, then Enter'
const KEYS_ONLY = 'To answer, press Y then Enter'
const CONFIRM = 'Allow this call once?'

const PREPARING = 'Preparing approval…'
const KEYS = 'Y then Enter allows once · N rejects · Esc cancels'

/**
 * Waits until a freshly shown prompt takes keys. The input thread arms it
 * only after reading every key that was waiting once it was drawn; until
 * then the panel offers only Esc, and its keys row appears once armed.
 */
async function armed(session: Preview): Promise<void> {
  await session.wait('prompt armed', () => session.screen.includes(KEYS) && !session.screen.includes(PREPARING))
}

await liveScenario('an approval allows one write once, and typeahead, paste, a burst, and Enter alone do not', async (world) => {
  const session = world.launch(['--sandbox', 'read-only'])
  await session.ready()
  // Typed ahead with the prompt: read long before the request exists.
  session.send('WRITE ahead.txt\ry')
  await session.wait('first approval', () => session.screen.includes('Approval required: write  call_write')
    && session.screen.includes('escalate sandbox to workspace-write: create ahead.txt')
    && session.screen.includes('path: ahead.txt') && session.screen.includes('content: ahead.txt body')
    && (session.screen.includes(PREPARING) || session.screen.includes(KEYS)))
  assert(JSON.stringify(world.model.requests[0]).includes(ALLOWED_PROMPT), 'the interactive prompt does not offer approvals')
  await armed(session)
  // Enter alone does not allow: it says to press Y first.
  session.send('\r')
  await session.wait('Enter refused', () => session.screen.includes(CHOOSE_FIRST))
  // A pasted `y` decides nothing and says how to answer.
  session.send('\x1b[200~y\x1b[201~')
  await session.wait('paste refused', () => session.screen.includes(KEYS_ONLY))
  session.send('\r')
  await session.wait('Enter refused', () => session.screen.includes(CHOOSE_FIRST))
  // Text typed in one burst, as an unbracketed paste arrives, decides nothing.
  session.send('yes')
  await session.wait('burst refused', () => session.screen.includes(KEYS_ONLY))
  session.send('n')
  await session.wait('rejected write answered', () => session.screen.includes('Wrote:') && /^ {2}✓ Completed/mu.test(session.screen))
  assert(!existsSync(join(world.project, 'ahead.txt')), 'typeahead, a paste, a burst, or Enter approved the write')
  assert(session.screen.includes('❯ y'), `the typed-ahead key left the draft\n${session.screen}`)
  session.send('\x7f')

  // Two writes with one call id: the first answer allows only the first.
  session.send('WRITE one.txt two.txt\r')
  await session.wait('first of two', () => session.screen.includes('create one.txt') && session.screen.includes('Approval required: write'))
  await armed(session)
  session.send('y')
  await session.wait('allow chosen', () => session.screen.includes(CONFIRM))
  session.send('\r')
  await session.wait('second of two', () => session.screen.includes('create two.txt') && session.screen.includes('Approval required: write'))
  assert.equal(readFileSync(join(world.project, 'one.txt'), 'utf8'), 'one.txt body')
  assert(!existsSync(join(world.project, 'two.txt')), 'the first answer approved the second request')
  // The second prompt needs its own arming and choice: Enter alone, again, does not allow it.
  assert(!session.screen.includes(CONFIRM), 'the first choice carried over')
  await armed(session)
  session.send('\r')
  await session.wait('Enter refused again', () => session.screen.includes(CHOOSE_FIRST))
  assert(!existsSync(join(world.project, 'two.txt')), 'Enter allowed the second request')
  session.send('y')
  await session.wait('allow chosen again', () => session.screen.includes(CONFIRM))
  session.send('n')
  await session.wait('turn after two', () => /^ {2}✓ Completed/mu.test(session.screen) && !session.screen.includes('Approval required'))
  assert(!existsSync(join(world.project, 'two.txt')), 'a rejected write ran')
  await session.quit()
  const results = await toolResults(world.home)
  assert.equal(results.filter(text => text.includes('the user rejected escalating this operation to "workspace-write"')).length, 2, results.join('\n'))
  assert.deepEqual((await readdir(world.project)).sort(), ['one.txt'])
})

await liveScenario('an approval keeps the draft, its caret, and a pasted image, and Esc cancels it', async (world) => {
  const session = world.launch(['--sandbox', 'read-only'])
  await session.ready()
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==', 'base64')
  const imagePath = join(world.root, 'dot.png')
  await writeFile(imagePath, png)
  session.send('HOLDWRITE held.txt\r')
  await session.wait('held write call', () => world.model.holding === 1)
  session.send('keep ')
  session.send(`\x1b[200~${imagePath}\x1b[201~`)
  // The image is read off the loop and lands at the caret once read.
  await session.wait('image staged', () => session.screen.includes('❯ keep [Image #1]'))
  session.send('ZZ\x1b[D')
  await session.wait('draft with image', () => session.screen.includes('❯ keep [Image #1]ZZ') && session.screen.includes('Staged attachments: 1'))
  world.model.release()
  await session.wait('approval over the draft', () => session.screen.includes('create held.txt') && session.screen.includes('Approval required'))
  assert(session.screen.includes('keep [Image #1]ZZ') && session.screen.includes('Staged attachments: 1'), `the draft was not kept\n${session.screen}`)
  await armed(session)
  // Typing through the prompt is a burst, which answers nothing.
  session.send('xyz\x7f')
  await session.wait('typing refused', () => session.screen.includes(KEYS_ONLY))
  session.send('\x1b')
  await session.wait('cancelled', () => !session.screen.includes('Approval required') && /^ {2}✓ Completed/mu.test(session.screen))
  assert(!existsSync(join(world.project, 'held.txt')), 'a cancelled write ran')
  // The caret is where it was: typing lands before the last character.
  session.send('!')
  await session.wait('caret kept', () => session.screen.includes('❯ keep [Image #1]Z!Z'))
  await session.quit()
  const results = await toolResults(world.home)
  assert(results.some(text => text.includes('approval for escalating to "workspace-write" was cancelled')), results.join('\n'))
})

await liveScenario('a resize disarms a chosen approval until it is shown and armed again', async (world) => {
  const session = world.launch(['--sandbox', 'read-only'])
  await session.ready()
  session.send('WRITE resized.txt\r')
  await session.wait('approval', () => session.screen.includes('create resized.txt') && session.screen.includes('Approval required: write'))
  await armed(session)
  session.send('y')
  await session.wait('allow chosen', () => session.screen.includes(CONFIRM))
  // Too short to show the request whole: the choice is gone, and Enter does nothing.
  await session.resize(80, 3)
  session.send('\r')
  await session.resize(80, 24)
  await session.wait('shown again', () => session.screen.includes('create resized.txt'))
  assert(!session.screen.includes(CONFIRM), 'the choice survived the resize')
  assert(!existsSync(join(world.project, 'resized.txt')), 'Enter after the resize allowed the write')
  await armed(session)
  session.send('y')
  await session.wait('allow chosen again', () => session.screen.includes(CONFIRM))
  session.send('\r')
  await session.wait('allowed', () => /Wrote: Successfully wrote/u.test(session.screen))
  assert.equal(readFileSync(join(world.project, 'resized.txt'), 'utf8'), 'resized.txt body')
  await session.quit()
})

await liveScenario('inline approvals under the never policy show no prompt, and an approved write runs', async (world) => {
  const never = world.launch(['--sandbox', 'read-only', '--approval', 'never'], true)
  await never.ready()
  never.send('WRITE never.txt\r')
  await never.wait('refused write answered', () => never.screen.includes('Wrote:') && /^ {2}✓ Completed/mu.test(never.screen))
  assert(!never.raw.includes('Approval required'), 'the never policy showed a prompt')
  assert(JSON.stringify(world.model.requests[0]).includes(REFUSED_PROMPT), 'the never policy offered approvals')
  assert(!existsSync(join(world.project, 'never.txt')))
  await never.quit()
  never.assertShellHistory()

  const ask = world.launch(['--sandbox', 'read-only'], true)
  await ask.ready()
  ask.send('WRITE inline.txt\r')
  await ask.wait('inline approval', () => ask.screen.includes('Approval required: write  call_write'))
  await armed(ask)
  ask.send('y')
  await ask.wait('allow chosen', () => ask.screen.includes(CONFIRM))
  ask.send('\r')
  await ask.wait('approved write answered', () => /Wrote: Successfully wrote/u.test(ask.screen) && /^ {2}✓ Completed/mu.test(ask.screen))
  assert.equal(readFileSync(join(world.project, 'inline.txt'), 'utf8'), 'inline.txt body')
  await ask.quit()
  ask.assertShellHistory()
  ask.assertNoChromeResidue(true)
})

for (const [how, inline] of [['Ctrl+C', false], ['SIGTERM', true], ['hangup', false], ['inline hangup', true]] as const) {
  await liveScenario(`${how} with an approval waiting restores the terminal and the write never runs`, async (world) => {
    const session = world.launch(['--sandbox', 'read-only'], inline)
    await session.ready()
    session.send('WRITE pending.txt\r')
    await session.wait('approval waiting', () => session.screen.includes('Approval required: write'))
    const requests = world.model.requests.length
    if (how.endsWith('hangup')) await session.hangUp()
    else await session.quit(how === 'SIGTERM' ? 'SIGTERM' : undefined)
    if (inline && how === 'SIGTERM') session.assertShellHistory()
    assert(!existsSync(join(world.project, 'pending.txt')), 'the write ran after exit')
    assert.equal(world.model.requests.length, requests, 'the withdrawn call went on to another model request')
    assert.deepEqual(await readdir(world.project), [], 'the session wrote into the project')
  })
}

// ---- CLIProxyAPI sign-in, model and thinking controls, refresh, sign-out ----
//
// The fake model also answers `GET /v1/models` as a CLIProxyAPI lists its
// models, so the account's route sends prompts back to it. Keys are fake.


/** Waits until the open field takes keys: drawn whole and armed. */
async function fieldReady(session: Preview, prompt: string): Promise<void> {
  await session.wait(`${prompt} field ready`, () => session.screen.includes(`${prompt} (`) && session.screen.includes(FIELD_KEYS)
    && !session.screen.includes('Preparing'))
}

/** Waits until the composer takes keys again after a dialog closed. */
async function composerReady(session: Preview): Promise<void> {
  await session.wait('composer ready', () => !session.screen.includes('Preparing') && !session.screen.includes(FIELD_KEYS))
}

/** Types `text` one key per write, each after the last was drawn, as a person types. */
async function type(session: Preview, text: string, shown: (typed: number) => boolean): Promise<void> {
  for (let typed = 1; typed <= text.length; typed++) {
    session.send(text[typed - 1] ?? '')
    await session.wait(`typed ${typed}`, () => shown(typed))
  }
}

/** Awaits `promise`, failing with `message` after 10 seconds. */
async function bounded(promise: Promise<void>, message: string): Promise<void> {
  const timeout = Symbol('timeout')
  assert.notEqual(await Promise.race([promise, delay(10_000).then(() => timeout)]), timeout, message)
}

/** Clears the open field with Ctrl+U and waits until it is drawn empty. */
async function clearField(session: Preview): Promise<void> {
  session.send('\x15')
  await session.wait('field cleared', () => session.screen.split('\n').some(line => /^ {2}(?:│ )?>\s*(?:│\s*)?$/u.test(line.trimEnd())))
}

/** Enters the fake model's address in the open address field. */
async function enterUrl(session: Preview, world: LiveWorld): Promise<void> {
  await fieldReady(session, 'Base URL')
  await clearField(session)
  const url = `http://127.0.0.1:${world.model.port}`
  await type(session, url, typed => session.screen.includes(`> ${url.slice(0, typed)}`))
  session.send('\r')
  await session.wait('address answered', () => session.screen.includes('Working…') || session.screen.includes('API key ('))
}

/** Signs in through the dialogs with the fake model as the proxy. */
async function signIn(session: Preview, world: LiveWorld): Promise<void> {
  session.send('/login\r')
  await fieldReady(session, 'Base URL')
  await clearField(session)
  const url = `http://127.0.0.1:${world.model.port}`
  await type(session, url, typed => session.screen.includes(`> ${url.slice(0, typed)}`))
  session.send('\r')
  await fieldReady(session, 'API key')
  await type(session, PROXY_KEY, typed => session.screen.includes(`> ${'•'.repeat(typed)}`))
  session.send('\r')
}

function readJson(path: string): Record<string, unknown> {
  return JSON.parse(readFileSync(path, 'utf8')) as Record<string, unknown>
}

for (const inline of [false, true]) {
  await liveScenario(`${inline ? 'inline ' : ''}a keyless home signs in, prompts, changes model and thinking, refreshes, resumes, and signs out`, async (world) => {
    await rm(join(world.home, 'models.json'))
    const session = world.launch([], inline)
    await session.ready()
    await session.wait('unconfigured note', () => session.screen.includes(UNCONFIGURED))
    session.send('ECHO early\r')
    await session.wait('prompt refused', () => session.screen.includes('❯ ECHO early') && /No model/u.test(session.screen))
    session.send('\x15')
    await session.wait('draft cleared', () => !session.screen.includes('ECHO early'))
    await signIn(session, world)
    await session.wait('signed in', () => session.screen.includes('Signed in to CLIProxyAPI') && session.screen.includes('now using cliproxyapi/gpt-loop'))
    await composerReady(session)
    assert(!session.raw.includes(PROXY_KEY) && !session.history.includes(PROXY_KEY), 'the key reached the terminal')
    assert.equal(world.model.authorizations.at(-1), `Bearer ${PROXY_KEY}`)
    const auth = readJson(join(world.home, 'auth.json')) as { cliproxyapi?: { key?: string } }
    assert.equal(auth.cliproxyapi?.key, PROXY_KEY)
    assert(!readFileSync(join(world.home, 'settings.json'), 'utf8').includes(PROXY_KEY), 'the key reached settings.json')

    session.send('ECHO signed in\r')
    await session.wait('answer through the route', () => session.screen.includes('Echo: signed in.'))
    assert.equal(world.model.authorizations.at(-1), `Bearer ${PROXY_KEY}`)

    session.send('/model gpt-two\r')
    await session.wait('model changed', () => session.screen.includes('Model: cliproxyapi/gpt-two'))
    await composerReady(session)
    session.send('/thinking\r')
    await session.wait('thinking list', () => session.screen.includes('Thinking level') && session.screen.includes('Ctrl+S to set as default')
      && !session.screen.includes('Preparing'))
    session.send('\x1b[B')
    // The pointer moves off the current level, which keeps its mark.
    await session.wait('next level selected', () => session.screen.split('\n').some(line => /▸ \w+/u.test(line) && !line.includes('Current')))
    session.send('\x13')
    await session.wait('thinking saved', () => /Thinking level: \w+, saved as the default/u.test(session.screen))
    const level = /Thinking level: (\w+),/u.exec(session.screen)?.[1]
    assert(level, 'no thinking level shown')
    await composerReady(session)
    session.send('/refresh-models\r')
    await session.wait('refreshed', () => /CLIProxyAPI model list/u.test(session.screen))
    await composerReady(session)
    session.send('ECHO with two\r')
    await session.wait('answer with gpt-two', () => session.screen.includes('Echo: with two.'))
    await session.quit()
    if (inline) session.assertShellHistory()
    assert(!session.raw.includes(PROXY_KEY), 'the key reached the terminal')
    const [entries] = await sessionEntries(world.home)
    assert(entries, 'no session file was written')
    assert(!JSON.stringify(entries).includes(PROXY_KEY), 'the key reached the session file')
    const changes = entries.filter(entry => entry.type === 'model_change').map(entry => `${String(entry.provider)}/${String(entry.modelId)}`)
    assert.deepEqual(changes.slice(-2), ['cliproxyapi/gpt-loop', 'cliproxyapi/gpt-two'])
    assert(entries.some(entry => entry.type === 'thinking_level_change' && entry.thinkingLevel === level), 'the thinking change was not recorded')

    // Resumed: the session's model and level come back, and the route still works.
    const resumed = world.launch(['--continue'], inline)
    await resumed.ready()
    await resumed.wait('resumed status', () => resumed.screen.includes('gpt-two') && resumed.screen.includes(level))
    resumed.send('/logout\r')
    await resumed.wait('signed out', () => resumed.screen.includes('Signed out of CLIProxyAPI'))
    await composerReady(resumed)
    resumed.send('ECHO after\r')
    await resumed.wait('prompt refused after sign-out', () => resumed.screen.includes('❯ ECHO after') && resumed.screen.includes('No API key found'))
    await resumed.quit()
    const signedOut = readJson(join(world.home, 'auth.json'))
    assert.equal(signedOut.cliproxyapi, undefined, 'the key stayed in auth.json')
    assert.equal(readJson(join(world.home, 'settings.json')).cliproxyapi, null)
    assert.deepEqual(await readdir(world.project), [], 'the session wrote into the project')
  })
}

await liveScenario('a sign-in cancelled at each step, during its check, or by a signal writes nothing and keeps the draft', async (world) => {
  await rm(join(world.home, 'models.json'))
  const session = world.launch([], true)
  await session.ready()
  // Esc at the address.
  session.send('/login\r')
  await fieldReady(session, 'Base URL')
  session.send('\x1b')
  await session.wait('cancelled at the address', () => session.screen.includes('❯ /login') && session.screen.includes('Cancelled'))
  await composerReady(session)
  session.send('\x15')
  await session.wait('draft cleared', () => !session.screen.includes('❯ /login'))
  // The address with typeahead behind its Enter: the queued keys reach
  // neither the key field nor the draft.
  session.send('/login\r')
  await fieldReady(session, 'Base URL')
  await clearField(session)
  const url = `http://127.0.0.1:${world.model.port}`
  await type(session, url, typed => session.screen.includes(`> ${url.slice(0, typed)}`))
  // Enter must arrive alone to answer: one in a burst with typing never does.
  session.send('\r')
  await session.wait('address answered', () => session.screen.includes('Working…') || session.screen.includes('API key ('))
  session.send('typed-ahead')
  await fieldReady(session, 'API key')
  assert(!/> •/u.test(session.screen), `typeahead reached the key field\n${session.screen}`)
  // Esc at the key.
  session.send('\x1b')
  await session.wait('cancelled at the key', () => session.screen.includes('❯ /login') && session.screen.includes('Cancelled'))
  await composerReady(session)
  assert(!session.screen.includes('typed-ahead'), 'typeahead reached the draft')
  session.send('\x15')
  // Esc while the proxy check is held: it is aborted.
  world.model.holdModels = true
  session.send('/login\r')
  await enterUrl(session, world)
  await fieldReady(session, 'API key')
  await type(session, PROXY_KEY, typed => session.screen.includes(`> ${'•'.repeat(typed)}`))
  session.send('\r')
  await session.wait('check held', () => world.model.holding === 1 && session.screen.includes('Working…'))
  session.send('spill')
  session.send('\x1b')
  await session.wait('cancelled during the check', () => session.screen.includes('❯ /login') && session.screen.includes('Cancelled'))
  await bounded(world.model.listAborted.promise, 'the held check was not aborted')
  await composerReady(session)
  assert(!session.screen.includes('spill'), 'keys typed while waiting reached the draft')
  // A signal during secret input restores the terminal and saves nothing.
  world.model.holdModels = false
  session.send('\x15')
  session.send('/login\r')
  await enterUrl(session, world)
  await fieldReady(session, 'API key')
  await type(session, PROXY_KEY.slice(0, 6), typed => session.screen.includes(`> ${'•'.repeat(typed)}`))
  await session.quit('SIGTERM')
  session.assertShellHistory()
  assert(!session.raw.includes(PROXY_KEY.slice(0, 6)) && !session.history.includes(PROXY_KEY.slice(0, 6)), 'the key reached the terminal')
  assert(!existsSync(join(world.home, 'auth.json')), 'a cancelled sign-in wrote auth.json')
  assert(!existsSync(join(world.home, 'settings.json')), 'a cancelled sign-in wrote settings.json')
  assert.deepEqual(await sessionEntries(world.home), [], 'a cancelled sign-in wrote a session file')
})

await liveScenario('a terminal hangup while a sign-in check is held aborts the check, restores the terminal, and saves nothing', async (world) => {
  await rm(join(world.home, 'models.json'))
  world.model.holdModels = true
  const session = world.launch([], true)
  await session.ready()
  session.send('/login\r')
  await enterUrl(session, world)
  await fieldReady(session, 'API key')
  await type(session, PROXY_KEY, typed => session.screen.includes(`> ${'•'.repeat(typed)}`))
  session.send('\r')
  await session.wait('check held', () => world.model.holding === 1 && session.screen.includes('Working…'))
  await session.hangUp()
  await bounded(world.model.listAborted.promise, 'the held check was not aborted')
  assert(!session.raw.includes(PROXY_KEY), 'the key reached the terminal')
  assert(!existsSync(join(world.home, 'auth.json')), 'a hung-up sign-in wrote auth.json')
  assert(!existsSync(join(world.home, 'settings.json')), 'a hung-up sign-in wrote settings.json')
  assert.deepEqual(await sessionEntries(world.home), [], 'a hung-up sign-in wrote a session file')
})

// ---- Session sheets: /name, /session, /new, /resume ----

/** Blank rows between the last row holding `text` and the composer's top edge. */
function gapAboveControls(screen: string, text: string): number {
  const lines = screen.split('\n')
  const at = lines.findLastIndex(line => line.includes(text))
  assert(at >= 0, `${text} is not on screen\n${screen}`)
  const box = lines.findIndex((line, index) => index > at && /^╭─/u.test(line))
  assert(box > at, `no composer under ${text}\n${screen}`)
  return lines.slice(at + 1, box).filter(line => line.trim() === '').length
}

/** Waits until an open sheet holds the keyboard: its keys are shown, not its preparing line. */
async function sheetReady(session: Preview, title: string): Promise<void> {
  await session.wait(`${title} sheet ready`, () => session.screen.includes(title) && /Esc (?:cancel|close)/u.test(session.screen)
    && !session.screen.includes('Preparing'))
}

for (const inline of [false, true]) {
  await liveScenario(`${inline ? 'inline ' : ''}session sheets name, show, start, and resume sessions without mixing their logs`, async (world) => {
    const port = world.model.port
    await writeFile(join(world.home, 'models.json'), JSON.stringify({
      providers: { loop: { baseUrl: `http://127.0.0.1:${port}/v1`, api: 'openai-responses', apiKey: 'test-key',
        models: [{ id: 'loop-model', name: 'Loop', contextWindow: 1000, input: ['text', 'image'] },
          ...Array.from({ length: 11 }, (_, n) => ({ id: `loop-extra-${n + 1}`, name: `Loop extra ${n + 1}`, contextWindow: 200_000 }))] } },
    }))
    const session = world.launch([], inline)
    await session.ready()
    session.send('ECHO alpha\r')
    await session.wait('first answer completed', () => session.screen.includes('Echo: alpha.')
      && /^ {2}✓ Completed/mu.test(session.screen))
    session.send('/name Alpha work\r')
    await session.wait('named', () => session.screen.includes('Session name set: Alpha work'))
    await composerReady(session)
    // Inline, the held region keeps its transcript viewport's rows (8) under
    // the newest settled row; a closed sheet or a switch must add none.
    const settledGap = inline ? gapAboveControls(session.screen, 'Session name set: Alpha work') : 0
    // A tall model sheet opens framed, then closes with the transcript back against the controls.
    session.send('/model\r')
    await sheetReady(session, 'Model · 12 models from loop')
    assert(session.screen.includes('▸ Loop') && session.screen.includes('Current') && session.screen.includes('> Type to filter'), session.screen)
    assert(/╭─+╮/u.test(session.screen) && /\d+\/12/u.test(session.screen), session.screen)
    session.send('\x1b')
    await session.wait('model sheet cancelled', () => session.screen.includes('❯ /model') && session.screen.includes('Cancelled'))
    await composerReady(session)
    // Inline, the closed sheet's rows do not stay as a blank gap over the controls.
    if (inline) assert(gapAboveControls(session.screen, 'Session name set: Alpha work') <= settledGap, `a blank gap stayed after the sheet closed\n${session.screen}`)
    session.send('\x15')
    await session.wait('draft cleared', () => !session.screen.includes('❯ /model'))
    // /session is a read-only sheet; closing it sends nothing and keeps no draft.
    const requests = world.model.requests.length
    session.send('/session\r')
    await sheetReady(session, 'Session info')
    assert(session.screen.includes('Alpha work') && /Messages/u.test(session.screen), session.screen)
    session.send('\x1b')
    await composerReady(session)
    await session.wait('info closed', () => !session.screen.includes('Session info') && session.screen.includes('❯ Type a draft'))
    assert.equal(world.model.requests.length, requests, 'a sheet sent a model request')
    session.send('/new\r')
    await session.wait('new session', () => session.screen.includes('New session started'))
    await composerReady(session)
    session.send('ECHO beta\r')
    await session.wait('second answer completed', () => session.screen.includes('Echo: beta.')
      && /^ {2}✓ Completed/mu.test(session.screen))
    session.send('/resume\r')
    await sheetReady(session, 'Resume session')
    assert(session.screen.includes('Alpha work') && session.screen.includes('Current'), session.screen)
    await type(session, 'alpha', typed => session.screen.includes(`> ${'alpha'.slice(0, typed)}`))
    await session.wait('one match', () => session.screen.includes('▸ Alpha work') && session.screen.includes('1/1'))
    session.send('\r')
    await session.wait('resumed', () => session.screen.includes('Echo: alpha.') && !session.screen.includes('Echo: beta.')
      && !session.screen.includes('Resume session'))
    await composerReady(session)
    if (!inline) assert(session.screen.includes('Resumed session Alpha work'), session.screen)
    if (inline) assert(gapAboveControls(session.screen, 'Echo: alpha.') <= settledGap, `the resumed transcript is not against the controls\n${session.screen}`)
    const before = world.model.requests.length
    session.send('ECHO gamma\r')
    await session.wait('resumed answer completed', () => session.screen.includes('Echo: gamma.')
      && /^ {2}✓ Completed/mu.test(session.screen))
    const sent = JSON.stringify(world.model.requests[before]?.input)
    assert(sent.includes('ECHO alpha') && !sent.includes('ECHO beta'), `the resumed request mixed sessions: ${sent}`)
    await session.quit()
    if (inline) session.assertShellHistory()
    const files = await sessionEntries(world.home)
    assert.equal(files.length, 2, 'expected one file per session')
    const users = files.map(entries => messageTexts(entries, 'user')).sort((a, b) => a.length - b.length)
    assert.deepEqual(users, [['ECHO beta'], ['ECHO alpha', 'ECHO gamma']])
    const named = files.find(entries => messageTexts(entries, 'user').includes('ECHO alpha'))
    assert(named?.some(entry => entry.type === 'session_info' && entry.name === 'Alpha work'), 'the name was not recorded')
    assert.deepEqual(await readdir(world.project), [], 'the session wrote into the project')
  })
}

// ---- Session tree navigation and forks: /tree, /fork ----

/** Waits until `answer` is on screen and the turn that wrote it has settled: its completed status sits below it. */
async function turnSettled(session: Preview, answer: string): Promise<void> {
  await session.wait(`${answer} settled`, () => {
    const lines = session.screen.split('\n')
    const at = lines.findLastIndex(line => line.includes(answer))
    const done = lines.findLastIndex(line => /^ {2}✓ Completed/u.test(line))
    return at >= 0 && done > at
  })
}

for (const inline of [false, true]) {
  await liveScenario(`${inline ? 'inline ' : ''}tree navigation and fork branch the right logs, restore drafts, and send no hidden request`, async (world) => {
    const port = world.model.port
    await writeFile(join(world.home, 'models.json'), JSON.stringify({
      providers: { loop: { baseUrl: `http://127.0.0.1:${port}/v1`, api: 'openai-responses', apiKey: 'test-key',
        models: [{ id: 'loop-model', name: 'Loop', contextWindow: 1000 }] } },
    }))
    const session = world.launch([], inline)
    await session.ready()
    session.send('ECHO alpha\r')
    await turnSettled(session, 'Echo: alpha.')
    session.send('ECHO beta\r')
    await turnSettled(session, 'Echo: beta.')
    await composerReady(session)
    const requests = world.model.requests.length
    // The tree marks the current path and leaf; typing filters it.
    session.send('/tree\r')
    await sheetReady(session, 'Session tree')
    assert(session.screen.includes('• user: ECHO alpha') && session.screen.includes('Current'), session.screen)
    if (!inline) {
      await session.resize(60, 20)
      await sheetReady(session, 'Session tree')
      await session.resize(80, 24)
      await sheetReady(session, 'Session tree')
    }
    await type(session, 'user: echo beta', typed => session.screen.includes(`> ${'user: echo beta'.slice(0, typed)}`))
    await session.wait('one match', () => session.screen.includes('1/1'))
    session.send('\r')
    // The chosen message is back in the draft, unsent, and the transcript replays the path before it.
    await session.wait('navigated', () => session.screen.includes('❯ ECHO beta') && !session.screen.includes('Echo: beta.')
      && session.screen.includes('Echo: alpha.') && !session.screen.includes('Session tree'))
    await composerReady(session)
    assert.equal(world.model.requests.length, requests, 'navigation sent a model request')
    session.send('\x15')
    await session.wait('draft cleared', () => !session.screen.includes('❯ ECHO beta'))
    let before = world.model.requests.length
    session.send('ECHO gamma\r')
    await turnSettled(session, 'Echo: gamma.')
    let sent = JSON.stringify(world.model.requests[before]?.input)
    assert(sent.includes('ECHO alpha') && sent.includes('ECHO gamma') && !sent.includes('ECHO beta'), `the branched request: ${sent}`)
    await composerReady(session)
    // /fork lists every user message, latest selected, and starts a new session before the chosen one.
    const forkRequests = world.model.requests.length
    session.send('/fork\r')
    await sheetReady(session, 'Fork from message')
    assert(session.screen.includes('ECHO beta') && session.screen.includes('▸ ECHO gamma'), session.screen)
    session.send('\r')
    await session.wait('forked', () => session.screen.includes('❯ ECHO gamma') && !session.screen.includes('Echo: gamma.')
      && session.screen.includes('Echo: alpha.') && !session.screen.includes('Fork from message'))
    await composerReady(session)
    if (!inline) assert(session.screen.includes('Forked to new session'), session.screen)
    assert.equal(world.model.requests.length, forkRequests, 'the fork sent a model request')
    session.send('\x15')
    await session.wait('draft cleared', () => !session.screen.includes('❯ ECHO gamma'))
    before = world.model.requests.length
    session.send('ECHO delta\r')
    await turnSettled(session, 'Echo: delta.')
    sent = JSON.stringify(world.model.requests[before]?.input)
    assert(sent.includes('ECHO alpha') && sent.includes('ECHO delta') && !sent.includes('ECHO gamma') && !sent.includes('ECHO beta'), `the forked request: ${sent}`)
    await session.quit()
    if (inline) session.assertShellHistory()
    const files = await sessionEntries(world.home)
    assert.equal(files.length, 2, 'expected the source and its fork')
    const source = files.find(entries => messageTexts(entries, 'user').includes('ECHO beta'))
    const fork = files.find(entries => messageTexts(entries, 'user').includes('ECHO delta'))
    assert(source && fork, 'a session file is missing')
    assert.deepEqual(messageTexts(source, 'user'), ['ECHO alpha', 'ECHO beta', 'ECHO gamma'])
    assert.deepEqual(messageTexts(fork, 'user'), ['ECHO alpha', 'ECHO delta'])
    const parentOf = (text: string) => source.find(entry => entry.type === 'message'
      && messageTexts([entry], 'user')[0] === text)?.parentId
    assert.equal(parentOf('ECHO beta'), parentOf('ECHO gamma'), 'beta and gamma are not siblings in the source log')
    const header = fork[0] as { type?: string; parentSession?: string }
    assert(header.type === 'session' && typeof header.parentSession === 'string' && header.parentSession.endsWith('.jsonl'), 'the fork does not name its source')
    assert.deepEqual(await readdir(world.project), [], 'the session wrote into the project')
  })
}
