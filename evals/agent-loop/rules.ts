/**
 * The statistics and regression rule `record.ts` applies to paired samples,
 * kept apart from its command line so tests can exercise them. See the
 * Regressions section of evals/README.md.
 */

/** A paired percent change with its bootstrap 95% interval. */
export interface Change { pct: number; lo: number; hi: number }

/** Seeded generator so a re-recorded run reproduces its intervals. */
export function mulberry32(seed: number) {
  return () => {
    seed |= 0; seed = seed + 0x6D2B79F5 | 0
    let t = Math.imul(seed ^ seed >>> 15, 1 | seed)
    t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t
    return ((t ^ t >>> 14) >>> 0) / 4294967296
  }
}

/** Percent change of the summed metric, candidate over base, with a paired bootstrap 95% interval. */
export function pairedChange<T>(pairs: [T, T][], metric: (sample: T) => number): Change | null {
  const base = pairs.map(pair => metric(pair[0])); const next = pairs.map(pair => metric(pair[1]))
  const sum = (values: number[]) => values.reduce((total, value) => total + value, 0)
  if (sum(base) === 0) return null
  const random = mulberry32(7)
  const draws: number[] = []
  for (let round = 0; round < 4000; round++) {
    let b = 0; let c = 0
    for (let index = 0; index < pairs.length; index++) { const pick = Math.floor(random() * pairs.length); b += base[pick]!; c += next[pick]! }
    if (b > 0) draws.push((c - b) / b * 100)
  }
  draws.sort((x, y) => x - y)
  const round1 = (value: number) => Math.round(value * 10) / 10
  return { pct: round1((sum(next) - sum(base)) / sum(base) * 100), lo: round1(draws[Math.floor(0.025 * draws.length)]!), hi: round1(draws[Math.ceil(0.975 * draws.length) - 1]!) }
}

/**
 * A failed sample's category: `runaway` when the whitespace-runaway guard
 * aborted a tool call, else the runner's abort cause, a non-zero exit, an
 * empty final reply, or a failed check. Null for a success.
 */
export function failureOf(sample: Record<string, any>): string | null {
  if (sample.success === true) return null
  if (sample.runawayAbort === true) return 'runaway'
  return sample.abortCause ?? (sample.code !== 0 ? `exit ${sample.code}` : (sample.final ?? '') === '' ? 'empty final reply' : 'validation failed')
}

/** A requests rise the gate flags: more than this percent, with the whole interval above zero. */
export const REQUESTS_GATE_PCT = 10

/** The all-tasks comparison of one model against one base that the rule reads. */
export interface RuleInput {
  totalTokens: Change | null
  requestsChange: Change | null
  failures: number[]
  toolErrors: number[]
}

/**
 * Regression rule: over all tasks for one model, a total-token increase whose
 * whole 95% interval is above zero; a requests increase of more than 10% whose
 * whole interval is above zero; more failed runs than the base by two or more;
 * or more tool errors than the base.
 */
export function regressions(label: string, result: RuleInput): string[] {
  const found: string[] = []
  if (result.totalTokens !== null && result.totalTokens.lo > 0) found.push(`${label}: total tokens ${result.totalTokens.pct}% [${result.totalTokens.lo}, ${result.totalTokens.hi}]`)
  if (result.requestsChange !== null && result.requestsChange.pct > REQUESTS_GATE_PCT && result.requestsChange.lo > 0) {
    found.push(`${label}: requests ${result.requestsChange.pct}% [${result.requestsChange.lo}, ${result.requestsChange.hi}]`)
  }
  if (result.failures[1]! >= result.failures[0]! + 2) found.push(`${label}: failures ${result.failures[0]} -> ${result.failures[1]}`)
  if (result.toolErrors[1]! > result.toolErrors[0]!) found.push(`${label}: tool errors ${result.toolErrors[0]} -> ${result.toolErrors[1]}`)
  return found
}

export const REGRESSION_RULE = 'all tasks, per model: total tokens with the 95% interval above zero; requests up more than 10% with the 95% interval above zero; failures +2 or more; any tool-error increase'
