/**
 * Presentation of a transcript row as positioned lines.
 *
 * Pure: a row in, display lines out, with no React and no terminal. The
 * component layer turns these into Ink boxes, which keeps every placement rule
 * testable without rendering and without an Ink input channel.
 *
 * Placement follows `tui/DESIGN-LAYOUT.md`: a marker column, a verb column
 * naming what the agent did, and output aligned under the verb's argument.
 *
 * @module @dsh-tui/ui/present
 */

import { COLUMN, MARKER, VERB, type Verb } from './layout.ts'
import { formatAttachment, type Row } from './rows.ts'

/** How a line is emphasized. Colour is chosen by the component layer. */
export type Tone =
  /** The user's own words. */
  | 'said'
  /** The answer, and anything else the user is waiting to read. */
  | 'plain'
  /** Supporting detail: reasoning, a tool's argument, successful output. */
  | 'quiet'
  /** A failure, and the output of one. */
  | 'failed'
  /** A question awaiting an answer. */
  | 'asking'

/**
 * Model name without its provider prefix.
 *
 * The provider is in `/model` when it is needed, which is when more than one is
 * configured. On the status line it is a word the user reads every frame and
 * acts on never.
 *
 * @param route - `provider/model`, or a bare model name.
 * @returns the model name alone.
 */
export const compactModel = (route: string): string => route.slice(route.lastIndexOf('/') + 1)

/**
 * Working directory, shortened against home.
 *
 * An absolute path spends most of its width on the part every path shares.
 *
 * @param cwd - absolute working directory.
 * @param home - home directory, when one is known.
 * @returns a home-relative path, or the original when it lies outside home.
 */
export function compactPath(cwd: string, home: string | undefined): string {
  if (home === undefined || home === '' || !cwd.startsWith(home)) return cwd
  const rest = cwd.slice(home.length)
  return rest === '' ? '~' : rest.startsWith('/') ? `~${rest}` : cwd
}

/** What the composer's right slot says, or nothing when there is nothing to say. */
export type Hint = 'send' | 'interrupt' | 'select' | 'answer' | undefined

/** What the surface is doing, as far as the composer is concerned. */
export interface ComposerState {
  /** Whether a turn is running. */
  readonly running: boolean
  /** Whether a question is waiting for an answer. */
  readonly asking: boolean
  /** Whether a completion list or picker is open. */
  readonly listing: boolean
  /** Whether the draft has any content. */
  readonly drafting: boolean
}

/**
 * Hint for the composer's right slot.
 *
 * Contextual, never permanent. A fixed hint row teaches nothing after the first
 * day, and on this surface it costs a row of the live region's budget on every
 * frame for the whole session. Returns a key rather than text, because copy is
 * locale-owned.
 *
 * @param state - what the surface is currently doing.
 * @returns the hint key, or undefined when the slot stays empty.
 */
export function hintFor(state: ComposerState): Hint {
  if (state.asking) return 'answer'
  if (state.listing) return 'select'
  if (state.running) return 'interrupt'
  return state.drafting ? 'send' : undefined
}

/** Colour and weight for a tone, decided here so it needs no terminal to test. */
export interface LineStyle {
  /** Semantic colour, or undefined to inherit the terminal's foreground. */
  readonly color?: 'red' | 'cyan'
  /** Whether the text is supporting detail. */
  readonly dim: boolean
  /** Whether the text carries the weight of the user's own words. */
  readonly bold: boolean
}

/**
 * Style for one tone.
 *
 * Colour is semantic and never decorative: red is a failure, cyan is a question
 * awaiting an answer. A failure is never dimmed, because dim means supporting
 * detail and a failure is the thing the user needs to read.
 *
 * @param tone - emphasis carried by the line.
 * @returns colour and weight for the component layer.
 */
export function styleOf(tone: Tone): LineStyle {
  switch (tone) {
    case 'said': return { dim: false, bold: true }
    case 'plain': return { dim: false, bold: false }
    case 'quiet': return { dim: true, bold: false }
    case 'failed': return { color: 'red', dim: false, bold: false }
    case 'asking': return { color: 'cyan', dim: false, bold: false }
    default: return { dim: false, bold: false }
  }
}

