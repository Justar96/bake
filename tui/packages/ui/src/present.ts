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
import type { Row } from './rows.ts'

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
      return linesOf(row.text).map((text, index) => ({
        marker: index === 0 ? MARKER.prompt : MARKER.none,
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
