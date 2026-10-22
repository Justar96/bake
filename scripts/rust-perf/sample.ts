/**
 * One fresh-process sample: seed a home, start the arm in its PTY, and
 * measure startup, history readiness, input during a streamed answer, idle
 * CPU, process-tree memory, and shutdown.
 *
 * A sample owns its temporary root and every process its program starts.
 * Whatever happens, it kills the program, then every process it ever saw in
 * the program's tree or session that is still alive, and removes its root,
 * before it returns or rethrows.
 */
import { readFileSync, rmSync } from 'node:fs'
import { mkdir, mkdtemp, realpath, rename, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { Arm } from './arms.ts'
import { STREAM_DONE, STREAM_START } from './model.ts'
import { cpuTicksBetween, identity, processTree, readProcStat, sessionMembers, survivors, TreeMonitor, type MonitorResult, type ProcStat } from './proc-tree.ts'
import { Terminal } from './terminal.ts'
import { expectedMarkers, type HistoryDimensions, type WorkloadName, WORKLOADS } from './workloads.ts'

export type CacheMode = 'cold' | 'warm'
export const CACHE_MODES: readonly CacheMode[] = ['cold', 'warm']

/** Typed at startup; its echo is first usable input, and it becomes the prompt. */
export const READY_PROBE = 'PERF_READY'
/** Typed after the answer's first delta; its echo is input latency under streaming. */
export const LIVE_PROBE = 'PERF_LIVE_DRAFT'
/** The iteration of the launch that prepares an arm's warm home. */
export const PREPARE_ITERATION = -2
/** The iteration of a warm mode's discarded warm-up launch. */
export const WARMUP_ITERATION = -1
/**
 * How long processes the program leaves may take to exit before they are
 * leaks: when the program, the terminal's session leader, exits, the kernel
 * hangs up its foreground jobs, and they die a moment later.
 */
export const LEAK_GRACE_MS = 250

/** What a completed sample measured; every duration is in milliseconds from the monotonic clock. */
export interface SampleMetrics {
  /**
   * Spawn through the arrival of the output that enabled bracketed paste,
   * or, if later, the first sight of changed termios flags: when the program
   * took the terminal for input. scripts/rust-perf/README.md says what that
   * is in each arm.
   */
  inputReadyMs: number
  /** Spawn through the composer echo of {@link READY_PROBE}, typed at input readiness. */
  startupMs: number
  /** History markers already shown when the probe echoed. */
  markersAtFirstInput: number
  /** Spawn through the last history marker's first appearance; null for a fresh session. */
  replayMs: number | null
  /** Spawn through both the probe echo and every history marker. */
  readyMs: number
  /** Enter through the first streamed delta. */
  firstDeltaMs: number
  /** {@link LIVE_PROBE} typed after the first delta, through its echo. */
  streamInputMs: number
  /** Enter through the settled turn after the last delta. */
  streamMs: number
  /** CPU time of the whole tree over the idle window after the turn settled. */
  idleCpuMs: number
  /** {@link idleCpuMs} as a share of one CPU over the window, in percent. */
  idleCpuPercent: number
  /** SIGTERM through the observed exit. */
  shutdownMs: number
  /**
   * Process-tree peak memory from the kernel's high-water marks: the sum
   * over every process the tree held of its `VmHWM`, read every 10 ms, in
   * MiB. Spikes between readings are kept; members that peaked at different
   * times are summed, so it bounds the simultaneous peak from above.
   */
  peakRssMiB: number
  /** The largest sum of the tree's `VmRSS` in one 10 ms reading, in MiB; it can miss a short spike. */
  sampledPeakRssMiB: number
}

/** Identifies a sample in a report. */
export interface SampleKey {
  readonly arm: string
  readonly workload: WorkloadName
  readonly mode: CacheMode
  /** From 0 for a measured sample; {@link WARMUP_ITERATION} or {@link PREPARE_ITERATION} otherwise. */
  readonly iteration: number
  /** A discarded launch, a warm-up or a warm home's preparation, kept in the report but never summarized. */
  readonly warmup: boolean
}

export type SampleRecord = SampleKey & {
  readonly loadAverage1m: number
  /** The seeded history's sizes; absent when seeding failed. */
  readonly dimensions: (HistoryDimensions & { fileBytes: number }) | undefined
  /** Processes of the program's tree or session alive after it exited, which the harness then killed. */
  readonly leakedProcesses: readonly string[]
} & ({
  readonly ok: true
  readonly metrics: SampleMetrics
  readonly exit: { code: number; signal: string | null }
  readonly terminalRestored: true
  readonly markerOccurrences: number
  readonly outputBytes: number
  readonly modelRequests: number
  readonly rssSampling: { ticks: number; maxGapMs: number; processes: number }
} | {
  readonly ok: false
  readonly error: string
})

/** Fixed settings of every sample in a run. */
export interface SampleSettings {
  readonly idleMs: number
  readonly timeoutMs: number
  /** How long after SIGTERM the program may take to exit before its tree is killed and the sample fails. */
  readonly shutdownTimeoutMs: number
  readonly clockTicks: number
  readonly signal?: AbortSignal
  /** Where sample roots are made; the system temporary directory by default. */
  readonly tempRoot?: string
}

/** One arm's warm state, which its warm samples share. */
export interface WarmState {
  /** Process caches (`TMPDIR`). */
  readonly cache: string
  /**
   * The home every warm sample runs in, at one fixed path, so absolute
   * paths the arm wrote into its home on the preparing launch stay valid.
   */
  readonly home: string
  /** The prepared home, without its sessions, that each warm sample's home is copied from. */
  readonly template: string
}

/** The 1-minute load average. */
export function loadAverage(): number {
  return Number(readFileSync('/proc/loadavg', 'utf8').split(' ')[0])
}

function commandOf(pid: number): string {
  try {
    return readFileSync(`/proc/${pid}/cmdline`, 'utf8').split('\0').filter(Boolean).join(' ')
  } catch {
    return String(pid)
  }
}

function kill(pid: number): void {
  try { process.kill(pid, 'SIGKILL') } catch { /* it exited meanwhile */ }
}

/** Kill `member` and its current descendants if `member` is still the same process. */
function killTree(member: ProcStat): void {
  if (readProcStat(member.pid)?.startTime !== member.startTime) return
  for (const descendant of processTree(member.pid).reverse()) kill(descendant.pid)
}

/** Settles after `ms`, or rejects when `signal` aborts. */
function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return within(Bun.sleep(ms), ms + 1000, signal).then(() => {})
}

