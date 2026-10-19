#!/usr/bin/env bun
/**
 * Drive the built Rust preview, and live `bake-rs` sessions against a fake
 * streaming model, in a real terminal and check their return to the shell.
 *
 * On Linux and macOS the terminal is a POSIX PTY (`Bun.Terminal`) with a
 * shell that waits for a handshake before it `exec`s the program, so the
 * driver reads cooked termios flags first. On Windows it is a pseudoconsole
 * (ConPTY) owned by the host in `rust/crates/bake-process/examples/conpty`
 * (`rust-pty-conpty.ts`): its first client reads the console modes before
 * the program starts and after it exits, and every process runs in the
 * host's kill-on-close job. The scenarios are the same on both; where
 * Windows cannot do or show what a scenario needs, the run names the gap
 * (`GAP`) instead of passing it.
 */
import assert from 'node:assert/strict'
import { appendFileSync, existsSync, readFileSync } from 'node:fs'
import { chmod, copyFile, mkdir, mkdtemp, readdir, rm, writeFile } from 'node:fs/promises'
import { execFileSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { parseArgs } from 'node:util'
import xterm from '@xterm/headless'
import { Unicode11Addon } from '@xterm/addon-unicode11'
import { buildConPtyHost, ConPty, modeRestoration, rawModeProblem, STATUS_CONTROL_C_EXIT, windowsEnv } from './rust-pty-conpty.ts'

const root = resolve(import.meta.dir, '..')
const { values } = parseArgs({ options: { binary: { type: 'string' }, filter: { type: 'string' }, conpty: { type: 'string' } } })
const windows = process.platform === 'win32'
const binary = resolve(values.binary ?? join(root, `rust/target/debug/bake-rs${windows ? '.exe' : ''}`))
/** The ConPTY host, on Windows: given with `--conpty`, or built now. */
const conptyHost = windows ? values.conpty === undefined ? buildConPtyHost(root) : resolve(values.conpty) : undefined

/** A live session's command line and the isolated environment it runs in. */
interface Live { readonly args: readonly string[]; readonly env: Readonly<Record<string, string>> }

type Signal = 'SIGINT' | 'SIGTERM' | 'SIGHUP'

/**
 * What a scenario could not do or show on this platform, by message. A
 * scenario that ends in one is reported as a gap, never as a pass; one
 * noted while a scenario passes is listed with the results.
 */
const gaps = new Set<string>()

/** A scenario ended where this platform has a named gap: reported as `GAP`, not a pass or a failure. */
class GapError extends Error {}

const NO_CONSOLE_HANDLER = 'bake-rs installs no Windows console control handler, so'

/** The terminal under the program: a POSIX PTY or a pseudoconsole, and the process on it. */
interface Backend {
  write(text: string): void
  resize(cols: number, rows: number): void
  /** The program's exit status. */
  readonly exited: Promise<number>
  readonly code: number | undefined
  /** The signal that ended the program, on POSIX; a Windows exit is only ever a code. */
  readonly signalCode: string | null
  /** Whether the terminal's output has ended, so every byte the program wrote has been read. */
  readonly streamEnded: boolean
  /** Asserts the program changed the terminal's modes to raw. */
  assertRaw(): Promise<void>
  /** Asserts the modes read after exit equal those read before the program started. */
  assertRestored(): Promise<void>
  unchangedModes(): Promise<boolean>
  signal(signal: Signal): void
  /** Ends the program at once, with no chance to restore anything: SIGKILL, or `TerminateProcess`. */
  kill(): void
  /** Closes the terminal under the program, as an emulator window closing does. */
  hangUp(): void
  /** Stops every thread of the program; [`resume`] continues it. */
  pause(): Promise<void>
  resume(): Promise<void>
  /** The program's descendants and other processes it started, by key, with a label: a command line or an image path. */
  owned(): Promise<Map<string, string>>
  /** Which of `owned` are still live. */
  survivors(owned: ReadonlyMap<string, string>): Promise<string[]>
  /** Ends what `survivors` found, after a failed check. */
  release(owned: ReadonlyMap<string, string>): Promise<void>
  /** Asserts nothing the program started is left. */
  assertGone(): Promise<void>
  close(): Promise<void>
}

/** A POSIX PTY with a shell that `exec`s the program after the driver's handshake. */
class PosixPty implements Backend {
  private readonly pty: Bun.Terminal
  private readonly process: Bun.Subprocess
  private readonly initialModes: readonly number[]
  readonly exited: Promise<number>
  code: number | undefined
  private streamStatus: number | undefined

  constructor(cwd: string, inline: boolean, live: Live | undefined, onData: (bytes: Uint8Array) => void) {
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
        data: (_terminal, bytes) => onData(bytes),
        exit: (_terminal, status) => { this.streamStatus = status },
      },
    })
    const pty = this.process.terminal
    assert(pty)
    this.pty = pty
    this.initialModes = this.modes()
    this.pty.write('start\n')
    this.exited = this.process.exited.then((code) => { this.code = code; return code })
  }

  private modes(): readonly number[] {
    return [this.pty.inputFlags, this.pty.outputFlags, this.pty.localFlags, this.pty.controlFlags]
  }

  write(text: string): void { this.pty.write(text) }

  resize(cols: number, rows: number): void { this.pty.resize(cols, rows) }

  get streamEnded(): boolean { return this.streamStatus !== undefined }

  get signalCode(): string | null { return this.process.signalCode }

  async assertRaw(): Promise<void> {
    assert.notDeepEqual(this.modes(), this.initialModes, 'preview did not acquire raw mode')
  }

  async assertRestored(): Promise<void> {
    assert.equal(this.signalCode, null)
    // Linux reports EIO when the last slave closes, including a clean exit.
    assert(this.streamStatus === 0 || (process.platform === 'linux' && this.streamStatus === 1))
    assert.deepEqual(this.modes(), this.initialModes, 'terminal mode flags were not restored')
  }

  async unchangedModes(): Promise<boolean> { return JSON.stringify(this.modes()) === JSON.stringify(this.initialModes) }

  signal(signal: Signal): void { this.process.kill(signal) }

  kill(): void { this.process.kill('SIGKILL') }

  hangUp(): void { this.pty.close() }

  async pause(): Promise<void> {
    this.process.kill('SIGSTOP')
    // Hold every application thread while the emulator reflows. Only Linux
    // shows the stopped state, so only Linux runs these scenarios.
    const deadline = performance.now() + 30_000
    while (!/^State:\s+T/mu.test(readFileSync(`/proc/${this.process.pid}/status`, 'utf8'))) {
      if (performance.now() > deadline) {
        this.process.kill('SIGCONT')
        throw new Error('the preview did not stop')
      }
      await delay(5)
    }
  }

  async resume(): Promise<void> { this.process.kill('SIGCONT') }

  async owned(): Promise<Map<string, string>> {
    return new Map([...descendants(this.process.pid)].map(([pid, command]) => [String(pid), command]))
  }

  async survivors(owned: ReadonlyMap<string, string>): Promise<string[]> {
    const table = processTable()
    return [...owned].filter(([pid, command]) => table.get(Number(pid))?.command === command).map(([pid]) => pid)
  }

  async release(owned: ReadonlyMap<string, string>): Promise<void> {
    for (const pid of await this.survivors(owned)) {
      try {
        process.kill(Number(pid), 'SIGKILL')
      } catch (error) {
        // It exited after the listing, or with a parent killed before it.
        if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error
      }
    }
  }

  async assertGone(): Promise<void> {
    assert.throws(() => process.kill(this.process.pid, 0), 'application remains alive after exit')
  }

  async close(): Promise<void> {
    if (this.code === undefined) this.process.kill('SIGKILL')
    await this.exited
    this.pty.close()
  }
}

/**
 * A pseudoconsole from the ConPTY host. Signals map as `consoleSignal`
 * documents; the program's exit and the job's settlement come from the
 * host's own handles and account.
 */
class WindowsConsole implements Backend {
  private readonly console: ConPty

  constructor(cwd: string, inline: boolean, live: Live | undefined, onData: (bytes: Uint8Array) => void) {
    assert(conptyHost)
    const args = live?.args ?? (inline ? ['preview', '--inline'] : ['preview'])
    this.console = new ConPty({
      host: [conptyHost], program: binary, args, cwd, cols: 80, rows: 24, history: inline, onData,
      env: windowsEnv(process.env, { TERM: 'xterm-256color', LANG: 'C.UTF-8', SSH_TTY: '/dev/pts/bake-pty', ...live?.env }),
    })
  }

  write(text: string): void { this.console.write(text) }

  resize(cols: number, rows: number): void { this.console.resize(cols, rows) }

  get exited(): Promise<number> { return this.console.exited }

  get code(): number | undefined { return this.console.exitCode }

  get streamEnded(): boolean { return this.console.closed }

  get signalCode(): null { return null }

  async assertRaw(): Promise<void> {
    const problem = rawModeProblem(await this.console.initial, await this.console.modes())
    assert.equal(problem, undefined, problem)
  }

  async assertRestored(): Promise<void> {
    await this.console.ended
    const { problem, gap } = modeRestoration(await this.console.initial, await this.console.final)
    assert.equal(problem, undefined, problem)
    if (gap !== undefined) gaps.add(gap)
    assert.deepEqual(this.console.errors, [], 'the ConPTY host reported errors')
  }

  async unchangedModes(): Promise<boolean> {
    const [initial, final] = await Promise.all([this.console.initial, this.console.final])
    return initial.input === final.input && initial.output === final.output
  }

  signal(signal: Signal): void { this.console.signal(signal) }

  kill(): void { this.console.signal('SIGKILL') }

  hangUp(): void { this.console.signal('SIGHUP') }

  async pause(): Promise<void> { await this.console.suspend() }

  async resume(): Promise<void> { await this.console.resume() }

