/**
 * Sampling, threshold, rate-limit, record, and forensics behavior of the
 * watchdog core. Node's delay histogram, heap and memory readings, the
 * monotonic clock, `process.report`, and the irreversible snapshot switch are
 * faked; the interval timer is Vitest's fake timer except where disposal
 * checks the real one.
 */

import { mkdtemp, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { format } from 'node:util'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  EVENT_LOOP_RESOLUTION_MS, nodeFlag, nodeOptionValue, nodeStartupOptions, startWatchdog,
  type DelayHistogram, type ReportSettings, type WatchdogHost, type WatchdogOptions, type WatchdogRecord,
} from '../src/watchdog.ts'

const MIB = 1024 * 1024
const INTERVAL_MS = 10_000

/** One sampling window's delays in milliseconds; the median and mean are half the 99th percentile. */
interface Window {
  p99: number
  max?: number
  count?: number
}

/** Reports what Node's histogram does: each value is the sampling period plus the delay, in nanoseconds. */
class FakeHistogram implements DelayHistogram {
  enabled = false
  window: Window = { p99: 0, count: 0 }
  enable(): boolean { this.enabled = true; return true }
  disable(): boolean { this.enabled = false; return true }
  reset(): void { this.window = { p99: 0, count: 0 } }
  percentile(percentile: number): number { return nanoseconds(percentile >= 99 ? this.window.p99 : this.window.p99 / 2) }
  get max(): number { return nanoseconds(this.window.max ?? this.window.p99) }
  get mean(): number { return this.count === 0 ? Number.NaN : nanoseconds(this.window.p99 / 2) }
  get count(): number { return this.window.count ?? 200 }
}

function nanoseconds(delayMs: number): number {
  return (delayMs + EVENT_LOOP_RESOLUTION_MS) * 1e6
}

let directory: string
let root: string

beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), 'runtime-watchdog-'))
  directory = join(root, 'diagnostics')
})

afterEach(async () => {
  vi.useRealTimers()
  vi.restoreAllMocks()
  await rm(root, { recursive: true, force: true })
})

function options(overrides: Partial<WatchdogOptions> = {}): WatchdogOptions {
  return {
    directory,
    intervalMs: INTERVAL_MS,
    eventLoopDelayMs: 250,
    sustainedMs: 30_000,
    heapFraction: 0.85,
    recordIntervalMs: 300_000,
    fatalErrorReport: false,
    heapSnapshots: 0,
    ...overrides,
  }
}

function bench(overrides: Partial<WatchdogHost> = {}) {
  let clock = 1_000
  const histogram = new FakeHistogram()
  const heap = { used: 100 * MIB, total: 150 * MIB, limit: 1000 * MIB }
  const report: ReportSettings = { directory: '', reportOnFatalError: false, excludeEnv: false, excludeNetwork: false }
  const snapshots: number[] = []
  const warnings: string[] = []
  const host: WatchdogHost = {
    now: () => clock,
    eventLoopDelay: () => histogram,
    heap: () => ({ ...heap }),
    rss: () => 300 * MIB,
    report,
    setHeapSnapshotNearHeapLimit: (count) => { snapshots.push(count) },
    execArgv: [],
    nodeOptions: undefined,
    pid: 4242,
    ...overrides,
  }
  const logger = { warn: (pattern: string, ...param: unknown[]) => { warnings.push(format(pattern, ...param)) } }
  return {
    host, histogram, heap, report, snapshots, warnings, logger,
    /** Close one window: the clock moves `elapsedMs`, then the interval fires once. */
    tick(window: Window, elapsedMs = INTERVAL_MS): void {
      histogram.window = window
      clock += elapsedMs
      vi.advanceTimersByTime(INTERVAL_MS)
    },
  }
}

async function records(): Promise<WatchdogRecord[]> {
  const files = (await readdir(directory).catch(() => [])).filter(file => file.endsWith('.jsonl'))
  const lines = await Promise.all(files.map(file => readFile(join(directory, file), 'utf8')))
  return lines.join('').split('\n').filter(line => line !== '').map(line => JSON.parse(line) as WatchdogRecord)
}

