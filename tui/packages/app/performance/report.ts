/** Fixed workload sizes and aggregates for local, uncalibrated terminal diagnostics. */
/** One explicit main-process sample with live session state still reachable. */
export interface Metrics {
  sequence: number
  beforeGc: NodeJS.MemoryUsage
  afterGc: NodeJS.MemoryUsage
  resources: NodeJS.ResourceUsage
  cpu: NodeJS.CpuUsage
}

/** Completed turns per workload; each fourth turn includes a tool result. */
export const WORKLOADS = { fresh: 0, small: 50, typical: 500, tail: 2000 } as const

/** One fully observed session, including its final delta and clean process exit. */
export interface Sample {
  workload: string
  iteration: number
  dimensions: { turns: number; events: number; deltaCount: number; toolCount: number; fileBytes: number; sessionId: string }
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
      readyMs: median(completed.map(sample => sample.readyMs)),
      maxIdleInputMs: median(completed.map(sample => Math.max(...sample.idleInputMs))),
      liveInputMs: median(completed.map(sample => sample.liveInputMs)),
      firstDeltaMs: median(completed.map(sample => sample.firstDeltaMs)),
      streamMs: median(completed.map(sample => sample.streamMs)),
      retainedHeapMiB: median(completed.map(sample => sample.readyMemory.afterGc.heapUsed / 1048576)),
      peakRssMiB: median(completed.map(sample => sample.settledMemory.resources.maxRSS / 1024)),
    }
  })
}
