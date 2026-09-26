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
 * Summarize completed samples and retain failed counts beside every median.
 * @param results - raw observations, including incomplete samples.
 * @returns per-workload medians, or null measurements when every sample failed.
 */
export function summarize(results: readonly (Sample | Failure)[]) {
  const median = (values: number[]): number | null => {
    if (values.length === 0) return null
    values.sort((left, right) => left - right)
    const middle = Math.floor(values.length / 2)
    return values.length % 2 === 0 ? (values[middle - 1]! + values[middle]!) / 2 : values[middle]!
  }
  return [...new Set(results.map(result => result.workload))].map(workload => {
    const matching = results.filter(result => result.workload === workload)
    const completed = matching.filter((result): result is Sample => !('error' in result))
    return { workload, completed: completed.length, failed: matching.length - completed.length,
      initialInputMs: median(completed.map(sample => sample.initialInputMs)),
      firstInputMs: median(completed.map(sample => sample.firstInputMs)),
      readyMs: median(completed.map(sample => sample.readyMs)),
      maxIdleInputMs: median(completed.map(sample => Math.max(...sample.idleInputMs))),
      liveInputMs: median(completed.map(sample => sample.liveInputMs)),
      firstDeltaMs: median(completed.map(sample => sample.firstDeltaMs)),
      streamMs: median(completed.map(sample => sample.streamMs)),
      retainedHeapMiB: median(completed.map(sample => sample.readyMemory.afterGc.heapUsed / 1048576)),
      settledRetainedHeapMiB: median(completed.map(sample => sample.settledMemory.afterGc.heapUsed / 1048576)),
      peakRssMiB: median(completed.map(sample => sample.settledMemory.resources.maxRSS / 1024)),
      initialBytes: median(completed.map(sample => sample.initialBytes)),
      idleBytes: median(completed.map(sample => sample.idleBytes)),
      streamBytes: median(completed.map(sample => sample.streamBytes)),
    }
  })
}
