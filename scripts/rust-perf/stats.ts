/**
 * Order statistics and bootstrap confidence intervals for performance samples.
 * Every function is pure and deterministic: the bootstrap draws from a seeded
 * generator, so a report recomputes to the same intervals.
 */

/** A two-sided interval. */
export interface Interval {
  readonly low: number
  readonly high: number
}

/** The spread of one metric over completed samples; statistics are null without samples. */
export interface Distribution {
  readonly samples: number
  readonly min: number | null
  readonly median: number | null
  readonly p95: number | null
  readonly max: number | null
  /** Unscaled median absolute deviation from the median. */
  readonly mad: number | null
  /** Percentile bootstrap interval of the median. */
  readonly medianCi: Interval | null
  /** Percentile bootstrap interval of the p95. */
  readonly p95Ci: Interval | null
}

/** Bootstrap settings: resample count, interval level, and generator seed. */
export interface BootstrapOptions {
  readonly resamples: number
  readonly confidence: number
  readonly seed: number
}

export const DEFAULT_BOOTSTRAP: BootstrapOptions = { resamples: 2000, confidence: 0.95, seed: 0x5eed }

/** The value at `index`, which callers have bounds-checked. */
function nth(values: ArrayLike<number>, index: number): number {
  const value = values[index]
  if (value === undefined) throw new RangeError(`no sample at index ${index}`)
  return value
}

function sorted(values: readonly number[]): number[] {
  for (const value of values) if (!Number.isFinite(value)) throw new RangeError(`not a finite sample: ${value}`)
  return [...values].sort((a, b) => a - b)
}

/** The median of sorted values; an even count averages the two middle values. */
function medianSorted(values: readonly number[]): number {
  const middle = values.length >> 1
  return values.length % 2 === 1 ? nth(values, middle) : (nth(values, middle - 1) + nth(values, middle)) / 2
}

/** The nearest-rank p95 of sorted values: the ceil(0.95 n)-th smallest. */
function p95Sorted(values: readonly number[]): number {
  return nth(values, Math.max(0, Math.ceil(0.95 * values.length) - 1))
}

/**
 * The median; an even count averages the two middle values.
 * @throws RangeError for an empty list or a non-finite value.
 */
export function median(values: readonly number[]): number {
  if (values.length === 0) throw new RangeError('median of no samples')
  return medianSorted(sorted(values))
}

/**
 * The nearest-rank 95th percentile: the ceil(0.95 n)-th smallest value, so
 * it equals the maximum below 20 samples.
 * @throws RangeError for an empty list or a non-finite value.
 */
export function p95(values: readonly number[]): number {
  if (values.length === 0) throw new RangeError('p95 of no samples')
  return p95Sorted(sorted(values))
}

/** The unscaled median absolute deviation from the median. */
export function mad(values: readonly number[]): number {
  const center = median(values)
  return median(values.map(value => Math.abs(value - center)))
}

/** Mulberry32: a small seeded generator of uniform values in [0, 1). */
export function seededRandom(seed: number): () => number {
  let state = seed >>> 0
  return () => {
    state = (state + 0x6d2b79f5) >>> 0
    let t = state
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

/**
 * A percentile bootstrap interval of `statistic` over `values`.
 * @param values - the samples, at least one.
 * @param statistic - computed on each sorted resample.
 * @param options - resamples, level, and seed.
 * @returns the interval between the (1-level)/2 and (1+level)/2 quantiles of the resampled statistic.
 */
export function bootstrapInterval(
  values: readonly number[],
  statistic: (sortedValues: readonly number[]) => number,
  options: BootstrapOptions = DEFAULT_BOOTSTRAP,
): Interval {
  if (values.length === 0) throw new RangeError('bootstrap of no samples')
  if (!Number.isSafeInteger(options.resamples) || options.resamples < 1) throw new RangeError('bootstrap needs a positive resample count')
  if (!(options.confidence > 0 && options.confidence < 1)) throw new RangeError('bootstrap confidence must be inside (0, 1)')
  const source = sorted(values)
  const random = seededRandom(options.seed)
  const estimates = new Float64Array(options.resamples)
  const resample = new Array<number>(source.length)
  for (let draw = 0; draw < options.resamples; draw++) {
    for (let index = 0; index < source.length; index++) resample[index] = nth(source, Math.floor(random() * source.length))
    resample.sort((a, b) => a - b)
    estimates[draw] = statistic(resample)
  }
  estimates.sort()
  const tail = (1 - options.confidence) / 2
  const at = (q: number): number => nth(estimates, Math.min(estimates.length - 1, Math.max(0, Math.floor(q * estimates.length))))
  return { low: at(tail), high: at(1 - tail) }
}

/**
 * Summarize one metric's completed samples.
 * @param values - the samples; empty gives null statistics.
 * @param options - bootstrap settings for the intervals.
 */
export function distribution(values: readonly number[], options: BootstrapOptions = DEFAULT_BOOTSTRAP): Distribution {
  if (values.length === 0) return { samples: 0, min: null, median: null, p95: null, max: null, mad: null, medianCi: null, p95Ci: null }
  const ordered = sorted(values)
  return {
    samples: ordered.length,
    min: nth(ordered, 0),
    median: medianSorted(ordered),
    p95: p95Sorted(ordered),
    max: nth(ordered, ordered.length - 1),
    mad: mad(ordered),
    medianCi: bootstrapInterval(ordered, medianSorted, options),
    p95Ci: bootstrapInterval(ordered, p95Sorted, options),
  }
}
