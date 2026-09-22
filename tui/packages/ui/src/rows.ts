/**
 * The view vocabulary the terminal renders. One row is one visually distinct
 * line group; the renderer never inspects session events directly.
 *
 * @module @dsh-tui/ui/rows
 */

import type { UserMessage } from '@deepseek-ai/dsh-session'

/** Display metadata only; source bytes and local storage paths never reach the UI. */
export interface AttachmentSummary {
  readonly name?: string
  readonly bytes: number
  readonly mediaType?: string
  readonly width?: number
  readonly height?: number
}

/**
 * Extract durable attachment metadata from Harness message blocks.
 * @param content - committed or queued user content.
 * @returns ordered metadata without object identifiers or source bytes.
 */
export function attachmentSummaries(content: UserMessage['content']): readonly AttachmentSummary[] {
  return content.flatMap((block): AttachmentSummary[] => {
    if (block.type === 'file') return [{ name: block.attachment.name, bytes: block.attachment.bytes }]
    if (block.type !== 'image') return []
    const { name, bytes, mediaType, width, height } = block.attachment
    return [{ ...name === undefined ? {} : { name }, bytes, mediaType, width, height }]
  })
}

/**
 * Format file metadata using standard byte and pixel units.
 * @param item - staged or durable attachment metadata.
 * @returns display name, media type, byte length, and optional dimensions.
 */
export function formatAttachment(item: AttachmentSummary): string {
  return [item.name, item.mediaType, `${item.bytes} B`,
    item.width === undefined ? undefined : `${item.width}×${item.height}`].filter(value => value !== undefined).join(' · ')
}

/** A committed transcript row, or a live row awaiting its commit. */
export type Row =
  | { readonly kind: 'user', readonly text: string, readonly attachments?: readonly AttachmentSummary[] }
  /**
   * A slash command the user ran. Separate from `user` because it addresses the
   * surface rather than the model: it opens no turn, and its result is the
   * notice under it. `name` and `args` stay apart so input recall can rebuild
   * the draft the user typed.
   */
  | { readonly kind: 'command', readonly name: string, readonly args: string }
  | { readonly kind: 'assistant', readonly text: string }
  | { readonly kind: 'reasoning', readonly text: string }
  /**
   * A tool call. `input` is the headline on the verb line: the title the tool's
   * own presenter gave this call, or the raw arguments when it declared none.
   * `detail` carries the rest of its card, already localized.
   */
  | { readonly kind: 'tool-call', readonly callId: string, readonly tool: string, readonly input: string, readonly detail?: readonly CardLine[] }
  /**
   * A tool result. `text` is the raw model-facing result, empty when the tool's
   * presenter supplied a card instead; `detail` is that card's lines. Both
   * render, so a card adds to the raw text rather than hiding it.
   */
  | { readonly kind: 'tool-result', readonly callId: string, readonly ok: boolean, readonly text: string, readonly detail?: readonly CardLine[] }
  | { readonly kind: 'notice', readonly tone: NoticeTone, readonly text: string }

/**
 * One line of a tool card, already localized and placed in order.
 *
 * A card is the terminal's rendering of the render intent a tool declared, so
 * its lines arrive as text rather than as the tool's own types: by the time a
 * row exists, every decision about wording and ordering has been made.
 */
export interface CardLine {
  /** The line's text, without a trailing newline. */
  readonly text: string
  /** Diff emphasis, absent for an ordinary supporting line. */
  readonly emphasis?: CardEmphasis
}

/** Which side of a change a card line shows. */
export type CardEmphasis = 'added' | 'removed'

/** How a notice reads: neutral progress, a recoverable problem, or a failure. */
export type NoticeTone = 'info' | 'warn' | 'error'

/**
 * Narrow a row to the kinds that carry free text, for width and wrap math.
 *
 * @param row - the row to test.
 * @returns whether `row` has a `text` field.
 */
export function hasText(row: Row): row is Extract<Row, { text: string }> {
  return row.kind !== 'tool-call' && row.kind !== 'command'
}
