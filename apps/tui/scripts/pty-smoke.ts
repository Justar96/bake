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
 * through `bun:ffi`, which is why no Python or native module is needed.
 *
 * A failing step writes the whole transcript under `apps/tui/.smoke/` and prints its
 * tail, so the screen that produced the failure survives the run.
 *
 * @module tui-pty-smoke
 */

import { dlopen, FFIType, ptr } from 'bun:ffi'
import { readSync, writeSync, closeSync, existsSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'
import { parseArgs } from 'node:util'
import xterm from '@xterm/headless'

import { COLUMN, MARKER } from '../packages/ui/src/layout.ts'
import { toolLabel } from '../packages/ui/src/present.ts'
import { dictionaries } from '../packages/ui/src/copy.ts'

const ROOT = resolve(import.meta.dir, '../../..')
const FIXTURE = join(ROOT, 'snapshots/session/bash-tool-turn/session.v3.jsonl')
const ARTIFACTS = join(ROOT, 'apps/tui/.smoke')
const ANSI = /\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]/g
/**
 * What the surface draws, taken from the surface instead of copied.
 *
 * `MARKER` and `toolLabel` are the rendering vocabulary itself, so a change there
 * reaches these scenarios without editing them. The rest names a screen element
 * whose owning module exports no constant, so the next vocabulary change is one
 * edit here instead of sixty string literals.
 */
