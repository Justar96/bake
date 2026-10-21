/**
 * The JSON report of a run: host and build identity, the fixed method, every
 * sample, and per arm, workload, and cache mode distributions of each metric.
 */
import { createHash } from 'node:crypto'
import { readdirSync, readFileSync } from 'node:fs'
import { cpus, totalmem } from 'node:os'
import { join } from 'node:path'
import type { ArmBuild } from './arms.ts'
import type { CacheMode, SampleMetrics, SampleRecord } from './sample.ts'
import { distribution, type Distribution } from './stats.ts'
import type { WorkloadName } from './workloads.ts'

export const REPORT_SCHEMA = 'bake-rust-perf-report/1'

/** Metrics summarized as distributions; `markersAtFirstInput` is a count, kept per sample only. */
export const SUMMARY_METRICS = ['inputReadyMs', 'startupMs', 'replayMs', 'readyMs', 'firstDeltaMs', 'streamInputMs', 'streamMs', 'idleCpuMs', 'shutdownMs', 'peakRssMiB'] as const satisfies readonly (keyof SampleMetrics)[]
export type SummaryMetric = typeof SUMMARY_METRICS[number]

/** One arm, workload, and cache mode. */
export interface SummaryRow {
  readonly arm: string
  readonly workload: WorkloadName
  readonly mode: CacheMode
  readonly completed: number
  readonly failed: number
  /** Leaked descendants over every completed sample. */
  readonly leakedProcesses: number
  readonly metrics: Record<SummaryMetric, Distribution>
}

/** The fixed method, recorded so a later run can reproduce it. */
export interface Method {
  readonly samplesPerCell: number
  readonly order: string
  readonly viewport: { columns: number; rows: number }
  readonly paceMs: number
  readonly idleMs: number
  readonly rssIntervalMs: number
  readonly timeoutMs: number
  readonly maxLoad: number
  readonly cache: Record<CacheMode, string>
  readonly endpoints: string
}

/** A complete report. */
export interface Report {
  readonly schema: typeof REPORT_SCHEMA
  readonly createdAt: string
  readonly command: readonly string[]
  readonly interrupted: boolean
  readonly host: Record<string, unknown>
  readonly harness: Record<string, unknown>
  readonly method: Method
  readonly arms: readonly { label: string; kind: string; build: ArmBuild }[]
  readonly workloads: readonly WorkloadName[]
  readonly modes: readonly CacheMode[]
  readonly summary: readonly SummaryRow[]
  readonly samples: readonly SampleRecord[]
}

/**
 * Distributions of every measured sample, by arm, workload, and mode. Warm-up
 * and failed samples never enter a distribution; failures are counted.
 */
export function summarize(samples: readonly SampleRecord[]): SummaryRow[] {
  const cells = new Map<string, SampleRecord[]>()
  for (const sample of samples) {
    if (sample.warmup) continue
    const key = JSON.stringify([sample.arm, sample.workload, sample.mode])
    cells.set(key, [...cells.get(key) ?? [], sample])
  }
  return [...cells.values()].flatMap((cell) => {
    const [first] = cell
    if (first === undefined) return []
    const { arm, workload, mode } = first
    const completed = cell.flatMap(sample => sample.ok ? [sample] : [])
    const values = (metric: SummaryMetric): number[] => completed.flatMap((sample) => {
      const value = sample.metrics[metric]
      return value === null ? [] : [value]
    })
    const metrics = Object.fromEntries(SUMMARY_METRICS.map(metric => [metric, distribution(values(metric))]))
    return [{
      arm, workload, mode, completed: completed.length, failed: cell.length - completed.length,
      leakedProcesses: completed.reduce((total, sample) => total + sample.leakedProcesses.length, 0),
      metrics: metrics as Record<SummaryMetric, Distribution>,
    }]
  })
}

function readOr(path: string, fallback: string): string {
  try {
    return readFileSync(path, 'utf8')
  } catch {
    return fallback
  }
}

/** The machine's identity for a report: kernel, CPU, memory, runtime, and load. */
export function hostMetadata(clockTicks: number, loadAverage1m: number): Record<string, unknown> {
  const uname = Bun.spawnSync(['uname', '-srvm'], { stdout: 'pipe', stderr: 'ignore' })
  const governor = readOr('/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor', '').trim()
  return {
    platform: process.platform, arch: process.arch,
    kernel: uname.success ? uname.stdout.toString().trim() : null,
    cpuModel: cpus()[0]?.model ?? null, cpus: cpus().length,
    memoryMiB: Math.round(totalmem() / 1048576),
    cpuGovernor: governor === '' ? null : governor,
    bun: Bun.version, clockTicks, loadAverage1mAtStart: loadAverage1m,
  }
}

/** The harness's own revision and the hash of its sources, so a report names the method's code. */
export function harnessMetadata(repository: string): Record<string, unknown> {
  const git = (args: string[]): string | null => {
    const result = Bun.spawnSync(['git', ...args], { cwd: repository, stdout: 'pipe', stderr: 'ignore' })
    return result.success ? result.stdout.toString().trim() : null
  }
  const directory = join(repository, 'scripts/rust-perf')
  const files = ['scripts/rust-perf.ts', ...readdirSync(directory).filter(file => file.endsWith('.ts')).sort().map(file => `scripts/rust-perf/${file}`)]
  const hash = createHash('sha256')
  for (const file of files) hash.update(file).update('\0').update(readFileSync(join(repository, file))).update('\0')
  return { revision: git(['rev-parse', 'HEAD']), dirty: (git(['status', '--porcelain', '--', 'scripts/rust-perf.ts', 'scripts/rust-perf']) ?? '') !== '', sourceSha256: hash.digest('hex') }
}
