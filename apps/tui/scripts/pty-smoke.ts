#!/usr/bin/env bun
/**
 * Drive the built `tui` profile through a real POSIX terminal.
 *
 * Coverage is split into named scenarios. `--list` prints them, `--only NAME`
 * runs one with its prerequisites, and every wait carries a description, so a
 * timeout names the condition that never arrived instead of dumping a screen
 * and leaving the reader to guess which of sixty predicates failed.
 *
 * ```sh
 * tui/scripts/pty-smoke.ts                 # every scenario, recorded replay
 * tui/scripts/pty-smoke.ts --list
 * tui/scripts/pty-smoke.ts --only cancel   # that scenario and its prerequisites
 * tui/scripts/pty-smoke.ts --trace         # print each step as it is satisfied
 * tui/scripts/pty-smoke.ts --live node24   # real API on the engine floor
 * ```
 *
 * The driver is Bun; the process it drives is Node, because `app-boot` reaches
 * V8 current-context symbols that JavaScriptCore does not have
 * ([PLAN.md](../PLAN.md#22-bun-cannot-run-the-harness)). Bun owns the terminal
 * through `Bun.Terminal`, which is why no Python or native module is needed.
 *
 * A failing step writes the whole transcript under `apps/tui/.smoke/` and prints its
 * tail, so the screen that produced the failure survives the run.
 *
 * @module tui-pty-smoke
 */

import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, readlinkSync, realpathSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { parseArgs } from 'node:util'
import xterm from '@xterm/headless'

import { COLUMN, MARKER } from '../packages/ui/src/layout.ts'
import { toolLabel } from '../packages/ui/src/present.ts'
import { dictionaries } from '../packages/ui/src/copy.ts'
import { FOLD_REST } from '../packages/ui/src/activity.ts'
import { MARKDOWN, PALETTE } from '../packages/ui/src/palette.ts'

const ROOT = resolve(import.meta.dir, '../../..')
const FIXTURE = join(ROOT, 'snapshots/session/bash-tool-turn/session.v3.jsonl')
const ARTIFACTS = join(ROOT, 'apps/tui/.smoke')
/**
 * What the surface draws, taken from the surface instead of copied.
 *
 * `MARKER` and `toolLabel` are the rendering vocabulary itself, so a change there
 * reaches these scenarios without editing them. The rest names a screen element
 * whose owning module exports no constant, so the next vocabulary change is one
 * edit here instead of sixty string literals.
 */
/** The models the replay profile declares; one of them names every status line a scenario draws. */
const MODELS = ['deepseek-v4-flash', 'tui-picked-model', 'gpt-test', 'claude-test'] as const

const SCREEN = {
  /** `line.tsx` draws the caret instead of using inverse video, which `NO_COLOR` would erase. */
  caret: '\u258c',
  /**
   * The status line, which opens at the draft's column with the selected
   * model's name and no label. It is drawn from the first frame, whatever
   * the session is doing. The names are the replay profile's models, or
   * the words a session with no model selected shows in their place.
   */
  status: new RegExp(` {${COLUMN.rail}}(?:${[...MODELS, dictionaries.en.noModel].join('|')})(?![\\w.-])`, 'u'),
  /**
   * An idle session with an empty draft. The composer's placeholder. A running
   * turn replaces it with the steering hint and blocked input with the reason,
   * so its last appearance after a turn's output means the turn has ended.
   */
  idle: dictionaries.en.prompt,
  /** The composer's prompt rail. */
  prompt: `${MARKER.prompt} `,
  /**
   * A started tool call. The tool's name with its arguments in parentheses.
   * The name alone would match the recorded prompt, which asks the model to
   * run a command.
   */
  toolCall: new RegExp(`\\b${toolLabel('bash')}\\(`),
  /** The recorded tool's output, which the transcript indents under its verb. */
  toolResult: 'TERMINAL_OK',
} as const

/**
 * Match a selected row in a list, whose marker and rail width belong to `line.tsx`.
 *
 * @param name - the row's visible name.
 * @returns a pattern matching that row while it is selected.
 */
function picked(name: string): RegExp {
  return new RegExp(`\\${MARKER.selected}\\s+${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`)
}

/** How often a wait re-reads a predicate between chunks of output, for predicates that also read files. */
const RECHECK_MS = 20

/**
 * Read a session log.
 *
 * @param path - the JSONL file to read, Zstandard-compressed when it ends in `.zstd`.
 * @returns every committed event, in log order.
 */
async function events(path: string): Promise<any[]> {
  const file = Bun.file(path)
  // A torn Zstandard frame fails to decompress, as a torn line fails to parse.
  const text = path.endsWith('.zstd') ? Bun.zstdDecompressSync(await file.bytes()).toString() : await file.text()
  const parsed = Bun.JSONL.parseChunk(text)
  // `Bun.JSONL.parse` would return the events before a malformed or torn line; a log read here must be whole.
  if (!parsed.done) throw parsed.error ?? new SyntaxError(`${path}: incomplete JSON line at character ${parsed.read}`)
  return parsed.values as any[]
}

/**
 * Read the termios flag words: input, output, local, and control.
 *
 * Node's raw mode also sets `VMIN` and `VTIME`, which `Bun.Terminal` does not
 * expose. Node restores them in the same `tcsetattr` call as these flags, so the
 * flags cannot come back while those stay changed.
 *
 * @param terminal - the PTY to read.
 * @returns the four flag words, in that order.
 */
function modes(terminal: Bun.Terminal): readonly number[] {
  return [terminal.inputFlags, terminal.outputFlags, terminal.localFlags, terminal.controlFlags]
}

/**
 * Format termios flag words for a failure report.
 *
 * @param flags - the words `modes` read.
 * @returns them in hex, named after their `struct termios` fields.
 */
function showModes(flags: readonly number[]): string {
  return ['c_iflag', 'c_oflag', 'c_lflag', 'c_cflag'].map((name, index) => `${name}=0x${flags[index]!.toString(16)}`).join(' ')
}

/**
 * Collect every file matching a glob.
 *
 * @param pattern - the glob, relative to `cwd`.
 * @param cwd - the directory to scan; a missing one matches nothing.
 * @returns the absolute paths, in scan order.
 */
async function glob(pattern: string, cwd: string): Promise<string[]> {
  if (!existsSync(cwd)) return []
  const found: string[] = []
  for await (const path of new Bun.Glob(pattern).scan({ cwd, absolute: true })) found.push(path)
  return found
}

/** State and identity of a process the hangup scenario owns. */
interface ProcessState { readonly pid: number; readonly state: string; readonly started?: string }

/** Read the kernel state so a dead, unreaped child does not count as running. */
function processState(pid: number): ProcessState | undefined {
  if (process.platform === 'linux') {
    try {
      const stat = readFileSync(`/proc/${pid}/stat`, 'utf8')
      // The executable name is parenthesized and may contain spaces or `)`.
      const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ')
      if (fields[0] === undefined || fields[19] === undefined) throw new Error(`unreadable process state for ${pid}`)
      return { pid, state: fields[0], started: fields[19] }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined
      throw error
    }
  }
  try {
    process.kill(pid, 0)
    return { pid, state: 'running' }
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM' ? { pid, state: 'running' } : undefined
  }
}

/** Resolve a private-PID-namespace process to its host PID and start identity. */
function namespaceProcess(namespace: string, localPid: number): ProcessState | undefined {
  for (const entry of readdirSync('/proc')) {
    if (!/^\d+$/.test(entry)) continue
    try {
      const hostPid = Number(entry)
      if (readlinkSync(`/proc/${entry}/ns/pid`) !== namespace) continue
      const status = readFileSync(`/proc/${entry}/status`, 'utf8')
      const nsPids = /^NSpid:\s+([\d\s]+)$/m.exec(status)?.[1]?.trim().split(/\s+/).map(Number)
      if (nsPids?.at(-1) !== localPid) continue
      return processState(hostPid)
    } catch (error) {
      if (['ENOENT', 'EACCES', 'EPERM'].includes((error as NodeJS.ErrnoException).code ?? '')) continue
      throw error
    }
  }
  return undefined
}

/** A PID is live only while it still names the acquired process and can run. */
function liveProcess(acquired: ProcessState): boolean {
  const current = processState(acquired.pid)
  return current !== undefined && current.state !== 'Z'
    && (acquired.started === undefined || current.started === acquired.started)
}

/** A named terminal step that never happened, reported with the screen that did. */
class StepFailed extends Error {}

/** Per-run limits and diagnostics, shared by every terminal a run opens. */
interface Options {
  /** Seconds one step may take. */
  step: number
  /** Seconds one terminal may take. */
  budget: number
  /** Whether to print each step to stderr as it is satisfied. */
  trace: boolean
  /** Where failing transcripts are written. */
  artifacts: string
}

/**
 * A `dsh` child driven through a PTY, waited on by named condition.
 *
 * `Bun.Terminal` makes the child a session leader with the PTY as its
 * controlling terminal, as a terminal emulator does. Each wait re-reads the
 * child's output with the ANSI escapes stripped, which is what the assertions
 * read, whenever more arrives. Failure throws `StepFailed` naming the step, why
 * it stopped, whether the child is still alive, and where the transcript landed.
 */
class Terminal {
  private readonly child: Bun.Subprocess
  private readonly terminal: Bun.Terminal
  /** The termios flags the PTY started with, which teardown must restore. */
  private readonly initial: readonly number[]
  private readonly decoder = new TextDecoder()
  private output = ''
  private stripped: string | undefined
  /**
   * The PTY status once its stream has ended: 0 at EOF, 1 on a read error.
   * Linux reports EIO once the last process holding the terminal closes it.
   */
  private status: number | undefined
  private arrival = Promise.withResolvers<void>()
  private readonly deadline: number
  private steps = 0

  constructor(readonly label: string, command: string[], cwd: string,
              env: Record<string, string>, readonly options: Options) {
    this.child = Bun.spawn(command, { cwd, env, terminal: {
      cols: 120, rows: 40,
      data: (_terminal, bytes) => {
        this.output += this.decoder.decode(bytes, { stream: true })
        this.stripped = undefined
        this.notify()
      },
      exit: (_terminal, status) => {
        this.output += this.decoder.decode()
        this.stripped = undefined
        this.status = status
        this.notify()
      },
    } })
    this.terminal = this.child.terminal!
    // Node starts far more slowly than this read returns. `ready` fails if the
    // app is up and these flags still hold, so a late read cannot pass silently.
    this.initial = modes(this.terminal)
    void this.child.exited.then(() => this.notify())
    this.deadline = performance.now() + options.budget * 1000
  }

  /** Everything the child has written, escapes included. */
  get raw(): string {
    return this.output
  }

  /** The child's output so far with ANSI escapes removed. */
  get text(): string {
    return this.stripped ??= Bun.stripANSI(this.output)
  }

  /** How the child ended, or `undefined` while it runs. */
  private get outcome(): string | undefined {
    if (this.child.signalCode !== null) return `was killed by ${this.child.signalCode}`
    return this.child.exitCode === null ? undefined : `exited with code ${this.child.exitCode}`
  }

  /** Whether the child has ended and its output has been read to the end of the stream. */
  private get finished(): boolean {
    return this.outcome !== undefined && this.status !== undefined
  }

  /** Wake every pending `change`. */
  private notify(): void {
    this.arrival.resolve()
    this.arrival = Promise.withResolvers<void>()
  }

  /**
   * Wait for output, the child's exit, or the end of the stream.
   *
   * @param ms - the longest to wait.
   */
  private async change(ms: number): Promise<void> {
    let timer: ReturnType<typeof setTimeout> | undefined
    await Promise.race([this.arrival.promise, new Promise<void>(resolve => { timer = setTimeout(resolve, ms) })])
    clearTimeout(timer)
  }

  /**
   * Current end of the screen, for waits that must ignore earlier output.
   *
   * @returns the offset to slice the screen from.
   */
  mark(): number {
    return this.text.length
  }

  /**
   * Block until the screen satisfies a named condition.
   *
   * @param description - what is being waited for, reported verbatim on failure.
   * @param predicate - reads the cleaned screen and returns whether the step happened.
   * @param timeout - seconds this single step may take; the run default otherwise.
   * @returns the screen once the condition holds.
   */
  async wait(description: string, predicate: (text: string) => boolean | Promise<boolean>, timeout?: number): Promise<string> {
    const started = performance.now()
    const limit = started + (timeout ?? this.options.step) * 1000
    while (true) {
      // Taken before the predicate runs, so a finished child means it read the whole screen.
      const complete = this.finished
      if (await predicate(this.text)) break
      if (this.raw.includes('failed to import')) this.fail(description, 'a profile plugin failed to import')
      const now = performance.now()
      if (now >= this.deadline) this.fail(description, `the terminal budget of ${this.options.budget}s ran out`)
      if (now >= limit) this.fail(description, `no match within ${timeout ?? this.options.step}s`)
      if (complete) this.fail(description, `the process ${this.outcome}`)
      await this.change(RECHECK_MS)
    }
    this.steps += 1
    this.trace(`ok   ${description}`, performance.now() - started)
    return this.text
  }

  /**
   * Wait for every substring to be on screen.
   *
   * @param needles - substrings that must all be present; a trailing number is the `after` offset.
   * @returns the screen once they are.
   */
  async expect(...needles: (string | number)[]): Promise<string> {
    const after = typeof needles.at(-1) === 'number' ? needles.pop() as number : 0
    const shown = (needles as string[]).map(needle => JSON.stringify(needle)).join(' and ')
    return this.wait(`screen shows ${shown}${after ? ` after offset ${after}` : ''}`,
                     text => (needles as string[]).every(needle => text.slice(after).includes(needle)))
  }

  /**
   * Wait for a pattern to match the screen.
   *
   * @param pattern - the regular expression to match.
   * @param after - ignore the screen before this offset, from `mark()`.
   * @returns the match, taken from the screen that satisfied the wait.
   */
  async search(pattern: RegExp, after = 0): Promise<RegExpMatchArray> {
    await this.wait(`screen matches ${pattern}${after ? ` after offset ${after}` : ''}`,
                    text => pattern.test(text.slice(after)))
    return this.text.slice(after).match(pattern)!
  }

  /**
   * Wait until the last `later` on screen comes after the last `earlier`.
   *
   * @param later - the marker that must appear last.
   * @param earlier - the marker it must come after.
   * @returns the screen once the order holds.
   */
  async follows(later: string, earlier: string): Promise<string> {
    return this.wait(`${JSON.stringify(later)} returns after the last ${JSON.stringify(earlier)}`,
                     text => text.includes(earlier) && text.lastIndexOf(later) > text.lastIndexOf(earlier))
  }

  /**
   * Wait for a mounted app. The status line is drawn and paste mode is enabled.
   *
   * Keys typed before Ink enables bracketed paste are swallowed by the terminal
   * and never reach `useInput`, so every scenario starts here.
   *
   * @returns the screen once the app accepts input.
   */
  async ready(): Promise<string> {
    const text = await this.wait('the app to mount and enable bracketed paste',
                                 text => SCREEN.status.test(text) && this.raw.includes('\x1b[?2004h'))
    // Ink enters raw mode before it enables paste. Without this, the teardown
    // comparison would also pass against a driver that cannot see the modes.
    this.check('the app to change the terminal modes', !same(modes(this.terminal), this.initial),
               `the modes are still ${showModes(this.initial)}`)
    return text
  }

  /**
   * Type into the terminal.
   *
   * @param data - the bytes to write, as text or raw bytes including escape sequences.
   * @param note - what the keys mean, for the trace.
   */
  send(data: string | Uint8Array, note?: string): void {
    this.trace(`send ${note ?? JSON.stringify(typeof data === 'string' ? data : [...data])}`)
    this.terminal.write(data)
  }

  /** Resize the PTY. The kernel sends SIGWINCH to the child, whose controlling terminal it is. */
  resize(columns: number, rows: number): void {
    this.terminal.resize(columns, rows)
  }

  /**
   * Assert a condition about the terminal, reporting the screen when it fails.
   *
   * @param description - what was expected.
   * @param condition - the expectation's truth.
   * @param reason - what happened instead.
   */
  check(description: string, condition: boolean, reason = 'it did not hold'): void {
    if (!condition) this.fail(description, reason)
    this.trace(`ok   ${description}`)
  }

  /**
   * Assert something has *not* happened yet.
   *
   * @param description - what must not have happened.
   * @param condition - whether it did.
   */
  refuse(description: string, condition: boolean): void {
    this.check(`not: ${description}`, !condition, 'it happened')
  }

  /**
   * Report a step to stderr under `--trace`, keeping stdout to results.
   *
   * @param line - the step description.
   * @param elapsed - milliseconds it took, when it was a wait.
   */
  private trace(line: string, elapsed?: number): void {
    if (this.options.trace) {
      process.stderr.write(`  [${this.label}] ${line}${elapsed === undefined ? '' : ` (${elapsed.toFixed(0)}ms)`}\n`)
    }
  }

  /**
   * Write the whole transcript where a reader can open it.
   *
   * @returns the transcript path.
   */
  save(): string {
    mkdirSync(this.options.artifacts, { recursive: true })
    const path = join(this.options.artifacts, `${this.label}.log`)
    Bun.write(path, this.text)
    return path
  }

  /**
   * Throw a failure naming the step, the process state, and the screen.
   *
   * @param description - the step that did not happen.
   * @param reason - why waiting stopped.
   */
  fail(description: string, reason: string): never {
    const tail = this.text.split('\n').slice(-30).map(line => `  | ${line}`).join('\n')
    throw new StepFailed(
      `waiting for ${description}\n`
      + `  reason:  ${reason}\n`
      + `  process: ${this.outcome ?? 'running'}\n`
      + `  steps:   ${this.steps} satisfied before this one\n`
      + `  screen:  ${this.save()} (last lines below)\n${tail}`)
  }

  /**
   * Exit through the double Ctrl-C the product documents, then verify teardown.
   *
   * Terminal modes, bracketed paste, and the exit code are all owned by
   * `releaseTerminal()`; checking them here is what proves a change to teardown
   * kept every path working.
   *
   * @returns the whole transcript.
   */
  async quit(): Promise<string> {
    this.send('\x03', 'Ctrl-C')
    await this.expect('Press Ctrl-C again')
    this.send('\x03', 'Ctrl-C again')
    return this.exited(0, 'the second Ctrl-C')
  }

  /** Verify process outcome and the PTY state after a normal or fatal exit. */
  async exited(code: number, reason: string): Promise<string> {
    await this.ended(code, reason)
    this.check('bracketed paste to be released',
               this.raw.includes('\x1b[?2004h') && this.raw.includes('\x1b[?2004l'),
               'paste mode was enabled but never released')
    if (this.raw.includes('\x1b[?1049h')) {
      this.check('the alternate screen to be released', this.raw.lastIndexOf('\x1b[?1049l') > this.raw.lastIndexOf('\x1b[?1049h'))
      this.check('mouse reporting to be released', this.raw.lastIndexOf('\x1b[?1000l') > this.raw.lastIndexOf('\x1b[?1000h'))
    }
    return this.text
  }

  /**
   * Wait for the child to end, then verify its exit status and the terminal modes.
   *
   * Unlike `exited`, this asks nothing of the screen, so it also fits a launch
   * refused before the app mounted.
   *
   * @param code - the exit code the child must end with.
   * @param reason - what ends it, for the report.
   * @param seconds - how long the child and its stream may take to end; the step timeout otherwise.
   */
  async ended(code: number, reason: string, seconds = this.options.step): Promise<void> {
    // The stream ends once nothing holds the terminal open, after the last
    // byte the exit wrote, so the checks below read the whole transcript.
    await this.settle(`the process to exit after ${reason}`, () => this.finished, seconds,
                      () => this.outcome === undefined ? `still running after ${seconds}s`
                        : `it ${this.outcome}, but its terminal was still open ${seconds}s later`)
    this.checkExit(code, reason)
    const after = modes(this.terminal)
    this.check('terminal modes to be restored', same(after, this.initial),
               `the child left ${showModes(after)} instead of ${showModes(this.initial)}`)
  }

  /**
   * Wait for the child to exit, whether or not its terminal is still open, then verify its exit status.
   *
   * @param code - the exit code the child must end with.
   * @param reason - what ends it, for the report.
   * @param seconds - how long the child may take to exit.
   */
  async exits(code: number, reason: string, seconds: number): Promise<void> {
    await this.settle(`the process to exit after ${reason}`, () => this.outcome !== undefined, seconds,
                      () => `still running after ${seconds}s`)
    this.checkExit(code, reason)
  }

  /**
   * Wait for a condition about the child that output, its exit, or the end of its stream can bring about.
   *
   * @param description - what is awaited, reported verbatim on failure.
   * @param done - whether it has happened.
   * @param seconds - how long it may take.
   * @param reason - why waiting stopped, once the time is up.
   */
  private async settle(description: string, done: () => boolean, seconds: number, reason: () => string): Promise<void> {
    const limit = performance.now() + seconds * 1000
    while (!done()) {
      const left = limit - performance.now()
      if (left <= 0) this.fail(description, reason())
      await this.change(left)
    }
  }

  /**
   * Assert how the child exited.
   *
   * @param code - the exit code it must have ended with, by exiting rather than by a signal.
   * @param reason - what ended it, for the report.
   */
  private checkExit(code: number, reason: string): void {
    this.check(`exit code ${code} after ${reason}`, this.child.exitCode === code && this.child.signalCode === null,
               `exit code ${this.child.exitCode}, signal ${this.child.signalCode}`)
  }

  /**
   * Signal the child alone, as `kill` from another terminal does.
   *
   * @param signal - the signal to send.
   */
  signal(signal: NodeJS.Signals): void {
    this.trace(`signal ${signal}`)
    this.child.kill(signal)
  }