  async owned(): Promise<Map<string, string>> {
    const members = await this.console.members()
    return new Map(members.filter(member => member.role === 'member' && member.alive).map(member => [String(member.pid), member.image]))
  }

  async survivors(owned: ReadonlyMap<string, string>): Promise<string[]> {
    // Once the program has exited the host closes and lists no members; its
    // settled job then says whether anything outlived the program: a job it
    // had to terminate held survivors, which it ended.
    const settled = async (): Promise<string[]> => {
      await this.console.ended
      assert(this.console.settled, 'the job did not settle')
      return this.console.settled.terminated ? [...owned.keys()] : []
    }
    if (this.console.closed) return settled()
    // Every pin is held open, so a pid names the same process until unpinned.
    let members
    try {
      members = await this.console.members()
    } catch (error) {
      // The program exited and the host closed while this was asked.
      if (this.console.closed) return settled()
      throw error
    }
    return members.filter(member => member.alive && owned.has(String(member.pid))).map(member => String(member.pid))
  }

  async release(): Promise<void> {
    // The host's job ends what is left when it closes; pins only hold handles.
    this.console.unpin()
  }

  async assertGone(): Promise<void> {
    await this.console.ended
    assert(this.console.settled, 'the job did not settle')
    assert.equal(this.console.settled.terminated, false, 'processes outlived the program until the job was terminated')
  }

  async close(): Promise<void> { await this.console.close() }
}

class Preview {
  private readonly terminal = new xterm.Terminal({ cols: 80, rows: 24, allowProposedApi: true })
  private readonly backend: Backend
  private parsed = Promise.resolve()
  /** Whether the emulator has parsed the start of a synchronized update but not its end. */
  private frameOpen = false
  private parsedTail = ''
  /** Everything the preview wrote, its last MiB, for sequences the screen does not show. */
  raw = ''

  constructor(cwd: string, readonly inline = false, readonly live?: Live) {
    // Emoji cases use Unicode 11 widths, matching modern terminal cell allocation.
    this.terminal.loadAddon(new Unicode11Addon())
    this.terminal.unicode.activeVersion = '11'
    const decoder = new TextDecoder()
    const data = (bytes: Uint8Array) => {
      const text = decoder.decode(bytes, { stream: true })
      this.raw = (this.raw + text).slice(-1_048_576)
      this.parsed = this.parsed.then(() => new Promise<void>(done => this.terminal.write(text, () => {
        this.track(text)
        done()
      })))
    }
    this.backend = windows ? new WindowsConsole(cwd, inline, live, data) : new PosixPty(cwd, inline, live, data)
    // The emulator's replies to the program's queries; none once the terminal is gone.
    this.terminal.onData((reply) => {
      if (!this.backend.streamEnded) this.backend.write(reply)
    })
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
    // ConPTY re-renders a row to the full width, with trailing blanks.
    const boxes = lines.filter(line => /^╭─.*╮ *$/u.test(line))
    assert.equal(boxes.length, exited ? 0 : 1, 'obsolete composer frames remain in terminal history')
    assert(!lines.some(line => line.includes('typing never reaches an agent')), 'obsolete inspector rows remain')
    const buffer = this.terminal.buffer.normal
    const scrollback = lines.slice(0, buffer.baseY).join('\n')
    assert(!scrollback.includes('❯') && !/^ {2}Agents .*Ctrl\+G$/mu.test(scrollback), 'live controls escaped into scrollback')
  }

  /**
   * Asserts the terminal saw `off` after its last `on`. A pseudoconsole
   * renders the program's screen itself and forwards a VT mode only when
   * its conhost passes it through; one it never forwarded is a named gap,
   * since conhost's own state for it is not observed.
   */
  private assertReleased(on: string, off: string, label: string, message: string): void {
    if (windows && !this.raw.includes(on)) {
      gaps.add(`ConPTY did not forward ${label} (${JSON.stringify(on)}); conhost's own state for it is not observed`)
      return
    }
    assert(this.raw.lastIndexOf(off) > this.raw.lastIndexOf(on), message)
  }

  /** Waits until `condition` holds of a whole frame, or of any output when `framed` is false. */
  async wait(label: string, condition: () => boolean, framed = true): Promise<void> {
    const deadline = performance.now() + 30_000
    while (true) {
      await this.parsed
      if ((!framed || !this.frameOpen) && condition()) return
      if ((this.backend.code !== undefined && this.backend.streamEnded) || performance.now() > deadline) {
        throw new Error(`${label}: ${this.backend.code === undefined ? 'timed out' : `exited ${this.backend.code}`}\n${this.screen}`)
      }
      await delay(5)
    }
  }

  async ready(): Promise<void> {
    // A pseudoconsole may keep the alternate screen to itself; quit names that gap.
    const screenType = () => this.terminal.buffer.active.type === (this.inline ? 'normal' : 'alternate')
      || (windows && !this.inline && !this.raw.includes('\x1b[?1049h'))
    await this.wait('Rust preview ready', () => screenType()
      // The sample session's newest lines show on the first frame, and so does the composer.
      && (this.live !== undefined || this.screen.includes('rejects it before splitting.')) && this.screen.includes('❯ ')
      && (windows || (this.raw.includes('\x1b[?2004h') && this.raw.includes('\x1b[?1000h\x1b[?1002h\x1b[?1006h'))))
    await this.backend.assertRaw()
  }

  send(text: string): void { this.backend.write(text) }

  /** Whether the composer's frame spans `cols` cells. */
  private composerSpans(cols: number): boolean {
    return this.screen.split('\n').some(line => /^╭─.*╮$/u.test(line) && line.length === cols)
  }

  async resize(cols: number, rows: number): Promise<void> {
    const offset = this.raw.length
    this.terminal.resize(cols, rows)
    this.backend.resize(cols, rows)
    // Fullscreen clears the screen on a resize; a pseudoconsole repaints its
    // own buffer instead of forwarding the clear, so only the redraw shows.
    await this.wait('resize redraw', () => this.inline
      ? this.raw.length > offset && this.composerSpans(cols)
      : this.raw.slice(offset).includes('\x1b[2J') || (windows && this.raw.length > offset && this.screen.includes('❯')))
  }

  /**
   * Resizes twice while the preview is stopped, after typing `keys`, so it
   * wakes to input and a resize at once and must read both.
   */
  async resizeBurst(keys: string): Promise<void> {
    const offset = this.raw.length
    await this.backend.pause()
    try {
      // Hold every application thread while the emulator reflows twice. The
      // final terminal size equals the previous frame, but that frame is stale.
      this.send(keys)
      for (const [cols, rows] of [[40, 12], [80, 24]] as const) {
        this.terminal.resize(cols, rows)
        this.backend.resize(cols, rows)
      }
    } finally { await this.backend.resume() }
    await this.wait('coalesced resize redraw', () => this.raw.slice(offset).includes(this.inline || windows ? '╭─' : '\x1b[2J'))
  }

  async quit(signal?: Signal): Promise<void> {
    if (signal === 'SIGTERM' && this.live === undefined) {
      this.send('\x07')
      await this.wait('agent list with hidden cursor', () => this.screen.includes('Fixed examples · nothing is running')
        && this.raw.lastIndexOf('\x1b[?25l') > this.raw.lastIndexOf('\x1b[?25h'))
    }
    if (signal === undefined) {
      // The first Ctrl+C only arms the quit; the second, inside its window, quits.
      this.send('\x03')
      await this.wait('quit armed', () => this.screen.includes('Press Ctrl-C again to quit'))
      assert.equal(this.backend.code, undefined, 'one Ctrl+C quit the preview')
      this.send('\x03')
    } else this.backend.signal(signal)
    await this.wait('preview process and PTY exit', () => this.backend.code !== undefined && this.backend.streamEnded, false)
    // The wait read the parse queue before its last output arrived; the
    // stream has ended now, so this queue holds every byte the preview wrote.
    await this.parsed
    const code = await this.backend.exited
    if (windows && signal !== undefined && code === STATUS_CONTROL_C_EXIT) {
      throw new GapError(`${NO_CONSOLE_HANDLER} ${signal === 'SIGHUP' ? 'a closed pseudoconsole (CTRL_CLOSE_EVENT)' : `${signal === 'SIGINT' ? 'CTRL_C_EVENT' : 'CTRL_BREAK_EVENT'}`} ends it through the default handler, without restoring the console`)
    }
    assert.equal(code, signal === 'SIGTERM' ? 143 : signal === 'SIGHUP' ? 129 : signal === 'SIGINT' ? 130 : 0)
    await this.backend.assertRestored()
    assert.equal(this.terminal.buffer.active.type, 'normal', 'alternate screen was not released')
    if (windows && !this.inline && !this.raw.includes('\x1b[?1049h')) gaps.add('ConPTY did not forward the alternate screen ("\\u001b[?1049h"); conhost keeps it')
    this.assertReleased('\x1b[?2004h', '\x1b[?2004l', 'bracketed paste', 'paste mode was not released')
    this.assertReleased('\x1b[?7l', '\x1b[?7h', 'autowrap', 'autowrap was not restored')
    for (const mode of ['1000', '1002', '1006'])
      this.assertReleased(`\x1b[?${mode}h`, `\x1b[?${mode}l`, `mouse mode ${mode}`, `mouse mode ${mode} was not released`)
    this.assertReleased('\x1b[?2026h', '\x1b[?2026l', 'synchronized output', 'synchronized output was left open')
    assert(this.raw.lastIndexOf('\x1b[?25h') > this.raw.lastIndexOf('\x1b[?25l'), 'cursor was not restored')
    await this.backend.assertGone()
  }

