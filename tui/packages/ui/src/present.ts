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

import { COLUMN, MARKER, tailOf, VERB, type Verb } from './layout.ts'
import { formatAttachment, type CardLine, type Row } from './rows.ts'

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
  /** A line a change introduced. */
  | 'added'
  /** A line a change took away. */
  | 'removed'

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
  readonly color?: 'red' | 'cyan' | 'green'
  /** Whether the text is supporting detail. */
  readonly dim: boolean
  /** Whether the text carries the weight of the user's own words. */
  readonly bold: boolean
}

/**
 * Style for one tone.
 *
 * Colour is semantic and never decorative: red is a failure or a removed
 * line, green is an added one, cyan is a question awaiting an answer. A failure is never dimmed, because dim means supporting
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
    // A diff is read as a pair, so both sides keep full weight: dimming the
    // removed side would make a deletion look like supporting detail.
    case 'added': return { color: 'green', dim: false, bold: false }
    case 'removed': return { color: 'red', dim: false, bold: false }
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

/**
 * A deliberate empty row, as opposed to a row with nothing to put in it.
 *
 * It opens a zone: a user turn, and each action the agent took inside one.
 * Zones are otherwise marked by indentation alone, which separates an answer at
 * the rail from output under a verb but cannot separate two actions that share
 * the verb column — a `think` directly under the previous call's output reads
 * as more of that output. The blank is what tells those apart.
 *
 * A call's own result continues its zone and gets none, and neither does a
 * command's notice, so a result never floats away from what produced it.
 */
const BLANK: PresentedLine =
  { marker: MARKER.none, verb: '', text: '', column: COLUMN.rail, tone: 'plain' }

/**
 * Argument slot for a call whose arguments have not finished streaming.
 *
 * Arguments arrive as a JSON string built from deltas, so every prefix of one
 * is invalid JSON and most of them end mid-token. Showing the partial text
 * would put unparsed syntax in front of the user and redraw it on every chunk;
 * the verb already names the action, and the complete arguments arrive with the
 * committed row a moment later.
 */
export const PENDING_ARGUMENTS = '...'

/**
 * Open a zone, unless the row turned out to have nothing to put in it.
 *
 * A blank belongs to the lines under it. On its own it is an empty row charged
 * to the live region's budget for content that never arrived — which is what an
 * empty text or reasoning block produces while a turn is still streaming.
 *
 * @param lines - the zone's lines, in order.
 * @returns the lines behind a blank row, or nothing when there are none.
 */
const opening = (lines: readonly PresentedLine[]): readonly PresentedLine[] =>
  lines.length === 0 ? [] : [BLANK, ...lines]

/** A continuation line: no marker, no verb, aligned under the argument. */
const continuation = (text: string, tone: Tone): PresentedLine =>
  ({ marker: MARKER.none, verb: '', text, column: COLUMN.output, tone })

/**
 * A tool card's lines, placed under the call they belong to.
 *
 * The card decided what to say; placement is decided here, so every card kind
 * lands in the same column whatever the tool that produced it.
 *
 * @param detail - the card's lines, absent when the tool declared no card.
 * @returns continuation lines in order, empty when there is no card.
 */
function cardLines(detail: readonly CardLine[] | undefined): readonly PresentedLine[] {
  return (detail ?? []).map(line => continuation(line.text,
    line.emphasis === 'added' ? 'added' : line.emphasis === 'removed' ? 'removed' : 'quiet'))
}

/**
 * Present one row as the lines that display it.
 *
 * A row may produce several lines: multi-line text keeps its own breaks, and
 * tool output is aligned under the call it came from. A row with nothing to say
 * produces no lines, so nothing occupies a row it cannot fill; the blank rows
 * that open a turn and each action inside it are the deliberate exception.
 *
 * @param row - a committed or live transcript row.
 * @returns display lines in order, possibly empty.
 */
export function present(row: Row): readonly PresentedLine[] {
  switch (row.kind) {
    case 'user': {
      const said = linesOf(row.text).map((text, index) => ({
        marker: index === 0 ? MARKER.turn : MARKER.none,
        verb: '', text, column: COLUMN.rail, tone: 'said' as const,
      }))
      // Attachment metadata is about the prompt rather than part of it, so it
      // recedes instead of carrying the weight of the user's own words.
      const staged = (row.attachments ?? []).map(formatAttachment).map(text => ({
        marker: MARKER.none, verb: '', text, column: COLUMN.rail, tone: 'quiet' as const,
      }))
      // One blank row opens the turn, and one opens each section inside it.
      // Indentation separates an answer at the rail from output under a verb,
      // but cannot separate two sections that share a column, and nothing at
      // all separates reasoning from the answer that follows it. The cost falls
      // on scrollback, which is unbounded, rather than on the live budget:
      // while a turn runs the live region holds a fixed height either way.
      return opening([...said, ...staged])
    }

    case 'command':
      // The rail carries the slash and the name follows it, so a command breaks
      // the left column the way it breaks the conversation. Quiet, because the
      // user just typed it: what they are reading for is the notice beneath.
      return [{
        marker: MARKER.command, verb: '', text: `${row.name}${row.args}`,
        column: COLUMN.rail, tone: 'quiet',
      }]

    case 'assistant':
      // The answer is a section like the others: without its own blank it butts
      // against the last line of the reasoning above it, and the two read as
      // one dim paragraph that happens to change colour partway through.
      return opening(linesOf(row.text).map(text => ({
        marker: MARKER.none, verb: '', text, column: COLUMN.rail, tone: 'plain' as const,
      })))

    case 'reasoning':
      return opening(linesOf(row.text).map((text, index) => index === 0
        ? { marker: MARKER.none, verb: VERB.think, text, column: COLUMN.output, tone: 'quiet' as const }
        : continuation(text, 'quiet')))

    case 'tool-call':
      return opening([
        {
          marker: MARKER.none, verb: verbFor(row.tool), text: row.input,
          column: COLUMN.output, tone: 'quiet',
        },
        ...cardLines(row.detail),
      ])

    case 'tool-result':
      // Output belongs to the call above it, so it carries no verb of its own.
      // A failure is not dimmed: dim means supporting detail, and this is the
      // thing the user needs to read.
      return [
        ...linesOf(row.text).map(text => continuation(text, row.ok ? 'quiet' : 'failed')),
        ...cardLines(row.detail),
      ]

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

/**
 * The trailing lines that fit, keeping the verb of the section they land in.
 *
 * A plain tail window cuts a long block below the line carrying its verb, and
 * what is left is continuation lines: dim text at the output column with
 * nothing saying whether it is reasoning or a tool's output. Restoring the verb
 * onto the first surviving line costs no row and keeps the section named for as
 * long as any of it is on screen.
 *
 * @param lines - every line the live rows produced, in order.
 * @param budget - rows the live region may draw.
 * @returns the trailing lines, the first of them carrying its section's verb.
 */
export function tailLines(lines: readonly PresentedLine[], budget: number): readonly PresentedLine[] {
  const shown = tailOf(lines, budget)
  const first = shown[0]
  if (first === undefined || first.verb !== '' || first.column !== COLUMN.output) return shown
  for (let index = lines.length - shown.length - 1; index >= 0; index--) {
    const line = lines[index]!
    // A line outside the output column ends the section, so there is no verb
    // above this one to restore.
    if (line.column !== COLUMN.output) break
    if (line.verb !== '') return [{ ...first, verb: line.verb }, ...shown.slice(1)]
  }
  return shown
}
