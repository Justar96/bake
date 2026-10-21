/**
 * One fresh-process sample: seed a private home, start the arm in its PTY,
 * and measure startup, history readiness, input during a streamed answer,
 * idle CPU, process-tree peak RSS, and shutdown.
 */
import { readFileSync } from 'node:fs'
import { mkdir, mkdtemp, realpath, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { Arm } from './arms.ts'
import { STREAM_DONE, STREAM_START } from './model.ts'
import { cpuTicksBetween, processTree, readProcStat, survivors, TreeMonitor, type MonitorResult } from './proc-tree.ts'
import { Terminal } from './terminal.ts'
import { expectedMarkers, type HistoryDimensions, type WorkloadName, WORKLOADS } from './workloads.ts'

export type CacheMode = 'cold' | 'warm'
export const CACHE_MODES: readonly CacheMode[] = ['cold', 'warm']

/** Typed at startup; its echo is first usable input, and it becomes the prompt. */
export const READY_PROBE = 'PERF_READY'
/** Typed after the answer's first delta; its echo is input latency under streaming. */
export const LIVE_PROBE = 'PERF_LIVE_DRAFT'

/** What a completed sample measured; every duration is in milliseconds from the monotonic clock. */
export interface SampleMetrics {
  /** Spawn through bracketed paste enabled with the termios flags changed: the input loop is live. */
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
  /** Largest process-tree RSS, read every 10 ms from spawn to exit, in MiB. */
  peakRssMiB: number
}

/** Identifies a sample in a report. */
export interface SampleKey {
  readonly arm: string
  readonly workload: WorkloadName
  readonly mode: CacheMode
  readonly iteration: number
  /** A warm mode's discarded first launch, kept in the report but never summarized. */
  readonly warmup: boolean
}

export type SampleRecord = SampleKey & {
  readonly loadAverage1m: number
  /** The seeded history's sizes; absent when seeding failed. */
  readonly dimensions: (HistoryDimensions & { fileBytes: number }) | undefined
} & ({
  readonly ok: true
  readonly metrics: SampleMetrics
  readonly exit: { code: number; signal: string | null }
  readonly terminalRestored: true
  /** Descendants still alive after the program exited, which the harness then killed. */
  readonly leakedProcesses: readonly string[]
  readonly markerOccurrences: number
  readonly outputBytes: number
  readonly modelRequests: number
  readonly rssSampling: { ticks: number; maxGapMs: number; pids: number }
} | {
  readonly ok: false
  readonly error: string
})

/** Fixed settings of every sample in a run. */
export interface SampleSettings {
  readonly idleMs: number
  readonly timeoutMs: number
  readonly clockTicks: number
  readonly signal?: AbortSignal
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

/**
 * Measure one fresh process of `arm` on `workload`.
 * @param model - the faux model, shared by the run, with its request count.
 * @param cache - the shared process-cache directory of warm samples; cold samples make their own.
 * @returns the record; a failure is recorded, never thrown, unless the run was interrupted.
 */
export async function runSample(
  arm: Arm,
  key: SampleKey,
  model: { baseUrl: string; requests: number },
  cache: string,
  settings: SampleSettings,
): Promise<SampleRecord> {
  const loadAverage1m = loadAverage()
  const root = await realpath(await mkdtemp(join(tmpdir(), 'bake-perf-')))
  let terminal: Terminal | undefined
  let monitor: TreeMonitor | undefined
  let dimensions: (HistoryDimensions & { fileBytes: number }) | undefined
  const { idleMs, timeoutMs, signal } = settings
  try {
    const sampleCache = key.mode === 'cold' ? join(root, 'cache') : cache
    await mkdir(sampleCache, { recursive: true })
    const launch = await arm.prepare(WORKLOADS[key.workload], { root, cache: sampleCache }, model.baseUrl)
    dimensions = launch.dimensions
    const markers = expectedMarkers(launch.dimensions)
    const requestsBefore = model.requests
    terminal = new Terminal(launch.argv, launch.cwd, launch.env)
    const term = terminal
    monitor = new TreeMonitor(term.pid, 10)
    await monitor.ready()
    const start = term.startedAt
    const inputReadyAt = await term.wait('input loop (bracketed paste and raw mode)', () => term.bracketedPasteAt !== undefined && term.modesChanged, timeoutMs, signal)
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
    await Bun.sleep(idleMs)
    const idleAfter = processTree(term.pid)
    const idleCpuMs = cpuTicksBetween(idleBefore, idleAfter) * 1000 / settings.clockTicks
    if (term.markerOccurrences !== markers.length) throw new Error(`expected each of ${markers.length} history markers once, saw ${term.markerOccurrences} sightings`)
    const members = processTree(term.pid)
    const commands = new Map(members.map(member => [member.pid, commandOf(member.pid)]))
    signal?.throwIfAborted()
    const termAt = performance.now()
    term.signal('SIGTERM')
    const exit = await term.exited
    const shutdownMs = exit.at - termAt
    const restored = term.restored
    const leaked = survivors(members, term.pid)
    for (const member of leaked) {
      try { process.kill(member.pid, 'SIGKILL') } catch { /* it exited meanwhile */ }
    }
    const rss: MonitorResult = await monitor.stop()
    monitor = undefined
    // 0.3's launcher exits 0 after SIGTERM; `bake` exits 128 + 15 after restoring the terminal.
    if (exit.signal !== null || (exit.code !== 0 && exit.code !== 143)) throw new Error(`unclean exit after SIGTERM: ${JSON.stringify({ code: exit.code, signal: exit.signal })}`)
    if (!restored) throw new Error('the terminal modes were not restored at exit')
    return {
      ...key, loadAverage1m, dimensions, ok: true,
      metrics: {
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
        peakRssMiB: rss.peakRssKiB / 1024,
      },
      exit: { code: exit.code, signal: exit.signal },
      terminalRestored: true,
      leakedProcesses: leaked.map(member => commands.get(member.pid) ?? String(member.pid)),
      markerOccurrences: term.markerOccurrences,
      outputBytes: term.bytes,
      modelRequests: model.requests - requestsBefore,
      rssSampling: { ticks: rss.ticks, maxGapMs: rss.maxGapMs, pids: rss.pids.length },
    }
  } catch (error) {
    if (signal?.aborted) throw error
    return { ...key, loadAverage1m, dimensions, ok: false, error: error instanceof Error ? error.message : String(error) }
  } finally {
    try {
      if (monitor !== undefined) await monitor.stop().catch(() => undefined)
      if (terminal !== undefined) {
        // A failed sample may leave descendants; end them with the program.
        const left = readProcStat(terminal.pid) === undefined ? [] : processTree(terminal.pid)
        await terminal.close()
        for (const member of survivors(left, terminal.pid)) {
          try { process.kill(member.pid, 'SIGKILL') } catch { /* already gone */ }
        }
      }
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  }
}
