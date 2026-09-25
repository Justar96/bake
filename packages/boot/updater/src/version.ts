/**
 * Release version syntax and ordering.
 * @module @deepseek-ai/dsh-updater/version
 */

/** A Bake release version: `major.minor.patch` with an optional prerelease. */
export const VERSION_PATTERN = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/

/**
 * Whether `value` is a release version the updater accepts.
 * @param value - candidate text.
 * @returns whether it matches {@link VERSION_PATTERN}.
 */
export function isReleaseVersion(value: unknown): value is string {
  return typeof value === 'string' && VERSION_PATTERN.test(value)
}

/**
 * Order two release versions by semantic-versioning precedence.
 *
 * A prerelease sorts before its release, and prerelease identifiers compare
 * numerically when both are numeric and lexically otherwise, with numeric
 * identifiers first. Build metadata is not part of the syntax.
 *
 * @param left - a release version.
 * @param right - a release version.
 * @returns negative, zero, or positive as `left` is older, equal, or newer.
 */
export function compareVersions(left: string, right: string): number {
  const [leftCore = '', leftPre] = split(left)
  const [rightCore = '', rightPre] = split(right)
  const cores = leftCore.split('.').map(Number)
  const others = rightCore.split('.').map(Number)
  for (let index = 0; index < 3; index++) {
    const difference = (cores[index] ?? 0) - (others[index] ?? 0)
    if (difference !== 0) return Math.sign(difference)
  }
  if (leftPre === undefined || rightPre === undefined) return leftPre === rightPre ? 0 : leftPre === undefined ? 1 : -1
  const leftIds = leftPre.split('.')
  const rightIds = rightPre.split('.')
  for (let index = 0; index < Math.max(leftIds.length, rightIds.length); index++) {
    const a = leftIds[index]
    const b = rightIds[index]
    if (a === undefined) return -1
    if (b === undefined) return 1
    if (a === b) continue
    const numericA = /^\d+$/.test(a)
    const numericB = /^\d+$/.test(b)
    if (numericA && numericB) return Math.sign(Number(a) - Number(b))
    if (numericA !== numericB) return numericA ? -1 : 1
    return a < b ? -1 : 1
  }
  return 0
}

function split(version: string): [string, string | undefined] {
  const dash = version.indexOf('-')
  return dash === -1 ? [version, undefined] : [version.slice(0, dash), version.slice(dash + 1)]
}
