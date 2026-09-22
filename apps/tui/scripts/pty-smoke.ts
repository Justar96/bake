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

import { MARKER, VERB } from '../packages/ui/src/layout.ts'
import { dictionaries } from '../packages/ui/src/copy.ts'

const ROOT = resolve(import.meta.dir, '../../..')
const FIXTURE = join(ROOT, 'snapshots/session/bash-tool-turn/session.v3.jsonl')
const ARTIFACTS = join(ROOT, 'apps/tui/.smoke')
const ANSI = /\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]/g
/**
 * What the surface draws, taken from the surface rather than copied.
 *
 * `MARKER` and `VERB` are the rendering vocabulary itself, so a change there
 * reaches these scenarios without editing them. The rest names a screen element
 * whose owning module exports no constant, so the next vocabulary change is one
 * edit here instead of sixty string literals.
 */
const SCREEN = {
  /** `line.tsx` draws the caret rather than using inverse video, which NO_COLOR would erase. */
  caret: '\u258c',
  /**
   * Idle status. The two spaces are the gap between status fields, so a
   * transcript sentence that contains the word does not match.
   */
  idle: `${dictionaries.en.ready}  `,
  /** The composer's prompt rail. */
  prompt: `${MARKER.prompt} `,
  /**
   * A started tool call: the verb followed by its arguments. The verb alone
   * would match the recorded prompt, which asks the model to run a command.
   */
  toolCall: new RegExp(`\\b${VERB.run}\\s+\\{`),
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
/** `openpty` lives in libutil on Linux and in libSystem on macOS. */
const pty = dlopen(darwin ? 'libSystem.B.dylib' : 'libutil.so.1', {
  openpty: { args: [FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr], returns: FFIType.int },
})
const libc = dlopen(darwin ? 'libSystem.B.dylib' : 'libc.so.6', {
  fcntl: { args: [FFIType.int, FFIType.int, FFIType.int], returns: FFIType.int },
  tcgetattr: { args: [FFIType.int, FFIType.ptr], returns: FFIType.int },
  ioctl: { args: [FFIType.int, FFIType.u64, FFIType.ptr], returns: FFIType.int },
})
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
    // Without this a read blocks forever whenever the child has nothing to say.
    libc.symbols.fcntl(this.master, F_SETFL, O_NONBLOCK)
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
   * Wait for a mounted app: the status line drawn and paste mode enabled.
   *
   * Keys typed before Ink enables bracketed paste are swallowed by the terminal
   * and never reach `useInput`, so every scenario starts here.
   *
   * @returns the screen once the app accepts input.
   */
  async ready(): Promise<string> {
    return this.wait('the app to mount and enable bracketed paste',
                     text => text.includes(SCREEN.idle) && this.raw.includes('\x1b[?2004h'))
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

  /** Trigger a process-local fixture after the interactive surface is ready. */
  signal(name: 'SIGUSR2'): void {
    this.child.kill(name)
  }

  /** Resize the PTY and notify its Node renderer even without a controlling terminal. */
  resize(columns: number, rows: number): void {
    const size = new Uint16Array([rows, columns, 0, 0])
    if (libc.symbols.ioctl(this.master, darwin ? 0x80087467 : 0x5414, ptr(size)) !== 0) throw new Error('TIOCSWINSZ failed')
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
  /** Whether it depends on the recorded fixture rather than a live model. */
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
   * @param persistence - storage root and encoding for a scenario-specific profile.
   */
  async writeOverlay(override?: string, persistence: { root?: string; compression?: 'none' | 'zstd' } = {}): Promise<void> {
    const replay: any = {
      id: 'tui-replay', name: join(ROOT, 'packages/test-support/llm-replay/lib/index.js'),
      config: { file: FIXTURE, providers: [{ id: 'deepseek-official', models: [
        { id: 'deepseek-v4-flash', contextWindow: 128000 },
        { id: 'tui-picked-model', contextWindow: 128000,
          reasoningEfforts: ['low', 'high'], defaultReasoningEffort: 'low' }] }] },
    }
    if (override !== undefined) replay.config.overrideFile = override
    const patches: any[] = [
      { id: 'session-title-llm', disabled: true },
      { id: 'agent-default-model', config: { provider: 'deepseek-official', model: 'deepseek-v4-flash' } },
      { id: 'session-persistence-jsonl', config: {
        root: persistence.root ?? this.sessionsRoot, compression: persistence.compression ?? 'none',
      } },
    ]
    await Bun.write(this.overlay, JSON.stringify(this.live ? patches : [
      { id: 'llm-deepseek', disabled: true }, { id: 'llm-pi-ai', disabled: true },
      ...patches, { insert: [replay] }]))
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

/** Compare two values the way the recorded fixtures are compared: by content. */
function same(left: unknown, right: unknown): boolean {
  return Bun.deepEquals(left, right)
}

const DONE_LINE = new RegExp(`\\n${MARKER.reply} DONE\\r?\\n`)

scenario('fresh', 'login, model and effort selection, paste, cursor editing, a bash tool turn, and the context estimate', {},
  async run => {
    const before = await run.logs()
    const prompt = run.prompt
    await run.terminal('fresh', [], async tty => {
      tty.send('/lo', 'a partial slash command')
      await tty.search(picked('/login'))
      tty.send('\t', 'Tab to complete it')
      await tty.expect(`> /login ${SCREEN.caret}`)
      tty.send('\r', 'Enter')
      await tty.expect('/login DEEPSEEK_API_KEY')
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
    assert(same(resultText(results), resultText(expectedResults)), 'real tool output disagrees with the recording')
    Object.assign(run.state, { log: path, id: log[0].id, model, headers })
  })

scenario('plan', 'plan-mode status follows the logged Harness projection', { replayOnly: true },
  async run => {
    const before = await run.logs()
    await run.terminal('plan', [], async tty => {
      tty.send('/plan\r', 'enter plan mode')
      await tty.search(/Ready  Plan  /)
      const after = tty.mark()
      tty.send('/plan off\r', 'leave plan mode')
      await tty.search(/Ready  deepseek-v4-flash  /, after)
    })
    const log = await events(await run.created(before, 'persisted'))
    const modes = log.filter(e => e.type === 'plan/mode').map(e => e.data.active)
    assert(same(modes, [true, false]), 'plan status did not follow the logged mode changes')
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
        await tty.wait('resumed answer in the terminal buffer', async () => (await capture()).some(line => line === '< DONE'))
        const shrunk = await resize(40, 4)
        await tty.wait('composer remains visible at 40x4', async () => {
          const visible = (await shown()).split('\n')
          return tty.raw.length > shrunk && visible[1]?.includes(`> ${SCREEN.caret}`) === true && visible[2]?.length === 40
        })
        const history = await capture()
        assert(history.filter(line => line === '< DONE').length === 1, 'resize lost or duplicated the resumed answer')
        tty.send(`\x1b[200~START ${'word '.repeat(100)}END\x1b[201~`, 'a wrapped draft')
        await tty.wait('end of the wrapped draft', async () => (await shown()).includes(`END${SCREEN.caret}`))
        tty.send('\x1b[H', 'Home inside the wrapped draft')
        await tty.wait('caret at the start of the wrapped draft', async () => (await shown()).includes(`${SCREEN.caret}START`))
        tty.send('\x1b[F', 'End inside the wrapped draft')
        await tty.wait('caret returns to the end', async () => (await shown()).includes(`END${SCREEN.caret}`))
        const expanded = await resize(120, 40)
        await tty.wait('expanded composer keeps its draft', async () => {
          // The caret row, the frame's bottom edge, then the status line: the
          // composer follows the history rather than a fixed terminal row.
          const visible = (await shown()).split('\n')
          const caret = visible.findIndex(line => line.includes(`END${SCREEN.caret}`))
          return tty.raw.length > expanded && caret > visible.indexOf('< DONE')
            && visible[caret + 1]?.startsWith('╰') === true && visible[caret + 2]?.includes(SCREEN.idle) === true
        })
        assert((await capture()).filter(line => line === '< DONE').length === 1, 'expanding the terminal lost or duplicated history')
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
      if (!run.live) await tty.expect('tui-picked-model (high)')
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
    const log = await events(run.state.log)
    assert(log.filter(e => e.type === 'assistant/message').length === run.state.model.length,
           'resume made an unsolicited model call')
  })

scenario('navigate', 'session picker cancellation, a new session, and switching back to committed history',
  { requires: ['fresh'], replayOnly: true },
  async run => {
    const before = await run.logs()
    const identity = run.state.id
    const text = await run.terminal('navigate', ['--resume', identity], async tty => {
      let start = tty.mark()
      tty.send('/sessions\r')
      await tty.expect('Choose session', start)
      tty.send('\x1b', 'Escape to cancel the picker')
      await tty.expect('Session navigation cancelled', start)

      start = tty.mark()
      tty.send('/sessions\r')
      await tty.expect('Choose session', start)
      tty.send('New session')
      await tty.search(picked('New session'), start)
      tty.send('\r', 'Enter to start a new session')
      const created = await tty.search(/Session: (session-[a-f0-9-]+)/, start)
      assert(created[1] !== identity, 'new-session selection reused the current identity')
      // Both fields must belong to the new footer. An older Ready row plus
      // the new model's still-busy row would send keys during retirement.
      await tty.expect(`${SCREEN.idle}deepseek-v4-flash`, start)

      start = tty.mark()
      tty.send('/sessions\r')
      await tty.expect('Choose session', start)
      tty.send(identity, 'the original session id')
      await tty.expect(`> ${identity}${SCREEN.caret}`, start)
      tty.send('\r', 'Enter to switch back')
      await tty.expect(SCREEN.toolResult, 'tui-picked-model (high)', start)
      await tty.follows(SCREEN.idle, SCREEN.toolResult)

      start = tty.mark()
      tty.send('\x1b[A', 'Up, recalling the last command')
      await tty.expect(`> /sessions${SCREEN.caret}`, start)
      tty.send('\x1b[B', 'Down, back to an empty composer')
      await tty.expect(`> ${SCREEN.caret}`, start)
    })

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
    const preload = join(run.root, 'fatal-exception.mjs')
    await Bun.write(preload,
      "process.on('SIGUSR2', () => { throw Object.assign(new Error('PTY_FATAL_EXCEPTION'), { code: 'TUI_FATAL_TEST' }) })\n")
    const tty = new Terminal('fatal-exception', run.command([], ['--import', preload]),
      run.workspace, run.env, run.options)
    try {
      await tty.ready()
      tty.signal('SIGUSR2')
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
