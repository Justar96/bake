/**
 * The measured process in a 120x40 POSIX PTY (`Bun.Terminal`), observed
 * through its output bytes.
 *
 * As in `scripts/rust-preview-pty.ts`, a shell waits for a handshake line
 * before it `exec`s the program, so the driver reads the cooked termios flags
 * first and the program keeps the shell's pid. Output is not emulated: each
 * chunk is stripped of control sequences in one byte loop, which also tracks
 * the DEC private modes the program sets and resets, and the visible bytes
 * are searched with `Buffer.indexOf` for typed echoes and history markers.
 * Every observation is stamped with its chunk's arrival time. The PTY reader
 * runs on the harness's main thread, so the program can write only as fast
 * as the observer reads; the README gives the measured ceiling.
 */

export const COLUMNS = 120
export const ROWS = 40
/** Visible bytes kept for {@link Terminal.visibleSince} and failure messages. */
const VISIBLE_TAIL = 262_144
/**
 * Visible bytes carried into the next chunk's search: longer than a marker
 * and every watched text, so a match split across chunks is found once.
 */
const SEARCH_CARRY = 64
/** A history marker is `H`, five digits, and this suffix. */
const MARKER_SUFFIX = Buffer.from('_END')
const MARKER_LENGTH = 10

/**
 * The DEC private modes the arms toggle, with the state a terminal starts
 * in: auto-wrap (7) and cursor visibility (25) set; mouse reporting, focus
 * events, the alternate screen, and bracketed paste reset.
 */
export const TRACKED_MODES: ReadonlyMap<number, boolean> = new Map([
  [7, true], [25, true], [1000, false], [1002, false], [1003, false], [1004, false], [1005, false], [1006, false], [1015, false],
  [1047, false], [1049, false], [2004, false],
])
const BRACKETED_PASTE = 2004

// Stripper states, as small integers for the byte loop.
const TEXT = 0
const ESCAPE = 1
const ESCAPE_INTERMEDIATE = 2
const CSI = 3
const STRING = 4
const STRING_ESCAPE = 5

/** Parameter bytes kept for one CSI sequence; a longer one is not a mode change the arms send. */
const CSI_PARAMS = 64

/**
 * An incremental ANSI stripper over bytes. Control sequences split across
 * chunks are removed whole; carriage returns and line feeds become line
 * feeds, tabs spaces, and other C0 controls nothing. `CSI ? Pm h` and
 * `CSI ? Pm l`, with any number of `;`-separated modes, update
 * {@link modes}; `ESC c`, a full reset, returns every tracked mode to its
 * initial state.
 */
export class AnsiStripper {
  private state = TEXT
  private readonly params = new Uint8Array(CSI_PARAMS)
  private paramLength = 0
  /** The sequence has intermediate bytes or too many parameters, so it changes no tracked mode. */
  private ignored = false
  /** The last state each tracked mode was given; a mode never named is absent. */
  readonly modes = new Map<number, boolean>()

  /** @param onMode - called for each tracked mode a sequence sets or resets. */
  constructor(private readonly onMode: (mode: number, set: boolean) => void = () => {}) {}

  /**
   * Strip `chunk`, writing its visible bytes into `out` from `offset`.
   * @param out - holds at least `offset + chunk.length` bytes.
   * @returns the end of the visible bytes in `out`.
   */
  strip(chunk: Uint8Array, out: Uint8Array, offset: number): number {
    let end = offset
    let state = this.state
    for (let index = 0; index < chunk.length; index++) {
      const byte = chunk[index] ?? 0
      switch (state) {
        case TEXT:
          if (byte === 0x1b) state = ESCAPE
          else if (byte === 0x0a || byte === 0x0d) out[end++] = 0x0a
          else if (byte === 0x09) out[end++] = 0x20
          else if (byte >= 0x20 && byte !== 0x7f) out[end++] = byte
          break
        case ESCAPE:
          if (byte === 0x5b) {
            state = CSI
            this.paramLength = 0
            this.ignored = false
          } else if (byte === 0x5d || byte === 0x50 || byte === 0x58 || byte === 0x5e || byte === 0x5f) {
            // OSC, DCS, SOS, PM, and APC strings end at BEL or ST.
            state = STRING
          } else if (byte >= 0x20 && byte <= 0x2f) {
            state = ESCAPE_INTERMEDIATE
          } else {
            if (byte === 0x63) this.reset()
            state = TEXT
          }
          break
        case ESCAPE_INTERMEDIATE:
          if (!(byte >= 0x20 && byte <= 0x2f)) state = TEXT
          break
        case CSI:
          if (byte >= 0x40 && byte <= 0x7e) {
            if (byte === 0x68 || byte === 0x6c) this.modeSequence(byte === 0x68)
            state = TEXT
          } else if (byte >= 0x30 && byte <= 0x3f) {
            if (this.paramLength < CSI_PARAMS) this.params[this.paramLength++] = byte
            else this.ignored = true
          } else if (byte >= 0x20 && byte <= 0x2f) {
            this.ignored = true
          }
          break
        case STRING:
          if (byte === 0x07) state = TEXT
          else if (byte === 0x1b) state = STRING_ESCAPE
          break
        case STRING_ESCAPE:
          state = byte === 0x5c ? TEXT : STRING
          break
      }
    }
    this.state = state
    return end
  }

