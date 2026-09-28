/** Sampling, thresholds, records, and fatal-error forensics for one mounted watchdog. */

import { appendFileSync, mkdirSync, realpathSync } from 'node:fs'
import { join, resolve } from 'node:path'

/**
 * Node's event-loop delay sampling period. Each sample wakes the loop, so a
 * coarser period costs less idle CPU; stalls near the delay threshold still
 * resolve to within one period. Node's histogram values include this period.
 */
export const EVENT_LOOP_RESOLUTION_MS = 50

const NS_PER_MS = 1e6
const MIB = 1024 * 1024

/** The slice of Node's event-loop delay histogram the watchdog reads; values are nanoseconds. */
export interface DelayHistogram {
  enable(): unknown
  disable(): unknown
  reset(): void
  percentile(percentile: number): number
  readonly max: number
  readonly mean: number
  readonly count: number
}

/** One V8 heap reading, in bytes. */
export interface HeapSample {
  readonly used: number
  readonly total: number
  readonly limit: number
}

/** The `process.report` fields the watchdog sets while mounted and restores on disposal. */
export interface ReportSettings {
  directory: string
  reportOnFatalError: boolean
  excludeEnv: boolean
  excludeNetwork: boolean
}

/** Node process facilities, injected so tests control samples, the clock, and process-global state. */
export interface WatchdogHost {
  /** Monotonic milliseconds since process start. */
  now(): number
  /** Create a disabled event-loop delay histogram sampling every `resolutionMs`. */
  eventLoopDelay(resolutionMs: number): DelayHistogram
  heap(): HeapSample
  rss(): number
  readonly report: ReportSettings
  /** Arms V8's near-limit heap snapshot for the rest of the process; Node ignores later calls. */
  setHeapSnapshotNearHeapLimit(count: number): void
  readonly execArgv: readonly string[]
  /** The `NODE_OPTIONS` value the process started with. */
  readonly nodeOptions: string | undefined
  readonly pid: number
}

/** Where human-readable watchdog warnings go; the Cordis logger in a mounted plugin. */
export interface WatchdogLogger {
  warn(format: string, ...param: unknown[]): void
}

/** Validated watchdog settings; the plugin `Config` documents each field. */
export interface WatchdogOptions {
  readonly directory: string
  readonly intervalMs: number
  readonly eventLoopDelayMs: number
  readonly sustainedMs: number
  readonly heapFraction: number
  readonly recordIntervalMs: number
  readonly fatalErrorReport: boolean
  readonly heapSnapshots: number
}

/** The threshold a record reports. */
export type WatchdogRecordKind = 'event-loop-delay' | 'heap-limit'

/** One line of a watchdog record file. Durations are milliseconds; memory figures are bytes. */
export interface WatchdogRecord {
  readonly time: string
  readonly kind: WatchdogRecordKind
  readonly pid: number
  readonly uptimeMs: number
  readonly eventLoop: {
    readonly p50Ms: number
    readonly p99Ms: number
    /** The longer of the histogram's maximum and the lateness of the sample that closed the window. */
    readonly maxMs: number
    readonly meanMs: number
    readonly samples: number
    /** Length of the sampling window these statistics cover. */
    readonly windowMs: number
    /** How long consecutive windows have stayed at or above the delay threshold; 0 when this one did not. */
    readonly delayedForMs: number
  }
  readonly memory: {
    readonly heapUsed: number
    readonly heapTotal: number
    readonly heapLimit: number
    readonly rss: number
    /** Highest sampled values since the watchdog started. */
    readonly peakHeapUsed: number
    readonly peakRss: number
  }
  /** Threshold crossings of this kind left unrecorded by the rate limit since the previous record. */
  readonly suppressed: number
}