  /** Close the PTY under the running child, as closing a terminal window does. The kernel hangs the child up. */
  hangup(): void {
    this.trace('hang up')
    this.terminal.close()
  }

  /**
   * Kill any surviving child, await it, and release the pty.
   *
   * When the child dies, the kernel hangs up its foreground process group, and
   * closing the terminal hangs up anything still holding it. A tool subprocess
   * that left the group and the terminal can outlive a failed run.
   */
  async close(): Promise<void> {
    try {
      if (this.outcome === undefined) this.child.kill('SIGKILL')
      await this.child.exited
    } finally {
      if (!this.terminal.closed) this.terminal.close()
    }
  }
}

/** One selectable terminal scenario and what it needs to have run first. */
interface Scenario {
  /** The name `--only` selects. */
  name: string
  /** What the scenario proves, printed by `--list` and on pass. */
  summary: string
  /** Scenarios whose state this one reads. */
  requires: readonly string[]
  /** Whether it depends on the recorded fixture. A live model is not required. */
  replayOnly: boolean
  /** The scenario itself. */
  body: (run: Run) => Promise<void>
}

const SCENARIOS = new Map<string, Scenario>()

/**
 * Register a scenario in declaration order.
 *
 * @param name - the name `--only` selects.
 * @param summary - what the scenario proves.
 * @param options - its prerequisites and whether it needs the recorded fixture.
 * @param body - the scenario itself.
 */
function scenario(name: string, summary: string,
                  options: { requires?: readonly string[], replayOnly?: boolean },
                  body: (run: Run) => Promise<void>): void {
  SCENARIOS.set(name, { name, summary, requires: options.requires ?? [], replayOnly: options.replayOnly ?? false, body })
}

/** The temporary home, workspace, profile, and overlay every scenario shares. */
class Run {
  readonly home: string
  readonly workspace: string
  readonly sessionsRoot: string
  readonly overlay: string
  readonly env: Record<string, string>
  readonly state: Record<string, any> = {}
  recorded!: any[]
  prompt!: string

  constructor(readonly root: string, readonly live: boolean, readonly node: string, readonly options: Options) {
    this.home = join(root, 'home')
    this.workspace = join(root, 'workspace')
    this.sessionsRoot = join(this.home, 'sessions')
    this.overlay = join(root, 'replay.patch.yml')
    mkdirSync(this.workspace, { recursive: true })
    // `Bun.Terminal` does not export TERM to the child, so the environment names the terminal.
    this.env = { ...process.env as Record<string, string>, DSH_HOME: this.home,
                 DSH_AGENTS_HOME: join(root, 'agents'), TERM: 'xterm-256color', NO_COLOR: '1' }
    // Replay must not pick up a developer's provider key; CI detection stays intact.
    delete this.env.DEEPSEEK_API_KEY
    delete this.env.CLIPROXYAPI_API_KEY
    // The replay adapter never sends this key; startup still requires a configured sign-in.
    if (!this.live) this.env.DEEPSEEK_API_KEY = 'tui-replay-placeholder'
  }

  /** Read the fixture the replay and the assertions share. */
  async load(): Promise<void> {
    this.recorded = await events(FIXTURE)
    this.prompt = this.recorded.find(event => event.type === 'user/message'
                                     && event.data.source.kind === 'user').data.content[0].text
    await this.writeOverlay()
  }

  /**
   * Rewrite the profile overlay the CLI loads over the built patch.
   *
   * @param override - a replay override file staging the next model response.
   * @param profile - scenario-specific storage, goal-round, replay, or balance-endpoint settings.
   */
  async writeOverlay(override?: string, profile: { root?: string; compression?: 'none' | 'zstd'; goalMaxRounds?: number; paceMs?: number; balanceBaseURL?: string; cliProxyApi?: boolean; noDefaultModel?: boolean } = {}): Promise<void> {
    const replay: any = {
      id: 'tui-replay', name: join(ROOT, 'packages/test-support/llm-replay/lib/index.js'),
      config: { file: FIXTURE, ...(profile.paceMs === undefined ? {} : { paceMs: profile.paceMs }), providers: [{ id: 'deepseek-official', models: [
        { id: 'deepseek-v4-flash', contextWindow: 128000 },
        { id: 'tui-picked-model', contextWindow: 128000,
          reasoningEfforts: ['low', 'high'], defaultReasoningEffort: 'low' }] }] },
    }
    if (override !== undefined) replay.config.overrideFile = override
    const patches: any[] = [
      { id: 'session-title-llm', disabled: true },
      // The shipped profile names no default; every other scenario starts on this one.
      ...profile.noDefaultModel === true ? [] : [{ id: 'agent-default-model', config: { provider: 'deepseek-official', model: 'deepseek-v4-flash' } }],
      { id: 'session-persistence-jsonl', config: {
        root: profile.root ?? this.sessionsRoot, compression: profile.compression ?? 'none',
      } },
      ...(profile.goalMaxRounds === undefined ? [] : [{ id: 'goal', config: { defaultMaxGoalRounds: profile.goalMaxRounds } }]),
    ]
    await Bun.write(this.overlay, JSON.stringify(this.live ? patches : [
      profile.balanceBaseURL === undefined
        ? { id: 'llm-deepseek', disabled: true }
        : { id: 'llm-deepseek', config: { baseURL: profile.balanceBaseURL, protocol: 'messages' } },
      ...profile.cliProxyApi ? [] : [{ id: 'llm-pi-ai', disabled: true }],
      ...patches, ...(profile.balanceBaseURL === undefined ? [{ insert: [replay] }] : []),
    ]))
  }

  /**
   * Drop the remembered `/model` choice. Every scenario shares this home, and
   * each expects a new session on the overlay's default model.
   */
  async forgetDefaultModel(): Promise<void> {
    const path = join(this.home, 'settings.yaml')
    if (!existsSync(path)) return
    const text = await Bun.file(path).text()
    const kept = text.replace(/^agent-default-model:\n(?:[ \t]+.*(?:\n|$)|\n)*/mu, '')
    if (kept !== text) await Bun.write(path, kept)
  }

  /**
   * Build the CLI invocation for one terminal.
   *
   * @param extra - arguments appended after the profile patches.
   * @param profile - the shipped profile to launch; the overlay applies to either.
   * @returns the argv to spawn.
   */
  command(extra: readonly string[], nodeArgs: readonly string[] = [], profile: 'tui' | 'headless' = 'tui'): string[] {
    return [this.node, ...nodeArgs, ...(this.live ? [`--env-file=${join(ROOT, '.env')}`] : []),
            join(ROOT, 'apps/cli/lib/bin.js'), '--profile', profile,
            '--patch', this.overlay, ...extra]
  }

  /**
   * Every persisted session log.
   *
   * @returns the set of log paths currently on disk.
   */
  async logs(): Promise<Set<string>> {
    return new Set(await glob('**/session.v*.jsonl', this.sessionsRoot))
  }

  /**
   * Find the files a scenario just created.
   *
   * @param before - the log set taken before the scenario ran.
   * @param what - what the caller expected, for the failure message.
   * @returns the single new log path.
   */
  async created(before: Set<string>, what: string): Promise<string> {
    const now = [...await this.logs()].filter(path => !before.has(path))
    if (now.length !== 1) throw new Error(`expected one ${what} session, got ${now.sort().join(', ') || 'none'}`)
    return now[0]!
  }

  /**
   * Open a terminal, hand it to the caller, and verify its teardown.
   *
   * @param label - the scenario name, used for the trace and the transcript file.
   * @param extra - extra CLI arguments, such as `--resume`.
   * @param drive - types into the terminal and waits on what it shows.
   * @returns the whole transcript, after a verified exit.
   */
  async terminal(label: string, extra: readonly string[],
                 drive: (tty: Terminal) => Promise<void>): Promise<string> {
    const terminal = new Terminal(label, this.command(extra), this.workspace, this.env, this.options)
    try {
      await terminal.ready()
      await drive(terminal)
      return await terminal.quit()
    } catch (error) {
      terminal.save()
      throw error
    } finally {
      await terminal.close()
    }
  }
}

/**
 * Assert a scenario expectation about persisted state.
 *
 * @param condition - the expectation's truth.
 * @param message - what went wrong.
 */
function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message)
}

/** Compare two values the way the recorded fixtures are compared. By content. */
function same(left: unknown, right: unknown): boolean {
  return Bun.deepEquals(left, right)
}

const DONE_LINE = new RegExp(`\\n {${COLUMN.rail}}DONE\\r?\\n`)
/** The recorded answer to a prompt queued behind `/compact`, and its line in the transcript. */
const QUEUED_REPLY = 'QUEUED_AFTER_COMPACTION'
const QUEUED_LINE = new RegExp(`\\n {${COLUMN.rail}}${QUEUED_REPLY}\\r?\\n`)

scenario('fresh', 'login, model and effort selection, paste, cursor editing, a bash tool turn, the context estimate, and billed tokens', {},
  async run => {
    const before = await run.logs()
    const prompt = run.prompt
    await run.terminal('fresh', [], async tty => {
      tty.send('/lo', 'a partial slash command')
      await tty.search(picked('/login'))
      tty.send('\t', 'Tab to complete it')
      await tty.expect(`> /login ${SCREEN.caret}`)
      tty.send('\x7f', 'remove the optional target separator')
      await tty.expect(`> /login${SCREEN.caret}`)
      tty.send('\r', 'Enter')
      await tty.expect('Choose a sign-in target')
      tty.send('\x1b', 'dismiss the login picker')
      await tty.expect('Sign-in cancelled')
      if (!run.live) {
        tty.send('/model\r')
        await tty.expect('Model \u00b7 2 from DeepSeek')
        await tty.expect('Recent 1')
        tty.send('tui-picked')
        await tty.search(picked('tui-picked-model'))
        await tty.expect('Effort   Default (low)   low   high')
        tty.send('\x1b[C\x1b[C', 'Right twice to step the effort to High')
        tty.send('\r', 'Enter to pick the model and effort together')
        await tty.expect('Model set for the next turn: deepseek-official/tui-picked-model (high)')
        await tty.expect('tui-picked-model  think high')
      }
      tty.send(`\x1b[200~X${prompt}Z\x1b[201~`, 'a bracketed paste padded with X and Z')
      await tty.expect(`> X${prompt}Z${SCREEN.caret}`)
      tty.send('\x1b[H', 'Home')
      await tty.expect(`> ${SCREEN.caret}X${prompt}`)
      tty.send('\x1b[3~', 'Delete, dropping the leading X')
      await tty.expect(`> ${SCREEN.caret}${prompt}Z`)
      tty.send('\x1b[F', 'End')
      await tty.expect(`> ${prompt}Z${SCREEN.caret}`)
      tty.send('\x7f', 'Backspace, dropping the trailing Z')
      await tty.expect(`> ${prompt}${SCREEN.caret}`)
      tty.send('\x1b[D', 'Left')
      await tty.expect(`> ${prompt.slice(0, -1)}${SCREEN.caret}${prompt.slice(-1)}`)
      tty.refuse('editing submitted the prompt without Enter', SCREEN.toolCall.test(tty.text))
      tty.send('\r', 'Enter to submit')
      await tty.wait("the bash result and the model's DONE line",
                     text => text.includes(SCREEN.toolResult) && DONE_LINE.test(text))
      await tty.follows(SCREEN.idle, SCREEN.toolResult)
      // The context reading leads with its percentage; no field has a colon.
      await tty.search(/ {2}ctx ~\d+% \([\d.]+k?\/128k\)/u)
      // Billed tokens as the provider reported them, cache reads included.
      if (run.live) await tty.search(/ {2}in [\d.]+k? {2}out [\d.]+k?(?: {2}cache hit \d+%)?/)
      else await tty.expect('  in 5.9k  out 115  cache hit 48%')
      // Ctrl-J breaks the line in any terminal, where Shift-Enter sends Enter's
      // own carriage return. The line feed arrives as a read of its own. The
      // log check below proves the draft was never submitted.
      const drafted = tty.mark()
      tty.send('Draft line one', 'the first line of a draft')
      await tty.expect(`> Draft line one${SCREEN.caret}`, drafted)
      const broken = tty.mark()
      tty.send('\n', 'Ctrl-J')
      await tty.expect(`  ${SCREEN.caret}`, broken)
      tty.send('draft line two', 'the second line of the draft')
      const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
      let consumed = 0
      try {
        await tty.wait('the draft to hold two rows, the caret at the end of the second', async () => {
          const raw = tty.raw
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
          const buffer = screen.buffer.active
          const rows = Array.from({ length: screen.rows }, (_, row) => buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? '')
          const first = rows.findIndex(row => row === '> Draft line one')
          return first >= 0 && rows[first + 1]?.startsWith(`  draft line two${SCREEN.caret}`) === true
        })
      } finally {
        screen.dispose()
      }
    })

    const path = await run.created(before, 'persisted')
    const log = await events(path)
    assert(log[0].agentPreset === 'standard', 'the session did not mount the standard preset')
    const submitted = log.filter(e => e.type === 'user/message' && e.data.source.kind === 'user').map(e => e.data.content)
    assert(same(submitted, [[{ type: 'text', text: run.prompt }]]), 'cursor editing changed the submitted prompt')
    const headers = log.filter(e => e.type === 'request/header').map(e => e.data.header)
    if (!run.live) {
      assert(headers.length > 0 && headers.every(h => h.config.model === 'tui-picked-model'
                                                 && h.config.reasoningEffort === 'high'),
             'picker selection did not reach recorded requests')
      // The pick is also the default a new session starts on.
      const saved = await Bun.file(join(run.home, 'settings.yaml')).text()
      assert(/^agent-default-model:\n(?:\s+.*\n)*?\s+model: tui-picked-model\n(?:\s+.*\n)*?\s+reasoningEffort: high$/mu.test(saved),
             `the picked model was not saved as the new-session default:\n${saved}`)
    }
    const model = log.filter(e => e.type === 'assistant/message').map(e => e.data.message.content)
    const expectedModel = run.recorded.filter(e => e.type === 'assistant/message').map(e => e.data.message.content)
    assert(run.live ? model.flat().some((block: any) => block.type === 'text' && block.text === 'DONE')
                    : same(model, expectedModel),
           'recorded model output changed or replay was not fully consumed')
    const resultText = (groups: any[][]) =>
      groups.map(blocks => blocks.map(block => [block.content, block.isError ?? false]))
    const results = log.filter(e => e.type === 'tool/result' && e.surfaceOp === 'append').map(e => e.data.message.content)
    const expectedResults = run.recorded.filter(e => e.type === 'tool/result' && e.surfaceOp === 'append')
      .map(e => e.data.message.content)
    assert(same(resultText(results), resultText(expectedResults)),
           `real tool output disagrees with the recording: ${JSON.stringify(resultText(results)).slice(0, 2000)}`
           + ` instead of ${JSON.stringify(resultText(expectedResults)).slice(0, 500)}`)
    Object.assign(run.state, { log: path, id: log[0].id, model, headers })
  })

scenario('welcome', 'a fresh session opens with the BAKE block, its version and session line, and /changelog prints the entry',
  { replayOnly: true },
  async run => {
    const version = (await Bun.file(join(ROOT, 'package.json')).json() as { version: string }).version
    const copy = dictionaries.en
    const heading = `${copy.session}: `
    await run.terminal('welcome', [], async tty => {
      // The line-break key is taught once, here, and nowhere on the composer.
      const opening = await tty.expect('BAKE', `v${version}`, heading, '/help', '/changelog', copy.welcomeChangelog,
                                       copy.newlineKey, copy.welcomeNewline)
      tty.check('the session line sits inside the welcome block', opening.indexOf('BAKE') < opening.indexOf(heading))
      const start = tty.mark()
      tty.send('/changelog\r', 'print the running version\'s changelog')
      await tty.expect(`## [${version}]`, start)
      await tty.follows(SCREEN.idle, `## [${version}]`)
      for (const command of ['/new', '/clear']) {
        const after = tty.mark()
        tty.send(`${command}\r`, 'open a fresh session')
        await tty.expect('BAKE', `v${version}`, heading, copy.welcomeChangelog, after)
        await tty.follows(SCREEN.idle, heading)
      }
    })
  })

scenario('terminal-setup', '/terminal-setup in VS Code shows the file and binding, writes Shift+Enter as ESC CR after a backup once '
  + 'accepted, and changes nothing on a second run',
  { replayOnly: true },
  async run => {
    const copy = dictionaries.en
    // A home of its own: the command must never read or write the developer's editor settings.
    const home = join(run.root, 'terminal-setup-home')
    // Where VS Code keeps user settings on this platform, as the command resolves it.
    const settings = process.platform === 'darwin' ? ['Library', 'Application Support'] : ['.config']
    const file = join(home, ...settings, 'Code', 'User', 'keybindings.json')
    const shown = `~/${[...settings, 'Code', 'User', 'keybindings.json'].join('/')}`
    const original = '// mine\n[\n]\n'
    mkdirSync(dirname(file), { recursive: true })
    await Bun.write(file, original)
    const saved = { ...run.env }
    // The terminal this scenario claims to be, and nothing the developer's own terminal exported.
    const outer = new RegExp('^(?:TERM_PROGRAM|VSCODE_|CURSOR_|WT_SESSION|KITTY_|GHOSTTY_|ALACRITTY_|WEZTERM_|ITERM_|LC_TERMINAL|TMUX'
      + '|SSH_|__CFBundleIdentifier)', 'u')
    for (const key of Object.keys(run.env).filter(key => outer.test(key))) delete run.env[key]
    Object.assign(run.env, { HOME: home, XDG_CONFIG_HOME: join(home, '.config'), APPDATA: join(home, 'AppData'), TERM_PROGRAM: 'vscode' })
    try {
      await run.terminal('terminal-setup', [], async tty => {
        const start = tty.mark()
        tty.send('/terminal-setup\r', 'run the terminal setup')
        await tty.expect(`${copy.terminalSetupTitle} · VS Code`, `${copy.terminalSetupAdds} ${shown}`,
          '"command": "workbench.action.terminal.sendSequence",', start)
        await tty.wait('the write choice to be selected', text => picked(copy.terminalSetupWrite).test(text.slice(start)))
        tty.check('nothing is written before the answer', readFileSync(file, 'utf8') === original)
        const answered = tty.mark()
        tty.send('\r', 'write the binding')
        await tty.expect(`VS Code · ${copy.terminalSetupDone}`, copy.terminalSetupTest, answered)
        const again = tty.mark()
        tty.send('/terminal-setup\r', 'run it again')
        await tty.expect(`VS Code · ${copy.terminalSetupPresent}`, again)
      })
      const text = readFileSync(file, 'utf8')
      assert(text.startsWith('// mine\n[\n') && text.includes('"key": "shift+enter"') && text.includes('"text": "\\u001b\\r"')
        && text.includes('"when": "terminalFocus"'), `keybindings.json lacks the binding:\n${text}`)
      const backups = readdirSync(dirname(file)).filter(name => name.startsWith('keybindings.json.bak-'))
      assert(backups.length === 1, `expected one backup, got ${backups.join(', ') || 'none'}`)
      assert(readFileSync(join(dirname(file), backups[0]!), 'utf8') === original, 'the backup is not the original file')
    } finally {
      for (const key of Object.keys(run.env)) delete run.env[key]
      Object.assign(run.env, saved)
    }
  })

scenario('status-colour', 'model and context use normal foreground while supporting status fields stay dim', { replayOnly: true },
  async run => {
    const colour = { NO_COLOR: run.env.NO_COLOR, FORCE_COLOR: run.env.FORCE_COLOR }
    delete run.env.NO_COLOR
    run.env.FORCE_COLOR = '3'
    try {
      await run.terminal('status-colour', [], async tty => {
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        let consumed = 0
        try {
          tty.send(`${run.prompt}\r`)
          await tty.follows(SCREEN.idle, SCREEN.toolResult)
          await tty.wait('the complete status row with context and billed tokens', async () => {
            const raw = tty.raw
            await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
            consumed = raw.length
            const buffer = screen.buffer.active
            for (let row = buffer.length - 1; row >= 0; row--) {
              const line = buffer.getLine(row)
              const text = line?.translateToString(true) ?? ''
              if (!SCREEN.status.test(text) || !text.includes('ctx ~') || !text.includes('in 5.9k')) continue
              // Values in the normal foreground: the model, and a context reading with room to spare.
              for (const field of ['deepseek-v4-flash', '~']) {
                const cell = line!.getCell(text.indexOf(field, text.indexOf(field === '~' ? 'ctx ~' : field)))!
                tty.check(`${field} uses normal foreground`, !cell.isDim() && cell.isFgDefault())
              }
              // Labels and billed totals stay dim.
              for (const field of ['ctx ~', 'in 5.9k', 'cache hit']) {
                tty.check(`${field} stays dim`, !!line!.getCell(text.indexOf(field))!.isDim())
              }
              return true
            }
            return false
          })
          screen.resize(60, 40)
          tty.resize(60, 40)
          await tty.wait('a context reading beside the model at 60 columns, with no access mode', async () => {
            const raw = tty.raw
            await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
            consumed = raw.length
            const line = screen.buffer.active.getLine(screen.buffer.active.viewportY + screen.rows - 2)?.translateToString(true) ?? ''
            return SCREEN.status.test(line) && !line.includes(dictionaries.en.permission) && /ctx ~\d+%/.test(line)
          })
        } finally { screen.dispose() }
      })
    } finally {
      for (const [key, value] of Object.entries(colour)) {
        if (value === undefined) delete run.env[key]
        else run.env[key] = value
      }
    }
  })

