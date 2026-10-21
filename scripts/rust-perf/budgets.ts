/**
 * A budget proposal from a baseline report, by the rule
 * `docs/roadmap/rust-0.4/verification.md` ("Performance and soak") proposes:
 * at most 10% degradation of the median and p95 of each latency and of
 * process-tree peak RSS, and no leaked process or failed sample. The proposal
 * is a file for the owner to approve; until approved it budgets nothing, and
 * this module never writes documentation.
 */
import { createHash } from 'node:crypto'
import { isAbsolute, relative, resolve } from 'node:path'
import { REPORT_SCHEMA, type Report, type SummaryMetric } from './report.ts'
import type { Distribution, Interval } from './stats.ts'

export const BUDGETS_SCHEMA = 'bake-rust-perf-budgets/1'
/** The proposed largest acceptable degradation, as a fraction of the baseline. */
export const DEGRADATION = 0.10
/** The sample count verification.md proposes for each cell. */
export const PROPOSED_SAMPLES = 20

/** Metrics that get a bound: latencies and peak memory. Idle CPU is reported without one. */
export const BUDGETED_METRICS = ['inputReadyMs', 'startupMs', 'replayMs', 'readyMs', 'firstDeltaMs', 'streamInputMs', 'streamMs', 'shutdownMs', 'peakRssMiB'] as const satisfies readonly SummaryMetric[]
export const OBSERVED_METRICS = ['idleCpuMs'] as const satisfies readonly SummaryMetric[]

/** One metric's proposed bounds and the baseline they come from. */
export interface MetricBudget {
  readonly samples: number
  readonly baselineMedian: number
  readonly baselineP95: number
  readonly baselineMedianCi: Interval | null
  readonly baselineP95Ci: Interval | null
  /** The candidate's median may not exceed this. */
  readonly maxMedian: number
  /** The candidate's p95 may not exceed this. */
  readonly maxP95: number
}

/** The proposal for one workload and cache mode. */
export interface CellBudget {
  readonly workload: string
  readonly mode: string
  readonly metrics: Partial<Record<typeof BUDGETED_METRICS[number], MetricBudget>>
  readonly observed: Partial<Record<typeof OBSERVED_METRICS[number], Distribution>>
  readonly maxLeakedProcesses: 0
  readonly maxFailedSamples: 0
}

/** A proposal file. */
export interface BudgetProposal {
  readonly schema: typeof BUDGETS_SCHEMA
  readonly status: 'proposed'
  readonly approval: { readonly approvedBy: null; readonly approvedAt: null; readonly instructions: string }
  readonly source: Record<string, unknown>
  readonly rule: Record<string, unknown>
  readonly cells: readonly CellBudget[]
  readonly warnings: readonly string[]
}

/** The baseline plus the allowed degradation, rounded up to three decimals once float noise is removed. */
function bound(value: number): number {
  return Math.ceil(Math.round(value * (1 + DEGRADATION) * 1e6) / 1e3) / 1000
}

/**
 * Propose budgets from the baseline arm of `report`.
 * @param report - a run report.
 * @param reportBytes - the report file's bytes, hashed into the proposal.
 * @param reportPath - recorded as the proposal's source.
 * @param arm - the baseline arm's label; required when the report has more than one arm.
 */
export function proposeBudgets(report: Report, reportBytes: Uint8Array, reportPath: string, arm?: string): BudgetProposal {
  if (report.schema !== REPORT_SCHEMA) throw new Error(`not a rust-perf report: schema ${JSON.stringify(report.schema)}`)
  const labels = report.arms.map(entry => entry.label)
  const baseline = arm ?? (labels.length === 1 ? labels[0] : undefined)
  if (baseline === undefined) throw new Error(`the report has arms ${labels.join(', ')}; name the baseline with --arm`)
  const armEntry = report.arms.find(entry => entry.label === baseline)
  if (armEntry === undefined) throw new Error(`the report has no arm ${baseline}; it has ${labels.join(', ')}`)
  const warnings: string[] = []
  if (report.interrupted) warnings.push('the baseline run was interrupted, so some cells may be incomplete')
  const cells: CellBudget[] = []
  for (const row of report.summary.filter(entry => entry.arm === baseline)) {
    const cell = `${row.workload}/${row.mode}`
    if (row.failed > 0) warnings.push(`${cell}: ${row.failed} baseline sample(s) failed; investigate before approving`)
    if (row.leakedProcesses > 0) warnings.push(`${cell}: the baseline itself leaked ${row.leakedProcesses} process(es)`)
    if (row.completed < PROPOSED_SAMPLES) warnings.push(`${cell}: ${row.completed} completed sample(s), fewer than the proposed ${PROPOSED_SAMPLES}`)
    const metrics: CellBudget['metrics'] = {}
    for (const metric of BUDGETED_METRICS) {
      const stats = row.metrics[metric]
      if (stats.median === null || stats.p95 === null) continue
      metrics[metric] = {
        samples: stats.samples, baselineMedian: stats.median, baselineP95: stats.p95,
        baselineMedianCi: stats.medianCi, baselineP95Ci: stats.p95Ci,
        maxMedian: bound(stats.median), maxP95: bound(stats.p95),
      }
    }
    const observed: CellBudget['observed'] = {}
    for (const metric of OBSERVED_METRICS) observed[metric] = row.metrics[metric]
    cells.push({ workload: row.workload, mode: row.mode, metrics, observed, maxLeakedProcesses: 0, maxFailedSamples: 0 })
  }
  if (cells.length === 0) throw new Error(`the report has no summarized samples for ${baseline}`)
  return {
    schema: BUDGETS_SCHEMA,
    status: 'proposed',
    approval: {
      approvedBy: null, approvedAt: null,
      instructions: 'Proposed, not frozen. The owner reviews the baseline report and these bounds, then records approval before any candidate measurement; only the approved values become D18 budgets.',
    },
    source: {
      report: reportPath, reportSha256: createHash('sha256').update(reportBytes).digest('hex'), reportCreatedAt: report.createdAt,
      arm: baseline, build: armEntry.build, host: report.host, harness: report.harness, method: report.method,
    },
    rule: {
      maxDegradation: DEGRADATION, statistics: ['median', 'p95'],
      metrics: BUDGETED_METRICS, observedWithoutBound: OBSERVED_METRICS,
      leakedProcesses: 0, failedSamples: 0, terminalRestoration: 'every sample',
      comparison: 'A candidate passes a bound when its own median or p95 over the same method stays at or below it; judge a result near a bound against both runs\' bootstrap intervals rather than one run.',
    },
    cells,
    warnings,
  }
}

/**
 * Refuse a path inside the repository's `docs/`, where budgets are written by hand after approval.
 * @param path - the intended output.
 * @param repository - the checkout root.
 */
export function refuseDocsPath(path: string, repository: string): void {
  const inside = relative(resolve(repository, 'docs'), resolve(path))
  if (inside === '' || (!inside.startsWith('..') && !isAbsolute(inside))) throw new Error(`refusing to write ${path}: rust-perf never writes under docs/; record approved budgets there by hand`)
}
