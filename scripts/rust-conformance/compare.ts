/**
 * Pure comparators for the synthetic conformance slice. Each returns one named
 * result with a mismatch location that names indices, byte offsets, or
 * workspace-relative paths but never the compared text itself.
 */

import type { FileEntry, JsonValue, Permission, SyntheticEvent } from './fixture.ts'

export type ComparatorName = 'prompt-bytes' | 'event-order' | 'permissions' | 'final-files'
export const COMPARATORS: readonly ComparatorName[] = ['prompt-bytes', 'event-order', 'permissions', 'final-files']

export interface ComparatorResult {
  comparator: ComparatorName
  /** `unavailable` when a side produced no usable observation. */
  outcome: 'pass' | 'fail' | 'unavailable'
  detail?: string
}

const pass = (comparator: ComparatorName): ComparatorResult => ({ comparator, outcome: 'pass' })
const mismatch = (comparator: ComparatorName, detail: string): ComparatorResult => ({ comparator, outcome: 'fail', detail })

/**
 * Compare prompts by their exact UTF-8 bytes.
 * @param left - one side's prompts.
 * @param right - the other side's prompts.
 */
export function comparePrompts(left: readonly string[], right: readonly string[]): ComparatorResult {
  if (left.length !== right.length) return mismatch('prompt-bytes', `prompt count ${left.length} differs from ${right.length}`)
  for (const [index, prompt] of left.entries()) {
    const a = Buffer.from(prompt, 'utf8')
    const b = Buffer.from(right[index] ?? '', 'utf8')
    if (a.equals(b)) continue
    let offset = 0
    while (offset < a.length && offset < b.length && a[offset] === b[offset]) offset++
    return mismatch('prompt-bytes', `prompt ${index} differs at byte ${offset} (${a.length} vs ${b.length} bytes)`)
  }
  return pass('prompt-bytes')
}

/**
 * Structural equality: arrays keep their order, object key order is
 * immaterial, and every field counts, including ones this tool does not know.
 * @param left - one value.
 * @param right - the other value.
 */
export function jsonEqual(left: JsonValue, right: JsonValue): boolean {
  if (left === null || right === null || typeof left !== 'object' || typeof right !== 'object') {
    return left === right
  }
  if (Array.isArray(left) || Array.isArray(right)) {
    return Array.isArray(left) && Array.isArray(right) && left.length === right.length
      && left.every((item, index) => jsonEqual(item, right[index] as JsonValue))
  }
  const keys = Object.keys(left)
  return keys.length === Object.keys(right).length
    && keys.every(key => Object.hasOwn(right, key) && jsonEqual(left[key] as JsonValue, right[key] as JsonValue))
}

function compareSequence(
  comparator: ComparatorName, noun: string, left: readonly JsonValue[], right: readonly JsonValue[],
): ComparatorResult {
  if (left.length !== right.length) return mismatch(comparator, `${noun} count ${left.length} differs from ${right.length}`)
  const index = left.findIndex((item, at) => !jsonEqual(item, right[at] as JsonValue))
  return index === -1 ? pass(comparator) : mismatch(comparator, `${noun} ${index} differs`)
}

/**
 * Compare events in order, with every field significant.
 * @param left - one side's events.
 * @param right - the other side's events.
 */
export const compareEvents = (left: readonly SyntheticEvent[], right: readonly SyntheticEvent[]): ComparatorResult =>
  compareSequence('event-order', 'event', left, right)

/**
 * Compare permission records in order.
 * @param left - one side's permissions.
 * @param right - the other side's permissions.
 */
export const comparePermissions = (left: readonly Permission[], right: readonly Permission[]): ComparatorResult =>
  compareSequence('permissions', 'permission', left as unknown as JsonValue[], right as unknown as JsonValue[])

/**
 * Compare final regular-file sets by exact path and bytes; listing order is
 * immaterial.
 * @param left - one side's files.
 * @param right - the other side's files.
 */
export function compareFiles(left: readonly FileEntry[], right: readonly FileEntry[]): ComparatorResult {
  const a = new Map(left.map(file => [file.path, file.hex]))
  const b = new Map(right.map(file => [file.path, file.hex]))
  for (const path of [...new Set([...a.keys(), ...b.keys()])].sort()) {
    if (!a.has(path)) return mismatch('final-files', `${JSON.stringify(path)} exists only on the right`)
    if (!b.has(path)) return mismatch('final-files', `${JSON.stringify(path)} exists only on the left`)
    if (a.get(path) !== b.get(path)) return mismatch('final-files', `${JSON.stringify(path)} has different bytes`)
  }
  return pass('final-files')
}

/** What one side of a comparison produced; a missing part makes its comparator unavailable. */
export interface Outcome {
  prompts?: readonly string[]
  events?: readonly SyntheticEvent[]
  permissions?: readonly Permission[]
  files?: readonly FileEntry[]
}

/**
 * Run all four comparators; each result stands on its own, so one mismatch
 * never hides another.
 * @param left - one side.
 * @param right - the other side.
 */
export function compareAll(left: Outcome, right: Outcome): ComparatorResult[] {
  const either = <T>(comparator: ComparatorName, a: T | undefined, b: T | undefined, compare: (a: T, b: T) => ComparatorResult) =>
    a === undefined || b === undefined ? { comparator, outcome: 'unavailable' as const } : compare(a, b)
  return [
    either('prompt-bytes', left.prompts, right.prompts, comparePrompts),
    either('event-order', left.events, right.events, compareEvents),
    either('permissions', left.permissions, right.permissions, comparePermissions),
    either('final-files', left.files, right.files, compareFiles),
  ]
}
