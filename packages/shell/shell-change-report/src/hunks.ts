/**
 * Time-bounded contextual hunks between two texts, in the shape the `edit`
 * diff card draws.
 * @module @deepseek-ai/dsh-shell-change-report/hunks
 */

import { structuredPatch } from 'diff'

/** Context lines on each side of a hunk, matching `edit`'s result card. */
export const HUNK_CONTEXT = 3

/** One contextual hunk; a pure insertion has `oldText: null`. */
export type ChangeHunk = {
  oldText: string | null
  newText: string
  oldStart: number
  newStart: number
}

/** The hunks of one file and its changed-line counts. */
export interface FileHunks {
  hunks: ChangeHunk[]
  added: number
  removed: number
}

/**
 * Diff two LF-normalized texts within a time budget. jsdiff is quadratic on
 * dissimilar inputs: a full rewrite of 10,000 lines took 9.7 s, synchronously,
 * without a bound. With one, it gives up and this returns `undefined`.
 * @param before - the text before the command.
 * @param after - the text after it.
 * @param timeoutMs - the most milliseconds the comparison may take.
 * @returns the hunks and counts, or `undefined` when the budget ran out.
 */
export function boundedHunks(before: string, after: string, timeoutMs: number): FileHunks | undefined {
  const patch = structuredPatch('', '', before, after, undefined, undefined, {
    context: HUNK_CONTEXT, timeout: Math.max(1, Math.floor(timeoutMs)),
  })
  if (patch === undefined) return undefined
  const hunks: ChangeHunk[] = []
  let added = 0
  let removed = 0
  for (const hunk of patch.hunks) {
    const oldLines: string[] = []
    const newLines: string[] = []
    for (const line of hunk.lines) {
      // The no-newline marker annotates the patch, not the content.
      if (line.startsWith('\\')) continue
      const text = line.slice(1)
      if (line.startsWith('-')) {
        oldLines.push(text)
        removed++
      } else if (line.startsWith('+')) {
        newLines.push(text)
        added++
      } else {
        oldLines.push(text)
        newLines.push(text)
      }
    }
    hunks.push({
      oldText: oldLines.length > 0 ? oldLines.join('\n') : null,
      newText: newLines.join('\n'),
      oldStart: Math.max(1, hunk.oldStart),
      newStart: Math.max(1, hunk.newStart),
    })
  }
  return { hunks, added, removed }
}
