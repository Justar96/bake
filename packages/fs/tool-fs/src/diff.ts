/**
 * Result-time contextual diff presentation for write and edit. Storage returns before/after
 * text; this model-facing layer derives one three-line-context card per applied hunk.
 * @module @deepseek-ai/dsh-tool-fs/src/diff
 */

import { structuredPatch } from 'diff'
import type { FileDiff } from '@deepseek-ai/dsh-tools'

/** Context lines shown on each side of an applied hunk. */
export const DIFF_CONTEXT = 3

/**
 * Lines added plus removed past which the line diff is not computed. jsdiff's
 * cost grows with the square of this edit distance: a full rewrite of 10,000
 * lines took 16 s on the event loop, while this cap gives up within about
 * 70 ms. The bound is a count, not a clock, so the same edit always yields
 * the same hunks, including the edited lines the model is shown.
 */
export const MAX_DIFF_EDIT_LENGTH = 1_000

/**
 * The `write`/`edit` tools' private `tool/result` `meta` payload: the applied
 * contextual-diff hunks, and for `write` whether the call created or updated
 * the file, which tells an empty hunk list of a create from one of an
 * unchanged overwrite. Attached opaquely (as `unknown`) on the tool result and
 * persisted with the session log — it must be JSON-serializable (the session
 * validates this at `append`), so `presentResult` reproduces the diff card on
 * replay. The producing tool owns and narrows this opaque shape.
 */
export type FsDiffMeta = { diffs: FileDiff[]; operation?: 'create' | 'update' }

/**
 * Compute one {@link FileDiff} per hunk between `before` and `after`, each carrying the
 * applied change plus {@link DIFF_CONTEXT} context lines and the line each side starts at.
 * Pure insertions use `oldText: null`, patch-only no-newline markers are omitted, and scattered
 * replacements remain separate hunks.
 *
 * @param path - the path stamped on every produced diff (the model-facing `file_path`; the
 *   bridge relativizes it).
 * @param before - the file text before the change (the backend's LF-normalized diff basis).
 * @param after - the file text after the change, on the same basis.
 * @returns one diff per applied hunk, in file order; empty when the texts are identical.
 */
export function computeHunkDiffs(path: string, before: string, after: string): (FileDiff & { oldStart: number; newStart: number })[] {
  const patch = structuredPatch('', '', before, after, undefined, undefined, {
    context: DIFF_CONTEXT, maxEditLength: MAX_DIFF_EDIT_LENGTH,
  })
  // Past the cap, one hunk spans the differing middle: the lines between the
  // shared head and tail, which is the exact change when it is one block.
  if (patch === undefined) return [coarseHunk(path, before, after)]
  const diffs: (FileDiff & { oldStart: number; newStart: number })[] = []
  for (const hunk of patch.hunks) {
    const oldLines: string[] = []
    const newLines: string[] = []
    for (const line of hunk.lines) {
      // The unified-diff marker for a missing trailing newline annotates the
      // patch, not the content — skip it so it never leaks into a diff block.
      if (line.startsWith('\\')) continue
      const text = line.slice(1)
      if (line.startsWith('-')) {
        oldLines.push(text)
      } else if (line.startsWith('+')) {
        newLines.push(text)
      } else {
        // A context (unchanged) line appears on both sides.
        oldLines.push(text)
        newLines.push(text)
      }
    }
    diffs.push({
      path,
      oldText: oldLines.length > 0 ? oldLines.join('\n') : null,
      newText: newLines.join('\n'),
      // A side with no lines names the line before the hunk, 0 at the start.
      oldStart: Math.max(1, hunk.oldStart),
      newStart: Math.max(1, hunk.newStart),
    })
  }
  return diffs
}

/**
 * One hunk from the first differing line to the last, with
 * {@link DIFF_CONTEXT} shared lines on each side, found by comparing the
 * shared head and tail in linear time.
 * @param path - the path stamped on the hunk.
 * @param before - the text before the change.
 * @param after - the text after the change; it differs from `before`.
 * @returns the hunk, in the shape {@link computeHunkDiffs} produces.
 */
function coarseHunk(path: string, before: string, after: string): FileDiff & { oldStart: number; newStart: number } {
  const lines = (text: string): string[] => {
    const split = text.split('\n')
    if (split.at(-1) === '') split.pop()
    return split
  }
  const old = lines(before)
  const next = lines(after)
  let head = 0
  while (head < old.length && head < next.length && old[head] === next[head]) head++
  let tail = 0
  while (tail < old.length - head && tail < next.length - head && old[old.length - 1 - tail] === next[next.length - 1 - tail]) tail++
  const from = Math.max(0, head - DIFF_CONTEXT)
  const oldTo = Math.min(old.length, old.length - tail + DIFF_CONTEXT)
  const newTo = Math.min(next.length, next.length - tail + DIFF_CONTEXT)
  const oldLines = old.slice(from, oldTo)
  return {
    path,
    oldText: oldLines.length > 0 ? oldLines.join('\n') : null,
    newText: next.slice(from, newTo).join('\n'),
    oldStart: from + 1,
    newStart: from + 1,
  }
}

/** Whether `value` is a valid {@link FileDiff} (defensive narrowing from opaque `meta`). */
function isFileDiff(value: unknown): value is FileDiff {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false
  const { path, oldText, newText, oldStart, newStart } = value as Record<string, unknown>
  return typeof path === 'string'
    && (oldText === null || typeof oldText === 'string')
    && typeof newText === 'string'
    && isLine(oldStart) && isLine(newStart)
}

/** Whether `value` is absent or a 1-based line number; logs written before it was recorded omit it. */
const isLine = (value: unknown): boolean => value === undefined || (Number.isInteger(value) && (value as number) >= 1)

/**
 * Narrow opaque live or replayed result metadata to non-empty file diffs. Malformed metadata
 * returns `undefined` so presentation can fall back instead of throwing during replay.
 * @param meta - result metadata.
 * @returns validated hunks, or `undefined` for absent or malformed data.
 */
export function diffsFromMeta(meta: unknown): FileDiff[] | undefined {
  if (typeof meta !== 'object' || meta === null || Array.isArray(meta)) return undefined
  const diffs = (meta as Record<string, unknown>).diffs
  if (!Array.isArray(diffs) || diffs.length === 0 || !diffs.every(isFileDiff)) return undefined
  return diffs
}