/** One display line, already placed in its columns. */
export interface PresentedLine {
  /** Marker column content: a prompt, a selection mark, or a space. */
  readonly marker: string
  /** Verb column content, empty for a line that continues one. */
  readonly verb: string
  /** Text for the remaining width. */
  readonly text: string
  /** Column the text starts at, which decides which budget bounds it. */
  readonly column: typeof COLUMN.rail | typeof COLUMN.output
  /** Emphasis for the component layer to colour. */
  readonly tone: Tone
}

/**
 * Verb naming what a tool did.
 *
 * A tool's own name is used when it already reads as an action and fits the
 * column; otherwise the closest verb in the vocabulary stands in. Naming the
 * action rather than the implementation keeps `bash`, `shell` and `zsh` from
 * reading as three different kinds of event.
 *
 * @param tool - tool name from the session log.
 * @returns the verb to display.
 */
export function verbFor(tool: string): Verb {
  const name = tool.toLowerCase()
  if (name.includes('bash') || name.includes('shell') || name.includes('exec')) return VERB.run
  if (name.includes('write') || name.includes('edit') || name.includes('patch')) return VERB.edit
  if (name.includes('read') || name.includes('cat') || name.includes('view')) return VERB.read
  if (name.includes('search') || name.includes('grep') || name.includes('glob')) return VERB.find
  if (name.includes('fetch') || name.includes('http') || name.includes('web')) return VERB.fetch
  return VERB.run
}

/** Split text into lines, dropping a trailing newline's empty line. */
const linesOf = (text: string): readonly string[] => {
  const lines = text.split('\n')
  return lines.at(-1) === '' ? lines.slice(0, -1) : lines
}

/** A continuation line: no marker, no verb, aligned under the argument. */
const continuation = (text: string, tone: Tone): PresentedLine =>
  ({ marker: MARKER.none, verb: '', text, column: COLUMN.output, tone })

/**
 * Present one row as the lines that display it.
 *
 * A row may produce several lines: multi-line text keeps its own breaks, and
 * tool output is aligned under the call it came from. An empty row produces no
 * lines rather than a blank one, so nothing occupies a row it cannot fill.
 *
 * @param row - a committed or live transcript row.
 * @returns display lines in order, possibly empty.
 */
export function present(row: Row): readonly PresentedLine[] {
  switch (row.kind) {
    case 'user':
      return [...linesOf(row.text), ...(row.attachments ?? []).map(formatAttachment)].map((text, index) => ({
        marker: index === 0 ? MARKER.turn : MARKER.none,
        verb: '', text, column: COLUMN.rail, tone: 'said' as const,
      }))

    case 'assistant':
      return linesOf(row.text).map(text => ({
        marker: MARKER.none, verb: '', text, column: COLUMN.rail, tone: 'plain' as const,
      }))

    case 'reasoning': {
      const lines = linesOf(row.text)
      return lines.map((text, index) => index === 0
        ? { marker: MARKER.none, verb: VERB.think, text, column: COLUMN.output, tone: 'quiet' as const }
        : continuation(text, 'quiet'))
    }

    case 'tool-call':
      return [{
        marker: MARKER.none, verb: verbFor(row.tool), text: row.input,
        column: COLUMN.output, tone: 'quiet',
      }]

    case 'tool-result':
      // Output belongs to the call above it, so it carries no verb of its own.
      // A failure is not dimmed: dim means supporting detail, and this is the
      // thing the user needs to read.
      return linesOf(row.text).map(text => continuation(text, row.ok ? 'quiet' : 'failed'))

    case 'notice': {
      const tone: Tone = row.tone === 'error' ? 'failed' : 'quiet'
      const verb = row.tone === 'error' ? VERB.error : VERB.note
      return linesOf(row.text).map((text, index) => index === 0
        ? { marker: MARKER.none, verb, text, column: COLUMN.output, tone }
        : continuation(text, tone))
    }

    default:
      // A build that does not know this row kind renders nothing rather than
      // guessing: the session log may carry events newer than this surface.
      return []
  }
}