  /**
   * Ends the program abruptly and waits for its terminal to drain: the
   * control for the restoration checks, which must then see changed modes.
   */
  async terminate(): Promise<void> {
    this.backend.kill()
    await this.wait('killed process and terminal exit', () => this.backend.code !== undefined && this.backend.streamEnded, false)
    await this.parsed
    assert(!(await this.backend.unchangedModes()), 'the mode reads did not see the raw mode an abrupt exit leaves')
    await this.backend.assertGone()
  }

  /** Waits for the process to exit by itself and returns its status, for a run that never takes the terminal. */
  async exit(): Promise<number> {
    const code = await Promise.race([this.backend.exited, delay(30_000).then(() => 'timed out')])
    assert.equal(typeof code, 'number', 'the process did not exit')
    await this.wait('PTY drained', () => this.backend.streamEnded, false)
    await this.parsed
    return code as number
  }

  /** Whether the terminal's modes after exit equal those before the program started. */
  unchangedModes(): Promise<boolean> { return this.backend.unchangedModes() }

  /** The processes the program started: its descendants, or the other members of its job. */
  owned(): Promise<Map<string, string>> { return this.backend.owned() }

  survivors(owned: ReadonlyMap<string, string>): Promise<string[]> { return this.backend.survivors(owned) }

  release(owned: ReadonlyMap<string, string>): Promise<void> { return this.backend.release(owned) }

  /** Close the terminal under the preview, as an emulator window closing does, and expect the quiet hangup exit. */
  async hangUp(): Promise<void> {
    this.backend.hangUp()
    const code = await Promise.race([this.backend.exited, delay(5_000).then(() => 'timed out')])
    if (windows && code === STATUS_CONTROL_C_EXIT) {
      throw new GapError(`${NO_CONSOLE_HANDLER} a closed pseudoconsole (CTRL_CLOSE_EVENT) ends it through the default handler instead of the quiet hangup exit`)
    }
    assert.equal(code, 129, 'a closed terminal did not end the preview as a hangup')
    assert.equal(this.backend.signalCode, null)
  }

  async close(): Promise<void> {
    try {
      await this.backend.close()
    } finally {
      await this.parsed
      this.terminal.dispose()
    }
  }

  async saveFailure(path: string): Promise<void> {
    await writeFile(path, this.raw)
    // ANSI alone cannot replay the emulator's resize/reflow operations.
    await writeFile(`${path}.screen.txt`, this.history)
  }
}

/** Results by outcome, printed at the end. */
const results = { passed: [] as string[], failed: [] as string[], gapped: [] as string[] }

/** Runs `body` as the scenario `name`, recording a pass, a failure, or a named gap; the run continues either way. */
async function run(name: string, body: () => Promise<void>): Promise<void> {
  try {
    await body()
    results.passed.push(name)
    console.log(`PASS Rust PTY: ${name}`)
  } catch (error) {
    if (error instanceof GapError) {
      results.gapped.push(name)
      gaps.add(error.message)
      console.log(`GAP  Rust PTY: ${name}: ${error.message}`)
      return
    }
    results.failed.push(name)
    console.error(`FAIL Rust PTY: ${name}`)
    console.error(error)
  }
}

/** Run one scenario in a fresh working directory holding only `files`, which must be all it holds at the end. */
async function scenario(
  name: string, body: (preview: Preview) => Promise<void>, files: readonly string[] = [], inline = false,
): Promise<void> {
  if (values.filter !== undefined && !name.includes(values.filter)) {
    console.log(`SKIP Rust PTY: ${name} (filter ${values.filter})`)
    return
  }
  await run(name, async () => {
    const cwd = await mkdtemp(join(tmpdir(), 'bake-rust-pty-'))
    let preview: Preview | undefined
    try {
      for (const file of files) {
        await mkdir(dirname(join(cwd, file)), { recursive: true })
        await writeFile(join(cwd, file), '')
      }
      preview = new Preview(cwd, inline)
      await preview.ready()
      await body(preview)
      const seeded = [...new Set(files.map(file => file.split('/')[0]))].sort()
      assert.deepEqual((await readdir(cwd)).sort(), seeded, 'preview wrote files into the working directory')
    } catch (error) {
      if (preview !== undefined && !(error instanceof GapError)) {
        await mkdir(join(root, '.preflight'), { recursive: true })
        const transcript = join(root, '.preflight', `${basename(cwd)}.ansi`)
        await preview.saveFailure(transcript)
        console.error(`Rust PTY transcript: ${transcript}`)
      }
      throw error
    } finally {
      await preview?.close()
      await rm(cwd, { recursive: true, force: true })
    }
  })
}


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
  // ConPTY can preserve display padding after the directory.
  await preview.wait('sample stopped', () => preview.screen.split('\n').some(line => /^ {2}✓ Completed {2}\d+s +no model {2}\S+$/u.test(line.trimEnd()))
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

// Stopping the program needs Linux's process state, or Windows thread suspension.
if (process.platform === 'linux' || windows) {
  await scenario('coalesced resize invalidates stale screen', async (preview) => {
    preview.send('RESIZE_DRAFT')
    await preview.wait('draft before resize', () => preview.screen.includes('RESIZE_DRAFT'))
    await preview.resizeBurst('_TYPED')
    await preview.wait('draft after resize', () => preview.screen.includes('RESIZE_DRAFT_TYPED'))
    assert.equal(preview.screen.split('RESIZE_DRAFT').length - 1, 1, 'resize duplicated the draft')
    await preview.quit()
  })
} else console.log('SKIP Rust PTY: coalesced resize barrier needs Linux /proc process state or Windows thread suspension')

for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP'] as const) {
  await scenario(`${signal} restores terminal`, async (preview) => { await preview.quit(signal) })
}

// The control for every restoration check above: the same reads must see
// the raw modes a program leaves when it cannot restore them.
await scenario('an abrupt termination leaves raw modes, which the mode reads report', async (preview) => {
  await preview.terminate()
})

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
      // ConPTY re-renders a row to the full width, so trailing blanks are not compared.
      assert.deepEqual(lines.slice(sampleEnd + 1, userStart).map(line => line.trimEnd()), ['', '-'.repeat(79)],
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


if (process.platform === 'linux' || windows) {
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
} else console.log('SKIP Rust PTY: inline coalesced resize barrier needs Linux /proc process state or Windows thread suspension')

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
  /** Streams this prompt's whole answer but holds its response.completed event. */
  holdAnswerFor: string | undefined
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
    // A compaction's summary requests: the history (with `/compact`'s instructions) and a split turn's prefix.
    if (prompt.startsWith('<conversation>') || prompt.startsWith('# Conversation\n')) {
      if (prompt.includes('FAIL-SUMMARY')) {
        return new Response(JSON.stringify({ error: { message: 'summary refused by the fake model', type: 'invalid_request_error' } }),
          { status: 400, headers: { 'content-type': 'application/json' } })
      }
      const events = text(n, 'Summary: the greek letters so far.')
      return prompt.includes('HOLD-SUMMARY') ? this.held(request, events, 1) : sse(events)
    }
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
      if (prompt.startsWith('MCP ')) return sse(text(n, `MCP said: ${toolOutput.split('\n')[0]}`))
      if (prompt.startsWith('BACKGROUND')) return sse(text(n, 'Started.'))
      return sse(text(n, 'Slept.'))
    }
    if (prompt.startsWith('READ ')) return sse(call(n, 'read', { path: prompt.slice(5).trim() }))
    // `MCP <text>` calls the configured fixture server's `echo` tool.
    if (prompt.startsWith('MCP ')) return sse(call(n, 'mcp__fx__echo', { text: prompt.slice(4).trim() }))
    if (prompt.startsWith('SLEEP')) return sse(call(n, 'bash', { command: 'sleep 30' }))
    // `BACKGROUND` starts a shell that prints a line a second, then sleeps.
    if (prompt.startsWith('BACKGROUND')) {
      return sse(call(n, 'bash', { command: 'for i in 1 2 3 4 5 6 7 8 9; do echo bg-$i; sleep 1; done; sleep 60', run_in_background: true }))
    }
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
    const events = text(n, `Echo: ${prompt.replace(/^ECHO\s*/u, '')}.`)
    return prompt === this.holdAnswerFor ? this.held(request, events, events.length - 1) : sse(events)
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
  /** Starts bake-rs in the project; `env` adds to or replaces the session's environment, such as `PATH`. */
  launch(args?: readonly string[], inline?: boolean, env?: Readonly<Record<string, string>>): Preview
  /** Runs `cleanup` once every launched session is closed, whatever the outcome; its failure fails the scenario. */
  defer(cleanup: () => Promise<void>): void
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
async function liveScenario(name: string, body: (world: LiveWorld) => Promise<void>): Promise<void> {
  if (values.filter !== undefined && !name.includes(values.filter)) {
    console.log(`SKIP Rust PTY: ${name} (filter ${values.filter})`)
    return
  }
  await run(name, () => live(body))
}