/**
 * Await `promise` for at most `ms`.
 * @returns its value, or undefined at the deadline.
 * @throws the abort reason when `signal` aborts first.
 */
async function within<T>(promise: Promise<T>, ms: number, signal?: AbortSignal): Promise<T | undefined> {
  signal?.throwIfAborted()
  let timer: ReturnType<typeof setTimeout> | undefined
  let onAbort: (() => void) | undefined
  try {
    return await Promise.race([
      promise,
      new Promise<undefined>((resolve) => { timer = setTimeout(() => { resolve(undefined) }, ms) }),
      new Promise<never>((_, reject) => {
        onAbort = () => { reject(signal?.reason) }
        signal?.addEventListener('abort', onAbort, { once: true })
      }),
    ])
  } finally {
    clearTimeout(timer)
    if (onAbort !== undefined) signal?.removeEventListener('abort', onAbort)
  }
}

async function copyTree(source: string, destination: string): Promise<void> {
  // A reflink where the file system has them; the copy is made before spawn and never timed.
  const copy = Bun.spawn(['cp', '-a', '--reflink=auto', source, destination], { stdout: 'ignore', stderr: 'pipe' })
  const [code, stderr] = await Promise.all([copy.exited, new Response(copy.stderr).text()])
  if (code !== 0) throw new Error(`could not copy the warm home ${source}: ${stderr.trim()}`)
}

/** What a sample in flight holds, for {@link abandonSamples}. */
interface InFlight {
  readonly dirs: string[]
  program: ProcStat | undefined
}
const inFlight = new Set<InFlight>()

/**
 * Synchronously kill every in-flight sample's program, its tree, and its
 * session, and remove the samples' directories: for a harness that must
 * exit at once, as on a second interrupt, without awaiting their cleanup.
 */
export function abandonSamples(): void {
  for (const flight of inFlight) {
    if (flight.program !== undefined) {
      const program = flight.program
      for (const member of sessionMembers(program)) killTree(member)
      killTree(program)
    }
    for (const dir of flight.dirs) rmSync(dir, { recursive: true, force: true })
  }
  inFlight.clear()
}

/**
 * Measure one fresh process of `arm` on `workload`.
 * @param model - the faux model, shared by the run, with its request count.
 * @param warm - the arm's warm state; a cold sample uses none of it.
 * @param preparing - for a warm sample, start from an empty home and keep it, without its sessions, as `warm.template`.
 * @returns the record; a failure is recorded, never thrown, unless the run was interrupted.
 */
