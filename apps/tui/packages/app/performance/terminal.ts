/** Bounded native PTY observations. Bun owns the terminal, and Node runs the measured app. */
import { existsSync, readFileSync } from 'node:fs'
import { readFile, rename, writeFile } from 'node:fs/promises'
import { setTimeout as delay } from 'node:timers/promises'
import { dictionaries } from '../../ui/src/copy.ts'
import type { Metrics } from './report.ts'
const ANSI = /\x1b\][^\x07]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]/g

/** Dedicated terminal observation with bounded capture. Historical output is counted without retaining it all. */
export class Terminal {
  private readonly child: Bun.Subprocess
  private readonly done: Promise<number>
  private exit: { code: number; signal: NodeJS.Signals | null } | undefined
  private streamError = false
  private markerTail = ''
  private sampleNumber = 0
  text = ''
  bytes = 0
  markerOccurrences = 0
  readonly markers = new Set<string>()

  /**
   * Start one measured child. Callers must await `close` even after a failed observation.
   * @param command - measured Node argv, including the metrics preload.
   * @param cwd - private workspace directory.
   * @param metrics - private sample file published by the preload.
   * @param env - environment for the measured process.
   * @param signal - optional observation cancellation. `close` still drains the process.
   */
  constructor(command: readonly string[], cwd: string, private readonly metrics: string, env: NodeJS.ProcessEnv, private readonly signal?: AbortSignal) {
    const decoder = new TextDecoder()
    this.child = Bun.spawn([...command], { cwd, env, terminal: {
      cols: 120, rows: 40,
      data: (_terminal, chunk) => {
        this.bytes += chunk.length
        const raw = decoder.decode(chunk, { stream: true })
        const joined = this.markerTail + raw
        for (const match of joined.matchAll(/H\d{5}_END/g)) {
          if (match.index + match[0].length > this.markerTail.length) {
            this.markerOccurrences++
            this.markers.add(match[0])
          }
        }
        this.markerTail = joined.slice(-20)
        this.text = (this.text + raw).slice(-131_072)
      },
      // PTY status describes EOF or a read failure, not the child process exit code.
      exit: (_terminal, code) => { this.streamError = code !== 0 },
    } })
    // Bun leaves `exitCode` null after a signal. `exited` settles for both exit forms.
    this.done = this.child.exited.then(code => {
      this.exit = { code, signal: this.child.signalCode }
      return code
    })
  }

  /** Measured Node process id. No shell or PTY helper process intervenes. */
  get pid(): number { return this.child.pid }

  /**
   * Observe readiness, rejecting on process failure, cancellation, or deadline.
   * @param description - completion condition included in failure diagnostics.
   * @param condition - externally observed state to await.
   * @param timeoutMs - maximum wait duration.
   * @returns after the condition holds.
   */
  async wait(description: string, condition: () => boolean, timeoutMs = 60_000): Promise<void> {
    const start = performance.now()
    while (true) {
      this.signal?.throwIfAborted()
      if (condition()) return
      if (this.exit !== undefined) throw new Error(`${description}: Node exited ${JSON.stringify(this.exit)}\n${this.clean.slice(-8000)}`)
      if (this.streamError) throw new Error(`${description}: PTY read failed\n${this.clean.slice(-8000)}`)
      if (performance.now() - start > timeoutMs) throw new Error(`${description}: timed out\n${this.clean.slice(-8000)}`)
      await delay(2)
    }
  }

  /** Bounded output tail with terminal escape sequences removed. */
  get clean(): string { return this.text.replace(ANSI, '') }

  /**
   * Write terminal input without awaiting a frame.
   * @param text - exact bytes to send.
   */
  send(text: string): void { this.child.terminal!.write(text) }

  /**
   * Measure input delivery through a newly observed composer echo.
   * @param text - input bytes.
   * @param expected - composer text required in subsequent output.
   * @returns elapsed milliseconds through observed PTY output, not display paint.
   */
  async input(text: string, expected: string): Promise<number> {
    this.text = ''
    const start = performance.now()
    this.send(text)
    await this.wait('composer echo', () => this.clean.includes(expected))
    return performance.now() - start
  }

  /**
   * Request an explicit GC sample while the session remains reachable, through
   * the request file `metrics.mjs` polls.
   * @returns memory and cumulative resource counters from the measured main process.
   */
  async sample(): Promise<Metrics> {
    const number = ++this.sampleNumber
    await writeFile(`${this.metrics}.request.tmp`, String(number))
    await rename(`${this.metrics}.request.tmp`, `${this.metrics}.request`)
    await this.wait('memory sample', () => existsSync(this.metrics) && (JSON.parse(readFileSync(this.metrics, 'utf8')) as Metrics).sequence === number)
    return JSON.parse(await readFile(this.metrics, 'utf8')) as Metrics
  }

  /**
   * Exercise normal terminal shutdown and require the measured Node process to exit cleanly.
   * @returns after Node exits. `close` releases the terminal separately.
   */
  async quit(): Promise<void> {
    this.text = ''
    this.send('\x03')
    await this.wait('quit hint', () => this.clean.includes(dictionaries.en.quit))
    this.send('\x03')
    await this.wait('clean process exit', () => this.exit !== undefined)
    const code = await this.done
    if (code !== 0 || this.child.signalCode !== null || this.streamError) throw new Error(`Unclean exit: ${JSON.stringify({ code, signal: this.child.signalCode, streamError: this.streamError })}`)
  }

  /**
   * Kill remaining measured work, await its exit, and release the PTY. Safe after a normal exit.
   * @returns after Node has been reaped and the terminal is closed.
   */
  async close(): Promise<void> {
    try {
      if (this.exit === undefined) this.child.kill('SIGKILL')
      await this.done
    } finally { this.child.terminal!.close() }
  }
}