async function live(body: (world: LiveWorld) => Promise<void>): Promise<void> {
  // A space and a non-ASCII letter in every path the session is given: its
  // home, project, and user directories, quoted on a Windows command line.
  const root = await mkdtemp(join(tmpdir(), 'bake rust-live ü-'))
  const home = join(root, 'home')
  const project = join(root, 'project')
  const user = join(root, 'user')
  for (const dir of [home, project, user]) await mkdir(dir, { recursive: true })
  const model = new FakeModel()
  await writeFile(join(home, 'models.json'), JSON.stringify({
    providers: { loop: { baseUrl: `http://127.0.0.1:${model.port}/v1`, api: 'openai-responses', apiKey: 'test-key', models: [{ id: 'loop-model', name: 'Loop', contextWindow: 1000, input: ['text', 'image'] }] } },
  }))
  const launched: Preview[] = []
  const cleanups: (() => Promise<void>)[] = []
  const world: LiveWorld = {
    root, home, project, model,
    defer: (cleanup) => { cleanups.push(cleanup) },
    launch: (args = [], inline = false, env = {}) => {
      const preview = new Preview(project, inline, { args: inline ? ['--inline', ...args] : args, env: { BAKE_HOME: home, HOME: user, USERPROFILE: user, ...env } })
      launched.push(preview)
      return preview
    },
  }
  // Every cleanup runs whatever the outcome; a cleanup failure fails the
  // scenario but never hides the scenario's own failure.
  let failure: { error: unknown } | undefined
  try {
    await body(world)
  } catch (error) {
    failure = { error }
    const last = launched.at(-1)
    if (last !== undefined && !(error instanceof GapError)) {
      try {
        await mkdir(join(join(import.meta.dir, '..'), '.preflight'), { recursive: true })
        const transcript = join(import.meta.dir, '..', '.preflight', `${basename(root).replaceAll(' ', '-')}.ansi`)
        await last.saveFailure(transcript)
        console.error(`Rust PTY transcript: ${transcript}`)
      } catch (saveError) { console.error('Rust PTY transcript was not saved:', saveError) }
    }
  }
  const cleanupFailures: unknown[] = []
  for (const preview of launched) await preview.close().catch((error: unknown) => { cleanupFailures.push(error) })
  for (const cleanup of cleanups.splice(0)) await cleanup().catch((error: unknown) => { cleanupFailures.push(error) })
  await model.stop()
  await rm(root, { recursive: true, force: true })
  for (const cleanupFailure of cleanupFailures) console.error('Rust PTY cleanup failed:', cleanupFailure)
  if (failure?.error instanceof GapError && cleanupFailures.length > 0) {
    throw new AggregateError([failure.error, ...cleanupFailures], 'A scenario reached a platform gap and its cleanup failed')
  }
  if (failure !== undefined) throw failure.error
  if (cleanupFailures.length > 0) throw cleanupFailures[0]
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
  world.model.holdAnswerFor = 'ECHO second prompt'
  session.send('ECHO second prompt\r')
  await session.wait('second answer text', () => session.screen.includes('Echo: second prompt.'))
  assert.equal(world.model.holding, 1, 'the final provider event must still be held')
  assert.equal(session.screen.split('Echo: second prompt.').length - 1, 1, 'the streamed answer was drawn twice')
  let completed = false
  const settled = turnSettled(session, 'Echo: second prompt.').then(() => { completed = true })
  // Observe a later PTY frame while completion is held, so text alone cannot satisfy the wait.
  void settled.catch(() => {})
  try {
    const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==', 'base64')
    const imagePath = join(world.root, 'dot.png')
    await writeFile(imagePath, png)
    session.send('ECHO picture ')
    session.send(`\x1b[200~${imagePath}\x1b[201~`)
    await session.wait('image staged', () => session.screen.includes('❯ ECHO picture [Image #1]') && session.screen.includes('Staged attachments: 1'))
    assert(!completed, 'answer text must not be mistaken for a completed turn')
    world.model.release()
    await settled
    const sentBefore = world.model.requests.length
    session.send('\r')
    await session.wait('image prompt answered', () => session.screen.includes('Echo: picture [Image #1].') && !session.screen.includes('Staged attachments'))
    assert(JSON.stringify(world.model.requests[sentBefore]?.input).includes('input_image'), 'the staged image was not sent')
  } finally {
    world.model.release()
    await settled.catch(() => {})
  }
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


await liveScenario('a live bash call cancelled by Esc fails its card and leaves no process', async (world) => {
  // No Windows sandbox is the default yet (D24), so workspace-write refuses
  // bash there; the call runs unconfined, still in the session's job.
  const session = world.launch(windows ? ['--permission', 'danger-full-access'] : [])
  await session.ready()
  // Processes the session already had, such as the ConPTY host's own
  // helper in its job, are not the call's.
  const before = await session.owned()
  session.send('SLEEP\r')
  // The call's own process tree (its sandbox runner, shell, and `sleep 30`):
  // on POSIX found under this session's process, so other processes on the
  // host with the same command line are not this test's; on Windows the
  // members of the job the session runs in, each pinned by the host.
  let owned = new Map<string, string>()
  const started = async () => new Map([...await session.owned()].filter(([pid]) => !before.has(pid)))
  const sleeping = (label: string) => label === 'sleep 30' || /(?:^|[\\/])sleep\.exe$/iu.test(label)
  await session.wait('bash running', () => session.screen.includes('running bash') && session.screen.includes('sleep 30'))
  const deadline = performance.now() + 30_000
  for (owned = await started(); ![...owned.values()].some(sleeping); owned = await started()) {
    if (performance.now() > deadline) throw new Error(`no sleep among the session's processes: ${[...owned.values()].join(', ')}`)
    await delay(5)
  }
  try {
    session.send('\x1b')
    await session.wait('bash interrupted', () => /^ {2}■ Interrupted/mu.test(session.screen))
    const left = await session.survivors(owned)
    assert.deepEqual(left, [], `the cancelled bash call left its process: ${left.map(pid => `${pid} ${owned.get(pid)}`).join(', ')}`)
    await session.quit()
  } finally {
    await session.release(owned)
  }
})

await liveScenario('background shells show under the box, open to their output, stop for the user, and end with the session', async (world) => {
  const session = world.launch(windows ? ['--permission', 'danger-full-access'] : [])
  await session.ready()
  const before = await session.owned()
  const started = async () => new Map([...await session.owned()].filter(([pid]) => !before.has(pid)))
  session.send('BACKGROUND\r')
  await session.wait('the shells row', () => session.screen.includes('Started.') && /● bash-1 .*↓ manage/u.test(session.screen))
  // The call that started the shell is tagged with its id.
  await session.wait('the start card tagged', () => /bash-1 +background/u.test(session.screen))
  session.send('\x1b[B')
  await session.wait('the shells list', () => session.screen.includes('Background shells') && /› ● bash-1/u.test(session.screen))
  session.send('\r')
  await session.wait('the shell output', () => session.screen.includes('Background shells / bash-1') && session.screen.includes('bg-2'))
  const owned = await started()
  try {
    const requests = world.model.requests.length
    session.send('x')
    // The stop reaches the model as the job's notice, in a turn it opens.
    await session.wait('the model told', () => world.model.requests.slice(requests).some(body => JSON.stringify(body).includes('The user stopped it.')))
    await session.wait('the shell stopped', () => /✗|■ stopped|stopped ·/u.test(session.screen))
    session.send('\x1b')
    await session.wait('back to the list', () => !session.screen.includes('Background shells /'))
    session.send('\x1b')
    await session.wait('the row gone', () => !session.screen.includes('↓ manage') && !session.screen.includes('Background shells'))
    // The notice reads as the shell's card, with how it ended.
    await session.wait('the notice card', () => /bash-1 +stopped/u.test(session.screen))
    const left = await session.survivors(owned)
    assert.deepEqual(left, [], `the stopped shell left its process: ${left.map(pid => `${pid} ${owned.get(pid)}`).join(', ')}`)
    // A shell still running when Bake quits does not outlive it.
    session.send('BACKGROUND\r')
    await session.wait('a second shell', () => /● bash-2 .*↓ manage/u.test(session.screen))
    const second = await started()
    await session.quit()
    const after = await session.survivors(second)
    assert.deepEqual(after, [], `quitting left a shell: ${after.map(pid => `${pid} ${second.get(pid)}`).join(', ')}`)
  } finally {
    await session.release(owned)
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
  session.send('/copy other\r')
  await session.wait('command refused', () => session.screen.includes('/copy is not available in bake-rs yet')
    && session.screen.includes('❯ /copy other'))
  await session.quit()

  const failed = world.launch(['--model', 'missing-model'])
  assert.equal(await failed.exit(), 1)
  assert(failed.raw.includes('missing-model'), `the error was not reported\n${failed.raw}`)
  assert(!failed.raw.includes('\x1b[?2004h') && !failed.raw.includes('\x1b[?1049h'), 'setup failure changed the terminal')
  assert(await failed.unchangedModes(), 'setup failure left terminal modes changed')

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
    assert(session.screen.includes('Alpha work'), session.screen)
    // A long temporary path, as on macOS, wraps the File and Directory rows
    // past the sheet's page, so the counts are reached by scrolling.
    for (let step = 0; !/Messages/u.test(session.screen); step++) {
      assert(step < 20, `no Messages heading in the session sheet\n${session.screen}`)
      const before = session.screen
      session.send('\x1b[B')
      await session.wait('session sheet scrolled', () => session.screen !== before)
    }
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

// ---- Session search in /resume ----

/** Every session file under the home, by path, as its text; the sessions directory must exist. */
async function sessionBytes(home: string): Promise<Map<string, string>> {
  const found = new Map<string, string>()
  const walk = async (dir: string): Promise<void> => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name)
      if (entry.isDirectory()) await walk(path)
      else if (entry.name.endsWith('.jsonl')) found.set(path, readFileSync(path, 'utf8'))
    }
  }
  await walk(join(home, 'sessions'))
  return found
}

/** Writes a Pi session file beside `beside`: a first ask, then a later answer holding `later`. */
async function seedSession(beside: string, id: string, cwd: string, later: string): Promise<string> {
  const path = join(dirname(beside), `2026-01-01T00-00-00-000Z_${id}.jsonl`)
  const cost = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 }
  const usage = { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost }
  const lines = [
    { type: 'session', version: 3, id, timestamp: '2026-01-01T00:00:00.000Z', cwd },
    { type: 'message', id: 'a1', parentId: null, timestamp: '2026-01-01T00:00:01.000Z', message: { role: 'user', content: 'first ask', timestamp: 1_767_225_601_000 } },
    { type: 'message', id: 'a2', parentId: 'a1', timestamp: '2026-01-01T00:00:02.000Z', message: { role: 'assistant', content: [{ type: 'text', text: 'short reply' }], api: 'openai-responses', provider: 'loop', model: 'loop-model', stopReason: 'stop', timestamp: 1_767_225_602_000, usage } },
    { type: 'message', id: 'a3', parentId: 'a2', timestamp: '2026-01-01T00:00:03.000Z', message: { role: 'user', content: 'and then?', timestamp: 1_767_225_603_000 } },
    { type: 'message', id: 'a4', parentId: 'a3', timestamp: '2026-01-01T00:00:04.000Z', message: { role: 'assistant', content: [{ type: 'text', text: later }], api: 'openai-responses', provider: 'loop', model: 'loop-model', stopReason: 'stop', timestamp: 1_767_225_604_000, usage } },
  ]
  await writeFile(path, lines.map(line => `${JSON.stringify(line)}\n`).join(''))
  return path
}