scenario('git-status', 'the status line names the workspace branch and its changes, and follows them while the terminal runs', { replayOnly: true },
  async run => {
    // The commands that set the tree up read no developer config, and no
    // variable a hook exports points them at another repository.
    const env: Record<string, string> = Object.fromEntries(Object.entries(run.env).filter(([key]) => !key.startsWith('GIT_')))
    Object.assign(env, { GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: join(run.root, 'gitconfig'), GIT_CEILING_DIRECTORIES: run.root })
    const git = (...args: string[]): void => {
      const done = Bun.spawnSync(['git', '-c', 'user.name=Bake', '-c', 'user.email=bake@example.test', ...args], { cwd: run.workspace, env })
      assert(done.exitCode === 0, `git ${args.join(' ')} failed: ${done.stderr.toString()}`)
    }
    const files = ['tracked.txt', 'untracked.txt'].map(name => join(run.workspace, name))
    // The workspace every scenario shares becomes a repository only for this one.
    const saved = { ...run.env }
    Object.keys(run.env).filter(key => key.startsWith('GIT_')).forEach(key => { delete run.env[key] })
    try {
      git('init', '-q', '-b', 'main')
      await Bun.write(files[0]!, 'one\n')
      git('add', '-A')
      git('commit', '-q', '-m', 'seed')
      await Bun.write(files[0]!, 'two\n')
      await Bun.write(files[1]!, 'new\n')
      await run.terminal('git-status', [], async tty => {
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        let consumed = 0
        // The status row as the terminal shows it now, not as the stream wrote it.
        const status = async (): Promise<string> => {
          const raw = tty.raw
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
          const buffer = screen.buffer.active
          for (let row = buffer.length - 1; row >= 0; row--) {
            const text = buffer.getLine(row)?.translateToString(true) ?? ''
            if (SCREEN.status.test(text)) return text
          }
          return ''
        }
        try {
          // The branch glyph is drawn only where the terminal draws the round frame.
          await tty.wait('the branch, its unstaged change, and its untracked path on the status line',
            async () => /(\u2387 )?main ~1 \?1 {2}/.test(await status()))
          git('switch', '-q', '-c', 'topic')
          git('add', 'untracked.txt')
          await tty.wait('the new branch and its staged path, read while the terminal runs',
            async () => /(\u2387 )?topic \+1 ~1 {2}/.test(await status()))
          git('add', '-A')
          git('commit', '-q', '-m', 'clean')
          await tty.wait('a clean tree as the branch alone', async () => {
            const row = await status()
            return /(\u2387 )?topic {2}/.test(row) && !/topic [+~?]/.test(row)
          })
        } finally { screen.dispose() }
      })
    } finally {
      Object.assign(run.env, saved)
      rmSync(join(run.workspace, '.git'), { recursive: true, force: true })
      for (const file of files) rmSync(file, { force: true })
    }
  })

scenario('thinking', 'selected and provider-default thinking levels follow model changes without wrapping', { replayOnly: true },
  async run => {
    const before = await run.logs()
    await run.terminal('thinking', [], async tty => {
      const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
      let consumed = 0
      const footer = async (description: string, accepts: (line: string) => boolean) => {
        await tty.wait(description, async () => {
          const raw = tty.raw
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
          const buffer = screen.buffer.active
          return accepts(buffer.getLine(buffer.viewportY + screen.rows - 2)?.translateToString(true) ?? '')
            && buffer.getLine(buffer.viewportY + screen.rows - 1)?.translateToString(true) === ''
        })
      }
      try {
        await footer('no invented level for a model without reasoning controls', line =>
          line.startsWith('  deepseek-v4-flash') && !line.includes('think'))
        tty.send('/model deepseek-official/tui-picked-model high\r')
        await footer('explicit high thinking level', line =>
          line.startsWith('  tui-picked-model  think high'))
        screen.resize(40, 12)
        tty.resize(40, 12)
        await footer('full thinking indicator at 40 columns', line =>
          line.includes('think high') && !line.includes('tui-picked-model (high)'))
        screen.resize(120, 40)
        tty.resize(120, 40)
        tty.send('/model deepseek-official/tui-picked-model\r')
        await footer('advertised provider default low', line =>
          line.startsWith('  tui-picked-model  think low'))
        tty.send('/model deepseek-official/deepseek-v4-flash\r')
        await footer('unsupported thinking level omitted after switching models', line =>
          line.startsWith('  deepseek-v4-flash') && !line.includes('think'))
      } finally { screen.dispose() }
    })
    const log = await events(await run.created(before, 'thinking'))
    assert(!log.some(e => e.type === 'request/header'), '/model commands unexpectedly called the model')
  })

scenario('permissions', 'workspace-write default, the access mode where a session opens, command feedback at narrow widths, and durable session selection', { replayOnly: true },
  async run => {
    const override = run.env.DSH_PERMISSION_MODE
    delete run.env.DSH_PERMISSION_MODE
    const access = dictionaries.en.permission
    /**
     * Drive one terminal. `opened` finds the mode where the session opens, in
     * the welcome block or on the heading; `set` switches it and waits for the
     * command's result. Both check that the footer is one row and names no mode.
     */
    const inspect = async (tty: Terminal, drive: (tools: {
      readonly opened: (mode: string, after?: number) => Promise<void>
      readonly set: (mode: string) => Promise<void>
      readonly resize: () => void
    }) => Promise<void>) => {
      const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
      let consumed = 0
      const footer = async () => {
        await tty.wait('a one-row footer without the access mode', async () => {
          const raw = tty.raw
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
          const buffer = screen.buffer.active
          const line = buffer.getLine(buffer.viewportY + screen.rows - 2)?.translateToString(true) ?? ''
          return SCREEN.status.test(line) && !line.includes(access)
            && buffer.getLine(buffer.viewportY + screen.rows - 1)?.translateToString(true) === ''
        })
      }
      const opened = async (mode: string, after = 0) => { await tty.expect(`${access} ${mode}`, after); await footer() }
      const set = async (mode: string) => {
        const after = tty.mark()
        tty.send(`/permission ${mode}\r`)
        await tty.expect(`preset ${mode}`, after)
        await footer()
      }
      try { await drive({ opened, set, resize: () => { screen.resize(40, 12); tty.resize(40, 12) } }) }
      finally { screen.dispose() }
    }
    try {
      const before = await run.logs()
      await run.terminal('permissions', [], tty => inspect(tty, async ({ opened, set, resize }) => {
        await opened('workspace-write')
        await set('read-only')
        await set('danger-full-access')
        resize()
        await set('read-only')
      }))
      const path = await run.created(before, 'permission')
      const log = await events(path)
      assert(same(log.filter(e => e.type === 'permission/preset').map(e => e.data.preset),
        ['workspace-write', 'read-only', 'danger-full-access', 'read-only']), 'permission changes did not reach the session log')
      assert(log.find(e => e.type === 'sandbox/mode')?.data.mode === 'workspace-write', 'new session sandbox was not workspace-write')
      assert(log.find(e => e.type === 'approval/policy')?.data.policy === 'ask', 'new session approval policy was not ask')
      const beforeNew = await run.logs()
      await run.terminal('permissions-resume', ['--resume', log[0].id], tty => inspect(tty, async ({ opened }) => {
        // A resumed session opens with the mode it was left in, on its heading.
        await opened('read-only')
        const after = tty.mark()
        tty.send('/new\r')
        await opened('workspace-write', after)
      }))
      const fresh = await events(await run.created(beforeNew, 'new permission'))
      assert(fresh.find(e => e.type === 'permission/preset')?.data.preset === 'workspace-write', '/new did not use the configured default')
      for (const record of [await events(path), fresh]) {
        assert(!record.some(e => e.type === 'request/header'), 'permission commands unexpectedly called the model')
      }
      run.env.DSH_PERMISSION_MODE = 'read-only'
      await run.terminal('permissions-override', [], tty => inspect(tty, async ({ opened }) => {
        await opened('read-only')
      }))
    } finally {
      if (override === undefined) delete run.env.DSH_PERMISSION_MODE
      else run.env.DSH_PERMISSION_MODE = override
    }
  })

scenario('questions', 'a real ask_user_question tool call offers choices and retains an Other draft',
  { replayOnly: true },
  async run => {
    const override = join(run.root, 'questions-replay.json')
    const args = JSON.stringify({ questions: [{ id: 'method', question: 'Choose a method',
      options: [{ label: 'Alpha' }, { label: 'Beta' }], multi_select: true }] })
    const call = { type: 'tool-call' as const, id: 'call-question-1', name: 'ask_user_question', arguments: args }
    await Bun.write(override, JSON.stringify([
      { kind: 'chunks', chunks: [
        { type: 'block-start', index: 0, blockType: 'tool-call' },
        { type: 'tool-call-delta', index: 0, id: call.id, name: call.name, argumentsDelta: args },
        { type: 'block-end', index: 0, block: call },
        { type: 'finish', reason: { kind: 'tool-calls' } },
      ] },
      { kind: 'chunks', chunks: [
        { type: 'block-start', index: 0, blockType: 'text' },
        { type: 'text-delta', index: 0, text: 'Question answered.' },
        { type: 'block-end', index: 0, block: { type: 'text', text: 'Question answered.' } },
        { type: 'finish', reason: { kind: 'stop' } },
      ] },
    ]))
    await run.writeOverlay(override)
    const before = await run.logs()
    try {
      await run.terminal('questions', [], async tty => {
        tty.send('Ask me to choose a method.\r', 'trigger the recorded question tool call')
        await tty.expect('Choose a method', 'Other answer:')
        tty.send(' ', 'select Alpha')
        await tty.expect('[✓] 1. Alpha')
        tty.send('A custom method', 'start an Other answer')
        await tty.expect('Other answer: A custom method')
        tty.send('\x1b[A', 'inspect the choices while keeping the draft')
        await tty.search(picked('[ ] 2. Beta'))
        await tty.expect('Other answer: A custom method')
        tty.send('\x1b[B\r', 'return to Other and submit the combined answer')
        await tty.expect('Question answered.')
        // The committed row is the tool's own card: what was asked and what was
        // answered, never the call's arguments or the answers as JSON.
        const row = await tty.expect('AskUserQuestion(Ask: Choose a method)', 'method \u2192 Alpha, "A custom method"')
        assert(!row.includes('"questions"') && !row.includes('{"answers"'), 'the question row printed its arguments or answers as JSON')
      })
    } finally { await run.writeOverlay() }
    const log = await events(await run.created(before, 'question answer'))
    const result = log.find(event => event.type === 'tool/result'
      && event.data.message.content.some((item: any) => item.type === 'tool-result'
        && item.toolCallId === 'call-question-1'))
    assert(result !== undefined, 'question tool result did not reach the session log')
    const toolResult = result.data.message.content.find((item: any) => item.type === 'tool-result'
      && item.toolCallId === 'call-question-1')
    const answerText = toolResult.content.find((item: any) => item.type === 'text')?.text
    assert(typeof answerText === 'string', 'question tool result has no answer text')
    assert(same(JSON.parse(answerText), { answers: [{ id: 'method', selected: ['Alpha'], custom: 'A custom method' }] }),
      'selected and custom answers were not both returned by the tool')
  })

scenario('tasks', 'a real todo_write call folds into one row above the header that opens the full checklist',
  { replayOnly: true },
  async run => {
    const override = join(run.root, 'tasks-replay.json')
    const args = JSON.stringify({ todos: [
      { content: 'Read startup', status: 'completed' },
      { content: 'Thread the home', status: 'in_progress' },
      { content: 'Test it', status: 'pending' },
    ] })
    const call = { type: 'tool-call' as const, id: 'call-todo-1', name: 'todo_write', arguments: args }
    await Bun.write(override, JSON.stringify([
      { kind: 'chunks', chunks: [
        { type: 'block-start', index: 0, blockType: 'tool-call' },
        { type: 'tool-call-delta', index: 0, id: call.id, name: call.name, argumentsDelta: args },
        { type: 'block-end', index: 0, block: call },
        { type: 'finish', reason: { kind: 'tool-calls' } },
      ] },
      { kind: 'chunks', chunks: [
        { type: 'block-start', index: 0, blockType: 'text' },
        { type: 'text-delta', index: 0, text: 'Planned.' },
        { type: 'block-end', index: 0, block: { type: 'text', text: 'Planned.' } },
        { type: 'finish', reason: { kind: 'stop' } },
      ] },
    ]))
    await run.writeOverlay(override)
    try {
      await run.terminal('tasks', [], async tty => {
        tty.send('Plan the work.\r', 'trigger the recorded todo_write call')
        await tty.follows(SCREEN.idle, 'Planned.')
        await tty.search(/☐ Tasks 1\/3 {2}━{4}─{8} {2}▸ Thread the home +Ctrl\+T/u)
        // The row is the whole list's cost: the other tasks get no rows of
        // their own. The call's card above still lists them in the transcript.
        const viewport = async (): Promise<string> => {
          const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
          try {
            await new Promise<void>(resolve => screen.write(tty.raw, resolve))
            return Array.from({ length: 40 }, (_, row) =>
              screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.translateToString(true) ?? '').join('\n')
          } finally { screen.dispose() }
        }
        assert(!/^\s*□ Test it/mu.test(await viewport()), 'the task row drew more than the current task')
        // Up walks back through the one prompt, then selects the row.
        const walk = tty.mark()
        tty.send('\x1b[A'.repeat(4), 'walk up through history to the task row')
        await tty.expect('> Tasks', walk)
        const opened = tty.mark()
        tty.send('\r', 'open the full checklist')
        await tty.expect('1/3 done', '✓ 1 Read startup', '▸ 2 Thread the home', '□ 3 Test it', opened)
        tty.send('\x1b', 'close the checklist')
        // A lone Escape decodes only once no sequence follows it.
        await tty.wait('the checklist to close over the empty draft', async () => {
          const visible = await viewport()
          return visible.includes(`${SCREEN.prompt}${SCREEN.caret}`) && !visible.includes('Esc closes')
        })
        // Closing replays the history the sheet pushed up back down: the answer
        // sits over the task row again, not over a block of the sheet's blank rows.
        const reanchored = async (): Promise<boolean> => {
          const rows = (await viewport()).split('\n')
          const answer = rows.findLastIndex(row => row.includes('Planned.'))
          const task = rows.findLastIndex(row => row.startsWith('☐ Tasks '))
          return answer >= 0 && task > answer && rows.slice(answer + 1, task).filter(row => row.trim() === '').length <= 1
        }
        await tty.wait('the history to come back down against the controls', reanchored)
        const shortcut = tty.mark()
        tty.send('\x14', 'open the checklist with Ctrl+T')
        await tty.expect('1/3 done', shortcut)
        tty.send('\x1b', 'close it again')
        await tty.wait('the checklist to close', async () => !(await viewport()).includes('Esc closes'))
        await tty.wait('the history to come back down after Ctrl+T', reanchored)
      })
    } finally { await run.writeOverlay() }
  })

