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
  /**
   * Answer text. `continued` marks the rest of a block whose opening lines
   * already printed: a streaming answer prints settled Markdown blocks, and
   * what follows carries neither the section's blank nor its marker again.
   */
  | { readonly kind: 'assistant', readonly text: string, readonly continued?: boolean }
  /** Reasoning text; `continued` as for `assistant`. */
  | { readonly kind: 'reasoning', readonly text: string, readonly continued?: boolean }
  | ToolCallRow
  /**
   * The calls one step made, two or more, drawn as one block: a head counting
   * them by verb, and each call hanging from it in the order the model made
   * them. Built by `Actions` once the step ends, so the block prints once
   * with every call in it; until then the live region draws the same block
   * as its calls run and finish.
   */
  | { readonly kind: 'tool-group', readonly calls: readonly ToolCallRow[] }
  /**
   * A tool result. Its call id and outcome precede the output, including when
   * empty. `text` is the raw model-facing result, empty when the presenter
   * supplied a card instead; `detail` is that card's lines. Failed results
   * retain failure emphasis for both raw text and card lines.
   *
   * `title` is the card's replacement headline, absent when the tool declared
   * none. It is held apart from `detail` because it is the one line that
   * survives a collapsed result: a surface that reports the body as a count
   * still says which file was edited or which command was run.
   */
  | {
    readonly kind: 'tool-result'
    readonly callId: string
    readonly ok: boolean
    readonly text: string
    readonly title?: string
    readonly detail?: readonly CardLine[]
  }
  | {
    readonly kind: 'notice'
    readonly tone: NoticeTone
    readonly text: string
    /**
     * Where the notice belongs: a recorded turn outcome closes the turn's
     * group, and a command's outcome hangs from the command it answers, which
     * is the row before it. Absent, the notice stands on its own.
     */
    readonly placement?: 'turn-end' | 'command'
  }

/**
 * A tool call, and once it has one, its outcome: one action, drawn as one
 * block. `input` is the presenter's title, or the raw arguments when it
 * declared none; `detail` carries the rest of its card, already localized.
 * The call id matches the result to its call and is not drawn.
 *
 * A call without `result` is still running, and lives in the live region;
 * the application commits it once its step ends, so the block prints to
 * history once, finished, rather than as a call and a result stacked apart.
 */
export interface ToolCallRow {
  readonly kind: 'tool-call'
  readonly callId: string
  readonly tool: string
  readonly input: string
  readonly detail?: readonly CardLine[]
  readonly result?: ToolOutcome
}

/**
 * The calls a row holds: itself for a call, its calls for a group.
 * @param row - any row.
 * @returns the calls, empty for a row that is neither.
 */
export const callsOf = (row: Row): readonly ToolCallRow[] =>
  row.kind === 'tool-call' ? [row] : row.kind === 'tool-group' ? row.calls : []

/** How a call ended: a `tool-result` row's fields, without its identity. */
export type ToolOutcome = Omit<Extract<Row, { readonly kind: 'tool-result' }>, 'kind' | 'callId'>

/**
 * One line of a tool card, already localized and placed in order.
 *
 * A card is the terminal's rendering of the render intent a tool declared, so
 * its lines arrive as text rather than as the tool's own types: by the time a
 * row exists, every decision about wording and ordering has been made.
 */
export interface CardLine {
  /**
   * The line's text, without a trailing newline. A line of a change opens
   * with its sign and a space, `+ ` or `- `, and the rest is the source line,
   * so a surface without colour still reads the change.
   */
  readonly text: string
  /** Diff emphasis, absent for an ordinary supporting line. */
  readonly emphasis?: CardEmphasis
  /** Source line number; discontinuities begin a fresh syntax state. Diffs draw it in the gutter. */
  readonly number?: number
  /** Source file path or explicit language naming the syntax grammar. */
  readonly source?: string
  /** UTF-16 length of a display prefix, such as a read's line number; zero by default. */
  readonly codeOffset?: number
  /** Begin a separate grammar state, even when the preceding line has the same source. */
  readonly codeStart?: boolean
  /**
   * The runs of `text` the change touched within a line that was edited
   * rather than replaced, as `[start, end)` UTF-16 offsets in order.
   */
  readonly changed?: readonly (readonly [number, number])[]
  /**
   * The card's own measure of its result: a count, the window a read took, a
   * status. A surface may draw it beside the headline rather than under it,
   * where a preview bound would hide it; `failure` is a status that says the
   * work did not succeed, such as a non-zero exit, even though the call did.
   */
  readonly summary?: 'count' | 'failure'
}

/**
 * Which side of a change a card line shows, or `gap` for the unchanged lines
 * a diff leaves out between two changes.
 */
export type CardEmphasis = 'added' | 'removed' | 'gap'

/** How a notice reads: neutral progress, a recoverable problem, or a failure. */
export type NoticeTone = 'info' | 'warn' | 'error'

/**
 * Narrow a row to the kinds that carry free text, for width and wrap math.
 *
 * @param row - the row to test.
 * @returns whether `row` has a `text` field.
 */
export function hasText(row: Row): row is Extract<Row, { text: string }> {
  return row.kind !== 'tool-call' && row.kind !== 'tool-group' && row.kind !== 'command'
}
