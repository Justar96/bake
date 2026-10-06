/** Fixed workload sizes and aggregates for local, uncalibrated terminal diagnostics. */
import type { HistoryDimensions, HistoryOptions } from './history.ts'

/** One explicit main-process sample with live session state still reachable. */
export interface Metrics {
  sequence: number
  beforeGc: NodeJS.MemoryUsage
  afterGc: NodeJS.MemoryUsage
  resources: NodeJS.ResourceUsage
  cpu: NodeJS.CpuUsage
}

/** Turn counts, tool density, and large-message size vary independently. */
export const WORKLOADS = {
  fresh: { turns: 0 },
  small: { turns: 50 },
  typical: { turns: 500 },
  tail: { turns: 2000 },
  tools: { turns: 500, toolEvery: 1, toolsPerTurn: 4 },
  'large-output': { turns: 12, assistantTextBytes: 64 * 1024 },
} as const satisfies Record<string, HistoryOptions & { turns: number }>

/** One fully observed session, including its final delta and clean process exit. */
export interface Sample {
  workload: string
  iteration: number
  dimensions: HistoryDimensions & { fileBytes: number; sessionId: string }
  /** First probe write through composer echo, before waiting for the replay tail. */
  initialInputMs: number
  /** Spawn through the first observed composer echo, even while history is arriving. */
  firstInputMs: number
  historyMarkersAtFirstInput: number
  /** Spawn through the first composer echo and every historical marker, including trailing tools. */
  readyMs: number
  idleInputMs: number[]
  idleBytes: number
  initialBytes: number
  firstDeltaMs: number
  liveInputMs: number
  streamMs: number
  streamBytes: number
  /** Second Ctrl-C write through the observed clean Node exit; excludes the quit-confirmation wait. */
  shutdownMs: number
  historyMarkerOccurrences: number
  readyMemory: Metrics
  settledMemory: Metrics
}

/** An incomplete sample; failures never enter latency or memory aggregates. */
export interface Failure {
  workload: string
  iteration: number
  error: string
  dimensions?: Sample['dimensions'] | undefined
}

/**
 * Spread of one latency field over a workload's completed samples. Every
 * statistic is null when no sample completed, so an empty set stays explicit.
 */
export interface Distribution {
  samples: number
  min: number | null
  /** Mean of the two middle values for an even count, matching the summary medians. */
  median: number | null
  /** Nearest rank: the ceil(0.95 n)-th smallest value. It equals `max` below 20 samples. */
  p95: number | null
  max: number | null
  /** Unscaled median absolute deviation from `median`. */
  mad: number | null
}

/** Summary latency fields that also report a distribution. */
export const LATENCY_FIELDS = ['initialInputMs', 'firstInputMs', 'readyMs', 'maxIdleInputMs', 'liveInputMs', 'firstDeltaMs', 'streamMs', 'shutdownMs'] as const

function median(values: readonly number[]): number | null {
  if (values.length === 0) return null
  const sorted = values.toSorted((left, right) => left - right)
  const middle = Math.floor(sorted.length / 2)
  return sorted.length % 2 === 0 ? (sorted[middle - 1]! + sorted[middle]!) / 2 : sorted[middle]!
}

/**
 * Describe completed observations without discarding outliers.
 * @param values - one value per completed sample; callers exclude failures.
 * @returns the distribution, with null statistics for an empty set.
 */
export function distribution(values: readonly number[]): Distribution {
  if (values.length === 0) return { samples: 0, min: null, median: null, p95: null, max: null, mad: null }
  const sorted = values.toSorted((left, right) => left - right)
  const middle = median(sorted)!
  return { samples: sorted.length, min: sorted[0]!, median: middle,
    p95: sorted[Math.ceil(0.95 * sorted.length) - 1]!, max: sorted.at(-1)!,
    mad: median(sorted.map(value => Math.abs(value - middle)))! }
}

/**
 * Summarize completed samples and retain failed counts beside every median.
 * @param results - raw observations, including incomplete samples.
 * @returns per-workload medians and latency distributions, with null statistics when every sample failed.
 */
export function summarize(results: readonly (Sample | Failure)[]) {
  return [...new Set(results.map(result => result.workload))].map(workload => {
    const matching = results.filter(result => result.workload === workload)
    const completed = matching.filter((result): result is Sample => !('error' in result))
    const latency: Record<typeof LATENCY_FIELDS[number], number[]> = {
      initialInputMs: completed.map(sample => sample.initialInputMs),
      firstInputMs: completed.map(sample => sample.firstInputMs),
      readyMs: completed.map(sample => sample.readyMs),
      maxIdleInputMs: completed.map(sample => Math.max(...sample.idleInputMs)),
      liveInputMs: completed.map(sample => sample.liveInputMs),
      firstDeltaMs: completed.map(sample => sample.firstDeltaMs),
      streamMs: completed.map(sample => sample.streamMs),
      shutdownMs: completed.map(sample => sample.shutdownMs),
    }
    return { workload, completed: completed.length, failed: matching.length - completed.length,
      initialInputMs: median(latency.initialInputMs),
      firstInputMs: median(latency.firstInputMs),
      readyMs: median(latency.readyMs),
      maxIdleInputMs: median(latency.maxIdleInputMs),
      liveInputMs: median(latency.liveInputMs),
      firstDeltaMs: median(latency.firstDeltaMs),
      streamMs: median(latency.streamMs),
      shutdownMs: median(latency.shutdownMs),
      retainedHeapMiB: median(completed.map(sample => sample.readyMemory.afterGc.heapUsed / 1048576)),
      settledRetainedHeapMiB: median(completed.map(sample => sample.settledMemory.afterGc.heapUsed / 1048576)),
      peakRssMiB: median(completed.map(sample => sample.settledMemory.resources.maxRSS / 1024)),
      initialBytes: median(completed.map(sample => sample.initialBytes)),
      idleBytes: median(completed.map(sample => sample.idleBytes)),
      streamBytes: median(completed.map(sample => sample.streamBytes)),
      distributions: Object.fromEntries(LATENCY_FIELDS.map(field => [field, distribution(latency[field])])) as Record<typeof LATENCY_FIELDS[number], Distribution>,
    }
  })
}
