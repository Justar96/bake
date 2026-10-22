/**
 * Process health diagnostics for long sessions. The plugin samples Node's
 * event-loop delay, V8 heap use, and resident memory on an unreferenced
 * timer. It appends a rate-limited record to a per-process file under
 * `directory` when a threshold is crossed, and arms Node's fatal-error report
 * in the same directory when Node keeps environment variables out of it. It
 * writes nothing to stdout or stderr and adds nothing to model context.
 * @module bake-runtime-watchdog
 */

import { isAbsolute } from 'node:path'
import { monitorEventLoopDelay, performance } from 'node:perf_hooks'
import { getHeapStatistics, setHeapSnapshotNearHeapLimit } from 'node:v8'
import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { startWatchdog, type ReportSettings, type WatchdogHost } from './watchdog.ts'

/** Cordis plugin name used by loader diagnostics. */
export const name = 'runtime-watchdog'

/** Plugin config. Every field except `directory` has a schema default. */
export interface Config {
  /** Absolute directory for watchdog records, fatal-error reports, and heap snapshots; created with mode 0700. */
  directory: string
  /** Milliseconds between samples; each sample closes one event-loop delay window. */
  intervalMs?: number
  /** A window whose 99th-percentile event-loop delay reaches this many milliseconds counts as delayed. */
  eventLoopDelayMs?: number
  /** Milliseconds of consecutive delayed windows, or of one continuous stall, before a delay is recorded. */
  sustainedMs?: number
  /** Heap use at or above this fraction of V8's heap size limit is recorded. */
  heapFraction?: number
  /** Minimum milliseconds between two records of the same kind; crossings in between are counted. */
  recordIntervalMs?: number
  /**
   * Write Node's diagnostic report into `directory` on a fatal error such as
   * running out of heap. A fatal error raised outside JavaScript ignores the
   * runtime `process.report.excludeEnv`, so reports are armed only when the
   * process started with `--report-exclude-env`.
   */
  fatalErrorReport?: boolean
  /**
   * Heap snapshots V8 may write as the heap nears its limit; 0 disables them.
   * Node writes them to its `--diagnostic-dir`, so they are armed only when
   * the process started with `--diagnostic-dir` set to `directory`.
   */
  heapSnapshots?: number
}

export const Config: z<Config> = z.object({
  directory: z.string().required(),
  intervalMs: z.natural().min(100).default(10_000),
  eventLoopDelayMs: z.natural().min(1).default(250),
  sustainedMs: z.natural().default(30_000),
  heapFraction: z.number().min(0).max(1).default(0.85),
  recordIntervalMs: z.natural().default(300_000),
  fatalErrorReport: z.boolean().default(true),
  heapSnapshots: z.natural().default(0),
})

/** Node's process facilities for the mounted plugin. */
function nodeHost(): WatchdogHost {
  return {
    now: () => performance.now(),
    eventLoopDelay: resolution => monitorEventLoopDelay({ resolution }),
    heap: () => {
      const statistics = getHeapStatistics()
      return { used: statistics.used_heap_size, total: statistics.total_heap_size, limit: statistics.heap_size_limit }
    },
    rss: () => process.memoryUsage.rss(),
    // Node 22.19 has `excludeNetwork`; the pinned @types/node does not declare it.
    report: process.report as NodeJS.ProcessReport & ReportSettings,
    setHeapSnapshotNearHeapLimit,
    execArgv: process.execArgv,
    nodeOptions: process.env.NODE_OPTIONS,
    pid: process.pid,
  }
}

/**
 * Start the watchdog for this fiber. Disposal clears the timer, disables the
 * delay histogram, and restores the previous `process.report` settings; an
 * armed near-limit heap snapshot stays armed until the process exits.
 * @param ctx - the plugin context whose fiber owns the watchdog.
 * @param config - validated plugin config.
 * @throws when `directory` is relative or cannot be created.
 */
export function apply(ctx: Context, config: Config): void {
  if (!isAbsolute(config.directory)) {
    throw new Error(`runtime-watchdog: directory must be an absolute path, got ${JSON.stringify(config.directory)}`)
  }
  const logger = ctx.logger(name)
  ctx.effect(() => startWatchdog(config as Required<Config>, nodeHost(), logger), 'runtime-watchdog')
}
