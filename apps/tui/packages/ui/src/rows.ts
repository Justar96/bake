/**
 * The view vocabulary the terminal renders.
 *
 * One row is one visually distinct line group. The renderer never inspects
 * session events directly.
 *
 * @module @dsh-tui/ui/rows
 */

import type { UserMessage } from 'bake-session'

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
   * A slash command the user ran. Separate from `user` because it addresses
   * the surface, not the model. It opens no turn, and its result is the
   * notice under it. `name` and `args` stay apart for logged input recall.
   * `inputOmitted` marks a redacted log entry; `recall` is a process-local
   * submitted line and must never be written to the Session log.
   */
  | { readonly kind: 'command', readonly name: string, readonly args: string,
    readonly inputOmitted?: true, readonly recall?: string }
  /**
   * Answer text. `continued` marks the rest of a block whose opening lines
   * already printed. A streaming answer prints settled Markdown blocks, and
   * what follows carries neither the section's blank nor its marker again.
   */
  | { readonly kind: 'assistant', readonly text: string, readonly continued?: boolean }
  /** Reasoning text; `continued` as for `assistant`. */
  | { readonly kind: 'reasoning', readonly text: string, readonly continued?: boolean }
  /**
   * Generation speed of a final answer: the provider's output tokens and the
   * logged milliseconds from the first streamed token to the finish. The
   * transcript draws nothing for it; the ended turn's summary on the header
   * reads it, and reports it only for a sample long enough to mean something.
   */
  | { readonly kind: 'rate', readonly tokens: number, readonly ms: number }
  | ToolCallRow
  /**
   * Two or more calls from one step, drawn as one block. The head counts them
   * by verb. Each call hangs from it in the order the model made them.
   * `Actions` builds the block once the step ends, so it prints once with
   * every call in it. Until then the live region draws the same block as its
   * calls run and finish.
   */
  | { readonly kind: 'tool-group', readonly calls: readonly ToolCallRow[] }
  /**
   * Background work a call started has finished: the completion notice the
   * job controller delivered to the agent, which the log records as plugin
   * context. Drawn as the head of the call that started it, so the two read
   * as one job's start and end.
   */
  | JobDoneRow
  /**
   * A tool result. Its call id and outcome precede the output, including when
   * the output is empty. `text` is the raw model-facing result, empty when
   * the presenter supplied a card instead. `detail` is that card's lines.
   * Failed results keep failure emphasis for both raw text and card lines.
   *
   * `title` is the card's replacement headline, absent when the tool declared
   * none. It is kept apart from `detail` because it is the line that survives
   * a collapsed result. A surface that reports the body as a count still says
   * which file was edited or which command was run.
   *
   * `changes` lists the files a command changed while it ran. It is apart
   * from `detail` because it keeps its own tones and its own bound: a
   * command that failed still shows its changes in green and red.
   */
  | {
    readonly kind: 'tool-result'
    readonly callId: string
    readonly ok: boolean
    readonly text: string
    readonly title?: string
    readonly detail?: readonly CardLine[]
    readonly changes?: CardChanges
  }
  | {
    readonly kind: 'notice'
    readonly tone: NoticeTone
    readonly text: string
    /**
     * Where the notice belongs. A recorded turn outcome closes the turn's
     * group. A command's outcome hangs from the command it answers, which is
     * the row before it. Absent, the notice stands on its own.
     */
    readonly placement?: 'turn-end' | 'command'
    /** Marks where history was compacted, drawn in the compaction tone the header uses while it runs. */
    readonly compaction?: true
  }

/**
 * A tool call, and its outcome once it has one. One action, drawn as one block.
 *
 * `input` is the presenter's title, or the raw arguments when it declared
 * none. `detail` carries the rest of its card, already localized. The call id
 * matches the result to its call and is not drawn.
 *
 * A call without `result` is still running and lives in the live region. The
 * application commits it once its step ends, so the block prints to history
 * once, finished, instead of as a call and a result stacked apart.
 */
export interface ToolCallRow {
  readonly kind: 'tool-call'
  readonly callId: string
  /** Root program owning a nested dispatch; opaque ids are paired by equality. */
  readonly rootCallId?: string
  /** Nested dispatches belonging to this root, in first-observed call order. */
  readonly dispatches?: readonly ToolCallRow[]
  readonly tool: string
  readonly input: string
  readonly detail?: readonly CardLine[]
  readonly result?: ToolOutcome
  /**
   * The call asked to run in the background (`run_in_background`), so its
   * result only acknowledges the start, and the work's end arrives later as
   * a {@link JobDoneRow}.
   */
  readonly background?: true
  /**
   * What the runtime said about the call while it waits for its result.
   * Process-local and never on a committed row: the logged result replaces
   * it, and a replayed session has none.
   */
  readonly live?: ToolCallLive
}

