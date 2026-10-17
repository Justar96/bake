/**
 * The Windows side of the Rust PTY driver: a client of the ConPTY host in
 * `rust/crates/bake-process/examples/conpty`, which runs the program on a
 * pseudoconsole inside a kill-on-close job and speaks JSON lines on its
 * standard streams. The pure helpers here (line splitting, the
 * environment, console-mode checks, signal mapping) are tested on every
 * platform by `rust-pty-conpty.test.ts`; the host itself runs only on
 * Windows.
 */
import { resolve } from 'node:path'

/** `GetConsoleMode` of the pseudoconsole's input buffer and screen buffer, read by its client. */
export interface ConsoleModes { readonly input: number; readonly output: number }

/** `ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT`: the cooked bits Crossterm's raw mode clears. */
export const COOKED_INPUT = 0x1 | 0x2 | 0x4
/** `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on a screen buffer. */
export const VT_PROCESSING = 0x4
/** `STATUS_CONTROL_C_EXIT`: how the default console control handler ends a process. */
export const STATUS_CONTROL_C_EXIT = 0xC000013A

/**
 * Whether the program put the console's input into raw mode: the modes read
 * before it started were cooked, and the ones read while it runs have every
 * cooked bit clear.
 * @returns the reason it did not, or undefined.
 */
export function rawModeProblem(initial: ConsoleModes, running: ConsoleModes): string | undefined {
  if ((initial.input & (0x2 | 0x4)) !== (0x2 | 0x4)) return `the console input was not cooked before the program started (${hex(initial.input)})`
  if ((running.input & COOKED_INPUT) !== 0) return `the program did not acquire raw mode (input ${hex(running.input)})`
  return undefined
}

/**
 * Compares the modes the client read after the program exited with those it
 * read before the program started. The input mode must be restored exactly.
 * An output mode that differs only in VT processing is not restored: it is
 * the named gap Crossterm leaves (its ANSI support switches the bit on and
 * never off), reported as a gap and never as restoration. Any other
 * difference is a failure.
 */
export function modeRestoration(initial: ConsoleModes, final: ConsoleModes): { readonly problem?: string; readonly gap?: string } {
  if (initial.input !== final.input) return { problem: `console input mode ${hex(final.input)} was not restored to ${hex(initial.input)}` }
  if (initial.output === final.output) return {}
  if ((initial.output ^ final.output) === VT_PROCESSING && (final.output & VT_PROCESSING) !== 0) {
    return { gap: 'Crossterm leaves ENABLE_VIRTUAL_TERMINAL_PROCESSING set on the console screen buffer after exit' }
  }
  return { problem: `console output mode ${hex(final.output)} was not restored to ${hex(initial.output)}` }
}

function hex(value: number): string { return `0x${value.toString(16)}` }

/** What this harness sends on a pseudoconsole for a POSIX signal a scenario names. */
export type ConsoleSignal = { readonly op: 'ctrl'; readonly event: 'c' | 'break' } | { readonly op: 'hangup' } | { readonly op: 'kill' }

/**
 * This test harness's own mapping of the scenarios' POSIX signals onto
 * Windows console events. Windows has no such signals, and other runtimes
 * map them differently (Node's `kill('SIGTERM')` on Windows, for one, is a
 * forced termination), so the mapping is the harness's choice:
 *
 * - SIGINT is `GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)`, sent by the
 *   pseudoconsole's client to every process attached to the console. It
 *   arrives as a control event whatever the input mode: raw mode turns
 *   only the Ctrl+C key into input.
 * - SIGTERM is `CTRL_BREAK_EVENT`, sent the same way.
 * - SIGHUP is `ClosePseudoConsole`: the terminal going away. conhost sends
 *   `CTRL_CLOSE_EVENT` to every attached client, and the system ends a
 *   client once its handler returns or times out.
 * - SIGKILL is `TerminateProcess` on the program, exit code 1.
 *
 * Exit expectations differ by platform. The scenarios expect POSIX's 130,
 * 143, and 129 from a handled SIGINT, SIGTERM, and SIGHUP. A Windows
 * process with no console control handler is ended by the default handler
 * with `STATUS_CONTROL_C_EXIT` (0xC000013A) and restores nothing; the
 * driver reports that outcome as a named gap, never as a pass, and any
 * other unexpected code as a failure.
 */