describe('event-loop delay', () => {
  beforeEach(() => { vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] }) })

  it('records a delay once consecutive windows stay at the threshold for the sustain period', async () => {
    const b = bench()
    const stop = startWatchdog(options(), b.host, b.logger)
    b.tick({ p99: 250, max: 400 })
    b.tick({ p99: 300, max: 900 })
    expect(await records()).toEqual([])
    b.tick({ p99: 320, max: 1200 })
    stop()

    const [record, ...rest] = await records()
    expect(rest).toEqual([])
    expect(record).toMatchObject({
      kind: 'event-loop-delay',
      pid: 4242,
      uptimeMs: 31_000,
      eventLoop: { p50Ms: 160, p99Ms: 320, maxMs: 1200, meanMs: 160, samples: 200, windowMs: 10_000, delayedForMs: 30_000 },
      memory: { heapUsed: 100 * MIB, heapTotal: 150 * MIB, heapLimit: 1000 * MIB, rss: 300 * MIB },
      suppressed: 0,
    })
    expect(Number.isNaN(Date.parse(record!.time))).toBe(false)
  })

  it('starts the sustain period over after a window below the threshold', async () => {
    const b = bench()
    const stop = startWatchdog(options(), b.host, b.logger)
    for (const p99 of [300, 300, 249, 300, 300]) b.tick({ p99 })
    expect(await records()).toEqual([])
    b.tick({ p99: 300 })
    stop()
    expect((await records()).map(record => record.eventLoop.delayedForMs)).toEqual([30_000])
  })

  it('records one continuous stall the histogram measured', async () => {
    const b = bench()
    const stop = startWatchdog(options(), b.host, b.logger)
    b.tick({ p99: 20, max: 29_999, count: 3 })
    expect(await records()).toEqual([])
    b.tick({ p99: 20, max: 30_000, count: 3 })
    stop()
    expect(await records()).toMatchObject([
      { kind: 'event-loop-delay', eventLoop: { p99Ms: 20, maxMs: 30_000, samples: 3, windowMs: 10_000, delayedForMs: 0 } },
    ])
  })

  it('records one continuous stall from the lateness of the sampling tick', async () => {
    const b = bench()
    const stop = startWatchdog(options(), b.host, b.logger)
    // The tick read and reset the histogram before its timer recorded the stall.
    b.tick({ p99: 0, count: 0 }, 45_000)
    stop()
    expect(await records()).toMatchObject([
      { kind: 'event-loop-delay', eventLoop: { p99Ms: 0, maxMs: 35_000, samples: 0, windowMs: 45_000, delayedForMs: 0 } },
    ])
  })

  it('measures delay beyond the sampling period', async () => {
    const b = bench()
    const stop = startWatchdog(options({ sustainedMs: 0, recordIntervalMs: 0 }), b.host, b.logger)
    b.histogram.window = { p99: 0 }
    b.tick({ p99: 249 })
    b.tick({ p99: 250 })
    stop()
    expect((await records()).map(record => record.eventLoop)).toEqual([
      { p50Ms: 125, p99Ms: 250, maxMs: 250, meanMs: 125, samples: 200, windowMs: 10_000, delayedForMs: 10_000 },
    ])
  })

  it('reports an empty window as zero delay', async () => {
    const b = bench()
    const stop = startWatchdog(options({ sustainedMs: 0, eventLoopDelayMs: 1 }), b.host, b.logger)
    b.tick({ p99: 0, count: 0 })
    stop()
    expect(await records()).toEqual([])
  })

  it('rate-limits records of one kind and counts the crossings it skipped', async () => {
    const b = bench()
    const stop = startWatchdog(options({ recordIntervalMs: 25_000 }), b.host, b.logger)
    for (let index = 0; index < 6; index++) b.tick({ p99: 400 })
    stop()
    expect((await records()).map(({ uptimeMs, suppressed }) => ({ uptimeMs, suppressed }))).toEqual([
      { uptimeMs: 31_000, suppressed: 0 },
      { uptimeMs: 61_000, suppressed: 2 },
    ])
  })
})

describe('heap', () => {
  beforeEach(() => { vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] }) })

  it('records heap use at the configured fraction of the limit with peak memory', async () => {
    const b = bench()
    const stop = startWatchdog(options(), b.host, b.logger)
    b.heap.used = 849 * MIB
    b.tick({ p99: 5 })
    expect(await records()).toEqual([])
    b.heap.used = 850 * MIB
    b.tick({ p99: 5 })
    b.heap.used = 700 * MIB
    b.tick({ p99: 5 })
    stop()
    expect(await records()).toMatchObject([{
      kind: 'heap-limit',
      eventLoop: { p99Ms: 5, delayedForMs: 0 },
      memory: { heapUsed: 850 * MIB, heapLimit: 1000 * MIB, peakHeapUsed: 850 * MIB, peakRss: 300 * MIB },
    }])
  })

  it('rate-limits heap and delay records independently', async () => {
    const b = bench()
    const stop = startWatchdog(options({ sustainedMs: 0 }), b.host, b.logger)
    b.heap.used = 900 * MIB
    b.tick({ p99: 400 })
    b.tick({ p99: 400 })
    stop()
    expect((await records()).map(record => record.kind).sort()).toEqual(['event-loop-delay', 'heap-limit'])
  })
})