for (const inline of [false, true]) {
  await liveScenario(`${inline ? 'inline ' : ''}resume search finds later answers, refuses a bad pattern, filters named sessions, and opens the chosen id`, async (world) => {
    const session = world.launch([], inline)
    await session.ready()
    session.send('ECHO alpha\r')
    await turnSettled(session, 'Echo: alpha.')
    session.send('/name Alpha work\r')
    await session.wait('named', () => session.screen.includes('Session name set: Alpha work'))
    await composerReady(session)
    const [own] = [...(await sessionBytes(world.home)).keys()]
    assert(own, 'no session file was written')
    const header = JSON.parse(readFileSync(own, 'utf8').split('\n')[0] ?? '{}') as { cwd?: string }
    assert(typeof header.cwd === 'string', 'the session header has no cwd')
    const target = await seedSession(own, '01dddddd-0000-7000-8000-000000000001', header.cwd, 'Zebra\n\n   crossing notes')
    await seedSession(own, '01ffffff-0000-7000-8000-000000000002', join(world.root, 'elsewhere'), 'zebra crossing notes elsewhere')
    const before = await sessionBytes(world.home)
    const requests = world.model.requests.length

    session.send('/resume\r')
    await sheetReady(session, 'Resume session')
    assert(session.screen.includes('^S sort: Recent · ^N named: off') && session.screen.includes('> Search · re:<regex> · "phrase"'), session.screen)
    assert(session.screen.includes('Alpha work') && session.screen.includes('Current') && session.screen.includes('first ask'), session.screen)
    assert(!session.screen.includes('elsewhere'), `another directory's session is listed\n${session.screen}`)
    // A phrase only a later answer holds, its whitespace collapsed, finds that session.
    const phrase = '"zebra crossing"'
    await type(session, phrase, typed => session.screen.includes(`> ${phrase.slice(0, typed)}`))
    await session.wait('phrase match', () => session.screen.includes('▸ first ask') && session.screen.includes('1/1') && !session.screen.includes('Current'))
    // A bad pattern lists nothing, says why, and Enter chooses nothing.
    session.send('\x15')
    await session.wait('search cleared', () => session.screen.includes('> Search · re:<regex>'))
    await type(session, 're:(', typed => session.screen.includes(`> ${'re:('.slice(0, typed)}`))
    await session.wait('invalid pattern', () => session.screen.includes('Invalid regex:') && session.screen.includes('No matching sessions'))
    session.send('\r')
    session.send('\x15')
    await session.wait('pattern cleared', () => session.screen.includes('^S sort: Recent') && session.screen.includes('first ask') && session.screen.includes('Alpha work'))
    assert(session.screen.includes('Resume session') && !session.screen.includes('Resumed session'), `Enter on no rows answered\n${session.screen}`)
    // Ctrl+N keeps named sessions only; Ctrl+S switches the order.
    session.send('\x0e')
    await session.wait('named only', () => session.screen.includes('^N named: on') && session.screen.includes('Current') && session.screen.includes('1/1') && !session.screen.includes('first ask'))
    session.send('\x13')
    await session.wait('relevance', () => session.screen.includes('^S sort: Relevance'))
    session.send('\x0e')
    await session.wait('all names', () => session.screen.includes('^N named: off') && session.screen.includes('first ask'))
    if (!inline) {
      await session.resize(42, 20)
      // Narrow, the keys take their short form; the search state stays whole.
      await session.wait('narrow sheet', () => session.screen.includes('Resume session') && session.screen.includes('↑↓ · Enter · Esc')
        && session.screen.includes('^S sort: Relevance · ^N named: off') && session.screen.includes('> Search · re:<regex> · "phrase"')
        && !session.screen.includes('Preparing'))
      await session.resize(80, 24)
      await sheetReady(session, 'Resume session')
    }
    session.send('\x1b')
    await session.wait('resume cancelled', () => session.screen.includes('Cancelled') && !session.screen.includes('Resume session'))
    await composerReady(session)
    assert.deepEqual(await sessionBytes(world.home), before, 'searching or cancelling wrote a session file')
    assert.equal(world.model.requests.length, requests, 'the sheet sent a model request')

    // The chosen search result opens by its id.
    session.send('\x15')
    session.send('/resume\r')
    await sheetReady(session, 'Resume session')
    await type(session, 're:crossing\\s+NOTES', typed => session.screen.includes(`> ${'re:crossing\\s+NOTES'.slice(0, typed)}`))
    await session.wait('regex match', () => session.screen.includes('▸ first ask') && session.screen.includes('1/1'))
    session.send('\r')
    await session.wait('resumed', () => session.screen.includes('crossing notes') && session.screen.includes('first ask')
      && !session.screen.includes('Echo: alpha.') && !session.screen.includes('Resume session'))
    await composerReady(session)
    assert.equal(world.model.requests.length, requests, 'resuming sent a model request')
    await session.quit()
    if (inline) session.assertShellHistory()
    const after = await sessionBytes(world.home)
    for (const [path, bytes] of before) {
      if (path !== target) assert.equal(after.get(path), bytes, `${basename(path)} changed`)
    }
    assert((after.get(target) ?? '').startsWith(before.get(target) ?? '\0'), 'the resumed session lost entries')
    assert.deepEqual(await readdir(world.project), [], 'the session wrote into the project')
  })
}

