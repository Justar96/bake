/**
 * The measured process in a 120x40 POSIX PTY (`Bun.Terminal`), observed
 * through its output bytes.
 *
 * As in `scripts/rust-preview-pty.ts`, a shell waits for a handshake line
 * before it `exec`s the program, so the driver reads the cooked termios flags
 * first and the program keeps the shell's pid. Output is not emulated: an
 * incremental ANSI stripper turns it into visible text, which is searched for
 * typed echoes and history markers as it arrives, so a backlog of history
 * never delays an observation. Visible text is kept as a bounded tail.
 */

export const COLUMNS = 120
export const ROWS = 40
const VISIBLE_TAIL = 262_144
const MARKER = /H\d{5}_END/gu
/** Longer than a marker, so one split across output chunks is still found once. */
const MARKER_CARRY = 16

type StripState = 'text' | 'escape' | 'escape-intermediate' | 'csi' | 'string' | 'string-escape'

/**
 * An incremental ANSI stripper. Control sequences split across chunks are
 * removed whole; carriage returns and line feeds become line feeds, tabs
 * spaces, and other C0 controls nothing.
 */
export class AnsiStripper {
  private state: StripState = 'text'

  /** The visible text of the next chunk. */
  push(chunk: string): string {
    let out = ''
    for (const ch of chunk) {
      const code = ch.codePointAt(0) ?? 0
      switch (this.state) {
        case 'text':
          if (code === 0x1b) this.state = 'escape'
          else if (code === 0x0a || code === 0x0d) out += '\n'
          else if (code === 0x09) out += ' '
          else if (code >= 0x20 && code !== 0x7f) out += ch
          break
        case 'escape':
          if (ch === '[') this.state = 'csi'
          // OSC, DCS, SOS, PM, and APC strings end at BEL or ST.
          else if (ch === ']' || ch === 'P' || ch === 'X' || ch === '^' || ch === '_') this.state = 'string'
          else if (code >= 0x20 && code <= 0x2f) this.state = 'escape-intermediate'
          else this.state = 'text'
          break
        case 'escape-intermediate':
          if (!(code >= 0x20 && code <= 0x2f)) this.state = 'text'
          break
        case 'csi':
          if (code >= 0x40 && code <= 0x7e) this.state = 'text'
          break
        case 'string':
          if (code === 0x07) this.state = 'text'
          else if (code === 0x1b) this.state = 'string-escape'
          break
        case 'string-escape':
          this.state = ch === '\\' ? 'text' : 'string'
          break
      }
    }
    return out
  }
}

/** A text the driver waits for, timed when the output holding it arrives. */
export interface Watch {
  readonly text: string
  /** Monotonic arrival time, once seen. */
  at: number | undefined
  carry: string
}

/** How the process ended. */
export interface Exit {
  readonly code: number
  readonly signal: string | null
  /** Monotonic time of the exit, before any observation poll. */
  readonly at: number
}

/** One measured program in its own PTY. */
export class Terminal {
  private readonly child: Bun.Subprocess
  private readonly pty: Bun.Terminal
  private readonly decoder = new TextDecoder()
  private readonly stripper = new AnsiStripper()
  private readonly initialModes: readonly number[]
  private rawCarry = ''
  private markerCarry = ''
  private visible = ''
  /** Visible characters before `visible`'s first. */
  private visibleStart = 0
  private streamStatus: number | undefined
  private exitState: Exit | undefined
  readonly exited: Promise<Exit>
  /** Monotonic time of the handshake that let the program start. */
  readonly startedAt: number
  /** Output bytes so far. */
  bytes = 0
  /** When bracketed paste was first enabled, the input loop's first sign of life. */
  bracketedPasteAt: number | undefined
  /** Each distinct history marker, with the time it was first seen. */
  readonly markers = new Map<string, number>()
  /** Every marker sighting, repeated ones included. */
  markerOccurrences = 0
  private readonly watches = new Set<Watch>()

  /**
   * Start `argv` in a new PTY.
   * @param argv - the program and its arguments.
   * @param cwd - the working directory.
   * @param env - the whole environment.
   */
  constructor(argv: readonly string[], cwd: string, env: Record<string, string>) {
    this.child = Bun.spawn(['sh', '-c', 'read -r ready; exec "$@"', 'rust-perf', ...argv], {
      cwd, env,
      terminal: {
        cols: COLUMNS, rows: ROWS,
        data: (_terminal, bytes) => { this.receive(bytes) },
        exit: (_terminal, status) => { this.streamStatus = status },
      },
    })
    const pty = this.child.terminal
    if (pty === undefined) throw new Error('Bun did not open a PTY')
    this.pty = pty
    this.initialModes = this.modes()
    this.exited = this.child.exited.then((code) => {
      this.exitState = { code, signal: this.child.signalCode, at: performance.now() }
      return this.exitState
    })
    this.startedAt = performance.now()
    this.pty.write('start\n')
  }