/**
 * Start sampling, and arm fatal-error forensics when configured and safe:
 * reports need Node's `--report-exclude-env`, and heap snapshots need its
 * `--diagnostic-dir` to be `options.directory`; otherwise `logger` says why.
 * Nothing is written to stdout or stderr: records go to a per-process JSONL
 * file in `options.directory`, created on the first record, and a one-line
 * summary goes to `logger`. A tick never throws, because an uncaught
 * exception in a timer is fatal to the process.
 * @param options - validated settings; `directory` must be absolute.
 * @param host - Node process facilities.
 * @param logger - warning sink for record summaries and write failures.
 * @returns a synchronous disposer that stops sampling and restores `process.report`.
 */
export function startWatchdog(options: WatchdogOptions, host: WatchdogHost, logger: WatchdogLogger): () => void {
  const restoreReport = armForensics(options, host, logger)
  let histogram: DelayHistogram | undefined
  try {
    histogram = host.eventLoopDelay(EVENT_LOOP_RESOLUTION_MS)
    histogram.enable()
    const sampler = new Sampler(options, host, logger, histogram)
    const timer = setInterval(() => { sampler.tick() }, options.intervalMs)
    // Diagnostics must never keep a finished process alive.
    timer.unref()
    const active = histogram
    return () => {
      clearInterval(timer)
      active.disable()
      restoreReport()
    }
  } catch (error) {
    histogram?.disable()
    restoreReport()
    throw error
  }
}

function armForensics(options: WatchdogOptions, host: WatchdogHost, logger: WatchdogLogger): () => void {
  const startup = nodeStartupOptions(host.execArgv, host.nodeOptions)
  const snapshots = options.heapSnapshots > 0 && snapshotsReachDirectory(options.directory, startup, logger)
  const report = options.fatalErrorReport && reportExcludesEnvironment(startup, logger)
  if (!snapshots && !report) return () => {}
  // Node writes neither artifact when its directory is missing.
  mkdirSync(options.directory, { recursive: true, mode: 0o700 })
  if (snapshots) host.setHeapSnapshotNearHeapLimit(options.heapSnapshots)
  return report ? armFatalErrorReport(options.directory, host.report) : () => {}
}

function snapshotsReachDirectory(directory: string, startup: readonly string[], logger: WatchdogLogger): boolean {
  const diagnosticDirectory = nodeOptionValue(startup, '--diagnostic-dir')
  if (diagnosticDirectory !== undefined && samePath(diagnosticDirectory, directory)) return true
  // Without a matching --diagnostic-dir, V8 writes heap-sized files into the working directory.
  logger.warn(
    'heap snapshots are not armed: Node writes them to %s, not %s; start Node with --diagnostic-dir=%s',
    diagnosticDirectory ?? 'the working directory',
    directory,
    directory,
  )
  return false
}

function reportExcludesEnvironment(startup: readonly string[], logger: WatchdogLogger): boolean {
  // A fatal error raised outside JavaScript, such as running out of heap, ignores the runtime
  // `excludeEnv` setting and includes environment variables unless the command line excluded them.
  if (nodeFlag(startup, '--report-exclude-env')) return true
  logger.warn('fatal-error reports are not armed: they could contain environment variables; start Node with --report-exclude-env')
  return false
}

function armFatalErrorReport(directory: string, report: ReportSettings): () => void {
  const previous: ReportSettings = {
    directory: report.directory,
    reportOnFatalError: report.reportOnFatalError,
    excludeEnv: report.excludeEnv,
    excludeNetwork: report.excludeNetwork,
  }
  report.directory = directory
  // Reverse DNS lookups for open sockets can stall the dying process.
  report.excludeEnv = true
  report.excludeNetwork = true
  report.reportOnFatalError = true
  return () => { Object.assign(report, previous) }
}

/**
 * List Node's startup options in the order Node applies them, so a later entry wins.
 * @param execArgv - `process.execArgv`, which Node applies after `NODE_OPTIONS`.
 * @param nodeOptions - the `NODE_OPTIONS` value, split as Node splits it.
 * @returns one entry per option or option value.
 */
export function nodeStartupOptions(execArgv: readonly string[], nodeOptions: string | undefined): string[] {
  return [...nodeOptions === undefined ? [] : splitNodeOptions(nodeOptions), ...execArgv]
}