describe('records', () => {
  beforeEach(() => { vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] }) })

  it('appends to one private file per process and logs where it went', async () => {
    const b = bench()
    const stop = startWatchdog(options({ sustainedMs: 0, recordIntervalMs: 0 }), b.host, b.logger)
    b.tick({ p99: 400 })
    b.tick({ p99: 500 })
    stop()

    const files = await readdir(directory)
    expect(files).toHaveLength(1)
    expect(files[0]).toMatch(/^watchdog\.\d{8}\.\d{6}\.4242\.jsonl$/)
    expect((await records()).map(record => record.eventLoop.p99Ms)).toEqual([400, 500])
    if (process.platform !== 'win32') {
      expect((await stat(directory)).mode & 0o777).toBe(0o700)
      expect((await stat(join(directory, files[0]!))).mode & 0o777).toBe(0o600)
    }
    expect(b.warnings).toHaveLength(2)
    expect(b.warnings[0]).toContain('event loop p99 delay 400 ms, max 400 ms')
    expect(b.warnings[0]).toContain(join(directory, files[0]!))
  })

  it('logs a failed write and keeps sampling', async () => {
    await writeFile(directory, 'not a directory')
    const b = bench()
    const stop = startWatchdog(options({ sustainedMs: 0, recordIntervalMs: 0 }), b.host, b.logger)
    b.tick({ p99: 400 })
    b.heap.used = 900 * MIB
    b.tick({ p99: 5 })
    stop()
    expect(b.warnings).toEqual([
      expect.stringContaining('could not write'),
      expect.stringContaining('heap 900 MiB of 1000 MiB limit'),
    ])
  })

  it('logs a failed reading instead of throwing from the timer', () => {
    const b = bench({ heap: () => { throw new Error('heap statistics unavailable') } })
    const stop = startWatchdog(options(), b.host, b.logger)
    expect(() => { b.tick({ p99: 5 }) }).not.toThrow()
    stop()
    expect(b.warnings).toEqual([expect.stringContaining('heap statistics unavailable')])
  })

  it('writes nothing to stdout or stderr', async () => {
    const stdout = vi.spyOn(process.stdout, 'write')
    const stderr = vi.spyOn(process.stderr, 'write')
    const b = bench({ execArgv: ['--diagnostic-dir=/elsewhere'] })
    const stop = startWatchdog(options({ sustainedMs: 0, fatalErrorReport: true, heapSnapshots: 1 }), b.host, b.logger)
    b.heap.used = 900 * MIB
    b.tick({ p99: 400 })
    stop()
    expect(await records()).toHaveLength(2)
    expect(stdout).not.toHaveBeenCalled()
    expect(stderr).not.toHaveBeenCalled()
  })
})

describe('lifecycle', () => {
  it('uses an unreferenced timer and leaves no sampling after disposal', async () => {
    const setIntervalSpy = vi.spyOn(globalThis, 'setInterval')
    const clearIntervalSpy = vi.spyOn(globalThis, 'clearInterval')
    const heap = vi.fn(() => ({ used: 1, total: 1, limit: 1000 }))
    const b = bench({ heap })
    const stop = startWatchdog(options({ intervalMs: 100 }), b.host, b.logger)
    const timer = setIntervalSpy.mock.results[0]!.value as NodeJS.Timeout
    expect(timer.hasRef()).toBe(false)
    expect(b.histogram.enabled).toBe(true)
    await vi.waitFor(() => { expect(heap).toHaveBeenCalled() })

    stop()
    expect(clearIntervalSpy).toHaveBeenCalledWith(timer)
    expect(b.histogram.enabled).toBe(false)
    const calls = heap.mock.calls.length
    await new Promise(resolve => setTimeout(resolve, 350))
    expect(heap).toHaveBeenCalledTimes(calls)
  })

  it('undoes forensics setup when sampling cannot start', () => {
    const b = bench({ execArgv: ['--report-exclude-env'], eventLoopDelay: () => { throw new Error('no histogram') } })
    b.report.directory = '/previous'
    expect(() => startWatchdog(options({ fatalErrorReport: true }), b.host, b.logger)).toThrow('no histogram')
    expect(b.report).toEqual({ directory: '/previous', reportOnFatalError: false, excludeEnv: false, excludeNetwork: false })
  })
})