  get pid(): number { return this.child.pid }

  get exit(): Exit | undefined { return this.exitState }

  private receive(bytes: Uint8Array): void {
    const now = performance.now()
    this.bytes += bytes.length
    const raw = this.decoder.decode(bytes, { stream: true })
    if (this.bracketedPasteAt === undefined) {
      const joined = this.rawCarry + raw
      if (joined.includes('\x1b[?2004h')) this.bracketedPasteAt = now
      this.rawCarry = joined.slice(-8)
    }
    const text = this.stripper.push(raw)
    if (text === '') return
    const joined = this.markerCarry + text
    for (const match of joined.matchAll(MARKER)) {
      if (match.index + match[0].length <= this.markerCarry.length) continue
      this.markerOccurrences++
      if (!this.markers.has(match[0])) this.markers.set(match[0], now)
    }
    this.markerCarry = joined.slice(-MARKER_CARRY)
    for (const watch of this.watches) {
      const seen = watch.carry + text
      if (seen.includes(watch.text)) {
        watch.at = now
        this.watches.delete(watch)
      } else {
        watch.carry = seen.slice(Math.max(0, seen.length - watch.text.length + 1))
      }
    }
    this.visible += text
    if (this.visible.length > VISIBLE_TAIL) {
      const drop = this.visible.length - VISIBLE_TAIL
      this.visible = this.visible.slice(drop)
      this.visibleStart += drop
    }
  }

  /**
   * Time the first later output that shows `text`; call before sending what should produce it.
   * @param text - visible text without line breaks.
   */
  watch(text: string): Watch {
    if (text.length === 0 || /[\r\n]/u.test(text)) throw new Error('a watched text must be one non-empty line')
    const watch: Watch = { text, at: undefined, carry: '' }
    this.watches.add(watch)
    return watch
  }

  /**
   * Wait for a watched text.
   * @returns its arrival time.
   */
  async seen(watch: Watch, description: string, timeoutMs: number, signal?: AbortSignal): Promise<number> {
    let at: number | undefined
    await this.wait(description, () => (at = watch.at) !== undefined, timeoutMs, signal)
    return at ?? Number.NaN
  }

  /** The visible-text position of the next output, for {@link visibleSince}. */
  get position(): number { return this.visibleStart + this.visible.length }

  /** Visible text from `position` on, as far as the bounded tail reaches. */
  visibleSince(position: number): string {
    return this.visible.slice(Math.max(0, position - this.visibleStart))
  }

  /** The last visible text, for failure messages. */
  tail(length = 4000): string { return this.visible.slice(-length) }

  private modes(): readonly number[] {
    return [this.pty.inputFlags, this.pty.outputFlags, this.pty.localFlags, this.pty.controlFlags]
  }

  /** Whether the program changed the termios flags, as entering raw mode does. */
  get modesChanged(): boolean { return this.modes().some((value, index) => value !== this.initialModes[index]) }

  /** Whether the flags equal those before the program started and the output ended cleanly. */
  get restored(): boolean {
    // Linux reports EIO, status 1, when the last slave closes, a clean exit included.
    const streamClean = this.streamStatus === undefined || this.streamStatus === 0 || this.streamStatus === 1
    return streamClean && !this.modesChanged
  }

  /** Write input bytes without waiting for a frame. */
  send(text: string): void { this.pty.write(text) }

  signal(signal: 'SIGTERM' | 'SIGINT' | 'SIGKILL'): void { this.child.kill(signal) }

  /**
   * Poll `condition` every 2 ms.
   * @param description - named in a failure.
   * @param condition - observed state to await.
   * @param timeoutMs - the deadline.
   * @param signal - cancels the wait.
   * @returns the monotonic time the condition was first seen to hold.
   * @throws when the program exits first, the deadline passes, or the wait is cancelled.
   */
  async wait(description: string, condition: () => boolean, timeoutMs: number, signal?: AbortSignal): Promise<number> {
    const start = performance.now()
    while (true) {
      signal?.throwIfAborted()
      if (condition()) return performance.now()
      if (this.exitState !== undefined) throw new Error(`${description}: the program exited ${JSON.stringify(this.exitState)}\n${this.tail()}`)
      if (performance.now() - start > timeoutMs) throw new Error(`${description}: timed out after ${timeoutMs} ms\n${this.tail()}`)
      await Bun.sleep(2)
    }
  }

  /**
   * Kill whatever still runs, await the exit, and close the PTY. Safe after a normal exit.
   */
  async close(): Promise<void> {
    try {
      if (this.exitState === undefined) this.child.kill('SIGKILL')
      await this.exited
    } finally {
      this.pty.close()
    }
  }
}