/**
 * Read the last value given to a Node option, as `--name=value` or `--name value`.
 * @param options - entries from {@link nodeStartupOptions}.
 * @param name - the option, spelled with dashes; underscores in `options` match too.
 * @returns the value, or `undefined` when the option is absent.
 */
export function nodeOptionValue(options: readonly string[], name: string): string | undefined {
  let value: string | undefined
  let valueFollows = false
  for (const entry of options) {
    if (valueFollows) {
      value = entry
      valueFollows = false
      continue
    }
    const [option, inline] = splitOption(entry)
    if (option !== name) continue
    if (inline === undefined) valueFollows = true
    else value = inline
  }
  return value
}

/**
 * Read a boolean Node flag, which its `--no-` form turns off again.
 * @param options - entries from {@link nodeStartupOptions}.
 * @param name - the flag, spelled with dashes; underscores in `options` match too.
 * @returns whether the last occurrence enables it.
 */
export function nodeFlag(options: readonly string[], name: string): boolean {
  const negated = `--no-${name.slice(2)}`
  let enabled = false
  for (const entry of options) {
    const [option] = splitOption(entry)
    if (option === name) enabled = true
    else if (option === negated) enabled = false
  }
  return enabled
}

function splitOption(entry: string): [option: string, value: string | undefined] {
  const equals = entry.indexOf('=')
  if (equals === -1) return [entry.replaceAll('_', '-'), undefined]
  return [entry.slice(0, equals).replaceAll('_', '-'), entry.slice(equals + 1)]
}

/** Split `NODE_OPTIONS` on spaces outside double quotes; a backslash inside quotes escapes the next character. */
function splitNodeOptions(value: string): string[] {
  const tokens: string[] = []
  let token = ''
  let pending = false
  let quoted = false
  let escaped = false
  for (const char of value) {
    if (escaped) {
      token += char
      escaped = false
    } else if (quoted && char === '\\') {
      escaped = true
    } else if (char === '"') {
      quoted = !quoted
      pending = true
    } else if (char === ' ' && !quoted) {
      if (pending) tokens.push(token)
      token = ''
      pending = false
    } else {
      token += char
      pending = true
    }
  }
  if (pending) tokens.push(token)
  return tokens
}

function samePath(left: string, right: string): boolean {
  const canonical = (path: string): string => {
    try {
      return realpathSync(path)
    } catch {
      return resolve(path)
    }
  }
  return canonical(left) === canonical(right)
}

interface DelayWindow {
  readonly p50Ms: number
  readonly p99Ms: number
  readonly maxMs: number
  readonly meanMs: number
  readonly samples: number
}

interface Reading {
  readonly now: number
  readonly window: DelayWindow
  readonly windowMs: number
  readonly delayedForMs: number
  readonly heap: HeapSample
  readonly rss: number
}

class Sampler {
  private windowStart: number
  private delayedSince: number | undefined
  private peakHeapUsed = 0
  private peakRss = 0
  private file: string | undefined
  private readonly lastRecorded = new Map<WatchdogRecordKind, number>()
  private readonly suppressed = new Map<WatchdogRecordKind, number>()

  constructor(
    private readonly options: WatchdogOptions,
    private readonly host: WatchdogHost,
    private readonly logger: WatchdogLogger,
    private readonly histogram: DelayHistogram,
  ) {
    this.windowStart = host.now()
  }

  tick(): void {
    try {
      this.sample()
    } catch (error) {
      this.logger.warn('sampling failed: %s', error instanceof Error ? error.message : String(error))
    }
  }