export function consoleSignal(signal: 'SIGINT' | 'SIGTERM' | 'SIGHUP' | 'SIGKILL'): ConsoleSignal {
  switch (signal) {
    case 'SIGINT': return { op: 'ctrl', event: 'c' }
    case 'SIGTERM': return { op: 'ctrl', event: 'break' }
    case 'SIGHUP': return { op: 'hangup' }
    case 'SIGKILL': return { op: 'kill' }
  }
}

/** Variables a Windows program needs to start, find Git Bash, and resolve its temp directory; read without case. */
const WINDOWS_PASSTHROUGH = [
  'SystemRoot', 'SystemDrive', 'windir', 'ComSpec', 'PATHEXT', 'TEMP', 'TMP', 'ProgramFiles', 'ProgramFiles(x86)',
  'ProgramW6432', 'ProgramData', 'CommonProgramFiles', 'APPDATA', 'LOCALAPPDATA', 'USERNAME', 'NUMBER_OF_PROCESSORS',
  'PROCESSOR_ARCHITECTURE', 'OS',
] as const

/**
 * The environment for a program on the pseudoconsole: `PATH` and the
 * Windows variables above from `source`, then `extra`. Windows names are
 * case-insensitive, so every name appears once, a later one replacing an
 * earlier one whatever its case, keeping the later spelling.
 */
export function windowsEnv(
  source: Readonly<Record<string, string | undefined>>, extra: Readonly<Record<string, string>>,
): Record<string, string> {
  const merged = new Map<string, [string, string]>()
  const set = (name: string, value: string) => merged.set(name.toUpperCase(), [name, value])
  const lookup = (name: string): string | undefined => {
    const wanted = name.toUpperCase()
    let found: string | undefined
    for (const [key, value] of Object.entries(source)) if (key.toUpperCase() === wanted && value !== undefined) found = value
    return found
  }
  for (const name of ['PATH', ...WINDOWS_PASSTHROUGH]) {
    const value = lookup(name)
    if (value !== undefined) set(name, value)
  }
  for (const [name, value] of Object.entries(extra)) set(name, value)
  return Object.fromEntries([...merged.values()])
}

/** Splits a byte stream into lines, accepting LF or CRLF, with a line split across chunks kept whole. */
export class LineSplitter {
  private readonly decoder = new TextDecoder()
  private rest = ''

  push(chunk: Uint8Array): string[] {
    const lines = (this.rest + this.decoder.decode(chunk, { stream: true })).split('\n')
    this.rest = lines.pop() ?? ''
    return lines.map(line => line.endsWith('\r') ? line.slice(0, -1) : line).filter(line => line.length > 0)
  }

  /** What remains once the stream has ended: a final line without its newline. */
  end(): string[] {
    const last = (this.rest + this.decoder.decode()).replace(/\r$/u, '')
    this.rest = ''
    return last.length > 0 ? [last] : []
  }
}

/** One event from the host. */
export type HostEvent = Readonly<Record<string, unknown>> & { readonly event: string }

/** Parses an event line; anything else is an error naming the line. */
export function parseEvent(line: string): HostEvent {
  let value: unknown
  try {
    value = JSON.parse(line)
  } catch {
    throw new Error(`the ConPTY host wrote a line that is not JSON: ${line.slice(0, 200)}`)
  }
  const object = typeof value === 'object' && value !== null && !Array.isArray(value)
  if (!object || typeof (value as { event?: unknown }).event !== 'string') {
    throw new Error(`the ConPTY host wrote a line that is not an event: ${line.slice(0, 200)}`)
  }
  return value as HostEvent
}

/** Reads a `modes` event's words; a missing or non-integer word is an error. */
export function eventModes(event: HostEvent): ConsoleModes {
  const { input, output } = event
  if (!Number.isInteger(input) || !Number.isInteger(output)) throw new Error(`malformed modes event: ${JSON.stringify(event)}`)
  return { input: input as number, output: output as number }
}

/** A job member the host pinned, with whether its process handle is unsignalled. */
export interface Member { readonly pid: number; readonly role: 'client' | 'program' | 'member'; readonly image: string; readonly alive: boolean }

/** How long a request to the host may take before it fails. */
const REQUEST_TIMEOUT = 15_000

interface Pending {
  readonly resolve: (event: HostEvent) => void
  readonly reject: (error: Error) => void
  readonly timer: ReturnType<typeof setTimeout>
}