  /** Apply a finished `CSI ? Pm h` or `CSI ? Pm l`. */
  private modeSequence(set: boolean): void {
    if (this.ignored || this.paramLength < 2 || this.params[0] !== 0x3f) return
    const modes: number[] = []
    let mode = 0
    let digits = 0
    for (let index = 1; index <= this.paramLength; index++) {
      const byte = index === this.paramLength ? 0x3b : this.params[index] ?? 0
      if (byte >= 0x30 && byte <= 0x39) {
        mode = mode * 10 + byte - 0x30
        digits++
      } else if (byte === 0x3b) {
        if (digits > 0) modes.push(mode)
        mode = 0
        digits = 0
      } else {
        // `:` subparameters or a second private marker: not a plain mode list.
        return
      }
    }
    for (const named of modes) {
      if (!TRACKED_MODES.has(named)) continue
      this.modes.set(named, set)
      this.onMode(named, set)
    }
  }

  private reset(): void {
    for (const mode of this.modes.keys()) {
      const initial = TRACKED_MODES.get(mode) ?? false
      this.modes.set(mode, initial)
      this.onMode(mode, initial)
    }
  }

  /** Tracked modes whose last state differs from a terminal's initial state, as `?<mode>h` or `?<mode>l`. */
  changedModes(): string[] {
    return [...this.modes].filter(([mode, set]) => TRACKED_MODES.get(mode) !== set).sort(([a], [b]) => a - b).map(([mode, set]) => `?${mode}${set ? 'h' : 'l'}`)
  }
}

/** A text the driver waits for, timed when the output holding it arrives. */
export interface Watch {
  readonly text: string
  /** Monotonic arrival time, once seen. */
  at: number | undefined
  readonly bytes: Buffer
  /** The visible position from which a match counts: output after the watch was made. */
  readonly from: number
}

/** How the process ended. */
export interface Exit {
  readonly code: number
  readonly signal: string | null
  /** Monotonic time the exit was observed, before any observation poll. */
  readonly at: number
}

const isDigit = (byte: number | undefined): boolean => byte !== undefined && byte >= 0x30 && byte <= 0x39