/** A background job's completion, as its notice recorded it. */
export interface JobDoneRow {
  readonly kind: 'job-done'
  /** The registry's `<kind>-N` id, when the notice names one. */
  readonly id?: string
  /** The producer kind, a tool name such as `bash`. */
  readonly tool: string
  /** The producer's one-line label: a command, or a delegated task. */
  readonly label: string
  readonly outcome: 'done' | 'failed' | 'stopped'
  /** How it ended, localized, with the producer's detail such as `exit code: 0`. */
  readonly status: string
}

/** A running call's process-local state, projected from runtime events. */
export interface ToolCallLive {
  /** The newest lines of its output so far, oldest first, as raw text. */
  readonly tail?: readonly string[]
  /**
   * It finished executing, and its result waits for an earlier call's to
   * commit first. `ok` is the outcome before post-execute policy.
   */
  readonly finished?: { readonly ok: boolean }
}

/**
 * Every call a row holds. Nested dispatches precede their owning root, so an
 * active nested tool names the phase while its program waits for it.
 * @param row - any row.
 * @returns the calls, empty when the row is neither.
 */
export const callsOf = (row: Row): readonly ToolCallRow[] =>
  (row.kind === 'tool-call' ? [row] : row.kind === 'tool-group' ? row.calls : [])
    .flatMap(call => [...call.dispatches ?? [], call])

/** How a call ended. A `tool-result` row's fields, without its identity. */
export type ToolOutcome = Omit<Extract<Row, { readonly kind: 'tool-result' }>, 'kind' | 'callId'>

/**
 * One line of a tool card, already localized and placed in order.
 *
 * A card is the terminal's rendering of the render intent a tool declared.
 * Its lines arrive as text, not as the tool's own types. By the time a row
 * exists, wording and order are already decided.
 */
export interface CardLine {
  /**
   * The line's text, without a trailing newline. A change line opens with
   * its sign and a space, `+ ` or `- `, and the rest is the source line, so
   * a surface without colour still shows the change.
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
   * Runs of `text` the change touched inside a line that was edited, not
   * replaced. `[start, end)` UTF-16 offsets, in order.
   */
  readonly changed?: readonly (readonly [number, number])[]
  /**
   * The card's own measure of its result. A count, the window a read took,
   * or a status. A surface may draw it beside the headline instead of under
   * it, where a preview bound would hide it. `failure` means the work did not
   * succeed, such as a non-zero exit, even though the call itself did.
   */
  readonly summary?: 'count' | 'failure'
}

/**
 * Which side of a change a card line shows, or `gap` for the unchanged lines
 * a diff leaves out between two changes.
 */
export type CardEmphasis = 'added' | 'removed' | 'gap'

/**
 * The workspace files a command changed while it ran, already localized.
 *
 * Drawn as a section of the command's own block, under its output. The
 * surface bounds it, so it carries every file the producer listed.
 */
export interface CardChanges {
  /** Each changed file, in the producer's order. */
  readonly files: readonly CardFileChange[]
  /** Files the producer listed beyond its own bound, absent when none. */
  readonly omitted?: number
  /** Caveats drawn dim under the section, such as that it may include other activity. */
  readonly notes?: readonly string[]
}

/** One file in a {@link CardChanges} section. */
export interface CardFileChange {
  /** Path relative to the session's working directory, as display text. */
  readonly path: string
  /** How the file changed when it was not a plain edit, such as `new` or `renamed from a.ts`. */
  readonly status?: string
  /** Lines added and removed, whether or not `lines` carries them. */
  readonly added: number
  readonly removed: number
  /** Its changed lines, as a `diff` card draws them; empty when the producer sent no hunks. */
  readonly lines: readonly CardLine[]
}

/** How a notice is classified. Neutral progress, a recoverable problem, or a failure. */
export type NoticeTone = 'info' | 'warn' | 'error'

/**
 * Narrow a row to the kinds that carry free text, for width and wrap math.
 *
 * @param row - the row to test.
 * @returns whether `row` has a `text` field.
 */
export function hasText(row: Row): row is Extract<Row, { text: string }> {
  return row.kind !== 'tool-call' && row.kind !== 'tool-group' && row.kind !== 'command' && row.kind !== 'rate' && row.kind !== 'job-done'
}