describe('fatal-error forensics', () => {
  it('writes fatal-error reports to the configured directory without environment or network data', async () => {
    const b = bench({ execArgv: ['--report-exclude-env'] })
    Object.assign(b.report, { directory: '/previous', excludeNetwork: true })
    const stop = startWatchdog(options({ fatalErrorReport: true }), b.host, b.logger)
    expect(b.report).toEqual({ directory, reportOnFatalError: true, excludeEnv: true, excludeNetwork: true })
    if (process.platform !== 'win32') expect((await stat(directory)).mode & 0o777).toBe(0o700)
    stop()
    expect(b.report).toEqual({ directory: '/previous', reportOnFatalError: false, excludeEnv: false, excludeNetwork: true })
  })

  it('leaves report settings and the directory alone when reports are off', async () => {
    const b = bench({ execArgv: ['--report-exclude-env'] })
    const stop = startWatchdog(options({ fatalErrorReport: false }), b.host, b.logger)
    stop()
    expect(b.report).toEqual({ directory: '', reportOnFatalError: false, excludeEnv: false, excludeNetwork: false })
    await expect(stat(directory)).rejects.toMatchObject({ code: 'ENOENT' })
    expect(b.warnings).toEqual([])
  })

  it('arms reports only when Node itself excludes environment variables from them', async () => {
    const armed = (host: Partial<WatchdogHost>) => {
      const b = bench(host)
      const stop = startWatchdog(options({ fatalErrorReport: true }), b.host, b.logger)
      const { reportOnFatalError } = b.report
      stop()
      return { reportOnFatalError, warnings: b.warnings }
    }
    expect(armed({})).toEqual({
      reportOnFatalError: false,
      warnings: [expect.stringContaining('start Node with --report-exclude-env')],
    })
    expect(armed({ execArgv: ['--report-exclude-env', '--no-report-exclude-env'] }).reportOnFatalError).toBe(false)
    await expect(stat(directory)).rejects.toMatchObject({ code: 'ENOENT' })
    expect(armed({ nodeOptions: '--report-exclude-env' })).toEqual({ reportOnFatalError: true, warnings: [] })
  })

  it('arms near-limit heap snapshots only when Node writes them to the configured directory', () => {
    const armed = (heapSnapshots: number, host: Partial<WatchdogHost>) => {
      const b = bench(host)
      startWatchdog(options({ heapSnapshots }), b.host, b.logger)()
      return { snapshots: b.snapshots, warnings: b.warnings }
    }
    expect(armed(2, { execArgv: [`--diagnostic-dir=${directory}`] })).toEqual({ snapshots: [2], warnings: [] })
    expect(armed(1, { nodeOptions: `--max-old-space-size=4096 --diagnostic-dir "${directory}/"` }).snapshots).toEqual([1])
    expect(armed(0, { execArgv: [`--diagnostic-dir=${directory}`] }).snapshots).toEqual([])

    const unset = armed(1, {})
    expect(unset.snapshots).toEqual([])
    expect(unset.warnings).toEqual([expect.stringContaining('the working directory')])
    const elsewhere = armed(1, { execArgv: ['--diagnostic-dir=/elsewhere'] })
    expect(elsewhere.snapshots).toEqual([])
    expect(elsewhere.warnings).toEqual([expect.stringContaining(`--diagnostic-dir=${directory}`)])
  })
})

describe('Node startup options', () => {
  it.each([
    [[], undefined, undefined],
    [['--diagnostic-dir=/a'], undefined, '/a'],
    [['--diagnostic-dir', '/a'], undefined, '/a'],
    [['--diagnostic_dir=/a'], undefined, '/a'],
    [['--diagnostic-dir'], undefined, undefined],
    [[], '--diagnostic-dir=/a', '/a'],
    [[], '--diagnostic-dir /a', '/a'],
    [[], '  --diagnostic-dir=/a   --inspect ', '/a'],
    [[], '--diagnostic-dir="/with space"', '/with space'],
    [[], '"--diagnostic-dir=/with space"', '/with space'],
    [[], '--diagnostic-dir="/quote\\"d"', '/quote"d'],
    [[], '--diagnostic-dir=/a --diagnostic-dir=/b', '/b'],
    [['--diagnostic-dir=/cli'], '--diagnostic-dir=/env', '/cli'],
    [['--max-old-space-size=64'], '--diagnostic-dir=/env', '/env'],
  ] as const)('reads --diagnostic-dir from %j with NODE_OPTIONS %j as %j', (execArgv, nodeOptions, expected) => {
    expect(nodeOptionValue(nodeStartupOptions(execArgv, nodeOptions), '--diagnostic-dir')).toBe(expected)
  })

  it.each([
    [[], undefined, false],
    [['--report-exclude-env'], undefined, true],
    [['--report_exclude_env'], undefined, true],
    [[], '--max-old-space-size=64 --report-exclude-env', true],
    [['--no-report-exclude-env'], '--report-exclude-env', false],
    [['--report-exclude-env', '--no-report-exclude-env'], undefined, false],
    [['--report-exclude-network'], undefined, false],
  ] as const)('reads --report-exclude-env from %j with NODE_OPTIONS %j as %j', (execArgv, nodeOptions, expected) => {
    expect(nodeFlag(nodeStartupOptions(execArgv, nodeOptions), '--report-exclude-env')).toBe(expected)
  })
})