/** One measured program in its own PTY. */
export class Terminal {
  private readonly child: Bun.Subprocess
  private readonly pty: Bun.Terminal
  private readonly stripper: AnsiStripper
  private readonly initialModes: readonly number[]
  /** The search carry at its start, then the current chunk's visible bytes. */
  private scratch = new Uint8Array(SEARCH_CARRY + 65_536)
  private carryLength = 0
  /** Visible bytes; twice the tail, so the tail moves to the front once per tail's worth of output. */
  private readonly visible = new Uint8Array(VISIBLE_TAIL * 2)
  private visibleLength = 0
  private visibleEnd = 0
  private chunkAt = 0
  private streamStatus: number | undefined
  private exitState: Exit | undefined
  readonly exited: Promise<Exit>
  /** Monotonic time of the handshake that let the program start. */
  readonly startedAt: number
  /** Output bytes so far. */
  bytes = 0
  /** Arrival of the chunk that first enabled bracketed paste. */
  bracketedPasteAt: number | undefined
  /**
   * When the termios flags were first seen changed, as entering raw mode
   * changes them. They are not in the output, so they are read at each
   * chunk's arrival and at each poll of {@link wait}, whichever is first.
   */
  termiosChangedAt: number | undefined
  /** Each distinct history marker, with the arrival of the chunk it was first seen in. */
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
    this.stripper = new AnsiStripper((mode, set) => {
      if (mode === BRACKETED_PASTE && set && this.bracketedPasteAt === undefined) this.bracketedPasteAt = this.chunkAt
    })
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
    this.initialModes = this.termios()
    this.exited = this.child.exited.then((code) => {
      this.exitState = { code, signal: this.child.signalCode, at: performance.now() }
      return this.exitState
    })
    this.startedAt = performance.now()
    this.pty.write('start\n')
  }

  get pid(): number { return this.child.pid }

  get exit(): Exit | undefined { return this.exitState }

  /**
   * When bracketed paste was enabled with the termios flags changed: the
   * later of {@link bracketedPasteAt} and {@link termiosChangedAt}.
   */
  get inputReadyAt(): number | undefined {
    const { bracketedPasteAt, termiosChangedAt } = this
    return bracketedPasteAt === undefined || termiosChangedAt === undefined ? undefined : Math.max(bracketedPasteAt, termiosChangedAt)
  }

  private observeTermios(now: number): void {
    if (this.termiosChangedAt === undefined && this.modesChanged) this.termiosChangedAt = now
  }

  private receive(bytes: Uint8Array): void {
    const now = performance.now()
    this.chunkAt = now
    this.bytes += bytes.length
    this.observeTermios(now)
    if (this.scratch.length < SEARCH_CARRY + bytes.length) {
      const grown = new Uint8Array(SEARCH_CARRY + bytes.length * 2)
      grown.set(this.scratch.subarray(0, this.carryLength))
      this.scratch = grown
    }
    const carry = this.carryLength
    const end = this.stripper.strip(bytes, this.scratch, carry)
    if (end === carry) return
    const region = Buffer.from(this.scratch.buffer, this.scratch.byteOffset, end)
    // The visible position of region[0].
    const regionStart = this.visibleEnd - carry
    // A marker that ends inside the carry was counted with the chunk before.
    const from = Math.max(0, carry - MARKER_SUFFIX.length + 1)
    for (let at = region.indexOf(MARKER_SUFFIX, from); at >= 0; at = region.indexOf(MARKER_SUFFIX, at + 1)) {
      const begin = at + MARKER_SUFFIX.length - MARKER_LENGTH
      if (begin < 0 || region[begin] !== 0x48) continue
      let digits = true
      for (let index = begin + 1; index < at; index++) digits &&= isDigit(region[index])
      if (!digits) continue
      this.markerOccurrences++
      const name = region.toString('latin1', begin, at + MARKER_SUFFIX.length)
      if (!this.markers.has(name)) this.markers.set(name, now)
    }
    for (const watch of this.watches) {
      if (region.indexOf(watch.bytes, Math.max(0, watch.from - regionStart)) >= 0) {
        watch.at = now
        this.watches.delete(watch)
      }
    }
    this.append(region.subarray(carry, end))
    const keep = Math.min(SEARCH_CARRY, end)
    this.scratch.copyWithin(0, end - keep, end)
    this.carryLength = keep
  }

  private append(data: Uint8Array): void {
    this.visibleEnd += data.length
    const kept = data.length > VISIBLE_TAIL ? data.subarray(data.length - VISIBLE_TAIL) : data
    if (this.visibleLength + kept.length > this.visible.length) {
      const keep = Math.min(this.visibleLength, VISIBLE_TAIL - kept.length)
      this.visible.copyWithin(0, this.visibleLength - keep, this.visibleLength)
      this.visibleLength = keep
    }
    this.visible.set(kept, this.visibleLength)
    this.visibleLength += kept.length
  }

  /**
   * Time the first later output that shows `text`; call before sending what should produce it.
   * @param text - visible text without line breaks, at most 64 bytes.
   */
  watch(text: string): Watch {
    const bytes = Buffer.from(text)
    if (text.length === 0 || /[\r\n]/u.test(text) || bytes.length > SEARCH_CARRY) throw new Error(`a watched text must be one non-empty line of at most ${SEARCH_CARRY} bytes`)
    const watch: Watch = { text, at: undefined, bytes, from: this.visibleEnd }
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

  /** The visible-byte position of the next output, for {@link visibleSince}. */
  get position(): number { return this.visibleEnd }

  /** Visible text from `position` on, as far as the bounded tail reaches. */
  visibleSince(position: number): string {
    const start = Math.max(0, position - (this.visibleEnd - this.visibleLength))
    return Buffer.from(this.visible.buffer, this.visible.byteOffset, this.visibleLength).toString('utf8', start)
  }

  /** The last visible text, for failure messages. */
  tail(length = 4000): string { return this.visibleSince(this.visibleEnd - length) }

  private termios(): readonly number[] {
    return [this.pty.inputFlags, this.pty.outputFlags, this.pty.localFlags, this.pty.controlFlags]
  }

  /** Whether the program changed the termios flags, as entering raw mode does. */
  get modesChanged(): boolean { return this.termios().some((value, index) => value !== this.initialModes[index]) }

  /**
   * What the program left changed: the termios flags, each tracked DEC
   * private mode whose last state differs from a terminal's initial state,
   * and an output stream that ended in error. Empty when it restored all.
   */
  unrestored(): string[] {
    const problems = this.stripper.changedModes().map(mode => `${mode} left in effect`)
    if (this.modesChanged) problems.unshift('termios flags changed')
    // Linux reports EIO, status 1, when the last slave closes, a clean exit included.
    if (this.streamStatus !== undefined && this.streamStatus !== 0 && this.streamStatus !== 1) problems.push(`output stream ended with status ${this.streamStatus}`)
    return problems
  }

  /** Whether {@link unrestored} is empty. */
  get restored(): boolean { return this.unrestored().length === 0 }

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
      this.observeTermios(performance.now())
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