/** Options for one program on a fresh pseudoconsole. */
export interface ConPtyOptions {
  /** The command that runs the host: its path, or an interpreter and a script for a stand-in. */
  readonly host: readonly string[]
  readonly program: string
  readonly args: readonly string[]
  readonly cwd: string
  readonly env: Readonly<Record<string, string>>
  readonly cols: number
  readonly rows: number
  /** Print forty shell rows before the program starts, as the POSIX shell does for inline runs. */
  readonly history: boolean
  readonly onData: (bytes: Uint8Array) => void
}

/** One host process: a pseudoconsole, its job, and the program on it. */
export class ConPty {
  private readonly process: Bun.Subprocess<'pipe', 'pipe', 'pipe'>
  private readonly pending = new Map<number, Pending>()
  private nextId = 1
  private readonly initialModes = Promise.withResolvers<ConsoleModes>()
  private readonly finalModes = Promise.withResolvers<ConsoleModes>()
  private readonly programExit = Promise.withResolvers<number>()
  private readonly hostClosed = Promise.withResolvers<void>()
  /** Error events and host failures, newest last, for failure messages. */
  readonly errors: string[] = []
  /** The program's pid, once the client reported it. */
  pid: number | undefined
  exitCode: number | undefined
  /** Whether the host reported its output pipe ended and its job settled, then closed. */
  closed = false
  settled: { readonly terminated: boolean } | undefined
  private stderr = ''
  /** The first failure, after which no command is sent. */
  private failure: Error | undefined
  private readonly hostExited: Promise<number>

  constructor(private readonly options: ConPtyOptions) {
    for (const deferred of [this.initialModes, this.finalModes, this.programExit, this.hostClosed]) deferred.promise.catch(() => {})
    this.process = Bun.spawn([
      ...options.host, 'host', '--cols', String(options.cols), '--rows', String(options.rows), '--cwd', options.cwd,
      ...options.history ? ['--history'] : [], '--', options.program, ...options.args,
    ], { cwd: options.cwd, env: options.env, stdin: 'pipe', stdout: 'pipe', stderr: 'pipe' })
    void this.readEvents()
    void this.readStderr()
    this.hostExited = this.process.exited.then((code) => {
      if (!this.closed) this.fail(`the ConPTY host exited ${code} before it closed${this.stderr ? `: ${this.stderr.slice(-2000)}` : ''}`)
      return code
    })
  }

  /** The console modes the client read before the program started. */
  get initial(): Promise<ConsoleModes> { return this.initialModes.promise }
  /** The console modes the client read once the program had exited. */
  get final(): Promise<ConsoleModes> { return this.finalModes.promise }
  /** The program's exit code, read from the host's own handle to it. */
  get exited(): Promise<number> { return this.programExit.promise }
  /** Settles once the host has closed: the program and client exited, the output drained, and the job settled. */
  get ended(): Promise<void> { return this.hostClosed.promise }

  private fail(message: string): void {
    this.errors.push(message)
    const error = new Error(message)
    this.failure ??= error
    for (const deferred of [this.initialModes, this.finalModes, this.programExit, this.hostClosed]) deferred.reject(error)
    for (const [id, request] of this.pending) {
      clearTimeout(request.timer)
      request.reject(error)
      this.pending.delete(id)
    }
  }

  private async readStderr(): Promise<void> {
    const decoder = new TextDecoder()
    for await (const chunk of this.process.stderr) this.stderr = (this.stderr + decoder.decode(chunk, { stream: true })).slice(-8192)
  }

  private async readEvents(): Promise<void> {
    const lines = new LineSplitter()
    try {
      for await (const chunk of this.process.stdout) for (const line of lines.push(chunk)) this.dispatch(parseEvent(line))
      for (const line of lines.end()) this.dispatch(parseEvent(line))
    } catch (error) {
      this.fail(error instanceof Error ? error.message : String(error))
    }
  }

  private dispatch(event: HostEvent): void {
    const id = typeof event.id === 'number' ? event.id : undefined
    switch (event.event) {
      case 'output': this.options.onData(Buffer.from(String(event.data), 'base64')); return
      case 'modes':
        if (event.phase === 'initial') this.initialModes.resolve(eventModes(event))
        else if (event.phase === 'final') this.finalModes.resolve(eventModes(event))
        break
      case 'spawned': this.pid = Number(event.pid); return
      case 'exit':
        this.exitCode = Number(event.code)
        this.programExit.resolve(this.exitCode)
        return
      case 'settled': this.settled = { terminated: event.terminated === true }; return
      case 'error': this.errors.push(String(event.message)); return
      case 'closed':
        this.closed = true
        if (this.exitCode === undefined) this.fail(`the ConPTY host closed without the program's exit${this.errorSuffix()}`)
        this.finalModes.reject(new Error(`the client reported no final console modes${this.errorSuffix()}`))
        this.hostClosed.resolve()
        // The host waits for the command stream to end before it exits.
        void this.process.stdin.end()
        return
      default: break
    }
    if (id !== undefined) {
      const request = this.pending.get(id)
      if (request !== undefined) {
        clearTimeout(request.timer)
        this.pending.delete(id)
        request.resolve(event)
      }
    }
  }