const SCREEN = {
  /** `line.tsx` draws the caret instead of using inverse video, which `NO_COLOR` would erase. */
  caret: '\u258c',
  /**
   * The status line's first field, which names the model. It is drawn from
   * the first frame, whatever the session is doing.
   */
  status: `${dictionaries.en.model}: `,
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

const darwin = process.platform === 'darwin'
/**
 * `fcntl` and `ioctl` are variadic, and Apple arm64 passes variadic arguments
 * on the stack rather than in registers. `bun:ffi` calls are never variadic,
 * so six filler arguments use up the remaining argument registers and push the
 * real third argument to the stack slot the callee reads.
 */
const VARIADIC_PAD: FFIType[] = darwin && process.arch === 'arm64' ? Array(6).fill(FFIType.i64) : []
const pad = VARIADIC_PAD.map(() => 0)
/** `openpty` lives in libutil on Linux and in libSystem on macOS. */
const pty = dlopen(darwin ? 'libSystem.B.dylib' : 'libutil.so.1', {
  openpty: { args: [FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr], returns: FFIType.int },
})
const libc = dlopen(darwin ? 'libSystem.B.dylib' : 'libc.so.6', {
  fcntl: { args: [FFIType.int, FFIType.int, ...VARIADIC_PAD, FFIType.i64], returns: FFIType.int },
  tcgetattr: { args: [FFIType.int, FFIType.ptr], returns: FFIType.int },
  ioctl: { args: [FFIType.int, FFIType.u64, ...VARIADIC_PAD, FFIType.ptr], returns: FFIType.int },
})
const F_GETFL = 3
const F_SETFL = 4
const O_NONBLOCK = darwin ? 0x0004 : 0o4000
/** Oversized and zeroed, so `struct termios` need not be sized per platform. */
const TERMIOS_BYTES = 128

/**
 * Read a session log.
 *
 * @param path - the JSONL file to read.
 * @returns every committed event, in log order.
 */
async function events(path: string): Promise<any[]> {
  const text = await Bun.file(path).text()
  return text.split('\n').filter(line => line !== '').map(line => JSON.parse(line))
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
 * Each wait polls the child's output with the ANSI escapes stripped, which is
 * what the assertions read. Failure throws `StepFailed` naming the step, why it
 * stopped, whether the child is still alive, and where the transcript landed.
 */
class Terminal {
  readonly master: number
  readonly slave: number
  private readonly modes: Uint8Array
  private readonly child: Bun.Subprocess
  private readonly chunks: Uint8Array[] = []
  private readonly deadline: number
  private steps = 0

  constructor(readonly label: string, command: string[], cwd: string,
              env: Record<string, string>, readonly options: Options) {
    const master = new Int32Array(1)
    const slave = new Int32Array(1)
    const winsize = new Uint16Array([40, 120, 0, 0])
    if (pty.symbols.openpty(ptr(master), ptr(slave), null, null, ptr(winsize)) !== 0) {
      throw new Error('openpty failed')
    }
    this.master = master[0]!
    this.slave = slave[0]!
    // Without this a read blocks forever whenever the child has nothing to say,
    // and no step timeout can fire, so a call that did not take is fatal here.
    libc.symbols.fcntl(this.master, F_SETFL, ...pad, O_NONBLOCK)
    if ((libc.symbols.fcntl(this.master, F_GETFL, ...pad, 0) & O_NONBLOCK) === 0) {
      throw new Error('could not make the pty master non-blocking')
    }
    this.modes = new Uint8Array(TERMIOS_BYTES)
    libc.symbols.tcgetattr(this.slave, ptr(this.modes))
    this.child = Bun.spawn(command, { cwd, env, stdin: this.slave, stdout: this.slave, stderr: this.slave })
    this.deadline = performance.now() + options.budget * 1000
  }

  /** Everything the child has written, escapes included. */
  get raw(): string {
    return new TextDecoder().decode(Bun.concatArrayBuffers(this.chunks as unknown as ArrayBuffer[]))
  }

  /** The child's output so far with ANSI escapes removed. */
  get text(): string {
    return this.raw.replace(ANSI, '')
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
   * Absorb whatever the child has written.
   *
   * @returns whether anything arrived.
   */
  private drain(): boolean {
    const buffer = new Uint8Array(65536)
    try {
      const count = readSync(this.master, buffer, 0, buffer.length, null)
      if (count <= 0) return false
      this.chunks.push(buffer.subarray(0, count))
      return true
    } catch (error: any) {
      // EAGAIN is the non-blocking fd saying "nothing yet"; EIO is the slave
      // side closing after the child exits.
      if (error?.code === 'EAGAIN' || error?.code === 'EIO') return false
      throw error
    }
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
    while (!await predicate(this.text)) {
      if (this.raw.includes('failed to import')) this.fail(description, 'a profile plugin failed to import')
      const now = performance.now()
      if (now >= this.deadline) this.fail(description, `the terminal budget of ${this.options.budget}s ran out`)
      if (now >= limit) this.fail(description, `no match within ${timeout ?? this.options.step}s`)
      if (!this.drain() && this.child.exitCode !== null) {
        this.fail(description, `the process exited with code ${this.child.exitCode}`)
      }
      await Bun.sleep(20)
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
    return this.wait('the app to mount and enable bracketed paste',
                     text => text.includes(SCREEN.status) && this.raw.includes('\x1b[?2004h'))
  }

  /**
   * Type into the terminal.
   *
   * @param data - the bytes to write, as text or raw bytes including escape sequences.
   * @param note - what the keys mean, for the trace.
   */
  send(data: string | Uint8Array, note?: string): void {
    const bytes = typeof data === 'string' ? new TextEncoder().encode(data) : data
    this.trace(`send ${note ?? JSON.stringify(typeof data === 'string' ? data : [...bytes])}`)
    writeSync(this.master, bytes)
  }

  /** Resize the PTY and notify its Node renderer even without a controlling terminal. */
  resize(columns: number, rows: number): void {
    const size = new Uint16Array([rows, columns, 0, 0])
    if (libc.symbols.ioctl(this.master, darwin ? 0x80087467 : 0x5414, ...pad, ptr(size)) !== 0) throw new Error('TIOCSWINSZ failed')
    this.child.kill('SIGWINCH')
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
    const exit = this.child.exitCode
    const tail = this.text.split('\n').slice(-30).map(line => `  | ${line}`).join('\n')
    throw new StepFailed(
      `waiting for ${description}\n`
      + `  reason:  ${reason}\n`
      + `  process: ${exit === null ? 'running' : `exited with code ${exit}`}\n`
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
    const limit = performance.now() + this.options.step * 1000
    while (this.child.exitCode === null) {
      if (performance.now() >= limit) {
        this.fail(`the process to exit after ${reason}`, `still running after ${this.options.step}s`)
      }
      this.drain()
      await Bun.sleep(20)
    }
    while (this.drain()) { /* absorb whatever the exit wrote */ }
    this.check(`exit code ${code} after ${reason}`, this.child.exitCode === code && this.child.signalCode === null,
               `exit code ${this.child.exitCode}, signal ${this.child.signalCode}`)
    const after = new Uint8Array(TERMIOS_BYTES)
    libc.symbols.tcgetattr(this.slave, ptr(after))
    this.check('terminal modes to be restored', Buffer.compare(Buffer.from(this.modes), Buffer.from(after)) === 0,
               'the child left the tty in a different mode')
    this.check('bracketed paste to be released',
               this.raw.includes('\x1b[?2004h') && this.raw.includes('\x1b[?2004l'),
               'paste mode was enabled but never released')
    return this.text
  }

  /**
   * Kill any surviving child and release the pty.
   *
   * `Bun.spawn` puts the child in this process's group, so this kills the
   * child alone; a tool subprocess it started can outlive a failed run.
   */
  close(): void {
    if (this.child.exitCode === null) this.child.kill('SIGKILL')
    closeSync(this.master)
    closeSync(this.slave)
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
    this.env = { ...process.env as Record<string, string>, DSH_HOME: this.home,
                 DSH_AGENTS_HOME: join(root, 'agents'), TERM: 'xterm-256color', NO_COLOR: '1' }
    // Replay must not pick up a developer's provider key; CI detection stays intact.
    delete this.env.DEEPSEEK_API_KEY
    delete this.env.CLIPROXYAPI_API_KEY
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
  async writeOverlay(override?: string, profile: { root?: string; compression?: 'none' | 'zstd'; goalMaxRounds?: number; paceMs?: number; balanceBaseURL?: string; cliProxyApi?: boolean } = {}): Promise<void> {
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
      { id: 'agent-default-model', config: { provider: 'deepseek-official', model: 'deepseek-v4-flash' } },
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
   * Build the CLI invocation for one terminal.
   *
   * @param extra - arguments appended after the profile patches.
   * @returns the argv to spawn.
   */
  command(extra: readonly string[], nodeArgs: readonly string[] = []): string[] {
    return [this.node, ...nodeArgs, ...(this.live ? [`--env-file=${join(ROOT, '.env')}`] : []),
            join(ROOT, 'apps/cli/lib/bin.js'), '--profile', 'tui',
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
      terminal.close()
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

scenario('fresh', 'login, model and effort selection, paste, cursor editing, a bash tool turn, the context estimate, and billed tokens', {},
  async run => {
    const before = await run.logs()
    const prompt = run.prompt
    await run.terminal('fresh', [], async tty => {
      tty.send('/lo', 'a partial slash command')
      await tty.search(picked('/login'))
      tty.send('\t', 'Tab to complete it')
      await tty.expect(`> /login ${SCREEN.caret}`)
      tty.send('\r', 'Enter')
      await tty.expect('Choose a sign-in target')
      tty.send('\x1b', 'dismiss the login picker')
      await tty.expect('Sign-in cancelled')
      if (!run.live) {
        tty.send('/model\r')
        await tty.expect('Choose model')
        tty.send('tui-picked-model')
        await tty.search(picked('deepseek-official/tui-picked-model'))
        tty.send('\r', 'Enter to pick the model')
        await tty.expect('Choose reasoning effort: deepseek-official/tui-picked-model')
        tty.send('high')
        await tty.search(picked('high'))
        tty.send('\r', 'Enter to pick the effort')
        await tty.expect('Model set for the next turn: deepseek-official/tui-picked-model (high)')
        await tty.expect(`${SCREEN.status}tui-picked-model  Access workspace-write  Think high`)
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
      await tty.expect('Context: ~')
      // Billed tokens as the provider reported them, cache reads included.
      if (run.live) await tty.search(/ {2}in [\d.]+k? {2}out [\d.]+k?(?: {2}cache hit \d+%)?/)
      else await tty.expect('  in 5.9k  out 115  cache hit 48%')
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
      const opening = await tty.expect('BAKE', `v${version}`, heading, '/help', '/changelog', copy.welcomeChangelog)
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
              if (!text.includes(SCREEN.status) || !text.includes('Context: ~') || !text.includes('in 5.9k')) continue
              for (const field of [SCREEN.status.trim(), 'Context: ~']) {
                const cell = line!.getCell(text.indexOf(field))!
                tty.check(`${field} uses normal foreground`, !cell.isDim() && cell.isFgDefault())
              }
              tty.check('billed input stays dim', !!line!.getCell(text.indexOf('in 5.9k'))!.isDim())
              return true
            }
            return false
          })
          screen.resize(60, 40)
          tty.resize(60, 40)
          await tty.wait('the compact context reading beside access at 60 columns', async () => {
            const raw = tty.raw
            await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
            consumed = raw.length
            const line = screen.buffer.active.getLine(screen.buffer.active.viewportY + screen.rows - 2)?.translateToString(true) ?? ''
            return line.includes('Access workspace-write') && /ctx ~\d+%/.test(line)
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

scenario('plan', 'plan-mode status follows the logged Harness projection', { replayOnly: true },
  async run => {
    const before = await run.logs()
    await run.terminal('plan', [], async tty => {
      tty.send('/plan\r', 'enter plan mode')
      await tty.expect(`${SCREEN.status}deepseek-v4-flash  Plan  `)
      const after = tty.mark()
      tty.send('/plan off\r', 'leave plan mode')
      await tty.expect(`${SCREEN.status}deepseek-v4-flash  Access workspace-write`, after)
    })
    const log = await events(await run.created(before, 'persisted'))
    const modes = log.filter(e => e.type === 'plan/mode').map(e => e.data.active)
    assert(same(modes, [true, false]), 'plan status did not follow the logged mode changes')
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
          line.includes('Model: deepseek-v4-flash') && !line.includes('Think'))
        tty.send('/model deepseek-official/tui-picked-model high\r')
        await footer('explicit high thinking level', line =>
          line.includes('Model: tui-picked-model  Access workspace-write  Think high'))
        screen.resize(40, 12)
        tty.resize(40, 12)
        await footer('full access and thinking indicators at 40 columns', line =>
          line.includes('Access workspace-write  Think high') && !line.includes('tui-picked-model (high)'))
        screen.resize(120, 40)
        tty.resize(120, 40)
        tty.send('/model deepseek-official/tui-picked-model\r')
        await footer('advertised provider default low', line =>
          line.includes('Model: tui-picked-model  Access workspace-write  Think low'))
        tty.send('/model deepseek-official/deepseek-v4-flash\r')
        await footer('unsupported thinking level omitted after switching models', line =>
          line.includes('Model: deepseek-v4-flash') && !line.includes('Think'))
      } finally { screen.dispose() }
    })
    const log = await events(await run.created(before, 'thinking'))
    assert(!log.some(e => e.type === 'request/header'), '/model commands unexpectedly called the model')
  })

scenario('permissions', 'workspace-write default, live permission status at narrow widths, and durable session selection', { replayOnly: true },
  async run => {
    const override = run.env.DSH_PERMISSION_MODE
    delete run.env.DSH_PERMISSION_MODE
    const inspect = async (tty: Terminal, drive: (footer: (mode: string) => Promise<void>, resize: () => void) => Promise<void>) => {
      const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
      let consumed = 0
      const footer = async (mode: string) => {
        await tty.wait(`current footer shows ${mode}`, async () => {
          const raw = tty.raw
          await new Promise<void>(resolve => screen.write(raw.slice(consumed), resolve))
          consumed = raw.length
          const buffer = screen.buffer.active
          return buffer.getLine(buffer.viewportY + screen.rows - 2)?.translateToString(true).includes(`Access ${mode}`) === true
            && buffer.getLine(buffer.viewportY + screen.rows - 1)?.translateToString(true) === ''
        })
      }
      try { await drive(footer, () => { screen.resize(40, 12); tty.resize(40, 12) }) }
      finally { screen.dispose() }
    }
    try {
      const before = await run.logs()
      await run.terminal('permissions', [], tty => inspect(tty, async (footer, resize) => {
        await footer('workspace-write')
        tty.send('/permission read-only\r')
        await footer('read-only')
        tty.send('/permission danger-full-access\r')
        await footer('danger-full-access')
        resize()
        await footer('danger-full-access')
        tty.send('/permission read-only\r')
        await footer('read-only')
      }))
      const path = await run.created(before, 'permission')
      const log = await events(path)
      assert(same(log.filter(e => e.type === 'permission/preset').map(e => e.data.preset),
        ['workspace-write', 'read-only', 'danger-full-access', 'read-only']), 'permission changes did not reach the session log')
      assert(log.find(e => e.type === 'sandbox/mode')?.data.mode === 'workspace-write', 'new session sandbox was not workspace-write')
      assert(log.find(e => e.type === 'approval/policy')?.data.policy === 'ask', 'new session approval policy was not ask')
      const beforeNew = await run.logs()
      await run.terminal('permissions-resume', ['--resume', log[0].id], tty => inspect(tty, async footer => {
        await footer('read-only')
        tty.send('/new\r')
        await footer('workspace-write')
      }))
      const fresh = await events(await run.created(beforeNew, 'new permission'))
      assert(fresh.find(e => e.type === 'permission/preset')?.data.preset === 'workspace-write', '/new did not use the configured default')
      for (const record of [await events(path), fresh]) {
        assert(!record.some(e => e.type === 'request/header'), 'permission commands unexpectedly called the model')
      }
      run.env.DSH_PERMISSION_MODE = 'read-only'
      await run.terminal('permissions-override', [], tty => inspect(tty, async footer => {
        await footer('read-only')
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
        await tty.expect('[x] 1. Alpha')
        tty.send('A custom method', 'start an Other answer')
        await tty.expect('Other answer: A custom method')
        tty.send('\x1b[A', 'inspect the choices while keeping the draft')
        await tty.search(picked('[ ] 2. Beta'))
        await tty.expect('Other answer: A custom method')
        tty.send('\x1b[B\r', 'return to Other and submit the combined answer')
        await tty.expect('Question answered.')
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

scenario('arrow-wave', 'the single-line processing wave loops in place and yields to a short composer',
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
          await tty.wait('all six arrow frames at the same position', async () => {
            const rows = await capture()
            const header = rows.findIndex(line => /^[\u2800-\u283f]{3} \S+…/.test(line))
            if (header < 1) return false
            // PTY reads may end mid-frame, before its final scroll anchors the controls.
            // Header, upper rule, input, base rule, then the status line.
            if (header !== 34 || !rows[35]!.startsWith('\u2500') || !rows[36]!.startsWith('> ')
              || !rows[37]!.startsWith('\u2500') || !rows[38]!.includes(SCREEN.status)) return false
            tty.check('there is no dot zone above the processing line', !rows.some(line => /^[\u2800-\u283f]{3}$/.test(line)))
            frames.add(rows[header]!.slice(0, 3))
            return frames.size === 6
          })
          tty.check('the wave stays in the header directly above the input', frames.size === 6)
          const mark = tty.raw.length
          screen.resize(40, 4)
          tty.resize(40, 4)
          await tty.wait('the short terminal keeps its input visible', async () => {
            const rows = await capture()
            // Three rows once the rules have yielded. The header, the input, and the status line.
            return tty.raw.length > mark && !rows[0]!.startsWith('─') && rows[1]!.includes('> ') && rows[2]!.includes(SCREEN.status)
          })
          screen.resize(80, 24)
          tty.resize(80, 24)
          await tty.follows(SCREEN.idle, 'WAVE_DONE')
          const rows = await capture()
          tty.check('completion removes the dot field', !rows.some(line => /[\u2800-\u283f]/.test(line)))
          tty.check('the completed composer stays at the bottom', rows[22]!.includes(SCREEN.status))
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

scenario('markdown', 'streamed Markdown formats once, survives resize and resume, and preserves the logged source',
  { replayOnly: true }, async run => {
    const reasoning = '**Review** the formatter.'
    const answer = '# Formatted response\n\n**Ready** with `snake_case` and [docs](https://example.com).\n\n'
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
    await run.writeOverlay(override)
    const checkScreen = (screen: InstanceType<typeof xterm.Terminal>): void => {
      const lines = Array.from({ length: screen.buffer.active.length }, (_, row) => screen.buffer.active.getLine(row)?.translateToString(true) ?? '')
      assert(lines.filter(line => line === '  Formatted response').length === 1, 'Markdown heading was lost or printed twice')
      const text = lines.join('\n')
      assert(text.includes('Review the formatter.') && text.includes('Check: Stream') && text.includes('State: ready'), 'reasoning or table was not formatted')
      assert(text.includes('const snake_case = "**literal**"') && !text.includes('```'), 'code was parsed as prose or retained its fences')
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
          checkScreen(screen)
          const mark = tty.raw.length
          screen.resize(40, 12)
          tty.resize(40, 12)
          await tty.wait('formatted history and composer after resize', async () => {
            await capture()
            return tty.raw.length > mark && screen.buffer.active.getLine(screen.buffer.active.viewportY + 10)?.translateToString(true).includes(SCREEN.status) === true
          })
          checkScreen(screen)
        } finally { screen.dispose() }
      })
      const path = await run.created(before, 'Markdown')
      const log = await events(path)
      const messages = log.filter(event => event.type === 'assistant/message').map(event => event.data.message.content)
      assert(same(messages, [[{ type: 'reasoning', text: reasoning }, { type: 'text', text: answer }]]), 'formatting changed the logged model source')
      await run.terminal('markdown-resume', ['--resume', log[0].id], async tty => {
        await tty.expect('FORMATTER_DONE')
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        try {
          await new Promise<void>(resolve => screen.write(tty.raw, resolve))
          checkScreen(screen)
        } finally { screen.dispose() }
      })
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
      delete run.env.DEEPSEEK_API_KEY
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

scenario('cliproxyapi', 'the built TUI configures a CLIProxyAPI URL and key and selects its models',
  { replayOnly: true },
  async run => {
    const before = await run.logs()
    const requests: Array<{ path: string, query: string, authorization: string | null, model?: string }> = []
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
      requests.push({ path: url.pathname, query: url.search, authorization: request.headers.get('authorization') })
      return Response.json({ models: [{ slug: 'gpt-test', display_name: 'GPT Test', context_window: 128000 }] })
    } })
    await run.writeOverlay(undefined, { cliProxyApi: true })
    try {
      const transcript = await run.terminal('cliproxyapi', [], async tty => {
        tty.send('/login\r', 'choose a login target')
        await tty.expect('Choose a sign-in target')
        await tty.expect('CLIProxyAPI', 'Not set', 'URL + API key')
        tty.send('\r', 'choose CLIProxyAPI')
        await tty.expect('1/2 · CLIProxyAPI base URL')
        tty.send(`${server.url.toString()}\r`, 'set the proxy URL')
        await tty.expect('2/2 · CLIProxyAPI API key')
        tty.send('smoke-proxy-key\r', 'store the proxy key')
        await tty.expect('cliproxyapi: 1 model ready; choose it with /model')
        tty.send('/model cliproxyapi/gpt-test\r', 'select the discovered model')
        await tty.expect('Model set for the next turn: cliproxyapi/gpt-test')
        tty.send('Say PROXY_OK\r', 'run one turn through the configured proxy')
        await tty.expect('  PROXY_OK')
        await tty.expect('✓ Completed')
      })
      assert(!transcript.includes('smoke-proxy-key'), 'CLIProxyAPI secret appeared on the terminal')
      assert(!transcript.includes('Could not parse message into JSON'), 'empty SSE framing leaked an SDK parse error')
    } finally {
      await run.writeOverlay()
      server.stop(true)
    }
    assert(same(requests, [
      { path: '/v1/models', query: '?client_version=pi', authorization: 'Bearer smoke-proxy-key' },
      { path: '/v1/responses', query: '', authorization: 'Bearer smoke-proxy-key', model: 'gpt-test' },
      { path: '/v1/responses', query: '', authorization: 'Bearer smoke-proxy-key', model: 'gpt-test' },
    ]), 'CLIProxyAPI did not validate the catalog and send the selected model to the entered proxy')
    assert(!(await Bun.file(join(run.home, 'settings.yaml')).text()).includes('smoke-proxy-key'),
      'CLIProxyAPI secret was saved in model settings')
    const log = await events(await run.created(before, 'proxy retry'))
    assert(log.filter(event => event.type === 'user/message' && event.data.source.kind === 'user').length === 1,
      'proxy retry duplicated user input')
    const retries = log.filter(event => event.type === 'llm/retry')
    assert(retries.length === 1 && retries[0]!.data.failure.code === 'TRANSPORT',
      'missing SSE completion did not produce exactly one transport retry')
    assert(log.filter(event => event.type === 'assistant/message').length === 1, 'proxy retry persisted an extra assistant message')
    assert(log.some(event => event.type === 'turn/end' && event.data.reason.kind === 'completed'),
      'proxy recovery did not complete the turn')
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

scenario('inspect-agent', 'select a saved child, read its session, and return with the parent draft intact',
  { requires: ['fresh'], replayOnly: true },
  async run => {
    const source = await events(run.state.log)
    const parentId = 'tui-inspection-parent'
    const parentPath = join(dirname(dirname(run.state.log)), parentId, basename(run.state.log))
    mkdirSync(dirname(parentPath), { recursive: true })
    await Bun.write(parentPath, [{ ...source[0], id: parentId }, ...source.slice(1)].map(event => JSON.stringify(event)).join('\n') + '\n')
    const childId = 'tui-inspected-child'
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
      await tty.expect('Review terminal output', 'Completed · Saved', 'Ctrl+G /agents')
      tty.send('Keep this parent draft', 'write a draft before inspecting a child')
      await tty.expect(`> Keep this parent draft${SCREEN.caret}`)
      let start = tty.mark()
      tty.send('\x07', 'Ctrl+G opens the child picker')
      await tty.expect('Select a child to view its session', 'Review terminal output', start)
      tty.send('\r', 'open the selected child session')
      await tty.expect(`Parent: ${parentId}`, 'Read-only', SCREEN.toolResult, 'Access read-only', 'Context: ~', start)
      tty.send('must not reach the model\r', 'inspection does not accept prompts')
      start = tty.mark()
      tty.send('\x1b', 'return to the running parent without cancelling it')
      await tty.expect(`> Keep this parent draft${SCREEN.caret}`, 'Access workspace-write', start)
      tty.send('!', 'continue editing the parent draft')
      await tty.expect(`> Keep this parent draft!${SCREEN.caret}`, start)
    })
    assert(await Bun.file(path).text() === recorded, 'inspection modified the saved child')
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
        tty.send('/goal Complete this test task\r', 'create a goal')
        await tty.expect('Goal created', goalStart)
        // The header names the goal once `/goal` arms it. The base rule is only a line.
        await tty.expect('\u25cf Goal active', goalStart)
        const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
        try {
          await new Promise<void>(resolve => screen.write(tty.raw, resolve))
          const visible = Array.from({ length: 40 }, (_, row) =>
            screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.translateToString(true) ?? '').join('\n')
          assert(visible.includes('Goal created'), 'goal result is absent from the current terminal viewport')
        } finally { screen.dispose() }
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

scenario('compact-history', 'manual compaction works after completed replayed turns',
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
    const override = join(run.root, 'compact-summary.json')
    await Bun.write(override, JSON.stringify({ patches: [{ at: 2, entry: { kind: 'chunks', chunks: [
      { type: 'block-start', index: 0, blockType: 'text' },
      { type: 'text-delta', index: 0, text: 'The previous work completed successfully.' },
      { type: 'block-end', index: 0, block: { type: 'text', text: 'The previous work completed successfully.' } },
      { type: 'finish', reason: { kind: 'stop' } },
    ] } }] }))
    await run.writeOverlay(override, { paceMs: 120 })
    try {
      await run.terminal('compact-history', ['--resume', id], async tty => {
        const start = tty.mark()
        tty.send(`${run.prompt}\r`, 'add another completed turn')
        await tty.wait('the replayed turn to finish', text => DONE_LINE.test(text.slice(start)))
        await tty.follows(SCREEN.idle, 'DONE')
        tty.send('/compact\r', 'compact completed history')
        await tty.expect('Compacting history…  summarizing')
        tty.send('hold this draft\r', 'try to submit while compaction owns the session')
        await tty.expect('Wait for compaction to finish, or press Esc to cancel')
        await tty.search(/Compacted \d+ history items/)
        // The result prints in the same write as the frame redrawn under it,
        // and a PTY read can end part-way through that write, so the viewport
        // is judged once the frame has arrived, not the moment the text does.
        // `wait` drains the terminal between checks; the assertions below say
        // which half never arrived.
        let visible = ''
        try {
          await tty.wait('the compaction result and the held draft in the viewport', async () => {
            const screen = new xterm.Terminal({ cols: 120, rows: 40, convertEol: true, allowProposedApi: true })
            try {
              await new Promise<void>(resolve => screen.write(tty.raw, resolve))
              visible = Array.from({ length: 40 }, (_, row) =>
                screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.translateToString(true) ?? '').join('\n')
            } finally { screen.dispose() }
            return visible.includes('Compacted') && visible.includes('hold this draft▌')
          }, 5)
        } catch {
          // Reported by the assertions, with what the viewport last showed.
        }
        assert(visible.includes('Compacted'), 'compaction result is absent from the current terminal viewport')
        assert(visible.includes('hold this draft▌'), 'the draft was lost while compaction was running')
      })
    } finally { await run.writeOverlay() }
    const log = await events(path)
    assert(log.some(event => event.type === 'compaction/summary'), '/compact did not commit a summary')
    assert(!log.some(event => event.type === 'user/message' && event.data.content.some((block: any) => block.type === 'text' && block.text === 'hold this draft')),
      'input submitted during compaction entered model history')
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
          return visible.length === 40 && visible[38]?.includes(SCREEN.status) === true && visible[39] === ''
        })
        const shrunk = await resize(40, 4)
        await tty.wait('composer remains visible at 40x4', async () => {
          const visible = (await shown()).split('\n')
          return tty.raw.length > shrunk && visible[1]?.includes(`> ${SCREEN.caret}`) === true && visible[2]?.length === 40
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
            && visible[caret + 1]?.startsWith('\u2500') === true && visible[caret + 2]?.includes(SCREEN.status) === true
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
      if (!run.live) await tty.expect(`${SCREEN.status}tui-picked-model  Access workspace-write  Think high`)
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
      const hints = tty.mark()
      tty.send('/', 'open the frequent slash commands')
      const menu = await tty.expect('/model', '/resume', '/new', '/clear', hints)
      const shown = menu.slice(hints)
      tty.check('frequent commands lead the slash menu', ['/model', '/resume', '/new', '/clear']
        .map(name => shown.indexOf(name)).every((position, index, positions) => position >= 0
          && (index === 0 || position > positions[index - 1]!)))
      const closed = tty.mark()
      tty.send('\x1b', 'close slash hints')
      await tty.expect(`> /${SCREEN.caret}`, closed)
      tty.send('\x7f', 'remove the slash draft')
      await tty.expect(`> ${SCREEN.caret}`)
      tty.send('/help\r', 'list session commands')
      await tty.search(/\/resume\b.* {2}Browse sessions or start a new one/u)
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
      await tty.expect(`${SCREEN.status}deepseek-v4-flash`, start)
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
      await tty.expect(SCREEN.toolResult, `${SCREEN.status}tui-picked-model  Access workspace-write  Think high`, start)
      await tty.follows(SCREEN.idle, SCREEN.toolResult)

      start = tty.mark()
      tty.send('\x1b[A', 'Up, recalling the last command')
      await tty.expect(`> /sessions${SCREEN.caret}`, start)
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

scenario('cancel', 'skill and quoted-file completion, steering a running turn, interruption, and discarding queued input',
  { replayOnly: true },
  async run => {
    const skill = join(run.workspace, '.agents/skills/tui-smoke/SKILL.md')
    mkdirSync(dirname(skill), { recursive: true })
    await Bun.write(skill, '---\nname: tui-smoke\ndescription: Terminal skill smoke\n'
                         + 'disable-model-invocation: true\n---\n\nTUI_SKILL_INSTRUCTIONS\n')
    const referenced = join(run.workspace, 'notes folder/read me.txt')
    mkdirSync(dirname(referenced), { recursive: true })
    await Bun.write(referenced, 'TUI_FILE_CONTENT_MUST_NOT_BE_INJECTED')
    // The replay hangs on this turn so the composer stays live while the agent runs.
    const ready = join(run.root, 'stream-ready')
    const override = join(run.root, 'cancel.json')
    await Bun.write(override, JSON.stringify([{ kind: 'hang', readyFile: ready }]))
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
      tty.send('Discard this steering\r', 'steering typed into a running turn')
      await tty.expect('Next step: Discard this steering', '/clear-pending discards queued input')
      tty.send('\x1b', 'Escape to interrupt the turn')
      await tty.follows(SCREEN.idle, 'Interrupted')
      tty.send('/clear-pending\r')
      await tty.expect('Queued input discarded')
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
    assert(log.some(e => e.type === 'agent/inbox/spliced' && e.data?.outcome === 'canceled'
                    && e.data?.removedCount === 1), 'discard did not persist an inbox removal')
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
    const tty = new Terminal('fatal-exception', run.command([], ['--import', preload]),
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
      tty.close()
    }
  })

scenario('resume-cleared',
  'a resumed session showing the discard and neither the discarded steering nor stale pending input',
  { requires: ['cancel'], replayOnly: true },
  async run => {
    const text = await run.terminal('resume-cleared', ['--resume', run.state.cancelledId], async tty => {
      await tty.expect('Queued input discarded', SCREEN.idle, '@"notes folder/read me.txt"')
    })
    assert(!text.includes('Discard this steering'), 'discarded steering returned after resume')
    assert(!text.includes('/clear-pending discards queued input'), 'resume shows stale pending input')
    const log = await events(run.state.cancelledLog)
    assert(log.filter(e => e.type === 'turn/start').length === 1, 'resume drove discarded input')
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