// ---- Session tree navigation and forks: /tree, /fork, /clone ----

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
  await liveScenario(`${inline ? 'inline ' : ''}tree navigation, fork, and clone branch the right logs, restore drafts, and send no hidden request`, async (world) => {
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
    // Pi's branch-summary prompt; Enter takes its first choice, no summary.
    await sheetReady(session, 'Summarize branch?')
    assert(session.screen.includes('No summary') && session.screen.includes('Summarize with custom prompt'), session.screen)
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
    await composerReady(session)
    // /clone copies the whole current path into a new session and leaves the draft empty.
    const cloneRequests = world.model.requests.length
    session.send('/clone\r')
    // Inline, the replacement's opening note is written into the terminal history.
    await session.wait('cloned', () => (inline ? session.history : session.screen).includes('Cloned to new session'))
    await composerReady(session)
    assert(session.screen.includes('Echo: delta.') && session.screen.includes('❯ Type a draft'), session.screen)
    assert.equal(world.model.requests.length, cloneRequests, 'the clone sent a model request')
    before = world.model.requests.length
    session.send('ECHO epsilon\r')
    await turnSettled(session, 'Echo: epsilon.')
    sent = JSON.stringify(world.model.requests[before]?.input)
    assert(sent.includes('ECHO alpha') && sent.includes('ECHO delta') && sent.includes('ECHO epsilon') && !sent.includes('ECHO gamma'), `the cloned request: ${sent}`)
    await session.quit()
    if (inline) session.assertShellHistory()
    const files = await sessionEntries(world.home)
    assert.equal(files.length, 3, 'expected the source, its fork, and the clone')
    const source = files.find(entries => messageTexts(entries, 'user').includes('ECHO beta'))
    const fork = files.find(entries => messageTexts(entries, 'user').includes('ECHO delta') && !messageTexts(entries, 'user').includes('ECHO epsilon'))
    const clone = files.find(entries => messageTexts(entries, 'user').includes('ECHO epsilon'))
    assert(source && fork && clone, 'a session file is missing')
    assert.deepEqual(messageTexts(clone, 'user'), ['ECHO alpha', 'ECHO delta', 'ECHO epsilon'])
    const forkHeader = fork[0] as { type?: string; id?: string }
    const cloneHeader = clone[0] as { type?: string; parentSession?: string }
    assert(cloneHeader.type === 'session' && typeof forkHeader.id === 'string' && typeof cloneHeader.parentSession === 'string'
      && cloneHeader.parentSession.includes(forkHeader.id), 'the clone does not name the fork as its source')
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

for (const inline of [false, true]) {
  await liveScenario(`${inline ? 'inline ' : ''}tree branch summaries run as cancellable work, reopen the tree on Esc, and reach the next request`, async (world) => {
    const port = world.model.port
    await writeFile(join(world.home, 'models.json'), JSON.stringify({
      providers: { loop: { baseUrl: `http://127.0.0.1:${port}/v1`, api: 'openai-responses', apiKey: 'test-key',
        models: [{ id: 'loop-model', name: 'Loop', contextWindow: 1000 }] } },
    }))
    await writeFile(join(world.home, 'settings.json'), JSON.stringify({ compaction: { enabled: false }, retry: { enabled: false } }))
    const session = world.launch([], inline)
    await session.ready()
    session.send('ECHO alpha\r')
    await turnSettled(session, 'Echo: alpha.')
    session.send('ECHO beta\r')
    await turnSettled(session, 'Echo: beta.')
    await composerReady(session)
    const requests = world.model.requests.length
    const summaries = async (): Promise<Record<string, unknown>[]> => (await sessionEntries(world.home)).flat().filter(entry => entry.type === 'branch_summary')

    // A custom prompt holds the summary request; Esc aborts it, writes nothing, and reopens the tree at the choice.
    session.send('/tree\r')
    await sheetReady(session, 'Session tree')
    await type(session, 'user: echo beta', typed => session.screen.includes(`> ${'user: echo beta'.slice(0, typed)}`))
    await session.wait('one match', () => session.screen.includes('1/1'))
    session.send('\r')
    await sheetReady(session, 'Summarize branch?')
    // Only a fresh Enter answers a sheet, never one read in a burst with the arrows.
    session.send('\x1b[B\x1b[B')
    await session.wait('custom prompt selected', () => session.screen.includes('▸ Summarize with custom prompt'))
    session.send('\r')
    await sheetReady(session, 'Custom summarization instructions')
    await type(session, 'HOLD-SUMMARY', typed => session.screen.includes('HOLD-SUMMARY'.slice(0, typed)))
    session.send('\r')
    // The closed sheet hands the keyboard back before Esc reaches the summary.
    await session.wait('summarizing', () => world.model.holding === 1 && session.screen.includes('summarizing branch')
      && !session.screen.includes('Custom summarization instructions') && !session.screen.includes('Preparing'))
    session.send('\x1b')
    await sheetReady(session, 'Session tree')
    assert.equal(world.model.aborted, 1, 'the summary request was not aborted')
    assert.equal(world.model.requests.length, requests + 1, 'a cancelled summary sent more than its request')
    assert(String(JSON.stringify(world.model.requests[requests]?.input)).includes('Additional focus: HOLD-SUMMARY'), 'the custom prompt is missing')
    assert.deepEqual(await summaries(), [], 'a cancelled summary was written')

    // The tree reopened at the chosen message: Enter takes it again, and Summarize writes the summary.
    session.send('\r')
    await sheetReady(session, 'Summarize branch?')
    session.send('\x1b[B')
    await session.wait('summarize selected', () => /▸ Summarize(?! with)/u.test(session.screen))
    session.send('\r')
    await session.wait('navigated with a summary', () => session.screen.includes('[branch] Branch summary')
      && session.screen.includes('❯ ECHO beta') && !session.screen.includes('Summarize branch?'))
    await composerReady(session)
    assert.equal(world.model.requests.length, requests + 2, 'expected one more summary request')
    session.send('\x15')
    await session.wait('draft cleared', () => !session.screen.includes('❯ ECHO beta'))
    const before = world.model.requests.length
    session.send('ECHO gamma\r')
    await turnSettled(session, 'Echo: gamma.')
    const sent = JSON.stringify(world.model.requests[before]?.input)
    assert(sent.includes('summary of a branch') && sent.includes('the greek letters so far') && sent.includes('ECHO alpha') && !sent.includes('ECHO beta'),
      `the request after the summary: ${sent}`)
    await session.quit()
    if (inline) session.assertShellHistory()
    const written = await summaries()
    assert.equal(written.length, 1, 'expected one branch summary')
    assert(String(written[0]?.summary).includes('Summary: the greek letters so far.'), 'the summary text is missing')
    assert.deepEqual(await readdir(world.project), [], 'the session wrote into the project')
  })
}

// ---- Manual compaction: /compact [instructions] ----

for (const inline of [false, true]) {
  await liveScenario(`${inline ? 'inline ' : ''}compact cancels on Esc, keeps drafts on failure, rebuilds the transcript, continues from the summary, and quits mid-summary`, async (world) => {
    // Automatic compaction is off and one token is kept, so only /compact compacts and a short session has history to summarize.
    await writeFile(join(world.home, 'settings.json'), JSON.stringify({ compaction: { enabled: false, keepRecentTokens: 1 }, retry: { enabled: false } }))
    const session = world.launch([], inline)
    await session.ready()
    for (const word of ['alpha', 'beta', 'gamma']) {
      session.send(`ECHO ${word}\r`)
      await turnSettled(session, `Echo: ${word}.`)
    }
    await composerReady(session)
    const compactions = async (): Promise<number> => (await sessionEntries(world.home)).flat().filter(entry => entry.type === 'compaction').length

    // Esc cancels a held summary; a prompt typed meanwhile is refused and both drafts come back.
    let requests = world.model.requests.length
    session.send('/compact HOLD-SUMMARY\r')
    await session.wait('compaction running', () => world.model.holding === 1 && session.screen.includes('compacting') && session.screen.includes('Esc'))
    await type(session, 'kept draft', typed => session.screen.includes(`❯ ${'kept draft'.slice(0, typed)}`))
    session.send('\r')
    await session.wait('prompt refused while compacting', () => session.screen.includes('being compacted') && session.screen.includes('❯ kept draft'))
    session.send('\x1b')
    await session.wait('compaction cancelled', () => /■ Interrupted/u.test(session.screen) && session.screen.includes('/compact HOLD-SUMMARY')
      && session.screen.includes('kept draft') && session.screen.includes('Cancelled; your draft is back.'))
    assert.equal(world.model.aborted, 1, 'the summary request was not aborted')
    assert.equal(world.model.requests.length, requests + 1, 'a cancelled compaction sent more than its summary request')
    assert.equal(await compactions(), 0, 'a cancelled compaction was written')
    // Clear the two-paragraph draft one key at a time: each line, then the blank lines between them.
    // /compact opens no dialog, so no keyboard handoff drops the next keys: they reach the draft at once.
    await composerReady(session)
    session.send('\x15')
    await session.wait('second paragraph cleared', () => !session.screen.includes('kept draft'))
    session.send('\x7f\x7f')
    await session.wait('paragraphs joined', () => session.screen.includes('❯ /compact HOLD-SUMMARY'))
    session.send('\x15')
    await session.wait('draft cleared', () => !session.screen.includes('❯ /compact'))

    // A failed summary keeps the draft and the session.
    session.send('/compact FAIL-SUMMARY\r')
    await session.wait('compaction failed', () => session.screen.includes('Compaction failed') && session.screen.includes('❯ /compact FAIL-SUMMARY'))
    assert.equal(await compactions(), 0, 'a failed compaction was written')
    session.send('\x15')
    await session.wait('draft cleared', () => !session.screen.includes('❯ /compact'))

    // Success: the instructions reach the summary request and the transcript starts over with the kept history.
    requests = world.model.requests.length
    session.send('/compact keep the greek letters\r')
    await session.wait('compacted', () => /Compacted from [\d,]+ tokens/u.test(session.screen) && !session.screen.includes('compacting'))
    await composerReady(session)
    const summaries = world.model.requests.slice(requests).map(request => JSON.stringify(request.input))
    assert.equal(summaries.length, 2, 'expected the history and turn-prefix summary requests')
    assert(summaries[0]?.includes('Additional focus: keep the greek letters'), `the instructions are missing: ${summaries[0]}`)
    const lines = session.screen.split('\n')
    assert(lines.findIndex(line => line.includes('Echo: gamma.')) < lines.findIndex(line => /Compacted from/u.test(line)), 'the note is not below the kept history')
    if (!inline) assert(!session.screen.includes('Echo: alpha.'), `the summarized history is still shown\n${session.screen}`)
    assert.equal(await compactions(), 1)

    // The next prompt is sent with the summary instead of the summarized history.
    requests = world.model.requests.length
    session.send('ECHO delta\r')
    await turnSettled(session, 'Echo: delta.')
    const sent = JSON.stringify(world.model.requests[requests]?.input)
    assert(sent.includes('Summary: the greek letters so far.') && !sent.includes('ECHO alpha') && sent.includes('ECHO delta'), `the continued request: ${sent}`)
    await composerReady(session)

    // Quitting while a summary is held aborts it, writes nothing, and restores the terminal.
    const aborted = world.model.aborted
    session.send('/compact HOLD-SUMMARY\r')
    await session.wait('second compaction running', () => world.model.holding >= 1 && session.screen.includes('compacting'))
    await session.quit()
    if (inline) session.assertShellHistory()
    assert.equal(world.model.aborted, aborted + 1, 'quitting did not abort the summary request')
    const [entries, ...others] = await sessionEntries(world.home)
    assert(entries && others.length === 0, 'expected one session file')
    assert.equal(entries.filter(entry => entry.type === 'compaction').length, 1, 'quitting mid-summary wrote a compaction')
    assert.deepEqual(messageTexts(entries, 'user'), ['ECHO alpha', 'ECHO beta', 'ECHO gamma', 'ECHO delta'], 'slash text reached the log')
    assert(!world.model.requests.some(request => JSON.stringify(request.input).includes('/compact')), 'slash text reached the model')
    assert.deepEqual(await readdir(world.project), [], 'the session wrote into the project')
  })
}

// ---- MCP servers from the Bake home's mcp.json across session replacements ----

/**
 * One process as it was when observed: its pid and start time, so a pid
 * reused by a later process is not mistaken for it. Read from `/proc` on
 * Linux and from `ps` elsewhere (macOS); a zombie has exited, so it counts
 * as gone.
 */
const IDENTITY_SOURCE = String.raw`
function identity(pid) {
  try {
    if (process.platform === 'linux') {
      const stat = readFileSync('/proc/' + pid + '/stat', 'utf8')
      const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ')
      return { state: fields[0], start: fields[19] }
    }
    const out = execFileSync('/bin/ps', ['-o', 'stat=', '-o', 'lstart=', '-p', String(pid)], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim()
    if (out === '') return undefined
    const [state, ...start] = out.split(/\s+/u)
    return { state, start: start.join(' ') }
  } catch { return undefined }
}
function alive(pid, start) {
  const now = identity(pid)
  return now !== undefined && !now.state.startsWith('Z') && now.start === start
}
`

/**
 * A stdio MCP server for the PTY scenarios, run by this script's Bun. On
 * start it reads every server and descendant earlier generations recorded
 * in `$FIXTURE_DIR/registry` and logs those still alive, so a replacement
 * started while its predecessor's processes run is caught by the process
 * that would overlap them, at the moment it starts. It then starts a
 * descendant that never exits by itself (only the session's process-tree
 * shutdown ends it), records both, and serves `echo` and `hidden`.
 */
const MCP_SERVER_SOURCE = String.raw`
import { appendFileSync, existsSync, readFileSync } from 'node:fs'
import { execFileSync, spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
${IDENTITY_SOURCE}
const dir = process.env.FIXTURE_DIR
const log = line => appendFileSync(dir + '/events', line + '\n')
const registry = dir + '/registry'
const earlier = existsSync(registry) ? readFileSync(registry, 'utf8').trim().split('\n').filter(Boolean) : []
const overlapping = earlier.filter(line => { const [pid, ...start] = line.split(' '); return alive(Number(pid), start.join(' ')) })
const child = spawn('/bin/sleep', ['3600'], { stdio: 'ignore' })
const me = identity(process.pid), held = identity(child.pid)
appendFileSync(registry, process.pid + ' ' + me.start + '\n' + child.pid + ' ' + held.start + '\n')
log(JSON.stringify({ event: 'start', pid: process.pid, descendant: child.pid, overlapping }))
const send = message => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', ...message }) + '\n')
const tool = (name, description) => ({ name, description, inputSchema: { type: 'object', properties: { text: { type: 'string' } } } })
for await (const line of createInterface({ input: process.stdin })) {
  if (line.trim() === '') continue
  const { id, method, params } = JSON.parse(line)
  if (id === undefined) continue
  if (method === 'initialize') send({ id, result: { protocolVersion: params?.protocolVersion ?? '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'pty-fixture', version: '1' } } })
  else if (method === 'tools/list') {
    log(JSON.stringify({ event: 'list', pid: process.pid }))
    send({ id, result: { tools: [tool('echo', 'Echo the text.'), tool('hidden', 'Never declared.')] } })
  }
  else if (method === 'tools/call') {
    log(JSON.stringify({ event: 'call', pid: process.pid, name: params?.name, text: params?.arguments?.text }))
    send({ id, result: { content: [{ type: 'text', text: 'echo ' + String(params?.arguments?.text) + ' from ' + process.pid }] } })
  } else if (method === 'ping') send({ id, result: {} })
  else send({ id, error: { code: -32601, message: 'unknown method ' + method } })
}
`

/** The driver's copy of the fixture's `alive` and `identity`, from the same source, so both judge a process alike. */
const { alive, identity } = new Function('readFileSync', 'execFileSync', `${IDENTITY_SOURCE}; return { alive, identity }`)(
  readFileSync, execFileSync,
) as { alive: (pid: number, start: string) => boolean; identity: (pid: number) => { state: string; start: string } | undefined }

/** The hidden argument that makes `bake-rs` a plugin helper. */
const PLUGIN_HELPER_ARG = '__bake-wasm-plugin-host'

/**
 * Installs and enables, through the binary, a plugin whose manifest lists
 * `mcp__fx__echo`, run by the hostile test component, which answers `none`
 * for it: a session's first echo call starts that session's helper, which
 * then runs until the session ends.
 */
async function enableEchoPlugin(world: LiveWorld): Promise<void> {
  const dir = join(world.root, 'plugin')
  await mkdir(dir)
  await writeFile(join(dir, 'plugin.json'), JSON.stringify({ api: '0.1.0', id: 'echo-watch', version: '0.1.0', vetoTools: ['mcp__fx__echo'] }))
  await copyFile(join(root, 'rust/plugins/fixtures/hostile/hostile.wasm'), join(dir, 'plugin.wasm'))
  const user = join(world.root, 'user')
  const manage = (args: string[]): string => {
    const ran = Bun.spawnSync([binary, 'plugin', ...args], { cwd: world.project, env: { PATH: process.env.PATH, BAKE_HOME: world.home, HOME: user, USERPROFILE: user } })
    assert.equal(ran.exitCode, 0, `bake-rs plugin ${args.join(' ')}: ${ran.stderr.toString()}`)
    return ran.stdout.toString()
  }
  const installed = manage(['install', dir])
  const hash = /^installed echo-watch 0\.1\.0 as sha256:([0-9a-f]{64})$/mu.exec(installed)?.[1]
  assert(hash !== undefined, installed)
  manage(['enable', hash])
}

interface McpStart { pid: number; descendant: number; overlapping: string[] }

/** The fixture server's home entry, its events, and the processes every generation recorded. */
class McpFixture {
  readonly dir: string
  constructor(readonly world: LiveWorld) { this.dir = join(world.root, 'mcp-fixture') }

  /** Writes the server and the owner-only `mcp.json` that starts it, and owns every process it records. */
  async install(): Promise<void> {
    await mkdir(this.dir, { recursive: true })
    const server = join(this.dir, 'server.mjs')
    await writeFile(server, MCP_SERVER_SOURCE)
    const config = join(this.world.home, 'mcp.json')
    const entry = { command: process.execPath, args: [server], env: { FIXTURE_DIR: this.dir } }
    await writeFile(config, JSON.stringify({ mcpServers: { fx: entry } }))
    await chmod(config, 0o600)
    this.world.defer(() => this.reap())
  }

  events(): Record<string, unknown>[] {
    const path = join(this.dir, 'events')
    return existsSync(path) ? readFileSync(path, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line) as Record<string, unknown>) : []
  }

  starts(): McpStart[] { return this.events().filter(event => event.event === 'start') as unknown as McpStart[] }

  /** Every process every generation recorded, descendants and servers, with its start time. */
  recorded(): { pid: number; start: string }[] {
    const path = join(this.dir, 'registry')
    if (!existsSync(path)) return []
    return readFileSync(path, 'utf8').trim().split('\n').filter(Boolean).map((line) => {
      const [pid, ...start] = line.split(' ')
      return { pid: Number(pid), start: start.join(' ') }
    })
  }

  living(): number[] { return this.recorded().filter(entry => alive(entry.pid, entry.start)).map(entry => entry.pid) }

  /**
   * Records the session's one plugin helper in the registry, so the next
   * server generation reports it as overlapping if it still runs when that
   * server starts, and the cleanup reaps it.
   */
  async recordHelper(session: Preview): Promise<number> {
    const helpers = [...(await session.owned())].filter(([, command]) => command.includes(PLUGIN_HELPER_ARG)).map(([pid]) => Number(pid))
    assert.equal(helpers.length, 1, `expected one plugin helper, saw ${JSON.stringify(helpers)}`)
    const [pid] = helpers
    assert(pid !== undefined)
    const seen = identity(pid)
    assert(seen !== undefined && !seen.state.startsWith('Z'), `plugin helper ${pid} is not running`)
    appendFileSync(join(this.dir, 'registry'), `${pid} ${seen.start}\n`)
    return pid
  }

  /** Waits for generation `count` to start and list its tools, then checks no earlier process was alive when it started. */
  async generation(session: Preview, count: number): Promise<McpStart> {
    await session.wait(`MCP server ${count} listed`, () => {
      const start = this.starts()[count - 1]
      return start !== undefined && this.events().some(event => event.event === 'list' && event.pid === start.pid)
    }, false)
    const starts = this.starts()
    assert.equal(starts.length, count, `expected ${count} server starts, saw ${JSON.stringify(starts)}`)
    const start = starts[count - 1]
    assert(start)
    assert.deepEqual(start.overlapping, [], `server ${count} started while an earlier server, its descendant, or a plugin helper was alive`)
    return start
  }

  /** Kills whatever a failed scenario left running, descendants first, and waits a bounded time for each to go. */
  async reap(): Promise<void> {
    const left = this.recorded().filter(entry => alive(entry.pid, entry.start)).reverse()
    for (const entry of left) {
      try { process.kill(entry.pid, 'SIGKILL') } catch { /* already gone */ }
    }
    const deadline = performance.now() + 5_000
    while (left.some(entry => alive(entry.pid, entry.start))) {
      if (performance.now() > deadline) throw new Error(`MCP fixture processes survived SIGKILL: ${left.map(entry => entry.pid).join(' ')}`)
      await delay(5)
    }
    assert.deepEqual(left, [], 'MCP fixture processes outlived the scenario')
  }
}

/** The tool names a model request declared. */
function declared(request: unknown): string[] {
  const tools = (request as { tools?: { name?: string }[] }).tools ?? []
  return tools.map(tool => tool.name ?? '').sort()
}

if (windows) {
  console.log('SKIP Rust PTY: home MCP servers and plugin helpers stop before each /new, /resume, and /fork replacement starts its own (Windows owner/DACL trust for mcp.json is not implemented)')
  console.log('SKIP Rust PTY: with --no-tools the home MCP server never starts and requests declare no tool (Windows owner/DACL trust for mcp.json is not implemented)')
} else {
  await liveScenario('home MCP servers and plugin helpers stop before each /new, /resume, and /fork replacement starts its own', async (world) => {
    const fixture = new McpFixture(world)
    await fixture.install()
    await enableEchoPlugin(world)
    const session = world.launch(['--sandbox', 'danger-full-access', '--exclude-tools', 'mcp__fx__hidden'])
    await session.ready()
    const first = await fixture.generation(session, 1)
    const turn = async (word: string, server: McpStart): Promise<void> => {
      const before = world.model.requests.length
      session.send(`MCP ${word}\r`)
      await turnSettled(session, `MCP said: echo ${word} from ${server.pid}`)
      await composerReady(session)
      // Every request the turn sent declares the server's allowed tool and never the excluded one.
      const requests = world.model.requests.slice(before)
      assert.equal(requests.length, 2, 'expected the call and its answer')
      for (const request of requests) {
        const names = declared(request)
        assert(names.includes('mcp__fx__echo') && !names.includes('mcp__fx__hidden'), `declared tools: ${names.join(', ')}`)
      }
      // The call asked this session's plugin helper, the only one running:
      // the next generation's server checks it is gone when it starts.
      await fixture.recordHelper(session)
    }
    await turn('one', first)
    session.send('/name First\r')
    await session.wait('named', () => session.screen.includes('Session name set: First'))
    await composerReady(session)

    session.send('/new\r')
    await session.wait('new session', () => session.screen.includes('New session started'))
    const second = await fixture.generation(session, 2)
    assert.deepEqual(fixture.living().sort(), [second.pid, second.descendant].sort(), 'only the new server and its descendant run')
    await turn('two', second)

    session.send('/resume\r')
    await sheetReady(session, 'Resume session')
    // A phrase only the first session holds: a bare word is matched loosely and can list the current session too.
    const phrase = '"MCP one"'
    await type(session, phrase, typed => session.screen.includes(`> ${phrase.slice(0, typed)}`))
    await session.wait('one match', () => session.screen.includes('▸ First') && session.screen.includes('1/1'))
    session.send('\r')
    await session.wait('resumed', () => session.screen.includes('Resumed session First'))
    const third = await fixture.generation(session, 3)
    assert.deepEqual(fixture.living().sort(), [third.pid, third.descendant].sort(), 'only the resumed session\'s server runs')
    await turn('three', third)

    session.send('/fork\r')
    await sheetReady(session, 'Fork from message')
    session.send('\r')
    await session.wait('forked', () => session.screen.includes('Forked to new session') && !session.screen.includes('Fork from message'))
    const fourth = await fixture.generation(session, 4)
    assert.deepEqual(fixture.living().sort(), [fourth.pid, fourth.descendant].sort(), 'only the fork\'s server runs')
    session.send('\x15')
    await session.wait('fork draft cleared', () => session.screen.includes('❯ Type a draft'))

    // Cancelled sheets replace nothing: the fork's server keeps serving and no other starts.
    for (const [command, title] of [['/resume', 'Resume session'], ['/fork', 'Fork from message']] as const) {
      session.send(`${command}\r`)
      await sheetReady(session, title)
      session.send('\x1b')
      await session.wait(`${command} cancelled`, () => session.screen.includes('Cancelled') && !session.screen.includes(title))
      await composerReady(session)
      session.send('\x15')
      await session.wait('draft cleared', () => session.screen.includes('❯ Type a draft'))
    }
    await turn('four', fourth)
    assert.equal(fixture.starts().length, 4, 'a cancelled sheet started a server')

    const recorded = fixture.recorded()
    assert.equal(recorded.length, 12, 'expected four servers, four descendants, and four plugin helpers')
    await session.quit()
    // Checked at once after the exit: shutdown awaited every server and its tree, and every helper.
    assert.deepEqual(fixture.living(), [], 'an MCP server, its descendant, or a plugin helper outlived the double Ctrl+C quit')
    const calls = fixture.events().filter(event => event.event === 'call')
    assert.deepEqual(calls.map(call => [call.name, call.pid]), [
      ['echo', first.pid], ['echo', second.pid], ['echo', third.pid], ['echo', fourth.pid],
    ], 'each call reached the server of the session that sent it')
    assert.deepEqual(await readdir(world.project), [], 'the session wrote into the project')
  })

  await liveScenario('with --no-tools the home MCP server never starts and requests declare no tool', async (world) => {
    const fixture = new McpFixture(world)
    await fixture.install()
    const session = world.launch(['--sandbox', 'danger-full-access', '-nt'])
    await session.ready()
    session.send('ECHO bare\r')
    // The first prompt waits for servers still connecting, so a server the session had started is recorded by now.
    await turnSettled(session, 'Echo: bare.')
    assert.deepEqual(declared(world.model.requests.at(-1)), [], 'a --no-tools request declared tools')
    session.send('/new\r')
    await session.wait('new session', () => session.screen.includes('New session started'))
    await composerReady(session)
    session.send('ECHO again\r')
    await turnSettled(session, 'Echo: again.')
    assert.deepEqual(declared(world.model.requests.at(-1)), [], 'the replacement declared tools')
    await session.quit()
    assert.deepEqual(fixture.events(), [], 'a --no-tools session started the MCP server')
    assert.deepEqual(fixture.recorded(), [])
  })
}

// ---- /changelog, /share, and /bug ----

/** A fake GitHub CLI on `PATH`: it records each call and copies what it is asked to upload, so nothing reaches the network. */
interface FakeGh {
  /** `PATH` with the fake first. */
  readonly path: string
  /** Each call's arguments, in order. */
  calls(): string[]
  /** A file the fake wrote, such as `uploaded.html` or `body.md`. */
  recorded(name: string): string | undefined
}

async function fakeGh(world: LiveWorld): Promise<FakeGh> {
  const dir = join(world.root, 'gh')
  await mkdir(dir, { recursive: true })
  const script = join(dir, 'gh')
  await writeFile(script, [
    '#!/bin/sh',
    'dir=$(dirname "$0")',
    'printf \'%s\\n\' "$*" >> "$dir/calls"',
    'case "$1 $2" in',
    '  "auth status") exit 0 ;;',
    '  "gist create") cp "$4" "$dir/uploaded.html"; echo "https://gist.github.com/someone/abc123"; exit 0 ;;',
    '  "issue create")',
    '    shift 2',
    '    while [ $# -gt 0 ]; do [ "$1" = "--body-file" ] && cp "$2" "$dir/body.md"; shift; done',
    '    echo "https://github.com/Justar96/bake/issues/42"; exit 0 ;;',
    'esac',
    'exit 2',
    '',
  ].join('\n'))
  await chmod(script, 0o755)
  const recorded = (name: string): string | undefined => existsSync(join(dir, name)) ? readFileSync(join(dir, name), 'utf8') : undefined
  return { path: `${dir}:${process.env.PATH ?? ''}`, calls: () => (recorded('calls') ?? '').split('\n').filter(Boolean), recorded }
}

if (windows) {
  console.log('SKIP Rust PTY: changelog, share, and bug (the fake GitHub CLI is a POSIX shell script)')
} else {
  for (const inline of [false, true]) {
    await liveScenario(`${inline ? 'inline ' : ''}changelog shows the entries, and share and bug upload through gh only after their sheets say yes`, async (world) => {
      const gh = await fakeGh(world)
      const session = world.launch([], inline, { PATH: gh.path })
      await session.ready()
      session.send('ECHO alpha\r')
      await turnSettled(session, 'Echo: alpha.')
      await composerReady(session)
      const requests = world.model.requests.length

      session.send('/changelog\r')
      // The newest entry, 0.3.8, comes last, so its last words end the transcript.
      await session.wait('changelog shown', () => session.screen.includes('Option-drag') && session.screen.includes('❯ Type a draft'))

      // No is selected first: Enter declines and nothing is uploaded.
      session.send('/share\r')
      await sheetReady(session, 'Share this session as a secret GitHub gist?')
      assert(session.screen.includes("Anyone with the gist's link") && session.screen.includes('▸ No'), session.screen)
      session.send('\r')
      await session.wait('share declined', () => session.screen.includes('Share cancelled; your draft is back.') && session.screen.includes('❯ /share'))
      assert.deepEqual(gh.calls(), ['auth status'])
      assert.equal(gh.recorded('uploaded.html'), undefined, 'a declined share uploaded')
      await composerReady(session)
      session.send('\x15')
      await session.wait('draft cleared', () => !session.screen.includes('❯ /share'))

      session.send('/share\r')
      await sheetReady(session, 'Share this session as a secret GitHub gist?')
      session.send('\x1b[A')
      await session.wait('yes selected', () => session.screen.includes('▸ Yes, create the gist'))
      session.send('\r')
      await session.wait('shared', () => session.screen.includes('Gist: https://gist.github.com/someone/abc123'))
      await composerReady(session)
      assert(gh.calls()[2]?.startsWith('gist create --public=false '), `${gh.calls()}`)
      assert(gh.recorded('uploaded.html')?.includes('id="session-data"'), 'the shared page is not the HTML export')

      // /bug: the description starts from the argument; nothing runs before the last choice.
      session.send('/bug the answer was cut\r')
      await sheetReady(session, 'Report a bug')
      assert(session.screen.includes('public GitHub issue') && session.screen.includes('> the answer was cut'), session.screen)
      session.send('\r')
      await sheetReady(session, 'Include the session transcript?')
      session.send('\x1b[B')
      await session.wait('no transcript selected', () => /▸ No/u.test(session.screen))
      session.send('\r')
      await sheetReady(session, 'Bug report')
      assert(session.screen.includes('Description: the answer was cut'), session.screen)
      assert.equal(gh.calls().length, 3, 'gh ran before the last choice')
      session.send('\r')
      await session.wait('issue filed', () => session.screen.includes('Bug report filed: https://github.com/Justar96/bake/issues/42'))
      await composerReady(session)
      const body = gh.recorded('body.md') ?? ''
      assert(body.startsWith('the answer was cut') && body.includes('report.json'), body)
      assert(!body.includes('test-key') && !body.includes('ECHO alpha'), 'the public issue holds a key or the transcript')
      assert.equal(world.model.requests.length, requests, 'a command sent a model request')
      // Inline, the whole changelog goes into the scrollback and pushes the
      // shell's rows past the test terminal's scrollback, so they are not checked here.
      await session.quit()
      const entries = (await sessionEntries(world.home)).flat()
      const records = entries.filter(entry => entry.type === 'custom' && entry.customType === 'bake.bug-report')
      assert.equal(records.length, 1, 'the filed report was not recorded once')
      assert.deepEqual(await readdir(world.project), [], 'a command wrote into the project')
    })
  }
}

console.log(`Rust PTY: ${results.passed.length} passed, ${results.gapped.length} ended in a named gap, ${results.failed.length} failed`)
for (const gap of [...gaps].sort()) console.log(`GAP  Rust PTY: ${gap}`)
if (results.failed.length > 0) {
  console.error(`Rust PTY failed: ${results.failed.join('; ')}`)
  process.exit(1)
}
