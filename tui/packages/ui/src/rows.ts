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