export async function runSample(
  arm: Arm,
  key: SampleKey,
  model: { baseUrl: string; requests: number },
  warm: WarmState,
  settings: SampleSettings,
  preparing = false,
): Promise<SampleRecord> {
  const loadAverage1m = loadAverage()
  const { idleMs, timeoutMs, shutdownTimeoutMs, signal } = settings
  const root = await realpath(await mkdtemp(join(settings.tempRoot ?? tmpdir(), 'bake-perf-')))
  const flight: InFlight = { dirs: [root], program: undefined }
  inFlight.add(flight)
  const home = key.mode === 'cold' ? join(root, 'home') : warm.home
  let terminal: Terminal | undefined
  let monitor: TreeMonitor | undefined
  let rss: MonitorResult | undefined
  let dimensions: (HistoryDimensions & { fileBytes: number }) | undefined
  /** Every process seen in the program's tree, by identity. */
  const seen = new Map<string, ProcStat>()
  const track = (members: readonly ProcStat[]): void => {
    for (const member of members) if (!seen.has(identity(member))) seen.set(identity(member), member)
  }

  const measure = async (): Promise<Extract<SampleRecord, { ok: true }>> => {
    if (key.mode === 'warm') {
      flight.dirs.push(home)
      await rm(home, { recursive: true, force: true })
      if (preparing) await mkdir(home, { recursive: true })
      else await copyTree(warm.template, home)
    }
    const sampleCache = key.mode === 'cold' ? join(root, 'cache') : warm.cache
    await mkdir(sampleCache, { recursive: true })
    const launch = await arm.prepare(WORKLOADS[key.workload], { root, home, cache: sampleCache }, model.baseUrl)
    dimensions = launch.dimensions
    const markers = expectedMarkers(launch.dimensions)
    const requestsBefore = model.requests
    terminal = new Terminal(launch.argv, launch.cwd, launch.env)
    const term = terminal
    flight.program = readProcStat(term.pid)
    monitor = new TreeMonitor(term.pid, 10)
    await monitor.ready()
    const start = term.startedAt
    await term.wait('input loop (bracketed paste and raw mode)', () => term.inputReadyAt !== undefined, timeoutMs, signal)
    const inputReadyAt = term.inputReadyAt ?? Number.NaN
    const ready = term.watch(READY_PROBE)
    term.send(READY_PROBE)
    const echoAt = await term.seen(ready, 'startup probe echo', timeoutMs, signal)
    const markersAtFirstInput = term.markers.size
    await term.wait(`${markers.length} history markers`, () => term.markers.size >= markers.length, timeoutMs, signal)
    for (const expected of markers) if (!term.markers.has(expected)) throw new Error(`history marker ${expected} never appeared`)
    const lastMarkerAt = markers.length === 0 ? undefined : Math.max(...term.markers.values())
    const submitPosition = term.position
    const first = term.watch(STREAM_START)
    const done = term.watch(STREAM_DONE)
    const submitAt = performance.now()
    term.send('\r')
    const firstDeltaAt = await term.seen(first, 'first streamed delta', timeoutMs, signal)
    const live = term.watch(LIVE_PROBE)
    const liveAt = performance.now()
    term.send(LIVE_PROBE)
    const liveEchoAt = await term.seen(live, 'live probe echo', timeoutMs, signal)
    if (done.at !== undefined && done.at < liveEchoAt) throw new Error('the answer finished before the live probe echoed, so the echo was not measured under streaming')
    const settledAt = await term.wait('settled turn', () => arm.settled(term.visibleSince(submitPosition)), timeoutMs, signal)
    const idleBefore = processTree(term.pid)
    track(idleBefore)
    await sleep(idleMs, signal)
    const idleAfter = processTree(term.pid)
    track(idleAfter)
    const idleCpuMs = cpuTicksBetween(idleBefore, idleAfter) * 1000 / settings.clockTicks
    if (term.markerOccurrences !== markers.length) throw new Error(`expected each of ${markers.length} history markers once, saw ${term.markerOccurrences} sightings`)
    // A program that ended on its own, during the idle window or before, has no shutdown to measure.
    const alive = readProcStat(term.pid)
    if (term.exit !== undefined || alive === undefined || alive.state === 'Z') throw new Error(`the program exited before SIGTERM${term.exit === undefined ? '' : ` with ${JSON.stringify({ code: term.exit.code, signal: term.exit.signal })}`}; a sample measures shutdown from SIGTERM`)
    track(processTree(term.pid))
    signal?.throwIfAborted()
    const termAt = performance.now()
    term.signal('SIGTERM')
    const exit = await within(term.exited, shutdownTimeoutMs, signal)
    if (exit === undefined) throw new Error(`the program did not exit within ${shutdownTimeoutMs} ms of SIGTERM; its tree was killed`)
    const shutdownMs = exit.at - termAt
    rss = await monitor.stop()
    monitor = undefined
    track(rss.members)
    // 0.3's launcher exits 0 after SIGTERM; `bake` exits 128 + 15 after restoring the terminal.
    if (exit.signal !== null || (exit.code !== 0 && exit.code !== 143)) throw new Error(`unclean exit after SIGTERM: ${JSON.stringify({ code: exit.code, signal: exit.signal })}`)
    const unrestored = term.unrestored()
    if (unrestored.length > 0) throw new Error(`the terminal was not restored at exit: ${unrestored.join(', ')}`)
    const metrics: SampleMetrics = {
      inputReadyMs: inputReadyAt - start,
      startupMs: echoAt - start,
      markersAtFirstInput,
      replayMs: lastMarkerAt === undefined ? null : lastMarkerAt - start,
      readyMs: Math.max(echoAt, lastMarkerAt ?? echoAt) - start,
      firstDeltaMs: firstDeltaAt - submitAt,
      streamInputMs: liveEchoAt - liveAt,
      streamMs: settledAt - submitAt,
      idleCpuMs,
      idleCpuPercent: idleCpuMs / idleMs * 100,
      shutdownMs,
      peakRssMiB: rss.peakHwmKiB / 1024,
      sampledPeakRssMiB: rss.sampledPeakRssKiB / 1024,
    }
    for (const [name, value] of Object.entries(metrics)) {
      if (value !== null && !(Number.isFinite(value) && value >= 0)) throw new Error(`${name} measured ${value}, not a duration or size; the sample is not recorded`)
    }
    return {
      ...key, loadAverage1m, dimensions, leakedProcesses: [], ok: true, metrics,
      exit: { code: exit.code, signal: exit.signal },
      terminalRestored: true,
      markerOccurrences: term.markerOccurrences,
      outputBytes: term.bytes,
      modelRequests: model.requests - requestsBefore,
      rssSampling: { ticks: rss.ticks, maxGapMs: rss.maxGapMs, processes: rss.members.length },
    }
  }

  /** Kill the program if it still runs, then every tracked process still alive; return those as leaks. */
  const end = async (): Promise<string[]> => {
    const program = flight.program
    const term = terminal
    // Taken while the program lives, since its descendants leave the tree when it dies.
    if (term !== undefined && term.exit === undefined) track(processTree(term.pid))
    if (monitor !== undefined) {
      const observed = await monitor.stop().catch(() => undefined)
      if (observed !== undefined) track(observed.members)
      monitor = undefined
    }
    await term?.close()
    if (program !== undefined) track(sessionMembers(program))
    let leaked = program === undefined ? [] : survivors([...seen.values()], program.pid)
    const graceEnd = performance.now() + LEAK_GRACE_MS
    while (leaked.length > 0 && performance.now() < graceEnd) {
      await Bun.sleep(10)
      leaked = survivors(leaked, Number.NaN)
    }
    const names = leaked.map(member => commandOf(member.pid))
    for (const member of leaked) killTree(member)
    return names
  }

  let record: SampleRecord | undefined
  let failure: string | undefined
  let leakedProcesses: string[] = []
  try {
    try {
      record = await measure()
    } catch (error) {
      if (signal?.aborted) throw error
      failure = error instanceof Error ? error.message : String(error)
    }
  } finally {
    try {
      leakedProcesses = await end()
      if (preparing && record !== undefined) {
        await rm(warm.template, { recursive: true, force: true })
        await rename(home, warm.template)
        // Each warm sample seeds its own session.
        await rm(join(warm.template, 'sessions'), { recursive: true, force: true })
      }
    } catch (error) {
      record = undefined
      failure = `the sample's cleanup failed: ${error instanceof Error ? error.message : String(error)}`
    } finally {
      for (const dir of flight.dirs) await rm(dir, { recursive: true, force: true })
      inFlight.delete(flight)
    }
  }
  if (record !== undefined) return { ...record, leakedProcesses }
  return { ...key, loadAverage1m, dimensions, leakedProcesses, ok: false, error: failure ?? 'the sample failed' }
}