  private sample(): void {
    const now = this.host.now()
    const start = this.windowStart
    const windowMs = now - start
    this.windowStart = now
    // After a stall inside a JavaScript callback, Node runs this overdue tick before
    // the histogram's own timer records the stall, and the reset below discards that
    // sample. The tick's lateness measures the same stall.
    const window = this.readWindow(Math.max(0, windowMs - this.options.intervalMs))
    const heap = this.host.heap()
    const rss = this.host.rss()
    this.peakHeapUsed = Math.max(this.peakHeapUsed, heap.used)
    this.peakRss = Math.max(this.peakRss, rss)

    const { eventLoopDelayMs, sustainedMs, heapFraction } = this.options
    if (window.p99Ms >= eventLoopDelayMs) this.delayedSince ??= start
    else this.delayedSince = undefined
    const delayedForMs = this.delayedSince === undefined ? 0 : now - this.delayedSince
    const reading: Reading = { now, window, windowMs, delayedForMs, heap, rss }
    // One stall as long as the sustain period needs no second window to prove it.
    if ((this.delayedSince !== undefined && delayedForMs >= sustainedMs)
      || window.maxMs >= Math.max(sustainedMs, eventLoopDelayMs)) {
      this.record('event-loop-delay', reading)
    }
    if (heap.used >= heap.limit * heapFraction) this.record('heap-limit', reading)
  }

  /** Delays exclude the sampling period, which every histogram value includes. */
  private readWindow(lateMs: number): DelayWindow {
    const { histogram } = this
    const samples = histogram.count
    const delay = (ns: number): number => Math.max(0, ns / NS_PER_MS - EVENT_LOOP_RESOLUTION_MS)
    // An empty histogram reports NaN for its mean.
    let window: DelayWindow = { p50Ms: 0, p99Ms: 0, maxMs: lateMs, meanMs: 0, samples }
    if (samples > 0) {
      window = {
        p50Ms: delay(histogram.percentile(50)),
        p99Ms: delay(histogram.percentile(99)),
        maxMs: Math.max(delay(histogram.max), lateMs),
        meanMs: delay(histogram.mean),
        samples,
      }
    }
    histogram.reset()
    return window
  }

  private record(kind: WatchdogRecordKind, reading: Reading): void {
    const { now, window, heap, rss } = reading
    const last = this.lastRecorded.get(kind)
    if (last !== undefined && now - last < this.options.recordIntervalMs) {
      this.suppressed.set(kind, (this.suppressed.get(kind) ?? 0) + 1)
      return
    }
    this.lastRecorded.set(kind, now)
    const record: WatchdogRecord = {
      time: new Date().toISOString(),
      kind,
      pid: this.host.pid,
      uptimeMs: Math.round(now),
      eventLoop: {
        p50Ms: Math.round(window.p50Ms),
        p99Ms: Math.round(window.p99Ms),
        maxMs: Math.round(window.maxMs),
        meanMs: Math.round(window.meanMs),
        samples: window.samples,
        windowMs: Math.round(reading.windowMs),
        delayedForMs: Math.round(reading.delayedForMs),
      },
      memory: {
        heapUsed: heap.used,
        heapTotal: heap.total,
        heapLimit: heap.limit,
        rss,
        peakHeapUsed: this.peakHeapUsed,
        peakRss: this.peakRss,
      },
      suppressed: this.suppressed.get(kind) ?? 0,
    }
    this.suppressed.delete(kind)
    const summary = kind === 'event-loop-delay'
      ? `event loop p99 delay ${record.eventLoop.p99Ms} ms, max ${record.eventLoop.maxMs} ms`
      : `heap ${Math.round(heap.used / MIB)} MiB of ${Math.round(heap.limit / MIB)} MiB limit`
    const file = this.file ??= join(this.options.directory, `watchdog.${fileStamp(new Date())}.${this.host.pid}.jsonl`)
    try {
      mkdirSync(this.options.directory, { recursive: true, mode: 0o700 })
      // Synchronous, so a record written just before an out-of-memory crash is on disk.
      appendFileSync(file, JSON.stringify(record) + '\n', { mode: 0o600 })
    } catch (error) {
      this.logger.warn('%s; could not write %s: %s', summary, file, error instanceof Error ? error.message : String(error))
      return
    }
    this.logger.warn('%s; recorded in %s', summary, file)
  }
}

/** `YYYYMMDD.HHMMSS` in local time, the stamp Node uses for diagnostic report names. */
function fileStamp(date: Date): string {
  const pad = (value: number): string => String(value).padStart(2, '0')
  return `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}`
    + `.${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
}