  private errorSuffix(): string {
    return this.errors.length > 0 ? `; host errors: ${this.errors.slice(-5).join(' | ')}` : ''
  }

  private send(command: Readonly<Record<string, unknown>>): void {
    if (this.closed) throw new Error(`the ConPTY host has closed; cannot ${String(command.op)}`)
    if (this.failure !== undefined) throw this.failure
    void this.process.stdin.write(`${JSON.stringify(command)}\n`)
    void this.process.stdin.flush()
  }

  private request(command: Readonly<Record<string, unknown>>): Promise<HostEvent> {
    const id = this.nextId++
    const { promise, resolve, reject } = Promise.withResolvers<HostEvent>()
    const timer = setTimeout(() => {
      this.pending.delete(id)
      reject(new Error(`the ConPTY host did not answer ${String(command.op)} within ${REQUEST_TIMEOUT} ms${this.errorSuffix()}`))
    }, REQUEST_TIMEOUT)
    this.pending.set(id, { resolve, reject, timer })
    try {
      this.send({ ...command, id })
    } catch (error) {
      clearTimeout(timer)
      this.pending.delete(id)
      reject(error instanceof Error ? error : new Error(String(error)))
    }
    return promise
  }

  write(text: string): void { this.send({ op: 'write', data: Buffer.from(text, 'utf8').toString('base64') }) }

  resize(cols: number, rows: number): void { this.send({ op: 'resize', cols, rows }) }

  /** The console modes now, read by the client. */
  async modes(): Promise<ConsoleModes> { return eventModes(await this.request({ op: 'modes' })) }

  signal(signal: 'SIGINT' | 'SIGTERM' | 'SIGHUP' | 'SIGKILL'): void { this.send(consoleSignal(signal)) }

  /** Suspends every thread of the program; resolves once the host has. */
  async suspend(): Promise<void> { await this.request({ op: 'suspend' }) }

  async resume(): Promise<void> { await this.request({ op: 'resume' }) }

  /** Pins the job's current members and lists every pin. */
  async members(): Promise<Member[]> {
    const reply = await this.request({ op: 'members' })
    if (!Array.isArray(reply.members)) throw new Error(`malformed members reply: ${JSON.stringify(reply)}`)
    return reply.members as Member[]
  }

  unpin(): void { if (!this.closed) this.send({ op: 'unpin' }) }

  /** Ends the host if it has not ended, and waits for it: the job is terminated and settled. */
  async close(): Promise<void> {
    if (!this.closed && this.process.exitCode === null) {
      try {
        // Sent even after a failure: the host still owns the job.
        void this.process.stdin.write(`${JSON.stringify({ op: 'close' })}\n`)
        void this.process.stdin.flush()
      } catch {
        // The host's input closed meanwhile; it is ending.
      }
    }
    const code = await Promise.race([this.hostExited, Bun.sleep(30_000).then(() => 'timed out' as const)])
    if (code === 'timed out') {
      // The host did not end; ending it closes the job's handle, whose
      // kill-on-close ends every member, which the next run cannot see.
      this.process.kill()
      await this.process.exited
      throw new Error(`the ConPTY host did not end within 30 s of close${this.errorSuffix()}`)
    }
    for (const request of this.pending.values()) clearTimeout(request.timer)
    this.pending.clear()
  }

  /** The host's own exit status, once it has exited. */
  get hostExitCode(): number | null { return this.process.exitCode }
}

/** Builds the ConPTY host with Cargo, from the pinned toolchain's directory, and returns its path. */
export function buildConPtyHost(root: string): string {
  const rust = resolve(root, 'rust')
  const built = Bun.spawnSync(['cargo', 'build', '--locked', '-p', 'bake-process', '--example', 'conpty'], { cwd: rust, stdout: 'inherit', stderr: 'inherit' })
  if (built.exitCode !== 0) throw new Error(`cargo could not build the ConPTY host (exit ${built.exitCode})`)
  return resolve(rust, 'target/debug/examples/conpty.exe')
}
