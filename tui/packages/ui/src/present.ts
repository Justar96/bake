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

import { COLUMN, MARKER, PAST, VERB, type Verb } from './layout.ts'
import { formatAttachment, type CardLine, type Row, type ToolOutcome } from './rows.ts'

/** How a line is emphasized. Colour is chosen by the component layer. */
export type Tone =
  /** The user's own words. */
  | 'said'
  /** The answer, and anything else the user is waiting to read. */
  | 'plain'
  /** Reasoning and secondary interface metadata. */
  | 'quiet'
  /** What names a section: an action's verb and its tool. */
  | 'strong'
  /** An action that finished. */
  | 'done'
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
  /** Whether the text carries the weight of the user's own words, or names a section. */
  readonly bold: boolean
}

/**
 * Style for one tone.
 *
 * Colour is semantic and never decorative: red is a failure or a removed
 * line, green is an added one or an action that finished, cyan is a question
 * awaiting an answer. A failure is never dimmed, because dim means supporting
 * detail and a failure is the thing the user needs to read. Weight carries
 * structure: the verb and tool that open an action are bold, so a scan down the
 * left of the transcript lands on each action rather than on its arguments.
 *
 * @param tone - emphasis carried by the line.
 * @returns colour and weight for the component layer.
 */
export function styleOf(tone: Tone): LineStyle {
  switch (tone) {
    case 'said': return { dim: false, bold: true }
    case 'plain': return { dim: false, bold: false }
    case 'quiet': return { dim: true, bold: false }
    case 'strong': return { dim: false, bold: true }
    case 'done': return { color: 'green', dim: false, bold: true }
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
  /** Whether this line separates turns across the prose width. */
  readonly divider?: boolean
  /** Limit indented prose to the reading measure instead of the tool-output width. */
  readonly prose?: boolean
  /** Marker column content: a prompt, a selection mark, or a space. */
  readonly marker: string
  /** Verb column content, empty for a line that continues one. */
  readonly verb: string
  /** Emphasis for the verb when it differs from the text's, as an outcome's does. */
  readonly verbTone?: Tone
  /** Emphasis for the marker when it differs from the text's: an action's state. */
  readonly markerTone?: Tone
  /** Whether the marker pulses, which it does while its action runs. */
  readonly pulse?: boolean
  /** Text for the remaining width. */
  readonly text: string
  /** Column the text starts at, which decides which budget bounds it. */
  readonly column: typeof COLUMN.rail | typeof COLUMN.output
  /** Emphasis for the component layer to colour. */
  readonly tone: Tone
  /**
   * Emphasis for consecutive runs of `text`, from its start; text past the
   * last run takes `tone`. Kept beside the text rather than splitting it, so
   * wrapping, measuring, and the plain-text form all read one string.
   */
  readonly spans?: readonly Span[]
}

/** A run of a line's text with its own emphasis. */
export interface Span {
  /** UTF-16 length of the run. */
  readonly length: number
  readonly tone: Tone
}

/**
 * Join parts two spaces apart, recording each part's emphasis.
 * @param parts - text and tone, in order; empty text is skipped.
 * @returns the joined text and its runs, separators taking the next part's tone.
 */
function joined(parts: readonly (readonly [string | undefined, Tone])[], strong = 0): { readonly text: string, readonly spans: readonly Span[] } {
  const kept = parts.filter((part): part is readonly [string, Tone] => part[0] !== undefined && part[0] !== '')
  const spans = kept.map(([text, tone], index) => ({ length: text.length + (index === 0 ? 0 : 2), tone }))
  // A leading run of the first part in bold, as a tool's own name in a head.
  const first = spans[0]
  return {
    text: kept.map(([text]) => text).join('  '),
    spans: strong > 0 && first !== undefined && first.length > strong
      ? [{ length: strong, tone: 'strong' }, { ...first, length: first.length - strong }, ...spans.slice(1)]
      : spans,
  }
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
  return familyOf(tool) ?? VERB.run
}

/** The verb a tool's name implies, or undefined when it names none of the families. */
function familyOf(tool: string): Verb | undefined {
  const name = tool.toLowerCase()
  if (name.includes('bash') || name.includes('shell') || name.includes('exec')) return VERB.run
  if (name.includes('write') || name.includes('edit') || name.includes('patch')) return VERB.edit
  if (name.includes('read') || name.includes('cat') || name.includes('view')) return VERB.read
  if (name.includes('search') || name.includes('grep') || name.includes('glob')) return VERB.find
  if (name.includes('fetch') || name.includes('http') || name.includes('web')) return VERB.fetch
  return undefined
}

/**
 * Verbs whose output is worth a preview once they finish.
 *
 * A command's output is its result and an edit's diff is what changed, so
 * both show their head. What a read or a search returned is the model's input,
 * not the reader's: the count says it worked, and the answer says what it found.
 */
const PREVIEWED: ReadonlySet<Verb> = new Set([VERB.run, VERB.edit])

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
 * @param failed - whether the entire result must remain visibly failed.
 * @returns continuation lines in order, empty when there is no card.
 */
function cardLines(detail: readonly CardLine[] | undefined, failed = false): readonly PresentedLine[] {
  return (detail ?? []).map(line => continuation(line.text,
    failed ? 'failed' : line.emphasis === 'added' ? 'added' : line.emphasis === 'removed' ? 'removed' : 'plain'))
}

/**
 * One action as one block: its head, its arguments, and, once it has one, its outcome.
 *
 * The head carries the state. While the call runs its marker pulses and its
 * verb reads `run`; when it finishes the same line prints once, finished, with
 * `ran` and a green or red marker. A reader sees one block per action, not a
 * call and a separate result a few rows below it, and nothing names the call
 * id, which only matched the two.
 *
 * @param row - the call, with its outcome when it has one.
 * @param bound - how much of the outcome's output to preview.
 * @returns the block's lines, without its opening blank.
 */
function action(row: Extract<Row, { readonly kind: 'tool-call' }>, bound: ResultBound): readonly PresentedLine[] {
  const family = familyOf(row.tool)
  const verb = family ?? VERB.run
  const outcome = row.result
  const [first = '', ...rest] = linesOf(row.input)
  const pending = first === PENDING_ARGUMENTS
  // A presenter's title often opens with the verb it is drawn under, as in
  // `Read /path` under `read`; the second copy is noise.
  const title = first.toLowerCase().startsWith(`${verb} `) ? first.slice(verb.length + 1) : first
  // A tool no verb family names is named by itself, so the head still says
  // what ran; a call whose arguments are still streaming has only its name.
  const named = family === undefined || pending
  const text = named ? [row.tool, pending ? '' : title].filter(part => part !== '').join(' ') : title
  const after = outcome === undefined ? undefined : outcomeLines(outcome, verb, bound, title)
  // A count with nothing under it rides on the head, so a read is one row.
  const inline = after?.inline === undefined ? { text, spans: named ? [{ length: row.tool.length, tone: 'strong' as const }] : [] }
    : joined([[text, 'plain'], [after.inline, 'quiet']], named ? row.tool.length : 0)
  const head: PresentedLine = {
    marker: MARKER.action,
    markerTone: outcome === undefined ? 'strong' : outcome.ok ? 'done' : 'failed',
    ...outcome === undefined ? { pulse: true } : {},
    verb: outcome === undefined ? verb : PAST[verb],
    verbTone: outcome?.ok === false ? 'failed' : 'strong',
    text: inline.text, column: COLUMN.output, tone: 'plain',
    ...inline.spans.length === 0 ? {} : { spans: inline.spans },
  }
  // The card's own lines describe the call, as its description does; they
  // recede under the head unless they carry a diff's colour.
  const described = cardLines(row.detail).map(line => line.tone === 'plain' ? { ...line, tone: 'quiet' as const } : line)
  const lines = [head, ...rest.map(text => continuation(text, 'plain')), ...described]
  return after === undefined ? lines : [...lines, ...after.lines]
}

/**
 * What a finished action reports: its headline and size, or a preview.
 * @param outcome - how the call ended.
 * @param verb - the action's verb, which decides whether output is previewed.
 * @param bound - preview length and the words for a count.
 * @param title - the head's text, which a headline repeating it adds nothing to.
 * @returns a count for the head line when that is all there is, and the
 *   lines under the head at the output column.
 */
function outcomeLines(outcome: ToolOutcome, verb: Verb, bound: ResultBound, title: string): {
  readonly inline?: string
  readonly lines: readonly PresentedLine[]
} {
  const failed = !outcome.ok
  const tone: Tone = failed ? 'failed' : 'plain'
  const body = [...linesOf(outcome.text).map(text => continuation(text, tone)), ...cardLines(outcome.detail, failed)]
  // A failure is always news, and a diff is what an edit did.
  const previewed = failed || PREVIEWED.has(verb) || body.some(line => line.tone === 'added' || line.tone === 'removed')
  const shown = previewed ? body.slice(0, Math.max(0, bound.lines)) : []
  const hidden = body.length - shown.length
  // Previewed output counts what it left out below it; a count alone says how
  // much there was beside the headline.
  const size = shown.length > 0 || body.length === 0 ? undefined : `${body.length} ${bound.unit}`
  const headline = outcome.title === undefined || sameWords(outcome.title, title, verb) ? undefined : outcome.title
  if (headline === undefined && !failed) return { ...size === undefined ? {} : { inline: size }, lines: previewLines(shown, hidden, bound, tone, failed) }
  const summary = joined([[headline, tone], [size, failed ? tone : 'quiet']])
  return {
    lines: [
      ...summary.text === '' ? [] : [{ ...continuation(summary.text, tone), spans: summary.spans }],
      ...previewLines(shown, hidden, bound, tone, failed),
    ],
  }
}

/** A preview's lines, then a count of what it left out. */
function previewLines(shown: readonly PresentedLine[], hidden: number, bound: ResultBound, tone: Tone, failed: boolean): readonly PresentedLine[] {
  return [...shown, ...hidden === 0 || shown.length === 0 ? [] : [continuation(`+${hidden} ${bound.more}`, failed ? tone : 'quiet')]]
}

/**
 * Whether a result's headline only repeats the call's, as `Edit one.ts` does
 * under `edit one.ts`.
 */
function sameWords(headline: string, title: string, verb: Verb): boolean {
  const bare = headline.toLowerCase().startsWith(`${verb} `) ? headline.slice(verb.length + 1) : headline
  return bare.trim() === title.trim()
}

/**
 * How much of a tool result a surface draws, and the words it reports the rest
 * with.
 *
 * Required rather than defaulted: how much output belongs in scrollback is a
 * deployment's choice, and a hidden default here is how an unbounded `ls` gets
 * back into it.
 */
export interface ResultBound {
  /**
   * Body lines drawn under the outcome before the rest is reported as a count.
   *
   * Zero collapses a result to its outcome line. The full text is in the
   * session log either way; 70 lines of directory listing in scrollback on
   * every call scrolls the answer off the screen.
   */
  readonly lines: number
  /** Locale-owned noun for a line count, as in `70 lines`. */
  readonly unit: string
  /** Locale-owned phrase for the lines a bound left out, as in `+67 more lines`. */
  readonly more: string
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
 * @param result - how much of a tool result this surface draws; ignored by
 *   every other row kind, whose length the harness does not choose.
 * @returns display lines in order, possibly empty.
 */
export function present(row: Row, result: ResultBound): readonly PresentedLine[] {
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
      return opening([{ ...BLANK, divider: true, tone: 'quiet' }, ...said, ...staged])
    }

    case 'command':
      // The rail carries the slash and the name follows it, so a command breaks
      // the left column the way it breaks the conversation. Quiet, because the
      // user just typed it: what they are reading for is the notice beneath.
      return [{
        marker: MARKER.command, verb: '', text: `${row.name}${row.args}`,
        column: COLUMN.rail, tone: 'quiet',
      }]

    case 'assistant': {
      const lines = linesOf(row.text).map((text, index) => ({
        marker: index === 0 && row.continued !== true ? MARKER.reply : MARKER.none,
        verb: '', text, column: COLUMN.rail, tone: 'plain' as const,
      }))
      return row.continued === true ? lines : opening(lines)
    }

    case 'reasoning': {
      const lines = linesOf(row.text).map((text, index) => index === 0 && row.continued !== true
        ? { marker: MARKER.none, verb: VERB.think, text, column: COLUMN.output, tone: 'quiet' as const, prose: true }
        : { ...continuation(text, 'quiet'), prose: true })
      return row.continued === true ? lines : opening(lines)
    }

    case 'tool-call':
      return opening(action(row, result))

    case 'tool-result': {
      const tone: Tone = row.ok ? 'plain' : 'failed'
      // A card leaves `text` empty and a tool without one leaves `detail`
      // absent, so exactly one of these carries the body.
      const body = [
        ...linesOf(row.text).map(text => continuation(text, tone)),
        ...cardLines(row.detail, !row.ok),
      ]
      // Calls may finish out of order, so the result names its own call
      // rather than the nearest preceding row. An empty result still
      // acknowledges completion, with no size to report.
      const size = body.length === 0 ? undefined : `${body.length} ${result.unit}`
      // A success reads as its outcome and headline, with the id and size as
      // detail; a failure stays red throughout, the whole line being news.
      const outcome = joined([[`[${row.callId}]`, 'quiet'], [row.title, 'plain'], [size, 'quiet']])
      const head: PresentedLine = {
        marker: MARKER.none, verb: row.ok ? VERB.done : VERB.error, verbTone: row.ok ? 'done' : 'failed',
        text: outcome.text, column: COLUMN.output, tone, ...row.ok ? { spans: outcome.spans } : {},
      }
      // The head of the output prints once, under the outcome, and stays: shown
      // for a moment and then removed, it was the rows the composer jumped by.
      const shown = body.slice(0, Math.max(0, result.lines))
      const hidden = body.length - shown.length
      return [head, ...shown, ...hidden === 0 || shown.length === 0 ? [] : [continuation(`+${hidden} ${result.more}`, row.ok ? 'quiet' : tone)]]
    }

    case 'notice': {
      const tone: Tone = row.tone === 'error' ? 'failed' : 'quiet'
      // A completed turn says so on the summary row above the input instead,
      // with its time and what it did; a line here as well repeated it.
      if (row.placement === 'turn-end' && row.tone === 'info') return []
      if (row.placement === 'turn-end') return opening(linesOf(row.text).map((text, index) => ({
        marker: index === 0 ? '-' : MARKER.none, verb: '', text, column: COLUMN.rail, tone,
      })))
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
 * The trailing lines that fit, shown section by section, keeping the verb of
 * the section the window cuts.
 *
 * Only the newest section is ever cut. It is the one still arriving, so its
 * newest lines are what the reader is watching. An older section is shown
 * whole or not at all. Cut from the top, a tool's output lost a line per
 * streamed line of the answer below it until only its footer was left, and the
 * eye read that as the surface shredding. Dropped whole, it leaves once.
 * Nothing is lost either way: the transcript already holds each section's
 * committed row.
 *
 * A plain tail window also cuts a long block below the line carrying its
 * verb, and what is left is continuation lines at the output column with
 * nothing saying whether they are reasoning or a tool's output. Restoring the
 * verb onto the first surviving line costs no row and keeps the section named
 * for as long as any of it is on screen.
 *
 * @param lines - every line the live rows produced, in order.
 * @param budget - rows the live region may draw.
 * @param height - rows one line occupies once wrapped; one each by default.
 * @returns the trailing lines, the first of them carrying its section's verb.
 */
export function tailLines(
  lines: readonly PresentedLine[],
  budget: number,
  height: (line: PresentedLine) => number = () => 1,
): readonly PresentedLine[] {
  if (budget <= 0 || lines.length === 0) return []
  const rows = lines.map(height)
  const opens = (index: number): boolean => isBlank(lines[index]!)
  // The newest section begins at the last blank; everything after it is still
  // arriving or has just arrived.
  let start = lines.length - 1
  while (start > 0 && !opens(start)) start--
  const newest = rows.slice(start).reduce((sum, count) => sum + count, 0)
  if (newest > budget) {
    // The cut falls inside the section, below its opening blank, which stays:
    // without it the section's first surviving line runs into whatever the
    // transcript printed last.
    const blank = opens(start) && budget > 1
    const room = budget - Number(blank)
    const floor = start + Number(blank)
    let used = 0
    let from = lines.length
    while (from > floor && used + rows[from - 1]! <= room) used += rows[--from]!
    // Prose fills the window exactly, its oldest line clipped from the top the
    // way a terminal scrolls: stopping at whole lines would leave the window a
    // row or two short whenever the next line wraps, and the composer below
    // would bob with every paragraph. Output keeps whole lines, so its verb
    // stays on screen. One line taller than the window always shows.
    if (from > floor && used < room && (from === lines.length || lines[from - 1]!.column === COLUMN.rail)) from--
    const kept = named(lines, from)
    return blank ? [lines[start]!, ...kept] : kept
  }
  let used = newest
  let from = start
  while (from > 0) {
    let begin = from - 1
    while (begin > 0 && !opens(begin)) begin--
    const size = rows.slice(begin, from).reduce((sum, count) => sum + count, 0)
    if (used + size > budget) break
    used += size
    from = begin
  }
  return lines.slice(from)
}

/** A row that opens a section: nothing in it, at the rail. */
export const isBlank = (line: PresentedLine): boolean =>
  line.text === '' && line.verb === '' && line.column === COLUMN.rail && line.marker === MARKER.none && line.divider !== true

/**
 * Cut `lines` at `from`, restoring the marker or verb the cut removed.
 * @param lines - every line, in order.
 * @param from - index of the first line kept.
 * @returns the kept lines, the first of them named.
 */
function named(lines: readonly PresentedLine[], from: number): readonly PresentedLine[] {
  const shown = lines.slice(from)
  const first = shown[0]
  if (first?.column === COLUMN.rail && first.tone === 'plain' && first.text !== '' && first.marker === MARKER.none) {
    for (let index = from - 1; index >= 0; index--) {
      const line = lines[index]!
      if (line.column !== COLUMN.rail || line.tone !== 'plain') break
      if (line.marker === MARKER.reply) return [{ ...first, marker: MARKER.reply }, ...shown.slice(1)]
    }
  }
  if (first === undefined || first.verb !== '' || first.column !== COLUMN.output) return shown
  for (let index = from - 1; index >= 0; index--) {
    const line = lines[index]!
    // A line outside the output column ends the section, so there is no verb
    // above this one to restore.
    if (line.column !== COLUMN.output) break
    if (line.verb !== '') return [{ ...first, verb: line.verb }, ...shown.slice(1)]
  }
  return shown
}
