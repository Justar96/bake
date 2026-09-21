/**
 * The view vocabulary the terminal renders. One row is one visually distinct
 * line group; the renderer never inspects session events directly.
 *
 * @module @dsh-tui/ui/rows
 */

/** A committed transcript row, or a live row awaiting its commit. */
export type Row =
  | { readonly kind: 'user', readonly text: string }
  | { readonly kind: 'assistant', readonly text: string }
  | { readonly kind: 'reasoning', readonly text: string }
  | { readonly kind: 'tool-call', readonly callId: string, readonly tool: string, readonly input: string }
  | { readonly kind: 'tool-result', readonly callId: string, readonly ok: boolean, readonly text: string }
  | { readonly kind: 'notice', readonly tone: NoticeTone, readonly text: string }

/** How a notice reads: neutral progress, a recoverable problem, or a failure. */
export type NoticeTone = 'info' | 'warn' | 'error'

/**
 * Narrow a row to the kinds that carry free text, for width and wrap math.
 *
 * @param row - the row to test.
 * @returns whether `row` has a `text` field.
 */
export function hasText(row: Row): row is Extract<Row, { text: string }> {
  return row.kind !== 'tool-call'
}