scenario('edit', 'a recorded edit draws only its changed lines, numbered, with changed words reversed and code highlighted',
  { replayOnly: true },
  async run => {
    const file = join(run.workspace, 'startup.ts')
    const source = `${Array.from({ length: 40 }, (_, index) => index === 12 ? '  const home = process.env.HOME ?? fallback()' : `  line(${index + 1})`).join('\n')}\n`
    await Bun.write(file, source)
    const override = join(run.root, 'edit-replay.json')
    const calls = [
      { type: 'tool-call' as const, id: 'call-read-1', name: 'read', arguments: JSON.stringify({ file_path: 'startup.ts' }) },
      { type: 'tool-call' as const, id: 'call-edit-1', name: 'edit', arguments: JSON.stringify({
        file_path: 'startup.ts', old_string: 'const home = process.env.HOME', new_string: 'const home = resolvedHome ?? process.env.HOME' }) },
    ]
    await Bun.write(override, JSON.stringify([
      ...calls.map(call => ({ kind: 'chunks', chunks: [
        { type: 'block-start', index: 0, blockType: 'tool-call' },
        { type: 'tool-call-delta', index: 0, id: call.id, name: call.name, argumentsDelta: call.arguments },
        { type: 'block-end', index: 0, block: call },
        { type: 'finish', reason: { kind: 'tool-calls' } },
      ] })),
      { kind: 'chunks', chunks: [
        { type: 'block-start', index: 0, blockType: 'text' },
        { type: 'text-delta', index: 0, text: 'Edited.' },
        { type: 'block-end', index: 0, block: { type: 'text', text: 'Edited.' } },
        { type: 'finish', reason: { kind: 'stop' } },
      ] },
    ]))
    await run.writeOverlay(override)
    // Colour on for this run alone. Reversed words and syntax colour are
    // escape sequences, and `NO_COLOR` erases both.
    const colour = { NO_COLOR: run.env.NO_COLOR, FORCE_COLOR: run.env.FORCE_COLOR }
    delete run.env.NO_COLOR
    run.env.FORCE_COLOR = '3'
    try {
      await run.terminal('edit', [], async tty => {
        const start = tty.mark()
        const from = tty.raw.length
        tty.send('Make the home configurable.\r', 'trigger the recorded read and edit')
        await tty.expect('startup.ts)  +1 −1', '13 -   const home = process.env.HOME ?? fallback()',
          '13 +   const home = resolvedHome ?? process.env.HOME ?? fallback()', 'Edited.', start)
        const shown = tty.text.slice(start)
        tty.check('the context lines around the change are not drawn', !shown.includes('line(12)') && !shown.includes('line(14)'))
        const raw = tty.raw.slice(from)
        tty.check('the words the edit added are reversed', raw.includes('\x1b[7mresolvedHome ?? '))
        // The number and the code carry separate styles, so the removed line is
        // found through the escapes between them.
        const at = raw.search(/13(?:\x1b\[[0-9;]*m)* (?:\x1b\[[0-9;]*m)*- /)
        tty.check('the removed line is drawn', at >= 0)
        const removed = raw.slice(at, raw.indexOf('\n', at))
        const colours = new Set(removed.match(/\x1b\[38;(?:5;\d+|2;\d+;\d+;\d+)m/g) ?? [])
        tty.check('the removed line carries syntax colour beside its red', colours.size >= 2)
      })
    } finally {
      for (const [name, value] of Object.entries(colour)) {
        if (value === undefined) delete run.env[name]
        else run.env[name] = value
      }
      await run.writeOverlay()
    }
    assert(await Bun.file(file).text() === source.replace('const home = process.env.HOME', 'const home = resolvedHome ?? process.env.HOME'),
      'the recorded edit did not change the file')
  })

scenario('tool-colour', 'real read, search, and shell results retain syntax colour, bounds, and readable replay without colour',
  { replayOnly: true }, async run => {
    const file = join(run.workspace, 'colours.ts')
    const source = 'export const colourValue = "ready"\n// source stays unchanged\n'
    await Bun.write(file, source)
    const json = '{"tool_colour":true,"count":3}'
    const requests = [
      ['read', { file_path: 'colours.ts' }],
      ['grep', { pattern: 'colourValue', path: '.', include: 'colours.ts' }],
      ['bash', { command: `printf '%s\\n' '${json}'`, description: 'Print JSON' }],
      ['bash', { command: "printf '%s\\n' 'WARN colours.ts:1' 'PASS syntax checks'", description: 'Print diagnostics' }],
    ] as const
    const override = join(run.root, 'tool-colour-replay.json')
    await Bun.write(override, JSON.stringify([
      ...requests.map(([name, args], index) => {
        const call = { type: 'tool-call', id: `colour-${index}`, name, arguments: JSON.stringify(args) }
        return { kind: 'chunks', chunks: [
          { type: 'block-start', index: 0, blockType: 'tool-call' },
          { type: 'tool-call-delta', index: 0, id: call.id, name, argumentsDelta: call.arguments },
          { type: 'block-end', index: 0, block: call },
          { type: 'finish', reason: { kind: 'tool-calls' } },
        ] }
      }),
      { kind: 'chunks', chunks: [
        { type: 'block-start', index: 0, blockType: 'text' },
        { type: 'text-delta', index: 0, text: 'TOOL_COLOUR_DONE' },
        { type: 'block-end', index: 0, block: { type: 'text', text: 'TOOL_COLOUR_DONE' } },
        { type: 'finish', reason: { kind: 'stop' } },
      ] },
    ]))
    const before = await run.logs()
    const colour = { NO_COLOR: run.env.NO_COLOR, FORCE_COLOR: run.env.FORCE_COLOR, COLORTERM: run.env.COLORTERM }
    await run.writeOverlay(override)
    delete run.env.NO_COLOR
    run.env.FORCE_COLOR = '3'
    run.env.COLORTERM = 'truecolor'
    try {
      await run.terminal('tool-colour', [], async tty => {
        const from = tty.raw.length
        tty.send('Inspect the source and structured output.\r')
        await tty.follows(SCREEN.idle, 'TOOL_COLOUR_DONE')
        const raw = tty.raw.slice(from)
        tty.check('read and grep source tokens are coloured', /\x1b\[38;[^m]+mcolourValue/.test(raw))
        tty.check('JSON keys are coloured', /\x1b\[38;[^m]+m"tool_colour"/.test(raw))
        tty.check('diagnostic labels use semantic colours', raw.includes('\x1b[38;2;234;179;8mWARN') && raw.includes('\x1b[38;2;34;197;94mPASS'))
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        try {
          await new Promise<void>(resolve => screen.write(tty.raw, resolve))
          const lines = Array.from({ length: screen.buffer.active.length }, (_, row) => screen.buffer.active.getLine(row)?.translateToString(true) ?? '')
          tty.check('both read and search show the source once', lines.filter(line => line.includes('export const colourValue = "ready"')).length === 2)
          tty.check('JSON remains exact', lines.some(line => line.trim() === json))
        } finally { screen.dispose() }
      })
    } finally {
      for (const [name, value] of Object.entries(colour)) {
        if (value === undefined) delete run.env[name]
        else run.env[name] = value
      }
      await run.writeOverlay()
    }
    const log = await events(await run.created(before, 'tool colour'))
    assert((await Bun.file(file).text()) === source, 'read or search modified its source')
    assert(log.filter(event => event.type === 'tool/result').length === requests.length, 'not every real tool completed')
    await run.terminal('tool-colour-resume', ['--resume', log[0].id], async tty => {
      await tty.expect('TOOL_COLOUR_DONE', 'colourValue', json, 'WARN colours.ts:1', 'PASS syntax checks')
      const colours = tty.raw.match(/\x1b\[(?:3[0-7]|38;(?:5;\d+|2;\d+;\d+;\d+))m/g) ?? []
      tty.check(`NO_COLOR replay emits no foreground colour (${JSON.stringify([...new Set(colours)])})`, colours.length === 0)
    })
  })

scenario('background-job', 'a background bash job that settles after the turn wakes the agent with exactly one completion notice',
  { replayOnly: true }, async run => {
    // The sleep outlasts the recorded turn, so the notice takes the idle-wake path.
    const args = JSON.stringify({ command: 'sleep 1; printf done', description: 'Finish later', run_in_background: true })
    const call = { type: 'tool-call' as const, id: 'call-job-1', name: 'bash', arguments: args }
    const text = (value: string) => ({ kind: 'chunks', chunks: [
      { type: 'block-start', index: 0, blockType: 'text' },
      { type: 'text-delta', index: 0, text: value },
      { type: 'block-end', index: 0, block: { type: 'text', text: value } },
      { type: 'finish', reason: { kind: 'stop' } },
    ] })
    const override = join(run.root, 'background-job-replay.json')
    await Bun.write(override, JSON.stringify([
      { kind: 'chunks', chunks: [
        { type: 'block-start', index: 0, blockType: 'tool-call' },
        { type: 'tool-call-delta', index: 0, id: call.id, name: call.name, argumentsDelta: args },
        { type: 'block-end', index: 0, block: call },
        { type: 'finish', reason: { kind: 'tool-calls' } },
      ] },
      text('JOB_STARTED'),
      text('JOB_NOTICED'),
    ]))
    const before = await run.logs()
    await run.writeOverlay(override)
    try {
      await run.terminal('background-job', [], async tty => {
        tty.send('Start the job in the background.\r', 'trigger the recorded background bash call')
        await tty.follows(SCREEN.idle, 'JOB_STARTED')
        await tty.follows(SCREEN.idle, 'JOB_NOTICED')
      })
    } finally { await run.writeOverlay() }
    const log = await events(await run.created(before, 'background job'))
    const notices = log.filter(event => event.type === 'user/message' && event.data.source?.plugin === 'tool-jobs')
    assert(notices.length === 1, `expected one job completion notice, the model saw ${notices.length}`)
  })

scenario('arrow-wave', 'the single-line kneading spinner loops in place and yields to a short composer',
  { replayOnly: true }, async run => {
    const override = join(run.root, 'arrow-wave-replay.json')
    const reasoning = 'Checking the processing indicator. '.repeat(80)
    await Bun.write(override, JSON.stringify([{ kind: 'chunks', chunks: [
      { type: 'block-start', index: 0, blockType: 'reasoning' },
      ...Array.from({ length: 80 }, () => ({ type: 'reasoning-delta', index: 0, text: 'Checking the processing indicator. ' })),
      { type: 'block-end', index: 0, block: { type: 'reasoning', text: reasoning } },
      { type: 'block-start', index: 1, blockType: 'text' },
      { type: 'text-delta', index: 1, text: 'WAVE_DONE' },
      { type: 'block-end', index: 1, block: { type: 'text', text: 'WAVE_DONE' } },
      { type: 'finish', reason: { kind: 'stop' } },
    ] }]))
    const colour = { NO_COLOR: run.env.NO_COLOR, FORCE_COLOR: run.env.FORCE_COLOR }
    delete run.env.NO_COLOR
    run.env.FORCE_COLOR = '3'
    try {
      await run.writeOverlay(override, { paceMs: 100 })
      await run.terminal('arrow-wave', [], async tty => {
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        let consumed = 0
        const capture = async (): Promise<string[]> => {
          const raw = tty.raw
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
          return Array.from({ length: screen.rows }, (_, row) =>
            screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.translateToString(true) ?? '')
        }
        try {
          const frames = new Set<string>()
          tty.send('Show the processing wave.\r')
          await tty.wait('six kneading frames at the same position', async () => {
            const rows = await capture()
            const header = rows.findIndex(line => /^[\u2800-\u28ff]{3} \S+…/.test(line))
            if (header < 1) return false
            // PTY reads may end mid-frame, before its final scroll anchors the controls.
            // Header, upper rule, input, base rule, then the status line.
            if (header !== 34 || !rows[35]!.startsWith('\u2500') || !rows[36]!.startsWith('> ')
              || !rows[37]!.startsWith('\u2500') || !SCREEN.status.test(rows[38]!)) return false
            tty.check('there is no dot zone above the processing line', !rows.some(line => /^[\u2800-\u28ff]{3}$/.test(line)))
            frames.add(rows[header]!.slice(0, 3))
            return frames.size === 6
          })
          tty.check('the spinner stays in the header directly above the input', frames.size === 6)
          const mark = tty.raw.length
          screen.resize(40, 4)
          tty.resize(40, 4)
          await tty.wait('the short terminal keeps its input visible', async () => {
            const rows = await capture()
            // Three rows once the rules have yielded. The header, the input, and the status line.
            return tty.raw.length > mark && !rows[0]!.startsWith('─') && rows[1]!.includes('> ') && SCREEN.status.test(rows[2]!)
          })
          screen.resize(80, 24)
          tty.resize(80, 24)
          await tty.follows(SCREEN.idle, 'WAVE_DONE')
          const rows = await capture()
          tty.check('completion removes the dot field', !rows.some(line => /[\u2800-\u28ff]/.test(line)))
          tty.check('the completed composer stays at the bottom', SCREEN.status.test(rows[22]!))
        } finally { screen.dispose() }
      })
    } finally {
      for (const [key, value] of Object.entries(colour)) {
        if (value === undefined) delete run.env[key]
        else run.env[key] = value
      }
      await run.writeOverlay()
    }
  })

scenario('fullscreen', 'alternate-screen scrolling, pinned input, resize, replay, and shell restoration',
  { replayOnly: true }, async run => {
    const answer = Array.from({ length: 100 }, (_, index) => `Fullscreen paragraph ${index}.`).join('\n\n') + '\n\nFULLSCREEN_DONE'
    const override = join(run.root, 'fullscreen-replay.json')
    await Bun.write(override, JSON.stringify([{ kind: 'chunks', chunks: [
      { type: 'block-start', index: 0, blockType: 'text' },
      ...answer.split(/(?<=\n\n)/).map(text => ({ type: 'text-delta', index: 0, text })),
      { type: 'block-end', index: 0, block: { type: 'text', text: answer } },
      { type: 'finish', reason: { kind: 'stop' } },
    ] }]))
    const before = await run.logs()
    await run.writeOverlay(override, { paceMs: 1 })
    try {
      const drive = async (label: string, resume?: string): Promise<void> => {
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        await new Promise<void>(resolve => screen.write('shell before Bake\r\n$ ', resolve))
        let consumed = 0
        const capture = async (raw: string): Promise<string[]> => {
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
          const buffer = screen.buffer.active
          return Array.from({ length: screen.rows }, (_, row) => buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? '')
        }
        let tty: Terminal | undefined
        try {
          await run.terminal(label, ['--screen', 'fullscreen', ...resume === undefined ? [] : ['--resume', resume]], async terminal => {
            tty = terminal
            if (resume === undefined) {
              await terminal.expect('BAKE')
              terminal.send('Show the fullscreen transcript.\r')
            }
            await terminal.wait('the fullscreen response and bottom status', async () => {
              const rows = await capture(terminal.raw)
              return screen.buffer.active.type === 'alternate' && rows.some(line => line.includes('FULLSCREEN_DONE'))
                && SCREEN.status.test(rows.at(-1) ?? '')
            })
            terminal.send('\x1b[5~', 'PageUp pauses transcript following')
            await terminal.wait('older output with the input pinned', async () => {
              const rows = await capture(terminal.raw)
              return rows.some(line => line.includes(dictionaries.en.transcriptPaused))
                && !rows.some(line => line.includes('FULLSCREEN_DONE')) && rows.at(-3)?.includes(SCREEN.caret) === true
            })
            terminal.send('\x1b[1;5H', 'Ctrl+Home reaches the opening prompt')
            await terminal.wait('the first prompt in the viewport', async () => (await capture(terminal.raw)).some(line => line.includes('Show the fullscreen transcript.')))
            terminal.send('\x1b[1;5F', 'Ctrl+End resumes following')
            await terminal.wait('the newest output again', async () => (await capture(terminal.raw)).some(line => line.includes('FULLSCREEN_DONE')))
            terminal.send('\x1b[<64;10;10M', 'the wheel scrolls up three rows')
            await terminal.wait('the jump-to-latest offer over older output', async () => {
              const rows = await capture(terminal.raw)
              return rows.some(line => line.includes(dictionaries.en.transcriptLatest)) && !rows.some(line => line.includes('FULLSCREEN_DONE'))
                && rows.at(-3)?.includes(SCREEN.caret) === true
            })
            const row = (await capture(terminal.raw)).findIndex(line => line.includes(dictionaries.en.transcriptLatest))
            terminal.send(`\x1b[<0;2;${row + 1}M\x1b[<0;2;${row + 1}m`, 'clicking the offer follows output')
            await terminal.wait('the newest output after the click', async () => {
              const rows = await capture(terminal.raw)
              return rows.some(line => line.includes('FULLSCREEN_DONE')) && rows.some(line => line.includes(dictionaries.en.transcriptScroll))
                && !rows.join('\n').includes('[<')
            })
            screen.resize(40,12)
            terminal.resize(40,12)
            await terminal.wait('the resized fullscreen keeps its last answer and composer', async () => {
              const rows = await capture(terminal.raw)
              return rows.some(line => line.includes('FULLSCREEN_DONE')) && SCREEN.status.test(rows.at(-1) ?? '')
            })
            terminal.send('\x1b[200~saved draft\x1b[201~', 'paste while viewing fullscreen history')
            await terminal.wait('the pasted draft stays above status', async () => (await capture(terminal.raw)).at(-3)?.includes('saved draft') === true)
          })
          await capture(tty!.raw)
          assert(screen.buffer.active.type === 'normal', 'fullscreen did not restore the primary screen')
          assert(screen.buffer.active.getLine(0)?.translateToString(true) === 'shell before Bake', 'fullscreen erased shell history')
          assert(screen.buffer.active.cursorY === 1 && screen.buffer.active.cursorX === 2, 'fullscreen moved the saved shell cursor')
        } finally { screen.dispose() }
      }
      await drive('fullscreen')
      const log = await events(await run.created(before, 'fullscreen'))
      const texts = log.filter(e => e.type === 'assistant/message').flatMap(e => e.data.message.content.filter((block: any) => block.type === 'text').map((block: any) => block.text))
      assert(texts.includes(answer), 'fullscreen changed the persisted assistant answer')
      await drive('fullscreen-resume', log[0].id)
    } finally { await run.writeOverlay() }
  })

scenario('markdown', 'streamed Markdown keeps semantic colours, formats once, survives resize and uncoloured resume, and preserves the logged source',
  { replayOnly: true }, async run => {
    const reasoning = '**Review** the formatter.'
    const answer = '# Formatted response\n\n**Ready** with `snake_case` and [docs](https://example.com).\n\n'
      + '[**https://example.org**](https://example.org) and [`src/helper.ts`](src/helper.ts).\n\n'
      + '- [x] parsed\n- [ ] verified\n\n```ts\nconst snake_case = "**literal**"\n```\n\n'
      + '| Check | State |\n| --- | --- |\n| Stream | ready |\n\n'
      + `${'Wide '.repeat(22)}end.\n\nFORMATTER_DONE`
    const override = join(run.root, 'markdown-replay.json')
    const chunks = [
      { type: 'block-start', index: 0, blockType: 'reasoning' },
      { type: 'reasoning-delta', index: 0, text: reasoning },
      { type: 'block-end', index: 0, block: { type: 'reasoning', text: reasoning } },
      { type: 'block-start', index: 1, blockType: 'text' },
      ...Array.from({ length: Math.ceil(answer.length / 7) }, (_, index) => ({
        type: 'text-delta', index: 1, text: answer.slice(index * 7, (index + 1) * 7),
      })),
      { type: 'block-end', index: 1, block: { type: 'text', text: answer } },
      { type: 'finish', reason: { kind: 'stop' } },
    ]
    await Bun.write(override, JSON.stringify([{ kind: 'chunks', chunks }]))
    const before = await run.logs()
    const colour = { NO_COLOR: run.env.NO_COLOR, FORCE_COLOR: run.env.FORCE_COLOR, COLORTERM: run.env.COLORTERM }
    await run.writeOverlay(override, { paceMs: 15 })
    delete run.env.NO_COLOR
    run.env.FORCE_COLOR = '3'
    run.env.COLORTERM = 'truecolor'
    const checkScreen = (screen: InstanceType<typeof xterm.Terminal>, coloured = false): void => {
      const lines = Array.from({ length: screen.buffer.active.length }, (_, row) => screen.buffer.active.getLine(row)?.translateToString(true) ?? '')
      assert(lines.filter(line => line === '  Formatted response').length === 1, 'Markdown heading was lost or printed twice')
      const text = lines.join('\n')
      assert(text.includes('Review the formatter.') && lines.some(line => /Check\s+\u2502\s+State/.test(line))
        && lines.some(line => /Stream\s+\u2502\s+ready/.test(line)), 'reasoning or table was not formatted')
      assert(text.includes('const snake_case = "**literal**"') && !text.includes('```'), 'code was parsed as prose or retained its fences')
      assert([...text.matchAll(/https:\/\/example\.org/g)].length === 1 && [...text.matchAll(/src\/helper\.ts/g)].length === 1,
        'a formatted link label duplicated its target')
      assert(text.includes('\u2022 [x] parsed') && text.includes('\u2022 [ ] verified'), 'task states were lost')
      if (coloured) {
        for (const [needle, expected] of [['Formatted response', PALETTE.reference], ['snake_case', PALETTE.code],
          ['[x]', PALETTE.done], ['Check', PALETTE.reference], ['Wide ', PALETTE.body],
          ['\u2022 [x]', MARKDOWN.bullet], ['src/helper.ts', PALETTE.reference]] as const) {
          const row = lines.findIndex(line => line.includes(needle))
          const cell = screen.buffer.active.getLine(row)?.getCell(lines[row]!.indexOf(needle))
          assert(cell?.isFgRGB() && cell.getFgColor() === Number.parseInt(expected.slice(1), 16), `${needle} lost its semantic colour`)
        }
      }
      const wide = lines.filter(line => line.includes('Wide '))
      assert(wide.length > 0 && text.includes('end.'), 'long response was lost')
      if (screen.cols >= 120) assert(wide.some(line => line.length > 90), 'wide terminal still caps response at a fixed prose measure')
      else assert(wide.length > 1, 'narrow terminal did not rewrap the response')
    }
    try {
      await run.terminal('markdown', [], async tty => {
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        let consumed = 0
        const capture = async (): Promise<void> => {
          const raw = tty.raw
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
        }
        try {
          tty.send('Show formatted output.\r')
          await tty.follows(SCREEN.idle, 'FORMATTER_DONE')
          await capture()
          checkScreen(screen, true)
          const mark = tty.raw.length
          screen.resize(40, 12)
          tty.resize(40, 12)
          await tty.wait('formatted history and composer after resize', async () => {
            await capture()
            return tty.raw.length > mark && SCREEN.status.test(screen.buffer.active.getLine(screen.buffer.active.viewportY + 10)?.translateToString(true) ?? '')
          })
          checkScreen(screen, true)
        } finally { screen.dispose() }
      })
      for (const [name, value] of Object.entries(colour)) {
        if (value === undefined) delete run.env[name]
        else run.env[name] = value
      }
      const path = await run.created(before, 'Markdown')
      const log = await events(path)
      const messages = log.filter(event => event.type === 'assistant/message').map(event => event.data.message.content)
      assert(same(messages, [[{ type: 'reasoning', text: reasoning }, { type: 'text', text: answer }]]), 'formatting changed the logged model source')
      await run.terminal('markdown-resume', ['--resume', log[0].id], async tty => {
        await tty.expect('FORMATTER_DONE')
        assert(!/\x1b\[(?:3[0-7]|38;(?:5;\d+|2;\d+;\d+;\d+))m/.test(tty.raw), 'NO_COLOR Markdown replay emitted foreground colour')
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        try {
          await new Promise<void>(resolve => screen.write(tty.raw, resolve))
          checkScreen(screen)
        } finally { screen.dispose() }
      })
    } finally {
      for (const [name, value] of Object.entries(colour)) {
        if (value === undefined) delete run.env[name]
        else run.env[name] = value
      }
      await run.writeOverlay()
    }
  })

scenario('tables', 'streamed tables align columns, wrap styled cells, reflow to labeled rows, and replay without changing source',
  { replayOnly: true }, async run => {
    const answer = '# Table response\n\n'
      + '| Item | Count | State | Description |\n| :--- | ---: | :---: | :--- |\n'
      + '| alpha | 125 | **ready** | Supports 中文 and `source.ts` across wrapped cells. |\n'
      + '| beta | 7 | waiting | Keeps every value readable. |\n'
      + '| gamma | 42 | ready | Last table entry. |\n\nTABLE_DONE'
    const override = join(run.root, 'tables-replay.json')
    await Bun.write(override, JSON.stringify([{ kind: 'chunks', chunks: [
      { type: 'block-start', index: 0, blockType: 'text' },
      ...Array.from({ length: Math.ceil(answer.length / 11) }, (_, index) => ({
        type: 'text-delta', index: 0, text: answer.slice(index * 11, (index + 1) * 11),
      })),
      { type: 'block-end', index: 0, block: { type: 'text', text: answer } },
      { type: 'finish', reason: { kind: 'stop' } },
    ] }]))
    const before = await run.logs()
    await run.writeOverlay(override, { paceMs: 15 })
    const drive = async (label: string, resume?: string): Promise<void> => {
      await run.terminal(label, ['--screen', 'fullscreen', ...resume === undefined ? [] : ['--resume', resume]], async tty => {
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        let consumed = 0
        const capture = async (): Promise<string[]> => {
          const raw = tty.raw
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
          const buffer = screen.buffer.active
          return Array.from({ length: screen.rows }, (_, row) => buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? '')
        }
        const checkContent = (lines: readonly string[]): void => {
          const text = lines.join('\n')
          for (const item of ['alpha', 'beta', 'gamma']) {
            assert([...text.matchAll(new RegExp(item, 'g'))].length === 1, `${item} was lost or repeated`)
          }
          for (const value of ['125', '7', '42', 'ready', 'waiting', '中文', 'source.ts', 'wrapped', 'cells.', 'readable.', 'entry.']) {
            assert(text.includes(value), `table value ${value} was lost`)
          }
          assert(!text.includes('**') && !text.includes('`'), 'table cells retained Markdown delimiters')
        }
        try {
          if (resume === undefined) tty.send('Show the response table.\r')
          let wide: string[] = []
          await tty.wait('the table in aligned columns', async () => {
            wide = await capture()
            return wide.some(line => line.includes('TABLE_DONE')) && wide.some(line => /Item\s+\u2502\s+Count\s+\u2502/.test(line))
              && SCREEN.status.test(wide.at(-1) ?? '')
          })
          checkContent(wide)
          const alpha = wide.find(line => /alpha\s+\u2502/.test(line))!
          const beta = wide.find(line => /beta\s+\u2502/.test(line))!
          assert(alpha.indexOf('125') + 3 === beta.indexOf('7') + 1, 'numeric table cells were not right-aligned')
          screen.resize(24, 40)
          tty.resize(24, 40)
          let narrow: string[] = []
          await tty.wait('the narrow table in labeled rows', async () => {
            narrow = await capture()
            return narrow.some(line => line.includes('Item: alpha')) && narrow.some(line => line.includes('Count: 125'))
              && narrow.some(line => line.includes('TABLE_DONE')) && narrow.some(line => line.includes(SCREEN.caret))
          })
          checkContent(narrow)
          screen.resize(80, 40)
          tty.resize(80, 40)
          await tty.wait('the table returns to columns', async () => {
            wide = await capture()
            return wide.some(line => /Item\s+\u2502\s+Count\s+\u2502/.test(line)) && SCREEN.status.test(wide.at(-1) ?? '')
          })
          checkContent(wide)
        } finally { screen.dispose() }
      })
    }
    try {
      await drive('tables')
      const log = await events(await run.created(before, 'tables'))
      const messages = log.filter(event => event.type === 'assistant/message').map(event => event.data.message.content)
      assert(same(messages, [[{ type: 'text', text: answer }]]), 'table layout changed the logged response source')
      await drive('tables-resume', log[0].id)
    } finally { await run.writeOverlay() }
  })

scenario('usage', 'the built TUI reads DeepSeek remaining credit through /usage without a model call',
  { replayOnly: true },
  async run => {
    const requests: Array<{ path: string; authorization: string | null }> = []
    const server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch(request) {
      const url = new URL(request.url)
      requests.push({ path: url.pathname, authorization: request.headers.get('authorization') })
      return Response.json({ is_available: true, balance_infos: [
        { currency: 'USD', total_balance: '7.50', granted_balance: '2.00', topped_up_balance: '5.50' },
      ] })
    } })
    const previousKey = run.env.DEEPSEEK_API_KEY
    run.env.DEEPSEEK_API_KEY = 'smoke-balance-key'
    await run.writeOverlay(undefined, { balanceBaseURL: new URL('anthropic', server.url).href })
    const before = await run.logs()
    try {
      await run.terminal('usage', [], async tty => {
        tty.send('/help\r', 'discover composed commands')
        await tty.search(/\/usage\b.* {2}Show remaining DeepSeek API credit/u)
        const start = tty.mark()
        tty.send('/usage\r', 'read DeepSeek account balance')
        await tty.expect('USD: 7.50 remaining (2.00 granted, 5.50 topped up)', start)
      })
    } finally {
      if (previousKey === undefined) delete run.env.DEEPSEEK_API_KEY
      else run.env.DEEPSEEK_API_KEY = previousKey
      await run.writeOverlay()
      server.stop(true)
    }
    assert(same(requests, [{ path: '/user/balance', authorization: 'Bearer smoke-balance-key' }]),
      'usage did not call the configured balance API with its key')
    const log = await events(await run.created(before, 'usage'))
    assert(log.some(event => event.type === 'command/done' && event.data.kind === 'success'
      && event.data.text?.includes('USD: 7.50 remaining')), '/usage result did not commit to the session')
    assert(!log.some(event => event.type === 'user/message'), '/usage entered model input')
  })

scenario('no-default', 'with nothing signed in the session starts on no model, keeps a message, and the first sign-in selects its provider',
  { replayOnly: true },
  async run => {
    const previousKey = run.env.DEEPSEEK_API_KEY
    delete run.env.DEEPSEEK_API_KEY
    await run.forgetDefaultModel()
    await run.writeOverlay(undefined, { noDefaultModel: true })
    try {
      const transcript = await run.terminal('no-default', [], async tty => {
        await tty.expect('no model  /login to start', 'Nothing is signed in yet; type /login to choose a provider')
        tty.send('/', 'open the command menu')
        await tty.search(picked('/login'))
        tty.send('\x7fhello\r', 'send a message with no model')
        await tty.expect('No model is selected yet: sign in with /login, or choose one with /model')
        await tty.expect(`> hello${SCREEN.caret}`)
        tty.send('\x7f'.repeat(5), 'clear the kept draft')
        tty.send('/login DeepSeek\r', 'sign in by the provider name')
        await tty.expect('Sign in · DeepSeek', 'DEEPSEEK_API_KEY · saved in')
        tty.send('\r', 'Enter with nothing typed')
        await tty.expect('✗ Type or paste a value, or press Esc to cancel')
        tty.send('smoke-deepseek-key\r', 'store the key')
        await tty.expect('DeepSeek: signed in · now using deepseek-official/deepseek-v4-flash')
        await tty.expect('deepseek-v4-flash')
        tty.send('/logout deepseek\r', 'sign out again')
        await tty.expect('DeepSeek: key removed · the current model used it; choose another with /model')
      })
      assert(!transcript.includes('smoke-deepseek-key'), 'the DeepSeek key appeared on the terminal')
      const saved = await Bun.file(join(run.home, 'settings.yaml')).text()
      assert(/^agent-default-model:\n(?:\s+.*\n)*?\s+model: deepseek-v4-flash$/mu.test(saved),
        `the first sign-in did not save its model as the new-session default:\n${saved}`)
      assert(!saved.includes('smoke-deepseek-key'), 'the DeepSeek key was saved in settings')
    } finally {
      if (previousKey === undefined) delete run.env.DEEPSEEK_API_KEY
      else run.env.DEEPSEEK_API_KEY = previousKey
      await run.forgetDefaultModel()
      await run.writeOverlay()
    }
  })

scenario('cliproxyapi', 'the built TUI configures a CLIProxyAPI URL and key and selects its models',
  { replayOnly: true },
  async run => {
    const before = await run.logs()
    const requests: Array<{ path: string, query: string, authorization: string | null, model?: string, cached?: boolean }> = []
    const server = Bun.serve({ hostname: '127.0.0.1', port: 0, async fetch(request) {
      const url = new URL(request.url)
      if (url.pathname === '/v1/responses') {
        const body = await request.json() as { model: string }
        requests.push({ path: url.pathname, query: url.search, authorization: request.headers.get('authorization'), model: body.model })
        const item = { id: 'msg_proxy', type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'PROXY_OK' }] }
        const events = [
          { type: 'response.created', response: { id: 'resp_proxy', status: 'in_progress' } },
          { type: 'response.output_item.added', output_index: 0, item: { ...item, content: [] } },
          { type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'PROXY_OK' },
          { type: 'response.output_item.done', output_index: 0, item },
          { type: 'response.completed', response: { id: 'resp_proxy', status: 'completed', output: [item], usage: { input_tokens: 4, output_tokens: 2, total_tokens: 6 } } },
        ]
        const empty = 'event: response.completed\n: keep-alive\n\n'
        // First request loses its terminal payload; the real retry plugin must recover.
        const wire = requests.filter(request => request.path === '/v1/responses').length === 1
          ? empty : events.map(event => `${empty}data: ${JSON.stringify(event)}\n\n`).join('') + empty + 'data: [DONE]\n\n'
        return new Response(wire,
          { headers: { 'content-type': 'text/event-stream' } })
      }
      if (url.pathname === '/v1/messages') {
        const body = await request.json() as { model: string, system?: { cache_control?: unknown }[] }
        requests.push({ path: url.pathname, query: url.search, authorization: request.headers.get('x-api-key'), model: body.model,
          cached: body.system?.some(block => block.cache_control !== undefined) ?? false })
        const events = [
          ['message_start', { type: 'message_start', message: { id: 'msg_claude', type: 'message', role: 'assistant', model: body.model,
            content: [], stop_reason: null, usage: { input_tokens: 4, output_tokens: 0, cache_read_input_tokens: 0 } } }],
          ['content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }],
          ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'CLAUDE_OK' } }],
          ['content_block_stop', { type: 'content_block_stop', index: 0 }],
          ['message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 2 } }],
          ['message_stop', { type: 'message_stop' }],
        ] as const
        return new Response(events.map(([name, data]) => `event: ${name}\ndata: ${JSON.stringify(data)}\n\n`).join(''),
          { headers: { 'content-type': 'text/event-stream' } })
      }
      requests.push({ path: url.pathname, query: url.search, authorization: request.headers.get('authorization') })
      if (request.headers.get('authorization') !== 'Bearer smoke-proxy-key') return new Response('unauthorized', { status: 401 })
      return Response.json({ models: [
        { slug: 'gpt-test', display_name: 'GPT Test', context_window: 128000 },
        { slug: 'claude-test', display_name: 'Claude Test', owned_by: 'anthropic', context_window: 200000 },
        { slug: 'image-test', output_modalities: ['image'] },
      ] })
    } })
    await run.writeOverlay(undefined, { cliProxyApi: true })
    try {
      const transcript = await run.terminal('cliproxyapi', [], async tty => {
        tty.send('/login\r', 'choose a login target')
        await tty.expect('Choose a sign-in target')
        await tty.expect('CLIProxyAPI', 'Not set', 'URL + API key')
        tty.send('\r', 'choose CLIProxyAPI')
        await tty.expect('Sign in · CLIProxyAPI', '1/2', 'Base URL')
        tty.send(`${server.url.toString()}\r`, 'set the proxy URL')
        await tty.expect('2/2', 'API key')
        tty.send('wrong-proxy-key\r', 'try a key the proxy rejects')
        await tty.expect('✗ The proxy rejected this API key (HTTP 401)', 'Enter tries again')
        tty.send('smoke-proxy-key\r', 'store the proxy key')
        await tty.expect('CLIProxyAPI: 2 models ready; choose one with /model')
        tty.send('/model cliproxyapi/gpt-test\r', 'select the discovered model')
        await tty.expect('Model set for the next turn: cliproxyapi/gpt-test')
        tty.send('Say PROXY_OK\r', 'run one turn through the configured proxy')
        await tty.expect('  PROXY_OK')
        await tty.expect('✓ Completed')
        const claude = tty.mark()
        tty.send('/model cliproxyapi/claude-test\r', 'select the Claude model')
        await tty.expect('Model set for the next turn: cliproxyapi/claude-test', claude)
        tty.send('Say CLAUDE_OK\r', 'run one turn over Anthropic Messages')
        await tty.expect('  CLAUDE_OK')
      })
      assert(!transcript.includes('smoke-proxy-key') && !transcript.includes('wrong-proxy-key'), 'CLIProxyAPI secret appeared on the terminal')
      assert(!transcript.includes('Could not parse message into JSON'), 'empty SSE framing leaked an SDK parse error')
    } finally {
      await run.writeOverlay()
      server.stop(true)
    }
    assert(same(requests, [
      // The rejected key is checked, refused in the panel, and never stored.
      { path: '/v1/models', query: '?client_version=pi', authorization: 'Bearer wrong-proxy-key' },
      { path: '/v1/models', query: '?client_version=pi', authorization: 'Bearer smoke-proxy-key' },
      { path: '/v1/responses', query: '', authorization: 'Bearer smoke-proxy-key', model: 'gpt-test' },
      { path: '/v1/responses', query: '', authorization: 'Bearer smoke-proxy-key', model: 'gpt-test' },
      // Claude goes to the proxy root over Messages, with a cache breakpoint on the system prompt.
      { path: '/v1/messages', query: '?beta=true', authorization: 'smoke-proxy-key', model: 'claude-test', cached: true },
    ]), `CLIProxyAPI did not validate the catalog and send each model to the entered proxy: ${JSON.stringify(requests)}`)
    assert(!(await Bun.file(join(run.home, 'settings.yaml')).text()).includes('smoke-proxy-key'),
      'CLIProxyAPI secret was saved in model settings')
    const log = await events(await run.created(before, 'proxy retry'))
    // One prompt per model: the retry must not add a third.
    assert(log.filter(event => event.type === 'user/message' && event.data.source.kind === 'user').length === 2,
      'proxy retry duplicated user input')
    const retries = log.filter(event => event.type === 'llm/retry')
    assert(retries.length === 1 && retries[0]!.data.failure.code === 'TRANSPORT',
      'missing SSE completion did not produce exactly one transport retry')
    assert(log.filter(event => event.type === 'assistant/message').length === 2, 'proxy retry persisted an extra assistant message')
    assert(log.some(event => event.type === 'turn/end' && event.data.reason.kind === 'completed'),
      'proxy recovery did not complete the turn')
  })

scenario('cliproxyapi-upgrade', 'a CLIProxyAPI route an earlier release wrote is upgraded at launch, announced, and served',
  { replayOnly: true },
  async run => {
    const requests: Array<{ path: string, affinity: string | null, session: string | null }> = []
    const server = Bun.serve({ hostname: '127.0.0.1', port: 0, async fetch(request) {
      const url = new URL(request.url)
      requests.push({ path: url.pathname, affinity: request.headers.get('x-session-affinity'),
        session: request.headers.get('x-deepseek-harness-session-id') })
      // The launch's model refresh, answered with the list the route already holds.
      if (request.method === 'GET' && url.pathname === '/v1/models') {
        return Response.json({ data: [{ id: 'gpt-test', name: 'GPT Test' }, { id: 'claude-test', name: 'Claude Test' }] })
      }
      const body = await request.json() as { model: string }
      const events = [
        ['message_start', { type: 'message_start', message: { id: 'msg_upgrade', type: 'message', role: 'assistant', model: body.model,
          content: [], stop_reason: null, usage: { input_tokens: 4, output_tokens: 0 } } }],
        ['content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }],
        ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'UPGRADED_OK' } }],
        ['content_block_stop', { type: 'content_block_stop', index: 0 }],
        ['message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 2 } }],
        ['message_stop', { type: 'message_stop' }],
      ] as const
      return new Response(events.map(([name, data]) => `event: ${name}\ndata: ${JSON.stringify(data)}\n\n`).join(''),
        { headers: { 'content-type': 'text/event-stream' } })
    } })
    // The route `/login cliproxyapi` wrote before 0.1.7: Claude on the route's
    // Responses protocol and no multi-account defaults. JSON is valid YAML.
    const settingsPath = join(run.home, 'settings.yaml')
    const saved = existsSync(settingsPath) ? await Bun.file(settingsPath).text() : undefined
    await Bun.write(settingsPath, JSON.stringify({ 'llm-pi-ai': { providers: { cliproxyapi: {
      displayName: 'CLIProxyAPI', apiKeyEnv: 'CLIPROXYAPI_API_KEY', api: 'openai-responses',
      baseURL: `${server.url.origin}/v1`,
      models: [{ id: 'gpt-test', name: 'GPT Test' }, { id: 'claude-test', name: 'Claude Test' }],
    } } } }))
    run.env['CLIPROXYAPI_API_KEY'] = 'smoke-upgrade-key'
    await run.writeOverlay(undefined, { cliProxyApi: true })
    let upgraded: string
    try {
      await run.terminal('cliproxyapi-upgrade', [], async tty => {
        await tty.expect('Updated the CLIProxyAPI route for this version')
        tty.send('/model cliproxyapi/claude-test\r', 'select the upgraded Claude model')
        await tty.expect('Model set for the next turn: cliproxyapi/claude-test')
        tty.send('Say UPGRADED_OK\r', 'run one turn over the upgraded route')
        await tty.expect('  UPGRADED_OK')
      })
      upgraded = await Bun.file(settingsPath).text()
    } finally {
      delete run.env['CLIPROXYAPI_API_KEY']
      await run.writeOverlay()
      if (saved === undefined) rmSync(settingsPath, { force: true })
      else await Bun.write(settingsPath, saved)
      server.stop(true)
    }
    // The upgrade was persisted, so the next launch starts current and says nothing.
    for (const fragment of ['anthropic-messages', 'maxDelayMs: 60000', 'sendSessionAffinityHeaders: true']) {
      assert(upgraded.includes(fragment), `upgraded settings lack ${fragment}:\n${upgraded}`)
    }
    // Claude moved to Anthropic Messages at the proxy root, carrying the session on both headers.
    const turns = requests.filter(request => request.path !== '/v1/models')
    assert(turns.length === 1 && turns[0]!.path === '/v1/messages'
      && turns[0]!.session !== null && turns[0]!.affinity === turns[0]!.session,
    `the upgraded Claude turn did not reach Messages with its session headers: ${JSON.stringify(requests)}`)
    // The launch read the proxy's list once, after the upgrade, and found the route current.
    assert(requests.filter(request => request.path === '/v1/models').length === 1, `the launch did not refresh the model list once: ${JSON.stringify(requests)}`)
  })

scenario('agents', 'the built TUI exposes the Harness subagent catalog through /agents',
  { replayOnly: true },
  async run => {
    const before = await run.logs()
    await run.terminal('agents', [], async tty => {
      tty.send('/help\r', 'discover the subagent command')
      await tty.search(/\/agents\b.* {2}List delegated agents/u)
      const start = tty.mark()
      tty.send('/agents\r', 'list this session’s children')
      await tty.expect('No subagents in this session', start)
    })
    const path = await run.created(before, 'agents')
    const log = await events(path)
    assert(!log.some(event => event.type === 'user/message'), '/agents entered model input')
  })

scenario('presets', 'minimal and cordis start, answer the recorded turn, and read what their presets mount; minimal gets only '
  + 'its shell, and its status line and sheet keys work without the task-list unit no preset registered',
  { replayOnly: true },
  async run => {
    for (const preset of ['minimal', 'cordis'] as const) {
      const before = await run.logs()
      await run.terminal(`preset-${preset}`, ['--preset', preset], async tty => {
        tty.send(`${run.prompt}\r`, 'submit the recorded prompt')
        await tty.wait("the shell result and the model's DONE line",
                       text => text.includes(SCREEN.toolResult) && DONE_LINE.test(text))
        await tty.follows(SCREEN.idle, SCREEN.toolResult)
        if (preset !== 'minimal') return
        // No sheet has anything to show, so each key leaves the composer in place.
        const keys = tty.mark()
        tty.send('\x14', 'Ctrl+T')
        tty.send('\x07', 'Ctrl+G')
        tty.send('\x0f', 'Ctrl+O')
        tty.send('still here', 'type after the sheet keys')
        await tty.expect(`${SCREEN.prompt}still here${SCREEN.caret}`, keys)
        tty.refuse('a sheet opened under minimal', tty.text.slice(keys).includes('Esc closes'))
        const erased = tty.mark()
        tty.send('\x7f'.repeat('still here'.length), 'erase the draft before quitting')
        await tty.expect(`${SCREEN.prompt}${SCREEN.caret}`, erased)
      })
      const log = await events(await run.created(before, preset))
      assert(log[0].agentPreset === preset, `the session did not mount the ${preset} preset`)
      const tools = log.find(e => e.type === 'request/header')?.data.header.tools.map((tool: any) => tool.name) ?? []
      const context = log.filter(e => e.type === 'user/message' && e.data.source.kind !== 'user')
      if (preset === 'minimal') {
        assert(same(tools, ['bash']), `minimal offered more than its shell: ${tools.join(', ')}`)
        assert(context.length === 0, `minimal received injected context: ${JSON.stringify(context.map(e => e.data.source))}`)
      } else {
        assert(['bash', 'cordis_inspect_list', 'cordis_inspect_query', 'plugin_manager'].every(name => tools.includes(name)),
               `cordis is missing its own tools: ${tools.join(', ')}`)
      }
      const results = log.filter(e => e.type === 'tool/result' && e.surfaceOp === 'append')
      assert(results.length === 1 && JSON.stringify(results[0].data.message.content).includes(SCREEN.toolResult),
             `${preset} did not run the recorded shell call: ${JSON.stringify(results.map(e => e.data.message.content)).slice(0, 500)}`)
    }
  })

scenario('settings', 'a /settings choice is saved to the settings file, plugin settings are found by search, and a saved fullscreen screen opens at the next launch',
  { replayOnly: true },
  async run => {
    const copy = dictionaries.en
    // Every scenario shares this home. The saved screen must not outlive this one.
    const settingsPath = join(run.home, 'settings.yaml')
    const original = existsSync(settingsPath) ? await Bun.file(settingsPath).text() : undefined
    try {
      const before = await run.logs()
      await run.terminal('settings', [], async tty => {
        const start = tty.mark()
        tty.send('/settings\r', 'open the settings panel')
        // Sections, each read by its values; the running composition fills the plugin ones.
        await tty.expect(copy.settingsSession, 'deepseek-official/deepseek-v4-flash', copy.settingsTerminal, copy.settingsScreenInline,
          copy.settingsAgent, copy.settingsAdvanced, start)
        await tty.wait('the session section to be selected', text => picked(copy.settingsSession).test(text.slice(start)))
        // The top page searches the settings inside every section.
        const search = tty.mark()
        tty.send('parallel', 'search for a plugin setting')
        await tty.wait('the agent loop setting found from the top', text =>
          picked(`${copy.settingsAgent} › ${copy.settingsParallelTools}`).test(text.slice(search)))
        const cleared = tty.mark()
        tty.send('\x7f'.repeat('parallel'.length), 'clear the search')
        await tty.wait('the session section to be selected again', text => picked(copy.settingsSession).test(text.slice(cleared)))
        const terminal = tty.mark()
        tty.send('\x1b[B', 'point at the terminal section')
        await tty.wait('the terminal section to be selected', text => picked(copy.settingsTerminal).test(text.slice(terminal)))
        tty.send('\r', 'open the terminal section')
        await tty.wait('the screen row to be selected', text => picked(copy.settingsScreen).test(text.slice(terminal)))
        const values = tty.mark()
        tty.send('\r', 'open the screen values')
        await tty.wait('the inline value to be selected', text => picked(copy.settingsScreenInline).test(text.slice(values)))
        await tty.expect(copy.settingsDefault, values)
        const down = tty.mark()
        tty.send('\x1b[B', 'point at fullscreen')
        await tty.wait('the fullscreen value to be selected', text => picked(copy.settingsScreenFullscreen).test(text.slice(down)))
        const chosen = tty.mark()
        tty.send('\r', 'choose fullscreen')
        await tty.expect(copy.settingsNextLaunch, chosen)
        const up = tty.mark()
        tty.send('\x1b', 'return to the sections')
        await tty.wait('the terminal section under the pointer again', text => picked(copy.settingsTerminal).test(text.slice(up)))
        const closed = tty.mark()
        tty.send('\x1b', 'close the panel')
        await tty.expect(`${SCREEN.prompt}${SCREEN.caret}`, closed)
        await tty.expect(`${copy.settingsSaved} `, closed)
        // The running process keeps its inline screen.
        tty.check('the running process stayed inline', !tty.raw.includes('\u001b[?1049h'))
      })
      const saved = await Bun.file(join(run.home, 'settings.yaml')).text()
      // Anywhere in the section: an earlier scenario's /model pick shares this home's `tui` section.
      assert(/^tui:\n(?:[ \t].*\n)*?[ \t]+screen: fullscreen$/mu.test(saved), `the screen choice was not saved:\n${saved}`)
      const log = await events(await run.created(before, 'settings'))
      assert(!log.some(event => event.type === 'user/message'), '/settings entered model input')
      await run.terminal('settings-fullscreen', [], async tty => {
        await tty.wait('the saved fullscreen screen at launch', text => text.includes('BAKE') && tty.raw.includes('\u001b[?1049h'))
      })
      // A flag still wins over the saved screen.
      await run.terminal('settings-flag', ['--screen', 'inline'], async tty => {
        await tty.expect('BAKE')
        tty.check('--screen inline overrode the saved screen', !tty.raw.includes('\u001b[?1049h'))
      })
    } finally {
      if (original === undefined) rmSync(settingsPath, { force: true })
      else await Bun.write(settingsPath, original)
    }
  })

scenario('settings-agent', 'Tab moves between /settings sections, subagent models are chosen from the catalog before the choice turns on and again under Advanced, and a list without a picker opens in $VISUAL',
  { replayOnly: true },
  async run => {
    const copy = dictionaries.en
    const settingsPath = join(run.home, 'settings.yaml')
    const original = existsSync(settingsPath) ? await Bun.file(settingsPath).text() : undefined
    // An editor that owns the terminal only if it reads the line typed into it.
    const editorDir = mkdtempSync(join(tmpdir(), 'bake-pty-editor-'))
    const editor = join(editorDir, 'editor.sh')
    await Bun.write(editor, '#!/bin/sh\nprintf "EDITOR-READY %s\\n" "$(basename "$1")"\nIFS= read -r line\nprintf "%s\\n" "$line" > "$1"\n')
    chmodSync(editor, 0o755)
    const previous = { VISUAL: run.env.VISUAL, EDITOR: run.env.EDITOR }
    run.env.VISUAL = editor
    delete run.env.EDITOR
    try {
      await run.terminal('settings-agent', [], async tty => {
        const start = tty.mark()
        tty.send('/settings\r', 'open the settings panel')
        await tty.expect(copy.settingsAgent, copy.pickerTabsHelp, start)
        // Tab opens the first section, and each Tab the next one.
        let at = tty.mark()
        tty.send('\t', 'open the first section')
        await tty.expect(`${copy.settingsTitle} › ${copy.settingsSession}`, at)
        at = tty.mark()
        tty.send('\t', 'move to the terminal section')
        await tty.expect(`${copy.settingsTitle} › ${copy.settingsTerminal}`, at)
        at = tty.mark()
        tty.send('\t', 'move to the agent section')
        await tty.expect(`${copy.settingsTitle} › ${copy.settingsAgent}`, copy.settingsSubagentAllowed, copy.settingsSubagentAllowedNone, at)
        at = tty.mark()
        tty.send('pick a model', 'filter to the model switch')
        await tty.wait('the model switch to be selected', text => picked(copy.settingsSubagentModels).test(text.slice(at)))
        at = tty.mark()
        tty.send('\r', 'switch it on')
        // Switched on from an empty list, it asks for a model from the catalog first.
        await tty.expect(copy.settingsSubagentAllowedFirst, 'deepseek-official/deepseek-v4-flash', at)
        at = tty.mark()
        tty.send('v4-flash', 'filter to one model')
        await tty.wait('the flash model to be selected', text => picked('deepseek-official/deepseek-v4-flash').test(text.slice(at)))
        at = tty.mark()
        tty.send('\r', 'allow it')
        await tty.expect(copy.settingsSubagentAllowedOn, at)
        at = tty.mark()
        tty.send('\x1b', 'return to the agent section')
        await tty.expect(`${copy.settingsTitle} › ${copy.settingsAgent}`, at)
        await tty.wait('the agent section to show the switch on and the model', text =>
          text.slice(at).includes(copy.settingsOn) && text.slice(at).includes('deepseek-official/deepseek-v4-flash'))
        at = tty.mark()
        tty.send('\x1b', 'return to the top')
        await tty.expect(copy.settingsAdvanced, at)
        // The model list found under Advanced opens the same catalog picker, never the editor.
        at = tty.mark()
        tty.send('advanced allowedModels', 'search for the model list under Advanced')
        await tty.wait('the model list found from the top', text =>
          picked(`${copy.settingsAdvanced} › subagent-model-selection › allowedModels`).test(text.slice(at)))
        at = tty.mark()
        tty.send('\r', 'open the list')
        await tty.expect(`${copy.settingsAgent} › ${copy.settingsSubagentAllowed}`, 'deepseek-official/tui-picked-model', at)
        at = tty.mark()
        tty.send('picked', 'filter to the other model')
        await tty.wait('the other model to be selected', text => picked('deepseek-official/tui-picked-model').test(text.slice(at)))
        at = tty.mark()
        tty.send('\r', 'allow it')
        await tty.expect(copy.settingsSubagentAllowedOn, at)
        at = tty.mark()
        tty.send('v4-flash', 'filter to the flash model')
        await tty.wait('the flash model to be selected', text => picked('deepseek-official/deepseek-v4-flash').test(text.slice(at)))
        at = tty.mark()
        tty.send('\r', 'remove it')
        await tty.expect(copy.settingsSubagentAllowed, at)
        at = tty.mark()
        tty.send('\x1b', 'return to the top')
        await tty.expect(copy.settingsAdvanced, at)
        // A list with no picker of its own opens in the editor, which reads the line typed into it.
        at = tty.mark()
        tty.send('advanced router.hints', 'search for the router hints under Advanced')
        await tty.wait('the hint list found from the top', text =>
          picked(`${copy.settingsAdvanced} › subagent-model-selection › router.hints`).test(text.slice(at)))
        at = tty.mark()
        tty.send('\r', 'open the list in the editor')
        await tty.expect('EDITOR-READY subagent-model-selection.router.hints.json', at)
        at = tty.mark()
        // A line feed ends the line whether or not the PTY maps carriage returns in cooked mode.
        tty.send('[{"provider": "deepseek-official", "model": "deepseek-v4-pro", "quality": "high"}]\n', 'type the list into the editor')
        // The panel is drawn again once the editor exits.
        await tty.expect(copy.pickerTabsHelp, at)
        at = tty.mark()
        tty.send('\x1b', 'close the panel')
        await tty.expect(`${SCREEN.prompt}${SCREEN.caret}`, at)
        await tty.expect(`${copy.settingsSaved} `, at)
      })
      const saved = await Bun.file(settingsPath).text()
      assert(saved.includes('enabled: true') && saved.includes('model: tui-picked-model') && !saved.includes('model: deepseek-v4-flash')
        && saved.includes('model: deepseek-v4-pro') && saved.includes('quality: high'),
      `the subagent models were not saved:\n${saved}`)
    } finally {
      for (const [name, value] of Object.entries(previous)) {
        if (value === undefined) delete run.env[name]
        else run.env[name] = value
      }
      rmSync(editorDir, { recursive: true, force: true })
      if (original === undefined) rmSync(settingsPath, { force: true })
      else await Bun.write(settingsPath, original)
    }
  })

scenario('inspect-agent', 'show recorded workflow progress, inspect its child, and preserve the parent draft',
  { requires: ['fresh'], replayOnly: true },
  async run => {
    const source = await events(run.state.log)
    const parentId = 'tui-inspection-parent'
    const parentPath = join(dirname(dirname(run.state.log)), parentId, basename(run.state.log))
    mkdirSync(dirname(parentPath), { recursive: true })
    const childId = 'tui-inspected-child'
    const progress = [
      { type: 'tool-workflow/run-start', data: { runId: 'review', name: 'terminal-review' } },
      { type: 'tool-workflow/agent-start', data: { runId: 'review', seq: 1, childId, label: 'Review terminal output', phase: 'Inspect' } },
      { type: 'tool-workflow/agent-end', data: { runId: 'review', seq: 1, outcome: 'completed' } },
      { type: 'tool-workflow/run-end', data: { runId: 'review', stopReason: 'completed' } },
      { type: 'tool-workflow/run-start', data: { runId: 'interrupted', name: 'interrupted-audit' } },
    ].map((event, index) => ({ ...event, seq: source.length - 1 + index, time: Date.now() }))
    const parentRecorded = [{ ...source[0], id: parentId }, ...source.slice(1), ...progress]
      .map(event => JSON.stringify(event)).join('\n') + '\n'
    await Bun.write(parentPath, parentRecorded)
    const path = join(dirname(dirname(run.state.log)), childId, basename(run.state.log))
    const child = [{ ...source[0], id: childId, parentSession: parentId, origin: 'subagent' },
      ...source.slice(1), { type: 'subagent/descriptor', seq: source.length - 1, time: Date.now(),
        data: { version: 3, mode: 'continuable', provider: 'spawn', label: 'Review terminal output' } },
      { type: 'permission/preset', seq: source.length, time: Date.now(), data: { preset: 'read-only' } },
      { type: 'sandbox/mode', seq: source.length + 1, time: Date.now(), data: { mode: 'read-only' } }]
    mkdirSync(dirname(path), { recursive: true })
    const recorded = child.map(event => JSON.stringify(event)).join('\n') + '\n'
    await Bun.write(path, recorded)
    await run.terminal('inspect-agent', ['--resume', parentId], async tty => {
      const viewport = async (): Promise<string> => {
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        try {
          await new Promise<void>(resolve => screen.write(tty.raw, resolve))
          return Array.from({ length: 40 }, (_, row) =>
            screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.translateToString(true) ?? '').join('\n')
        } finally { screen.dispose() }
      }
      await tty.expect('↳ Subagents 1')
      tty.send('\x1b[B', 'select subagents from the status line')
      await tty.expect('> Subagents 1')
      tty.send('\r', 'open the subagent sheet from the status line')
      await tty.expect('Select a child to view its session', 'Review terminal output')
      await tty.expect('Workflow terminal-review · Completed · 1/1 done', 'Workflow interrupted-audit · Unfinished', 'Continuable · Inspect')
      tty.send('\x1b', 'close the sheet')
      // A lone Escape decodes only once no sequence follows it, and the prompt
      // is drawn under an open sheet too, so typing waits for the sheet to leave
      // the viewport; otherwise Escape and the draft arrive as one Meta key.
      await tty.wait('the sheet to close over the empty prompt', async () => {
        const visible = await viewport()
        return visible.includes(`> ${SCREEN.caret}Ask anything`) && !visible.includes('Esc closes')
      })
      tty.send('Keep this parent draft', 'write a draft before inspecting a child')
      await tty.expect(`> Keep this parent draft${SCREEN.caret}`)
      let start = tty.mark()
      tty.send('\x07', 'Ctrl+G opens the subagent sheet')
      await tty.expect('Select a child to view its session', 'Review terminal output', start)
      tty.send('\r', 'open the selected child session')
      await tty.expect(`Parent: ${parentId}`, 'Read-only', SCREEN.toolResult, 'Access read-only', 'ctx ~', start)
      tty.send('must not reach the model\r', 'inspection does not accept prompts')
      start = tty.mark()
      tty.send('\x1b', 'return to the running parent without cancelling it')
      await tty.expect(`> Keep this parent draft${SCREEN.caret}`, 'Access workspace-write', start)
      tty.send('!', 'continue editing the parent draft')
      await tty.expect(`> Keep this parent draft!${SCREEN.caret}`, start)
    })
    assert(await Bun.file(path).text() === recorded, 'inspection modified the saved child')
    assert((await Bun.file(parentPath).text()).startsWith(parentRecorded), 'workflow display rewrote recorded history')
    const parent = await events(parentPath)
    assert(parent.filter(event => event.type === 'request/header').length === run.state.headers.length,
      'child inspection requested a model response')
    assert(!parent.some(event => event.type === 'user/message'
      && JSON.stringify(event).includes('must not reach')), 'inspection submitted input to the parent')
  })

scenario('goal-compact', 'the built TUI exposes goal and compact commands and shows their results',
  { replayOnly: true },
  async run => {
    await run.writeOverlay(undefined, { goalMaxRounds: 1 })
    const before = await run.logs()
    try {
      await run.terminal('goal-compact', [], async tty => {
        tty.send('/help\r', 'list composed commands')
        await tty.search(/\/goal\b.* {2}Set or view the goal for a long-running task/u)
        await tty.search(/\/compact\b.* {2}Compact older conversation history/u)
        tty.send('/goal\r', 'inspect the current goal')
        await tty.expect('No goal is currently set.')
        for (const [index, character] of [...'/compact'].entries()) {
          const after = tty.mark()
          tty.send(character, `type /compact key ${index + 1}`)
          await tty.expect(`${SCREEN.prompt}${'/compact'.slice(0, index + 1)}${SCREEN.caret}`, after)
        }
        const edit = tty.mark()
        tty.send('\x7f', 'leave /compac as a completion prefix')
        await tty.expect(`${SCREEN.prompt}/compac${SCREEN.caret}`, edit)
        tty.send('\r', 'run the selected /compact completion')
        await tty.expect('No compactable history yet.')
        const menuStart = tty.mark()
        tty.send('/', 'open the slash menu with only a slash in the draft')
        await tty.expect(`${SCREEN.prompt}/${SCREEN.caret}`, menuStart)
        let compactSelected = false
        for (let step = 0; step < 40; step++) {
          const beforeSelection = tty.mark()
          tty.send('\x1b[B', 'move through the slash menu')
          const frame = await tty.wait('a slash menu selection to render', text =>
            text.slice(beforeSelection).includes(`${MARKER.selected} /`))
          if (picked('/compact').test(frame.slice(beforeSelection))) { compactSelected = true; break }
        }
        tty.check('/compact can be selected from the slash menu', compactSelected)
        const selection = tty.mark()
        tty.send('\r', 'run /compact from the slash menu')
        await tty.expect('No compactable history yet.', selection)
        const goalStart = tty.mark()
        const objective = `${'Complete this test task and inspect the terminal goal. '.repeat(5)}FULL_GOAL_END`
        tty.send(`/goal ${objective}\r`, 'create a long goal')
        await tty.expect('Goal created', goalStart)
        // The goal shares the processing header once `/goal` arms it: its
        // glyph, its name, and its round count, `● Goal 0/1`.
        await tty.search(/\u25cf Goal \d+\/\d+/u, goalStart)
        // The raw stream can end inside a frame: a macOS PTY hands one render
        // over in small reads. The header is checked once the viewport draws it.
        const viewport = async (): Promise<readonly string[]> => {
          const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
          try {
            await new Promise<void>(resolve => screen.write(tty.raw, resolve))
            return Array.from({ length: 40 }, (_, row) =>
              screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.translateToString(true) ?? '')
          } finally { screen.dispose() }
        }
        const headed = (lines: readonly string[]): number => {
          const row = lines.findIndex(line => /[●○✗✓] Goal/u.test(line))
          return row >= 0 && /^─+$/u.test(lines[row + 1]?.trim() ?? '') ? row : -1
        }
        let lines: readonly string[] = []
        await tty.wait('the goal result, and the goal on the processing header above the composer rule', async () => {
          lines = await viewport()
          return lines.join('\n').includes('Goal created') && headed(lines) >= 0
        })
        assert(!lines[headed(lines)]!.includes('FULL_GOAL_END'), 'the long goal was not truncated in the header')
        // Up walks back through the commands above before it reaches the goal.
        // Repeated entries draw no new frame, and further Ups keep the goal
        // selected, so the walk is sent at once. The one-round goal may
        // finish its round meanwhile, so any phase counts.
        const walkUp = tty.mark()
        tty.send('\x1b[A'.repeat(12), 'walk up through history to the goal')
        await tty.wait('Up past the oldest history entry to select the goal', text => /> [●○✗✓] Goal /u.test(text.slice(walkUp)))
        const opened = tty.mark()
        tty.send('\r', 'open the full goal')
        await tty.expect('FULL_GOAL_END', opened)
        await tty.expect('↑↓ scroll · Esc closes', opened)
        tty.send('\x1b', 'close the goal view')
        // A lone Escape is only decoded once no sequence follows it, so the
        // next key waits for the view to leave the viewport. The raw stream
        // cannot say so: a spinner beat may redraw the view after the key.
        await tty.wait('the goal view to close over the restored empty draft', async () => {
          const visible = (await viewport()).join('\n')
          // The empty draft, not the oldest command the walk passed.
          return visible.includes(`${SCREEN.prompt}${SCREEN.caret}`) && !visible.includes('Esc closes')
        })
        await tty.wait('the goal driver to run its first round', text => DONE_LINE.test(text.slice(goalStart)))
      })
    } finally { await run.writeOverlay() }
    const log = await events(await run.created(before, 'persisted'))
    assert(log.some(event => event.type === 'goal/change' && event.data.operation === 'create'), 'goal creation did not persist')
    assert(log.some(event => event.type === 'user/message' && event.data.source.kind === 'goal'), 'goal driver did not submit a round')
    const runs = log.filter(event => event.type === 'command/run')
    const done = new Map(log.filter(event => event.type === 'command/done')
      .map(event => [event.data.commandId, event.data] as const))
    for (const name of ['goal', 'compact']) {
      const matches = runs.filter(event => event.data.name === name)
      assert(matches.length > 0, `/${name} did not execute`)
      assert(matches.every(event => done.get(event.data.commandId)?.kind === 'success'), `/${name} did not settle successfully`)
    }
    assert(runs.filter(event => event.data.name === 'compact').length === 2,
      'the prefix and selected-menu /compact attempts did not both execute')
    assert(!log.some(event => event.type === 'user/message' && event.data.source.kind === 'user'
      && event.data.content.some((block: any) => block.type === 'text' && block.text === '/')),
    'selecting /compact sent the slash prefix to the model')
  })

scenario('compact-history', 'manual compaction works after completed replayed turns, and a prompt sent meanwhile queues and runs after it',
  { replayOnly: true },
  async run => {
    const before = await run.logs()
    const seed = await run.terminal('compact-seed', [], async tty => {
      tty.send(`${run.prompt}\r`, 'record the first completed turn')
      await tty.wait('the first turn to finish', text => DONE_LINE.test(text))
      await tty.follows(SCREEN.idle, 'DONE')
    })
    const id = seed.match(/Session: (session-[a-f0-9-]+)/)?.[1]
    assert(id !== undefined, 'compaction seed session identity was not shown')
    const path = await run.created(before, 'compaction seed')
    // This terminal's model calls: the replayed turn's two, the summary, and
    // the queued prompt's own turn. The summary streams in many small deltas,
    // so it is still running when the prompt is typed.
    const reply = (text: string, deltas = 1) => ({ kind: 'chunks', chunks: [
      { type: 'block-start', index: 0, blockType: 'text' },
      ...Array.from({ length: deltas }, (_, index) => ({ type: 'text-delta', index: 0,
        text: text.slice(Math.floor(index * text.length / deltas), Math.floor((index + 1) * text.length / deltas)) })),
      { type: 'block-end', index: 0, block: { type: 'text', text } },
      { type: 'finish', reason: { kind: 'stop' } },
    ] })
    const replay = await import(join(ROOT, 'packages/test-support/llm-replay/lib/index.js')) as {
      loadReplayScript: (config: { file: string }) => unknown[]
    }
    const override = join(run.root, 'compact-summary.json')
    await Bun.write(override, JSON.stringify([
      ...replay.loadReplayScript({ file: FIXTURE }),
      reply('The previous work completed successfully.', 30),
      reply(QUEUED_REPLY),
    ]))
    await run.writeOverlay(override, { paceMs: 120 })
    const queued = 'run this once compaction is done'
    try {
      await run.terminal('compact-history', ['--resume', id], async tty => {
        const start = tty.mark()
        tty.send(`${run.prompt}\r`, 'add another completed turn')
        await tty.wait('the replayed turn to finish', text => DONE_LINE.test(text.slice(start)))
        await tty.follows(SCREEN.idle, 'DONE')
        const compacting = tty.mark()
        tty.send('/compact\r', 'compact completed history')
        await tty.expect('Compacting history…  summarizing', dictionaries.en.compactWait, compacting)
        tty.send(`${queued}\r`, 'queue a prompt while compaction owns the session')
        // The pending panel reads the agent's inbox: the prompt waits there.
        await tty.expect(dictionaries.en.pending, `${dictionaries.en.nextTurn}: ${queued}`, compacting)
        // The header works the laminating dough, not the kneading. `NO_COLOR`
        // turns motion off, so it holds the dough's pressed block.
        await tty.expect(`${FOLD_REST} ${dictionaries.en.compacting}…`, compacting)
        tty.refuse('the compaction settled before the prompt queued', /Compacted \d+ history items/.test(tty.text.slice(compacting)))
        await tty.search(/Compacted \d+ history items/, compacting)
        await tty.wait('the queued prompt to run as its own turn', text => QUEUED_LINE.test(text.slice(compacting)))
        await tty.follows(SCREEN.idle, QUEUED_REPLY)
        // The result prints in the same write as the frame redrawn under it,
        // and a PTY read can end part-way through that write, so the viewport
        // is judged once the frame has arrived, not the moment the text does.
        // `wait` drains the terminal between checks; the assertions below say
        // which part never arrived.
        let visible = ''
        try {
          await tty.wait('the compaction result, the queued turn, and an empty pending panel in the viewport', async () => {
            const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
            try {
              await new Promise<void>(resolve => screen.write(tty.raw, resolve))
              visible = Array.from({ length: 40 }, (_, row) =>
                screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.translateToString(true) ?? '').join('\n')
            } finally { screen.dispose() }
            return visible.includes('Compacted') && visible.includes(QUEUED_REPLY) && !visible.includes(dictionaries.en.pending)
          }, 5)
        } catch {
          // Reported by the assertions, with what the viewport last showed.
        }
        assert(visible.includes('Compacted'), 'compaction result is absent from the current terminal viewport')
        assert(visible.includes(QUEUED_REPLY), 'the queued prompt\'s answer is absent from the current terminal viewport')
        assert(!visible.includes(dictionaries.en.pending), 'the pending panel still holds the prompt after it ran')
      })
    } finally { await run.writeOverlay() }
    const log = await events(path)
    const at = (predicate: (event: any) => boolean): number => log.findIndex(predicate)
    const texts = (event: any): string[] => event.data.content.flatMap((block: any) => block.type === 'text' ? [block.text] : [])
    const summary = at(event => event.type === 'compaction/summary')
    assert(summary >= 0, '/compact did not commit a summary')
    const closed = at(event => event.type === 'compaction/end' && event.data.error === undefined)
    assert(closed > summary, 'the compaction did not close after its summary')
    const inbox = at(event => event.type === 'agent/inbox/spliced' && (event.data.inserted ?? []).some((message: any) => texts({ data: message }).includes(queued)))
    assert(inbox >= 0 && inbox < closed, 'the prompt was not queued in the inbox while the compaction ran')
    const sent = log.flatMap((event, index) => event.type === 'user/message' && texts(event).includes(queued) ? [index] : [])
    assert(sent.length === 1, `the queued prompt entered model history ${sent.length} times`)
    const opened = log.findLastIndex((event, index) => index < sent[0]! && event.type === 'turn/start')
    assert(opened > closed, 'the queued prompt did not open its own turn after the compaction')
    assert(log.slice(sent[0]!).some(event => event.type === 'assistant/message' && JSON.stringify(event.data.message.content).includes(QUEUED_REPLY)),
      'the queued prompt was not answered')
  })

scenario('thai', 'Thai and Lao grapheme editing, cell widths, resize, and exact multilingual submission', { replayOnly: true },
  async run => {
    const before = await run.logs()
    const override = join(run.root, 'thai-replay.json')
    const reply = 'รับข้อความแล้ว ສະບາຍດີ'
    const suffix = ' ກຳ ສະບາຍດີ ភាសាខ្មែរ မြန်မာ e\u0301 👩🏽‍💻'
    const prompt = `${'น้ำ'.repeat(24)}${suffix}`
    await Bun.write(override, JSON.stringify([{ kind: 'chunks', chunks: [
      { type: 'block-start', index: 0, blockType: 'text' },
      { type: 'text-delta', index: 0, text: reply },
      { type: 'block-end', index: 0, block: { type: 'text', text: reply } },
      { type: 'finish', reason: { kind: 'stop' } },
    ] }]))
    await run.writeOverlay(override)
    try {
      await run.terminal('thai', [], async tty => {
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        let consumed = 0
        const capture = async (): Promise<string[]> => {
          const raw = tty.raw
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
          return Array.from({ length: screen.rows }, (_, row) =>
            screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.translateToString(true) ?? '')
        }
        try {
          for (const [character, typed] of [['น', 'น'], ['้', 'น้'], ['ำ', 'น้ำ']] as const) {
            tty.send(character, 'type a Thai consonant, tone, and spacing vowel separately')
            await tty.wait('the composed Thai draft', async () => (await capture()).some(line => line.includes(`> ${typed}${SCREEN.caret}`)))
          }
          tty.send('\x1b[D', 'Left over the complete Thai grapheme')
          await tty.wait('caret before the Thai grapheme', async () => (await capture()).some(line => line.includes(`> ${SCREEN.caret}น้ำ`)))
          tty.send('\x1b[3~', 'Delete the complete grapheme')
          await tty.wait('an empty draft after forward deletion', async () => (await capture()).some(line => line.includes(`> ${SCREEN.caret}${SCREEN.idle}`)))
          let typed = ''
          for (const character of 'น้ำ'.repeat(24)) {
            typed += character
            tty.send(character, 'type the next Thai character')
            await tty.wait('one copy of the growing Thai draft on one row', async () => {
              const rows = await capture()
              const drafts = rows.filter(line => /\p{Script=Thai}/u.test(line))
              return drafts.length === 1 && drafts[0]!.includes(`> ${typed}${SCREEN.caret}`)
            })
          }
          tty.send('\x1b[200~ກຳ\x1b[201~', 'paste a Lao spacing vowel')
          await tty.wait('the pasted draft', async () => (await capture()).some(line => line.includes(`ກຳ${SCREEN.caret}`)))
          tty.send('\x7f', 'Backspace removes the complete Lao grapheme')
          await tty.wait('Thai draft after Lao deletion', async () => (await capture()).some(line => line.includes(`${'น้ำ'.repeat(24)}${SCREEN.caret}`)))
          await capture()
          screen.resize(40, 24)
          tty.resize(40, 24)
          await tty.wait('Thai wraps by terminal cells at 40 columns', async () => {
            const rows = await capture()
            const first = rows.indexOf(`> ${'น้ำ'.repeat(18)}`)
            return first >= 0 && rows[first + 1] === `  ${'น้ำ'.repeat(6)}${SCREEN.caret}`
          })
          tty.send(`\x1b[200~${suffix}\x1b[201~`, 'paste a mixed-script suffix without submitting')
          await tty.wait('the mixed-script draft remains editable', async () => (await capture()).some(line => line.includes(`👩🏽‍💻${SCREEN.caret}`)))
          tty.send('\r', 'submit the complete multilingual draft')
          await tty.follows(SCREEN.idle, reply)
          await tty.wait('Thai and Lao response in the terminal', async () => (await capture()).some(line => line.includes(reply)))
        } finally { screen.dispose() }
      })
    } finally { await run.writeOverlay() }
    const log = await events(await run.created(before, 'Thai input'))
    const submitted = log.filter(event => event.type === 'user/message' && event.data.source.kind === 'user')
    assert(same(submitted.map(event => event.data.content), [[{ type: 'text', text: prompt }]]), 'multilingual input changed before persistence')
  })

scenario('rendering', 'preserved scrollback after resize and a visible caret in short terminals and wrapped drafts', { requires: ['fresh'], replayOnly: true },
  async run => {
    await run.terminal('rendering', ['--resume', run.state.id], async tty => {
      const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
      let consumed = 0
      const capture = async (): Promise<string[]> => {
        const raw = tty.raw
        await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
        consumed = raw.length
        return Array.from({ length: screen.buffer.active.length }, (_, row) => screen.buffer.active.getLine(row)?.translateToString(true) ?? '')
      }
      const shown = async (): Promise<string> => (await capture()).slice(screen.buffer.active.viewportY).join('\n')
      const resize = async (columns: number, rows: number): Promise<number> => {
        await capture()
        const mark = tty.raw.length
        screen.resize(columns, rows)
        tty.resize(columns, rows)
        return mark
      }
      try {
        await tty.wait('resumed answer in the terminal buffer', async () => (await capture()).some(line => line === '  DONE'))
        // The runner starts the frame on the bottom row, however short the
        // history above it. The status line, then only Ink's cursor row.
        await tty.wait('composer resting on the bottom rows', async () => {
          const visible = (await shown()).split('\n')
          return visible.length === 40 && SCREEN.status.test(visible[38] ?? '') && visible[39] === ''
        })
        const shrunk = await resize(40, 4)
        await tty.wait('composer remains visible at 40x4', async () => {
          const visible = (await shown()).split('\n')
          // The status line, redrawn within the new width rather than reflowed from the old one.
          return tty.raw.length > shrunk && visible[1]?.includes(`> ${SCREEN.caret}`) === true
            && SCREEN.status.test(visible[2] ?? '') && (visible[2]?.length ?? 0) <= 40
        })
        const history = await capture()
        assert(history.filter(line => line === '  DONE').length === 1, 'resize lost or duplicated the resumed answer')
        tty.send(`\x1b[200~START ${'word '.repeat(100)}END\x1b[201~`, 'a wrapped draft')
        await tty.wait('end of the wrapped draft', async () => (await shown()).includes(`END${SCREEN.caret}`))
        tty.send('\x1b[H', 'Home inside the wrapped draft')
        await tty.wait('caret at the start of the wrapped draft', async () => (await shown()).includes(`${SCREEN.caret}START`))
        tty.send('\x1b[F', 'End inside the wrapped draft')
        await tty.wait('caret returns to the end', async () => (await shown()).includes(`END${SCREEN.caret}`))
        const expanded = await resize(120, 40)
        await tty.wait('expanded composer keeps its draft', async () => {
          // The caret row, then the base rule, then the status line,
          // under the history the resize replayed, back on the bottom rows.
          const visible = (await shown()).split('\n')
          const caret = visible.findIndex(line => line.includes(`END${SCREEN.caret}`))
          return tty.raw.length > expanded && caret > visible.indexOf('  DONE')
            && visible[caret + 1]?.startsWith('\u2500') === true && SCREEN.status.test(visible[caret + 2] ?? '')
            && caret + 2 === 38
        })
        assert((await capture()).filter(line => line === '  DONE').length === 1, 'expanding the terminal lost or duplicated history')
      } finally {
        await capture()
        screen.dispose()
      }
    })
  })

scenario('resume', 'exact replay of committed history, the restored draft, and history recall', { requires: ['fresh'] },
  async run => {
    const text = await run.terminal('resume', ['--resume', run.state.id], async tty => {
      await tty.expect(SCREEN.toolResult, SCREEN.idle)
      if (!run.live) await tty.expect('tui-picked-model  think high')
      tty.send('Unsent draft')
      await tty.expect(`> Unsent draft${SCREEN.caret}`)
      tty.send('\x1b[D', 'Left')
      await tty.expect(`> Unsent draf${SCREEN.caret}t`)
      tty.send('\x1b[A', 'Up, recalling the submitted prompt')
      await tty.expect(`> ${run.prompt}${SCREEN.caret}`)
      const restored = tty.mark()
      tty.send('\x1b[B', 'Down, restoring the draft and its cursor')
      await tty.expect(`> Unsent draf${SCREEN.caret}t`, restored)
    })
    assert([...text.matchAll(new RegExp(DONE_LINE.source, 'g'))].length === 1,
           'resume duplicated or omitted committed output')
    assert(!text.includes('BAKE'), 'a resumed session printed the welcome block')
    const log = await events(run.state.log)
    assert(log.filter(e => e.type === 'assistant/message').length === run.state.model.length,
           'resume made an unsolicited model call')
  })

scenario('navigate', 'session picker cancellation, a new session, and switching back to committed history',
  { requires: ['fresh'], replayOnly: true },
  async run => {
    const before = await run.logs()
    const identity = run.state.id
    delete run.env.NO_COLOR
    run.env.FORCE_COLOR = '3'
    let text: string
    try { text = await run.terminal('navigate', ['--resume', identity], async tty => {
      await tty.follows(SCREEN.idle, SCREEN.toolResult)
      const hints = tty.mark()
      tty.send('/', 'open the frequent slash commands')
      const menu = await tty.expect('/model', '/resume', '/new', '/clear', hints)
      const shown = menu.slice(hints)
      tty.check('frequent commands lead the slash menu', ['/model', '/resume', '/new', '/clear']
        .map(name => shown.indexOf(name)).every((position, index, positions) => position >= 0
          && (index === 0 || position > positions[index - 1]!)))
      const composed = tty.mark()
      tty.send('help', 'complete /help in the slash draft')
      await tty.expect(`> /help${SCREEN.caret}`, composed)
      const help = tty.mark()
      tty.send('\r', 'list session commands')
      await tty.search(/\/resume\b.* {2}Browse sessions or start a new one/u, help)
      await tty.follows(SCREEN.idle, 'Command: /help')
      let start = tty.mark()
      tty.send('/sessions\r')
      await tty.expect('Choose session', '● ', 'Current', '+ New session', start)
      const indicatorColor = (glyph: string) => tty.raw.slice(start)
        .match(new RegExp(`\\x1b\\[38;(?:5;\\d+|2;\\d+;\\d+;\\d+)m${glyph.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`))?.[0]
      const currentColor = indicatorColor('●')
      const newColor = indicatorColor('+')
      tty.check('current and new sessions have distinct colors', currentColor !== undefined
        && newColor !== undefined && currentColor !== newColor)
      tty.send('\x1b', 'Escape to cancel the picker')
      await tty.expect('Session navigation cancelled', start)

      start = tty.mark()
      tty.send('/sessions\r')
      await tty.expect('Choose session', start)
      tty.send('New session')
      await tty.expect('▸ + New session', start)
      tty.send('\r', 'Enter to start a new session')
      const created = await tty.search(/Session: (session-[a-f0-9-]+)/, start)
      assert(created[1] !== identity, 'new-session selection reused the current identity')
      // The new footer names the new model, and its composer accepts input. A
      // key sent while the old session retires is refused as navigation busy.
      await tty.search(/ {2}deepseek-v4-flash(?![\w.-])/u, start)
      await tty.wait('the new session to accept input', text => {
        const shown = text.slice(start)
        return shown.lastIndexOf(SCREEN.idle) > Math.max(shown.indexOf(created[0]), shown.lastIndexOf(dictionaries.en.sessionsBusy))
      })

      start = tty.mark()
      tty.send('/resume\r')
      await tty.expect('Choose session', '○ ', dictionaries.en.ageNow, start)
      tty.check('saved sessions have a subdued indicator', tty.raw.slice(start).includes('\x1b[2m○'))
      tty.send(identity, 'the original session id')
      await tty.expect(`> ${identity}${SCREEN.caret}`, start)
      tty.send('\r', 'Enter to switch back')
      await tty.expect(SCREEN.toolResult, 'tui-picked-model  think high', start)
      await tty.wait('the resumed session to accept input', text => {
        const shown = text.slice(start)
        return shown.lastIndexOf(SCREEN.idle) > Math.max(shown.lastIndexOf(SCREEN.toolResult), shown.lastIndexOf(dictionaries.en.sessionsBusy))
      })

      start = tty.mark()
      tty.send('\x1b[A', 'Up, recalling the last saved prompt')
      await tty.expect(`> ${run.prompt}${SCREEN.caret}`, start)
      tty.send('\x1b[B', 'Down, back to an empty composer')
      await tty.expect(`> ${SCREEN.caret}`, start)

      const known = new Set([...tty.text.matchAll(/Session: (session-[a-f0-9-]+)/g)].map(match => match[1]))
      tty.send('/new\r', 'start a session without the picker')
      const directText = await tty.wait('/new opens a different session', text =>
        [...text.matchAll(/Session: (session-[a-f0-9-]+)/g)].some(match => !known.has(match[1])))
      const direct = [...directText.matchAll(/Session: (session-[a-f0-9-]+)/g)].at(-1)![1]!
      await tty.follows(SCREEN.idle, `Session: ${direct}`)
      known.add(direct)
      tty.send('/clear\r', 'start another fresh session')
      const clearedText = await tty.wait('/clear opens another session', text =>
        [...text.matchAll(/Session: (session-[a-f0-9-]+)/g)].some(match => !known.has(match[1])))
      const cleared = [...clearedText.matchAll(/Session: (session-[a-f0-9-]+)/g)].at(-1)![1]!
      await tty.follows(SCREEN.idle, `Session: ${cleared}`)
    }) } finally { run.env.NO_COLOR = '1'; delete run.env.FORCE_COLOR }

    assert([...text.matchAll(new RegExp(DONE_LINE.source, 'g'))].length === 2,
           'navigation did not replay the selected history exactly once per visit')
    const log = await events(run.state.log)
    assert(same(log.filter(e => e.type === 'assistant/message').map(e => e.data.message.content), run.state.model),
           'navigation changed model history')
    assert(log.filter(e => e.type === 'request/header').length === run.state.headers.length,
           'navigation requested a model response')
    for (const path of [...await run.logs()].filter(item => !before.has(item))) {
      const saved = await events(path)
      assert(!saved.some(e => e.type === 'user/message'), 'old draft or input crossed into the new session')
      const runs = saved.filter(e => e.type === 'command/run').map(e => e.data.commandId)
      const settled = saved.filter(e => e.type === 'command/done').map(e => e.data.commandId)
      assert(same(runs, settled), 'navigation disposed a session before command settlement')
    }
  })

/**
 * Another process holding a session's write lock: `node -e` with the root, the
 * session id, and its workspace. It creates the session there, says `holding`,
 * and keeps the lock until it is killed.
 */
const LOCK_HOLDER = `
import { Context } from '@deepseek-ai/cordis'
import { SESSION_FORMAT_VERSION } from '@deepseek-ai/dsh-session'
import Persistence from '@deepseek-ai/dsh-session-persistence-jsonl'
const [root, id, cwd] = process.argv.slice(1)
const ctx = new Context()
await ctx.plugin(Persistence, { root, compression: 'none' })
const handle = await ctx.sessionPersistence.create({ version: SESSION_FORMAT_VERSION, id, createdAt: Date.now(), cwd, isSeeded: false })
await handle.append([
  { type: 'turn/start', seq: 0, time: 1, data: { turn: 1 } },
  { type: 'turn/end', seq: 1, time: 2, data: { turn: 1, reason: { kind: 'completed' } } },
])
await handle.flush()
process.stdout.write('holding\\n')
setInterval(() => {}, 60_000)
`

scenario('session-in-use', 'a session another Bake process has open: --resume exits 75 with one stderr line and an untouched'
  + ' terminal, /resume keeps the current session with a notice, and a headless --resume prints the same line and exits 75',
  { requires: ['fresh'] },
  async run => {
    const id: string = run.state.id
    const copy = dictionaries.en
    const short = (session: string): string => /[0-9a-f]{8}/i.exec(session)![0]
    // The notice wraps at the terminal's width, so any space in it may end a row.
    const notice = new RegExp(copy.sessionInUse.split(' ').map(word => word.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('\\s+'), 'u')
    const before = await events(run.state.log)
    let refusing = Infinity
    await run.terminal('session-in-use-owner', ['--resume', id], async owner => {
      // The transcript replays once the session is open, so its write lock is held from here.
      await owner.follows(SCREEN.idle, SCREEN.toolResult)
      refusing = Date.now()
      // Refused before the first frame: no `ready`, and no paste to release.
      const refused = new Terminal('session-in-use-launch', run.command(['--resume', id]), run.workspace, run.env, run.options)
      try {
        await refused.ended(75, 'a --resume of the held session')
        refused.check('the refusal to leave the screen alone', !refused.raw.includes('\x1b'), 'it wrote an escape sequence')
        // The PTY's output processing ends the launcher's line with CRLF.
        refused.check('the refusal to be one stderr line', refused.text === `dsh: ${id}: ${copy.sessionInUseLaunch}\r\n`,
                      `it wrote ${JSON.stringify(refused.text)}`)
      } catch (error) {
        refused.save()
        throw error
      } finally {
        await refused.close()
      }
      await run.terminal('session-in-use-switch', [], async tty => {
        const own = (await tty.search(/Session: (session-[a-f0-9-]+)/))[1]!
        const start = tty.mark()
        tty.send('/resume\r', 'open the session picker')
        await tty.expect(copy.chooseSession, start)
        // Every scenario's session shares this workspace, so the held one is found by its short id.
        const typed = tty.mark()
        tty.send(short(id), 'filter to the held session')
        await tty.search(/(?<!\d)1\/1(?!\d)/, typed)
        const chosen = tty.mark()
        tty.send('\r', 'choose the session the other process holds')
        await tty.wait('the in-use notice above an idle composer', text => {
          const shown = text.slice(chosen)
          const match = notice.exec(shown)
          return match !== null && shown.lastIndexOf(SCREEN.idle) > match.index
        })
        const shown = tty.text.slice(chosen)
        tty.refuse('navigation reporting a failure', shown.includes(copy.sessionsError))
        tty.refuse('the held transcript replaying', shown.includes(SCREEN.toolResult))
        tty.refuse('a heading for another session', [...shown.matchAll(/Session: (session-[a-f0-9-]+)/g)].some(match => match[1] !== own))
        // The picker marks the session in force as current, so its row names the one this terminal kept.
        const again = tty.mark()
        tty.send('/resume\r', 'reopen the session picker')
        await tty.wait('this terminal\'s own session marked current', async () => {
          const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
          try {
            await new Promise<void>(resolve => screen.write(tty.raw, resolve))
            const buffer = screen.buffer.active
            return tty.text.slice(again).includes(copy.chooseSession) && Array.from({ length: screen.rows }, (_, row) =>
              buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? '')
              .some(row => row.includes(short(own)) && row.includes(copy.currentSelection))
          } finally { screen.dispose() }
        })
        tty.send('\x1b', 'close the picker')
        await tty.expect(copy.sessionsCancelled, again)
      })
    })
    // The owner resumed as usual, appending a resume marker unless the log
    // already ended in one. Any other event is a refusal's write.
    const after = await events(run.state.log)
    const added = after.slice(before.length)
    assert(same(after.slice(0, before.length), before)
      && added.length === (before.at(-1)?.type === 'session/end-seed' ? 0 : 1)
      && added.every(event => event.type === 'session/end-seed' && event.time < refusing),
    `a refused open wrote to the held session log: ${JSON.stringify(added.map(event => event.type))}`)
    // Nothing either refusal did outlives it: the session opens again as usual.
    await run.terminal('session-in-use-retry', ['--resume', id], async tty => {
      await tty.follows(SCREEN.idle, SCREEN.toolResult)
    })

    // The headless profile refuses with the same line and status. It adopts no
    // preset session, so another process's store holds a presetless session of
    // this workspace, in a root of its own that later scenarios never list.
    const heldRoot = join(run.root, 'held-sessions')
    const held = 'session-held-headless'
    await run.writeOverlay(undefined, { root: heldRoot })
    // Recorded as Bake records it, from the resolved working directory: macOS's
    // temporary directory sits behind the /var -> /private/var symlink.
    const holder = Bun.spawn([run.node, '--input-type=module', '-e', LOCK_HOLDER, heldRoot, held, realpathSync(run.workspace)],
                             { cwd: ROOT, stdin: 'pipe', stdout: 'pipe', stderr: 'inherit' })
    try {
      const reader = holder.stdout.getReader()
      let said = ''
      await Promise.race([
        (async () => {
          while (!said.includes('holding\n')) {
            const { value, done } = await reader.read()
            if (done) throw new Error(`the lock holder exited before holding the session: ${JSON.stringify(said)}`)
            said += new TextDecoder().decode(value)
          }
        })(),
        Bun.sleep(run.options.step * 1000).then(() => {
          throw new Error(`the lock holder did not hold the session within ${run.options.step}s`)
        }),
      ])
      reader.releaseLock()
      const headless = new Terminal('session-in-use-headless', run.command(['--resume', held, 'continue'], [], 'headless'),
                                    run.workspace, run.env, run.options)
      try {
        await headless.ended(75, 'a headless --resume of the held session')
        headless.check('the headless refusal to be the same one line', headless.text === `dsh: ${held}: ${copy.sessionInUseLaunch}\r\n`,
                       `it wrote ${JSON.stringify(headless.text)}`)
      } catch (error) {
        headless.save()
        throw error
      } finally {
        await headless.close()
      }
    } finally {
      holder.kill()
      await holder.exited
      await run.writeOverlay()
    }
  })

scenario('corrupt-picker', 'a damaged compressed header does not hide healthy sessions from the picker',
  { replayOnly: true },
  async run => {
    const root = join(run.root, 'zstd-sessions')
    await run.writeOverlay(undefined, { root, compression: 'zstd' })
    try {
      const seed = await run.terminal('zstd-seed', [], async tty => {
        tty.send(`${run.prompt}\r`, 'record a compressed session')
        await tty.wait('the recorded turn to finish', text => DONE_LINE.test(text))
      })
      const id = seed.match(/Session: (session-[a-f0-9-]+)/)?.[1]
      assert(id !== undefined, 'compressed session identity was not shown')
      const [healthy] = await glob('**/session.v*.jsonl.zstd', root)
      assert(healthy !== undefined, 'compressed session log was not created')
      const damaged = Buffer.from(await Bun.file(healthy).arrayBuffer())
      damaged[0] = damaged[0]! ^ 0xFF
      const corrupt = join(dirname(dirname(healthy)), 'corrupt-zstd', basename(healthy))
      mkdirSync(dirname(corrupt), { recursive: true })
      await Bun.write(corrupt, damaged)

      await run.terminal('corrupt-picker', ['--resume', id], async tty => {
        tty.send('/sessions\r', 'open the picker beside a damaged header')
        await tty.expect('Choose session')
        tty.refuse('the damaged session being listed', tty.text.includes('corrupt-zstd'))
        tty.send('\x1b', 'close the picker')
        await tty.expect('Session navigation cancelled')
      })
    } finally {
      await run.writeOverlay()
    }
  })

scenario('cancel', 'skill and quoted-file completion, and Alt-Up sending steering typed into a running turn now instead of at the next step',
  { replayOnly: true },
  async run => {
    // Keep project skill discovery inside this fixture even when the temp parent has a .git marker.
    mkdirSync(join(run.workspace, '.git'), { recursive: true })
    const skill = join(run.workspace, '.agents/skills/tui-smoke/SKILL.md')
    mkdirSync(dirname(skill), { recursive: true })
    await Bun.write(skill, '---\nname: tui-smoke\ndescription: Terminal skill smoke\n'
                         + 'disable-model-invocation: true\n---\n\nTUI_SKILL_INSTRUCTIONS\n')
    const referenced = join(run.workspace, 'notes folder/read me.txt')
    mkdirSync(dirname(referenced), { recursive: true })
    await Bun.write(referenced, 'TUI_FILE_CONTENT_MUST_NOT_BE_INJECTED')
    // The replay hangs on this turn so the composer stays live while the agent runs,
    // then answers the turn the steering opens once Alt-Up sends it.
    const ready = join(run.root, 'stream-ready')
    const override = join(run.root, 'cancel.json')
    const steered = [
      { type: 'block-start', index: 0, blockType: 'text' },
      { type: 'text-delta', index: 0, text: 'STEERED_NOW' },
      { type: 'block-end', index: 0, block: { type: 'text', text: 'STEERED_NOW' } },
      { type: 'finish', reason: { kind: 'stop' } },
    ]
    await Bun.write(override, JSON.stringify([{ kind: 'hang', readyFile: ready }, { kind: 'chunks', chunks: steered }]))
    await run.writeOverlay(override)

    const before = await run.logs()
    await run.terminal('cancel', [], async tty => {
      tty.send('/tui-sm', 'a partial skill command')
      await tty.search(picked('/tui-smoke'))
      await tty.expect('Terminal skill smoke')
      tty.send('\t', 'Tab to complete it')
      await tty.expect(`> /tui-smoke ${SCREEN.caret}`)
      tty.refuse('completion submitting the skill before Enter', existsSync(ready))
      tty.send('Pause for a new direction about @notes', 'a file mention')
      await tty.search(picked('@"notes folder/'))
      tty.send('\t', 'Tab to complete the directory')
      await tty.search(picked('@"notes folder/read me.txt"'))
      tty.send('\t', 'Tab to complete the file')
      await tty.expect(`> /tui-smoke Pause for a new direction about @"notes folder/read me.txt" ${SCREEN.caret}`)
      tty.refuse('file completion starting a model request before Enter', existsSync(ready))
      tty.send('\r', 'Enter to submit')
      await tty.wait('the turn to start streaming and hang',
                     text => existsSync(ready) && text.includes('partial'))
      await tty.expect('Alt+\u2191 sends now')
      tty.send('Send this steering now', 'steering typed into a running turn')
      await tty.expect(`Send this steering now${SCREEN.caret}`)
      tty.send('\x1b[1;3A', 'Alt-Up to send the draft now instead of at the next step')
      await tty.expect('Interrupted', 'STEERED_NOW')
      await tty.follows(SCREEN.idle, 'STEERED_NOW')
    })

    const path = await run.created(before, 'cancellation')
    const log = await events(path)
    assert(log.some(e => e.type === 'user/message' && e.data.source?.kind === 'skill-invocation'
                    && e.data.source?.name === 'tui-smoke'
                    && JSON.stringify(e.data.content).includes('TUI_SKILL_INSTRUCTIONS')),
           'Harness did not log the selected skill instructions')
    assert(log.some(e => e.type === 'user/message' && e.data.source?.kind === 'user'
                    && same(e.data.content, [{ type: 'text',
                        text: '/tui-smoke Pause for a new direction about @"notes folder/read me.txt" ' }])),
           'completed file mention was not submitted literally')
    assert(!JSON.stringify(log).includes('TUI_FILE_CONTENT_MUST_NOT_BE_INJECTED'),
           'file completion injected file contents')
    assert(log.filter(e => e.type === 'turn/start').length === 2, 'Alt-Up did not open a turn for the steering')
    assert(log.some(e => e.type === 'user/message' && e.data.source?.kind === 'user'
                    && same(e.data.content, [{ type: 'text', text: 'Send this steering now' }])), 'the sent steering was not logged')
    Object.assign(run.state, { cancelledLog: path, cancelledId: log[0].id })
  })

scenario('fatal-exception', 'an uncaught callback restores terminal modes before the launcher exits 1',
  { replayOnly: true },
  async run => {
    // A timer polls for a trigger file rather than listening for a signal: a
    // child of Bun on macOS can inherit a mask that blocks SIGUSR2, and Node
    // unblocks only SIGINT, so a signal may never arrive.
    const preload = join(run.root, 'fatal-exception.mjs')
    const trigger = join(run.root, 'fatal-exception.trigger')
    await Bun.write(preload, `import { existsSync } from 'node:fs'
setInterval(() => {
  if (existsSync(${JSON.stringify(trigger)})) throw Object.assign(new Error('PTY_FATAL_EXCEPTION'), { code: 'TUI_FATAL_TEST' })
}, 20).unref()
`)
    const tty = new Terminal('fatal-exception', run.command(['--screen', 'fullscreen'], ['--import', preload]),
      run.workspace, run.env, run.options)
    try {
      await tty.ready()
      await Bun.write(trigger, '')
      const output = await tty.exited(1, 'the uncaught exception')
      tty.check('the fatal exception to be reported', output.includes('fatal uncaught exception: Error: PTY_FATAL_EXCEPTION'))
      tty.check('the error code to be retained', output.includes("code: 'TUI_FATAL_TEST'"))
    } catch (error) {
      tty.save()
      throw error
    } finally {
      await tty.close()
    }
  })

scenario('hangup', 'a closed terminal or a repeated SIGHUP exits 129 once disposal stops a tool that ignores hangups,'
  + ' with the compressed session flushed and released and the terminal restored', { replayOnly: true },
  async run => {
    const root = join(run.root, 'hangup-sessions')
    const pidFile = join(run.workspace, 'child.pid')
    // A shell and its own child, both ignoring what a hangup and an ordinary
    // stop send them, so only the app's teardown of the tool's tree ends them.
    const namespace = process.platform === 'linux' ? '$(readlink /proc/$$/ns/pid)' : 'host'
    const command = `sh -c 'trap "" TERM HUP; sleep 300 & echo $$ $! ${namespace} > child.pid; wait'`
    const args = JSON.stringify({ command, description: 'Hold a child' })
    const call = { type: 'tool-call' as const, id: 'call-hangup-1', name: 'bash', arguments: args }
    const override = join(run.root, 'hangup-replay.json')
    await Bun.write(override, JSON.stringify([{ kind: 'chunks', chunks: [
      { type: 'block-start', index: 0, blockType: 'tool-call' },
      { type: 'tool-call-delta', index: 0, id: call.id, name: call.name, argumentsDelta: args },
      { type: 'block-end', index: 0, block: call },
      { type: 'finish', reason: { kind: 'tool-calls' } },
    ] }]))
    /**
     * Start a session whose turn is running the shell, end it through `drive`, and check what it left.
     *
     * @param label - the terminal's name.
     * @param extra - extra CLI arguments.
     * @param drive - ends the app, and checks how it went.
     * @returns the session's id, once its log has been read back whole.
     */
    const hold = async (label: string, extra: readonly string[], drive: (tty: Terminal) => Promise<void>): Promise<string> => {
      rmSync(pidFile, { force: true })
      const known = new Set(await glob('**/session.v*.jsonl.zstd', root))
      const tty = new Terminal(label, run.command(extra), run.workspace, run.env, run.options)
      let processes: ProcessState[] = []
      try {
        await tty.ready()
        tty.send('Hold a child process.\r', 'trigger the recorded bash call')
        await tty.wait('the tool\'s shell and its child to record their pids', () => {
          const text = existsSync(pidFile) ? readFileSync(pidFile, 'utf8') : ''
          if (/^\d+ \d+ (?:pid:\[\d+\]|host)\n$/.test(text)) {
            const [shell, child, namespace] = text.trim().split(' ') as [string, string, string]
            const found = process.platform === 'linux'
              ? [Number(shell), Number(child)].map(pid => namespaceProcess(namespace, pid))
              : [Number(shell), Number(child)].map(pid => processState(pid))
            if (found.every((state): state is ProcessState => state !== undefined && state.state !== 'Z')) processes = found
          }
          return processes.length === 2
        })
        await drive(tty)
        const deadline = performance.now() + 2000
        while (processes.some(liveProcess) && performance.now() < deadline) await Bun.sleep(20)
        const remaining = processes.filter(liveProcess)
        tty.check('the tool\'s shell and its child to stop within 2s of the exit', remaining.length === 0,
                  `${remaining.map(owned => `${owned.pid} (${processState(owned.pid)?.state ?? 'exited'})`).join(' and ')} still running`)
      } catch (error) {
        tty.save()
        // They ignore both the hangup and the stop, so a failed run would leave them behind.
        for (const owned of processes.filter(liveProcess)) {
          try { process.kill(owned.pid, 'SIGKILL') } catch { /* it ended meanwhile */ }
        }
        throw error
      } finally {
        await tty.close()
        rmSync(pidFile, { force: true })
      }
      const created = (await glob('**/session.v*.jsonl.zstd', root)).filter(path => !known.has(path))
      assert(created.length === 1, `expected one ${label} session, got ${created.sort().join(', ') || 'none'}`)
      const log = await events(created[0]!)
      assert(log.some(event => event.type === 'tool/call' && event.data.callId === call.id), `the ${label} session log lacks the bash call`)
      const result = log.find(event => event.type === 'tool/result' && event.data.message?.source?.callId === call.id)
      assert(result !== undefined, `the ${label} session log lacks the cancelled bash result`)
      assert(result.data.error?.code === 'ABORTED' || result.data.error?.code === 'ABORTED_BEFORE_DISPATCH',
        `the ${label} bash result was not cancelled: ${JSON.stringify(result.data.error)}`)
      const ended = log.findLast(event => event.type === 'turn/end')
      assert(ended?.data.reason?.kind === 'aborted' && ended.data.reason.reason?.kind === 'disposed',
        `the ${label} turn did not end as disposed: ${JSON.stringify(ended?.data.reason)}`)
      assert(!log.some(event => event.type === 'agent/error'), `the ${label} session logged an agent error`)
      return log[0].id
    }
    // Compressed as by default, where a flush cut short tears a frame.
    await run.writeOverlay(override, { root, compression: 'zstd' })
    try {
      const closed = await hold('hangup-close', [], async tty => {
        tty.hangup()
        await tty.exits(129, 'its terminal closed', 7)
      })
      // No lock outlived the process: the session opens instead of being refused as open elsewhere.
      await run.terminal('hangup-resume', ['--resume', closed], async tty => {
        await tty.search(SCREEN.toolCall)
      })
      await hold('hangup-signal', ['--screen', 'fullscreen'], async tty => {
        const from = tty.raw.length
        // One close can deliver more than one SIGHUP. The second joins the disposal the first started.
        tty.signal('SIGHUP')
        await Bun.sleep(10)
        tty.signal('SIGHUP')
        await tty.ended(129, 'two SIGHUPs 10ms apart', 7)
        const after = tty.raw.slice(from)
        const released = [['bracketed paste', '\x1b[?2004l'], ['the cursor', '\x1b[?25h'], ['the alternate screen', '\x1b[?1049l']] as const
        for (const [what, sequence] of released) {
          tty.check(`${what} to be released after the hangup`, after.includes(sequence), `no ${JSON.stringify(sequence)} after the signal`)
        }
      })
    } finally { await run.writeOverlay() }
  })

scenario('resume-cleared',
  'a resumed session showing the steering Alt-Up sent answered, and no stale pending input',
  { requires: ['cancel'], replayOnly: true },
  async run => {
    const text = await run.terminal('resume-cleared', ['--resume', run.state.cancelledId], async tty => {
      await tty.expect('STEERED_NOW', SCREEN.idle, '@"notes folder/read me.txt"')
    })
    assert(!text.includes('Next step: Send this steering now'), 'sent steering returned as pending after resume')
    assert(!text.includes('Alt+\u2191 sends it now'), 'resume shows stale pending input')
    const log = await events(run.state.cancelledLog)
    assert(log.filter(e => e.type === 'turn/start').length === 2, 'resume drove the sent input again')
  })

scenario('attachments', 'attachment staging, exact stored bytes, recorded model output, and metadata on resume',
  { replayOnly: true },
  async run => {
    await run.writeOverlay()
    const before = await run.logs()
    const name = 'notes with spaces.bin'
    const data = new Uint8Array([...new TextEncoder().encode('TUI_ATTACHMENT_BYTES'), 0x00, 0xff])
    await Bun.write(join(run.workspace, name), data)
    await run.terminal('attachments', [], async tty => {
      tty.send(`/attach ${name}\r`)
      await tty.expect('Staged attachments: 1', `${name} · `)
      const start = tty.mark()
      tty.send('/sessions\r')
      await tty.expect('Send or clear staged attachments before switching sessions', start)
      tty.send(`\x1b[200~${run.prompt}\x1b[201~`)
      await tty.expect(`> ${run.prompt}${SCREEN.caret}`)
      tty.send('\r', 'Enter to submit text and the staged file')
      await tty.wait('the recorded reply after attachment admission',
                     text => text.includes(SCREEN.toolResult) && DONE_LINE.test(text))
      await tty.follows(SCREEN.idle, SCREEN.toolResult)
    })

    const path = await run.created(before, 'attachment')
    const log = await events(path)
    const users = log.filter(e => e.type === 'user/message' && e.data.source.kind === 'user').map(e => e.data.content)
    assert(users.length === 1 && same(users[0][0], { type: 'text', text: run.prompt }),
           'attachment submission changed prompt text')
    assert(users[0].length === 2 && users[0][1].type === 'file', 'attachment was not logged beside the prompt')
    const attachment = users[0][1].attachment
    assert(attachment.name === name && attachment.bytes === data.length, 'file metadata changed')
    const stored = await glob(`**/${name}`, run.home)
    assert(stored.length === 1, 'the attachment was not stored exactly once')
    assert(same([...new Uint8Array(await Bun.file(stored[0]!).arrayBuffer())], [...data]),
           'stored attachment bytes differ from the source')
    const model = log.filter(e => e.type === 'assistant/message').map(e => e.data.message.content)
    const expected = run.recorded.filter(e => e.type === 'assistant/message').map(e => e.data.message.content)
    assert(same(model, expected), 'attachment flow did not consume the recorded model output')
    await run.terminal('attachments-resume', ['--resume', log[0].id], async tty => {
      await tty.expect(SCREEN.toolResult, `${name} · ${data.length} B`)
      tty.refuse('sent attachments returning as a draft', tty.text.includes('Staged attachments:'))
    })
    const after = await events(path)
    assert(same(after.filter(e => e.type === 'assistant/message').map(e => e.data.message.content), model),
           'resume made an unsolicited model call')
  })

/**
 * Resolve the scenarios to run, in declaration order, with prerequisites.
 *
 * @param names - the names asked for, or an empty list for all of them.
 * @param live - whether the run uses the real API, which skips replay-only scenarios.
 * @returns the scenarios to run, each preceded by what it requires.
 */
function selected(names: readonly string[], live: boolean): Scenario[] {
  let wanted: Set<string>
  if (names.length > 0) {
    const unknown = names.filter(name => !SCENARIOS.has(name))
    if (unknown.length > 0) {
      throw new Error(`unknown scenario(s): ${unknown.join(', ')}\nknown: ${[...SCENARIOS.keys()].join(', ')}`)
    }
    wanted = new Set()
    const pending = [...names]
    while (pending.length > 0) {
      const name = pending.pop()!
      if (wanted.has(name)) continue
      wanted.add(name)
      pending.push(...SCENARIOS.get(name)!.requires)
    }
  } else {
    wanted = new Set(SCENARIOS.keys())
  }
  let chosen = [...SCENARIOS.values()].filter(item => wanted.has(item.name))
  if (live) {
    const skipped = chosen.filter(item => item.replayOnly).map(item => item.name)
    if (names.length > 0 && skipped.length > 0) {
      throw new Error(`--live cannot run replay-only scenario(s): ${skipped.join(', ')}`)
    }
    chosen = chosen.filter(item => !item.replayOnly)
  }
  return chosen
}

/** Parse arguments, run the selected scenarios, and report each one. */
async function main(): Promise<void> {
  const { values, positionals } = parseArgs({
    args: Bun.argv.slice(2),
    allowPositionals: true,
    options: {
      live: { type: 'boolean', default: false },
      only: { type: 'string', multiple: true, default: [] },
      list: { type: 'boolean', default: false },
      trace: { type: 'boolean', default: false },
      'keep-home': { type: 'boolean', default: false },
      'step-timeout': { type: 'string', default: '30' },
      budget: { type: 'string', default: '120' },
      help: { type: 'boolean', default: false },
    },
  })

  if (values.help) {
    process.stdout.write(`${import.meta.file}: drive the built tui profile through a real terminal\n\n`
      + '  --live            use the DeepSeek key in the root .env instead of recorded replay\n'
      + '  --only NAME       run this scenario and its prerequisites; repeatable\n'
      + '  --list            print the scenarios and exit\n'
      + '  --trace           print each step to stderr as it is satisfied\n'
      + '  --keep-home       keep the temporary DSH_HOME and workspace for inspection\n'
      + '  --step-timeout N  how long one step may take (default: 30)\n'
      + '  --budget N        how long one terminal may take (default: 120)\n'
      + '  [node]            the node binary to run the product with (default: node)\n')
    return
  }

  if (values.list) {
    for (const item of SCENARIOS.values()) {
      const marks = [...(item.requires.length > 0 ? [`requires ${item.requires.join(', ')}`] : []),
                     ...(item.replayOnly ? ['replay only'] : [])]
      process.stdout.write(`${item.name.padEnd(16)} ${item.summary}\n`
        + (marks.length > 0 ? `${''.padEnd(16)} (${marks.join('; ')})\n` : ''))
    }
    return
  }

  const chosen = selected(values.only as string[], values.live as boolean)
  const options: Options = {
    step: Number(values['step-timeout']), budget: Number(values.budget),
    trace: values.trace as boolean, artifacts: ARTIFACTS,
  }
  rmSync(options.artifacts, { recursive: true, force: true })
  const mode = values.live ? 'live API' : 'recorded replay'
  const node = positionals[0] ?? 'node'
  const directory = mkdtempSync(join(tmpdir(), 'dsh-tui-pty-'))
  const started = performance.now()
  let failed: unknown
  let current: Scenario | undefined
  try {
    const run = new Run(directory, values.live as boolean, node, options)
    await run.load()
    for (const item of chosen) {
      current = item
      const step = performance.now()
      await item.body(run)
      await run.forgetDefaultModel()
      process.stdout.write(`PASS ${item.name} (${((performance.now() - step) / 1000).toFixed(1)}s): ${item.summary}\n`)
    }
    process.stdout.write(`PASS ${node} (${mode}): ${chosen.length} scenario(s)`
      + ` in ${((performance.now() - started) / 1000).toFixed(1)}s\n`)
  } catch (error) {
    // The message already carries the step, the process state, and the screen;
    // a stack through the wait helpers would bury it.
    failed = error
    process.stderr.write(`\nFAIL ${current?.name ?? 'setup'}: ${(error as Error).message}\n`)
  } finally {
    if (failed !== undefined || values['keep-home']) {
      process.stderr.write(`\nsession logs and workspace kept at ${directory}\n`)
    } else {
      rmSync(directory, { recursive: true, force: true })
    }
  }
  if (failed !== undefined) process.exit(1)
}

await main()
