/**
 * Presentation of a transcript row as positioned lines.
 *
 * Pure: a row in, display lines out, with no React and no terminal. The
 * component layer turns these into Ink boxes, which keeps every placement rule
 * testable without rendering and without an Ink input channel.
 *
 * Placement follows `apps/tui/DESIGN-LAYOUT.md`: a marker column, a verb column
 * naming what the agent did, and output aligned under the verb's argument.
 *
 * @module @dsh-tui/ui/present
 */

import wrapAnsi from 'wrap-ansi'
import { markdownLines, sliceSpans } from './markdown.ts'
import { outputLines, outputSpans, toolText } from './tool-output.ts'
import { COLUMN, MARKER, PAST, TREE, VERB, type Verb } from './layout.ts'
import { PALETTE, type PaletteColor } from './palette.ts'
import { formatAttachment, type CardLine, type Row, type ToolCallRow, type ToolOutcome } from './rows.ts'

/** How a line is emphasized. Colour is chosen by the component layer. */
export type Tone =
  /** The user's own words. */
  | 'said'
  /** The answer, and anything else the user is waiting to read. */
  | 'plain'
  /** Secondary interface metadata. */
  | 'quiet'
  /** Reasoning: the model's working-out, which recedes as metadata does and reads as its own voice. */
  | 'thought'
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
  /** Semantic palette colour, or undefined to inherit the terminal's foreground. */
  readonly color?: PaletteColor
  /** Whether the text is supporting detail. */
  readonly dim: boolean
  /** Whether the text carries the weight of the user's own words, or names a section. */
  readonly bold: boolean
  /** Whether the text is the model thinking aloud rather than saying or doing. */
  readonly italic?: boolean
}

/**
 * Style for one tone.
 *
 * Colour is semantic and never decorative: red is a failure or a removed line,
 * green is an added one or an action that finished, ocean blue is a question
 * awaiting an answer. `PALETTE` holds the tones; this function only says which
 * meaning a tone carries. A failure is never dimmed, because dim means
 * supporting detail and a failure is the thing the user needs to read. Weight
 * carries structure: the verb and tool that open an action are bold, so a scan
 * down the left of the transcript lands on each action rather than on its
 * arguments.
 *
 * @param tone - emphasis carried by the line.
 * @returns colour and weight for the component layer.
 */
export function styleOf(tone: Tone): LineStyle {
  switch (tone) {
    case 'said': return { dim: false, bold: true }
    case 'plain': return { dim: false, bold: false }
    case 'quiet': return { dim: true, bold: false }
    // Dim like metadata, and slanted, so a block of reasoning reads as the
    // working-out and not as a tool's output in the same column.
    case 'thought': return { dim: true, bold: false, italic: true }
    case 'strong': return { dim: false, bold: true }
    case 'done': return { color: PALETTE.done, dim: false, bold: true }
    case 'failed': return { color: PALETTE.failed, dim: false, bold: false }
    case 'asking': return { color: PALETTE.asking, dim: false, bold: false }
    // A diff is read as a pair, so both sides keep full weight: dimming the
    // removed side would make a deletion look like supporting detail.
    case 'added': return { color: PALETTE.done, dim: false, bold: false }
    case 'removed': return { color: PALETTE.failed, dim: false, bold: false }
    default: return { dim: false, bold: false }
  }
}

/** One display line, already placed in its columns. */
export interface PresentedLine {
  /** Preserve code whitespace instead of applying prose soft breaks. */
  readonly literal?: boolean
  /** Whether this line separates turns across the prose width. */
  readonly divider?: boolean
  /** Limit indented prose to the reading measure instead of the tool-output width. */
  readonly prose?: boolean
  /** Marker column content: a prompt, a selection mark, or a space. */
  readonly marker: string
  /** Verb column content, empty for a line that continues one. */
  readonly verb: string
  /**
   * What the verb column shows when the line has no verb: a changed line's
   * number, drawn right-aligned against the text in the line's tone.
   */
  readonly gutter?: string
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
  /**
   * Whether the text starts in the rail rather than after it, taking the
   * marker's columns: a command, whose slash is the first thing typed and the
   * first thing on its row. `marker` is then not drawn.
   */
  readonly flush?: boolean
  /**
   * Whether text at the rail wraps at the full width, as tool output does,
   * rather than at the prose measure: a command's outcome, which is often a
   * table such as `/help` prints, and reads like output rather than prose.
   */
  readonly wide?: boolean
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
  /** A highlighter's colour for code, drawn instead of the tone's. */
  readonly color?: string
  readonly italic?: boolean
  readonly bold?: boolean
  readonly underline?: boolean
  readonly strikethrough?: boolean
  /** Drawn with foreground and background swapped: the words an edit changed. */
  readonly inverse?: boolean
}

/**
 * A run of highlighted code, as a {@link Highlight} reports it.
 *
 * Colour is the one thing on the surface that is not semantic: it comes from
 * a highlighting theme, and says what kind of token the run is.
 */
export interface CodeToken {
  /** UTF-16 length of the run. */
  readonly length: number
  /** Foreground, or undefined for the terminal's own. */
  readonly color?: string
  /** Whether the run is supporting text, as a comment is. */
  readonly dim?: boolean
  readonly italic?: boolean
}

/**
 * Syntax colour for consecutive lines of one file.
 *
 * Synchronous, because presentation is: a highlighter that does not know the
 * language, or is still loading its grammar, returns undefined, and the lines
 * draw in their side's tone as they would with no highlighter at all.
 *
 * @param lines - consecutive source lines.
 * @param path - the file they are from, which names their language.
 * @returns one token list per line, or undefined.
 */
export type Highlight = (lines: readonly string[], path: string) => readonly (readonly CodeToken[])[] | undefined

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
  // Before `write`: a plan the agent rewrites is not an edit to the workspace.
  if (name.includes('todo') || name.includes('plan')) return VERB.plan
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
 * File, search, and web snippets share the configured bound with command
 * output and edits; plan updates retain their dedicated presentation.
 */
const PREVIEWED: ReadonlySet<Verb> = new Set([VERB.run, VERB.edit, VERB.read, VERB.find, VERB.fetch])

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
 * the rail from output under a verb but cannot separate two zones that share a
 * column — reasoning directly under an answer, or an action under the output
 * of the one before, reads as more of it. The blank is what tells those apart.
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
 * lands in the same column whatever the tool that produced it. A changed line
 * carries its number in the gutter, and its code, highlighted when a
 * highlighter knows the language, with the words the edit changed reversed
 * in its side's tone.
 *
 * @param detail - the card's lines, absent when the tool declared no card.
 * @param failed - whether the entire result must remain visibly failed.
 * @param code - colours code by the file it is from.
 * @returns continuation lines in order, empty when there is no card.
 */
function cardLines(detail: readonly CardLine[] | undefined, failed = false, code?: Highlight): readonly PresentedLine[] {
  const lines = detail ?? []
  const tokens = failed || code === undefined ? [] : highlighted(lines, code)
  return lines.map((line, index) => {
    const tone: Tone = failed ? 'failed' : line.emphasis === 'gap' ? 'quiet' : line.emphasis ?? 'plain'
    const changed = line.emphasis === 'added' || line.emphasis === 'removed'
    const text = changed ? line.text : toolText(line.text)
    const placed = continuation(text, tone)
    if (!changed) {
      if (failed || line.emphasis === 'gap') return placed
      const syntax = tokens[index]
      const spans = syntax === undefined
        ? line.source === undefined ? outputSpans(text) : undefined
        : sourceSpans(line, syntax)
      return { ...placed, ...line.source === undefined ? {} : { literal: true }, ...spans === undefined ? {} : { spans } }
    }
    // A number wider than the column would wrap it; the sign still says
    // which side the line is on.
    const number = line.number === undefined ? '' : String(line.number)
    const gutter = number !== '' && number.length < COLUMN.verb ? { gutter: number } : {}
    return failed ? { ...placed, ...gutter } : { ...placed, ...gutter, spans: changeSpans(line, tone, tokens[index]) }
  })
}

/**
 * Syntax tokens for each source run, excluding line numbers and diff signs.
 *
 * A grammar carries state from line to line — an open string, a block
 * comment — so the lines of one side of one change are highlighted together,
 * in the order the file has them.
 *
 * @param lines - a card's lines.
 * @param code - the highlighter.
 * @returns tokens by line index, absent where the line is not code or the
 *   highlighter declined.
 */
function highlighted(lines: readonly CardLine[], code: Highlight): readonly (readonly CodeToken[] | undefined)[] {
  const tokens: (readonly CodeToken[] | undefined)[] = []
  let from = 0
  while (from < lines.length) {
    const first = lines[from]!
    let to = from + 1
    if (first.source !== undefined) {
      while (to < lines.length && lines[to]!.source === first.source && lines[to]!.emphasis === first.emphasis
        && lines[to]!.codeStart !== true
        && (lines[to - 1]!.number === undefined || lines[to]!.number === lines[to - 1]!.number! + 1)) to++
      const run = code(lines.slice(from, to).map(line => {
        const text = line.emphasis === 'added' || line.emphasis === 'removed' ? line.text : toolText(line.text)
        return text.slice(codeOffset(line))
      }), first.source)
      for (let index = from; index < to; index++) tokens[index] = run?.[index - from]
    }
    from = to
  }
  return tokens
}

/** Display prefixes are never passed to the syntax grammar. */
const codeOffset = (line: CardLine): number => line.emphasis === 'added' || line.emphasis === 'removed' ? 2 : line.codeOffset ?? 0

/** Code tokens keep full brightness; only syntax colour and italics are used. */
function sourceSpans(line: CardLine, tokens: readonly CodeToken[]): readonly Span[] {
  const prefix = codeOffset(line)
  return [
    ...prefix === 0 ? [] : [{ length: prefix, tone: 'plain' as const, color: PALETTE.reference }],
    ...tokens.map(token => ({ length: token.length, tone: 'plain' as const,
      ...token.color === undefined ? {} : { color: token.color },
      ...token.italic === true ? { italic: true } : {},
    })),
  ]
}

/**
 * The runs of one changed line: its sign, then its code.
 *
 * The side's tone is the line's colour, and syntax colour is laid over it:
 * the sign, punctuation, and plain words keep the tone, so a line reads as
 * added or removed however much of it is highlighted, and a token the theme
 * colours takes that colour. Code the edit changed is reversed in the tone.
 *
 * @param line - a changed line, its sign in the first two characters.
 * @param tone - its side's tone.
 * @param tokens - its syntax tokens, from after the sign.
 * @returns runs covering the whole text.
 */
function changeSpans(line: CardLine, tone: Tone, tokens: readonly CodeToken[] | undefined): readonly Span[] {
  const spans: Span[] = []
  const push = (span: Span): void => {
    const last = spans.at(-1)
    if (last !== undefined && last.tone === span.tone && last.color === span.color && last.italic === span.italic && last.inverse === span.inverse) {
      spans[spans.length - 1] = { ...last, length: last.length + span.length }
    } else {
      spans.push(span)
    }
  }
  push({ length: Math.min(2, line.text.length), tone })
  const changed = line.changed ?? []
  // Each token placed on the line, and the edges where the style may change.
  const runs: { readonly from: number, readonly to: number, readonly token: CodeToken }[] = []
  const cuts = new Set<number>([line.text.length])
  for (const token of tokens ?? []) {
    const from = runs.at(-1)?.to ?? 2
    runs.push({ from, to: from + token.length, token })
    cuts.add(from + token.length)
  }
  for (const [from, to] of changed) cuts.add(from).add(to)
  let offset = 2
  let run = 0
  for (const edge of [...cuts].filter(cut => cut > 2 && cut <= line.text.length).sort((a, b) => a - b)) {
    while (run < runs.length && runs[run]!.to <= offset) run++
    const syntax = runs[run] !== undefined && runs[run]!.from <= offset ? runs[run]!.token : undefined
    const length = edge - offset
    if (changed.some(([from, to]) => from <= offset && offset < to)) push({ length, tone, inverse: true })
    else if (tokens === undefined) push({ length, tone })
    else push({ length, tone: syntax?.dim === true ? 'quiet' : tone,
      ...syntax?.color === undefined ? {} : { color: syntax.color }, ...syntax?.italic === true ? { italic: true } : {} })
    offset = edge
  }
  return spans
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
function action(row: ToolCallRow, bound: ResultBound): readonly PresentedLine[] {
  const family = familyOf(row.tool)
  const verb = family ?? VERB.run
  const outcome = row.result
  const [first = '', ...rest] = linesOf(row.input)
  const pending = first === PENDING_ARGUMENTS
  const title = bare(first, verb)
  // A tool no verb family names is named by itself, so the head still says
  // what ran; a call whose arguments are still streaming has only its name.
  const named = family === undefined || pending
  const text = named ? [row.tool, pending ? '' : title].filter(part => part !== '').join(' ') : title
  const after = outcome === undefined ? undefined : outcomeLines(outcome, verb, bound, title)
  // A count with nothing under it rides on the head, so a read is one row,
  // and so does the size of a change, so an edit says how big it was.
  const inline = after?.inline === undefined ? { text, spans: named ? [{ length: row.tool.length, tone: 'strong' as const }] : [] }
    : beside(joined([[text, 'plain']], named ? row.tool.length : 0), after.inline)
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
  // recede under the head unless they carry a diff's colour. Both they and
  // the rest of a multi-line input are bounded as output is: a script the
  // model wrote can be as long as a file, and it prints under the head once
  // for every call. A line of each always shows, so a description survives
  // a bound that collapses output.
  const limit = Math.max(1, bound.lines)
  const described = cardLines(row.detail).map(line => line.tone === 'plain' ? { ...line, tone: 'quiet' as const } : line)
  const lines = [head,
    ...excerpt(rest.map(text => continuation(text, 'plain')), limit, bound, 'plain', false).lines,
    ...excerpt(described, limit, bound, 'quiet', false).lines]
  return after === undefined ? lines : [...lines, ...after.lines]
}

/**
 * A step's calls as one block: a head counting them, and each call hanging
 * from it, joined by the tree in the rail.
 *
 * The rail already carries an action's state in its marker, so each call's
 * branch takes that marker's place and colour — pulsing while the call runs,
 * green or red once it ends — and the verb, its argument, and its output keep
 * their columns. The head's marker is the step's state: running while any
 * call is, red when one failed. No blank row separates the calls, because
 * they were one decision the model made, and the stem is what reads as that.
 *
 * @param calls - the step's calls, in the order the model made them.
 * @param bound - how much of each outcome's output to preview.
 * @returns the block's lines, without its opening blank.
 */
function group(calls: readonly ToolCallRow[], bound: ResultBound): readonly PresentedLine[] {
  const running = calls.some(call => call.result === undefined)
  const failed = calls.filter(call => call.result?.ok === false).length
  // Counted in the order the verbs first appear, in the tense of the step.
  const counts = new Map<Verb, number>()
  for (const call of calls) counts.set(verbFor(call.tool), (counts.get(verbFor(call.tool)) ?? 0) + 1)
  const tally = [...counts].map(([verb, count]) => `${running ? verb : PAST[verb]} ${count}`).join(' \u00b7 ')
  const failures = failed === 0 || running || bound.failures === undefined ? undefined : ` \u00b7 ${failed} ${bound.failures}`
  const head: PresentedLine = {
    marker: MARKER.action,
    markerTone: running ? 'strong' : failed > 0 ? 'failed' : 'done',
    ...running ? { pulse: true } : {},
    verb: '', text: `${tally}${failures ?? ''}`, column: COLUMN.rail, tone: 'strong',
    ...failures === undefined ? {} : { spans: [{ length: tally.length, tone: 'strong' as const }, { length: failures.length, tone: 'failed' as const }] },
  }
  return [head, ...calls.flatMap((call, index) => {
    const last = index === calls.length - 1
    return action(call, bound).map((line, row) => row === 0
      ? { ...line, marker: last ? TREE.corner : TREE.branch }
      : { ...line, marker: last ? MARKER.none : TREE.stem, markerTone: 'quiet' as const })
  })]
}

/**
 * A title without a leading word naming its own verb, as `Grep` does under
 * `find` and `Read` under `read`: the second copy is noise. A command keeps
 * its words unless the first is `run` itself, since under `run` `bash
 * deploy.sh` is the command and not a name for it.
 * @param title - the head's first line.
 * @param verb - the verb it is drawn under.
 * @returns the title, its redundant first word removed.
 */
function bare(title: string, verb: Verb): string {
  const space = title.indexOf(' ')
  if (space <= 0) return title
  const word = title.slice(0, space).toLowerCase()
  return word === verb || (verb !== VERB.run && familyOf(word) === verb) ? title.slice(space + 1) : title
}

/** Text with its runs, as a head carries it. */
interface Styled {
  readonly text: string
  readonly spans: readonly Span[]
}

/** `extra` two spaces after `base`, or alone when `base` is empty; the separator takes `extra`'s tone, as in {@link joined}. */
function beside(base: Styled, extra: Styled): Styled {
  if (base.text === '') return extra
  const [first, ...rest] = extra.spans
  return { text: `${base.text}  ${extra.text}`, spans: [...base.spans, ...first === undefined ? [] : [{ ...first, length: first.length + 2 }], ...rest] }
}

/**
 * What a finished action reports: its headline and size, or a preview.
 *
 * A card's summary — a search's count, a read's window, a command's exit
 * status — rides on the head, where no preview bound can hide it: a failing
 * test run's `exit 1` is the line that matters, and it is the last line of
 * the output.
 *
 * @param outcome - how the call ended.
 * @param verb - the action's verb, which decides whether output is previewed.
 * @param bound - preview length and the words for a count.
 * @param title - the head's text, which a headline repeating it adds nothing to.
 * @returns a size, a summary, or both for the head line, and the lines under
 *   the head at the output column.
 */
function outcomeLines(outcome: ToolOutcome, verb: Verb, bound: ResultBound, title: string): {
  readonly inline?: Styled
  readonly lines: readonly PresentedLine[]
} {
  const failed = !outcome.ok
  const tone: Tone = failed ? 'failed' : 'plain'
  const card = outcome.detail ?? []
  const summaries = card.filter(line => line.summary !== undefined)
  const body = [...cardLines(outputLines(outcome.text), failed, bound.code),
    ...cardLines(card.filter(line => line.summary === undefined), failed, bound.code)]
  const stat = failed ? undefined : changeSize(outcome.detail)
  // A failure is always news, and a diff is what an edit did.
  const previewed = failed || PREVIEWED.has(verb) || stat !== undefined
  const { lines: shown, hidden } = previewed ? excerpt(body, bound.lines, bound, tone, failed) : { lines: [], hidden: body.length }
  // Previewed output counts what it left out below it; a count alone says how
  // much there was beside the headline. A change's size, or the card's own
  // count, says it either way.
  const size = stat !== undefined || summaries.length > 0 || shown.length > 0 || body.length === 0 || hidden === 0
    ? undefined : countOf(body.length, bound)
  const summary = summaries.length === 0 ? undefined
    : joined(summaries.map(line => [line.text, line.summary === 'failure' || failed ? 'failed' : 'quiet'] as const))
  const headline = outcome.title === undefined || sameWords(outcome.title, title, verb) ? undefined : outcome.title
  const quiet = (text: string | undefined): Styled | undefined =>
    text === undefined ? undefined : { text, spans: [{ length: text.length, tone: 'quiet' }] }
  const inline = [stat, summary, headline === undefined && !failed ? quiet(size) : undefined]
    .filter((part): part is Styled => part !== undefined)
    .reduce<Styled | undefined>((all, part) => all === undefined ? part : beside(all, part), undefined)
  const heading = headline === undefined && !failed ? undefined : joined([[headline, tone], [size, failed ? tone : 'quiet']])
  return {
    ...inline === undefined ? {} : { inline },
    lines: [
      ...heading === undefined || heading.text === '' ? [] : [{ ...continuation(heading.text, tone), spans: heading.spans }],
      ...shown,
    ],
  }
}

/**
 * A count in the bound's words: `1 line`, `70 lines`.
 * @param count - how many lines.
 * @param bound - the locale-owned nouns.
 * @returns the count and its noun.
 */
const countOf = (count: number, bound: ResultBound): string => `${count} ${count === 1 ? bound.single ?? bound.unit : bound.unit}`

/**
 * How much a change added and removed, as `+2 −1` in each side's tone.
 * @param detail - a card's lines.
 * @returns the size, or undefined when the card is not a change.
 */
function changeSize(detail: readonly CardLine[] | undefined): Styled | undefined {
  const added = (detail ?? []).filter(line => line.emphasis === 'added').length
  const removed = (detail ?? []).filter(line => line.emphasis === 'removed').length
  const parts: Styled[] = [
    ...added === 0 ? [] : [{ text: `+${added}`, spans: [{ length: `+${added}`.length, tone: 'added' as const }] }],
    ...removed === 0 ? [] : [{ text: `−${removed}`, spans: [{ length: `−${removed}`.length, tone: 'removed' as const }] }],
  ]
  if (parts.length === 0) return undefined
  return parts.reduce((all, part) => ({ text: `${all.text} ${part.text}`, spans: [...all.spans, { length: 1, tone: 'plain' }, ...part.spans] }))
}

/** Whether a line is one side of a change, as opposed to the lines around one. */
const isChange = (line: PresentedLine): boolean => line.tone === 'added' || line.tone === 'removed'

/**
 * The first lines of a body, up to a bound.
 *
 * A change counts only its changed lines against the bound, so the preview
 * holds as much of the edit as it says it does; a gap or a path between
 * them rides along, but never ends the preview.
 *
 * @param body - the lines under a head.
 * @param limit - lines the preview may show.
 * @returns the lines shown, and how many counted lines it left out.
 */
function preview(body: readonly PresentedLine[], limit: number): { readonly shown: readonly PresentedLine[], readonly hidden: number } {
  const counted = body.some(isChange) ? isChange : () => true
  const total = body.filter(counted).length
  const shown: PresentedLine[] = []
  let taken = 0
  for (const line of body) {
    if (counted(line) && taken >= Math.max(0, limit)) break
    if (counted(line)) taken++
    shown.push(line)
  }
  while (shown.length > 0 && !counted(shown.at(-1)!)) shown.pop()
  return { shown, hidden: total - taken }
}

/**
 * A preview of a body, with a count of what it left out.
 *
 * Output shows its first lines and its last, the count between them: what a
 * command ends with — a test summary, the error it stopped on — is as often
 * the news as what it opened with. The blank lines a command pads its output
 * with are dropped from either end. A change shows its first changed lines,
 * as a patch reads from the top. A count that would stand for a single line
 * costs the row it saves, so that line is drawn instead.
 *
 * @param body - the lines under a head.
 * @param limit - lines the preview may show; zero shows none.
 * @param bound - the words for the count.
 * @param tone - the body's tone, which a failure's count keeps.
 * @param failed - whether the body is a failure's.
 * @returns the lines to draw, and how many the count stands for.
 */
function excerpt(body: readonly PresentedLine[], limit: number, bound: ResultBound, tone: Tone, failed: boolean): {
  readonly lines: readonly PresentedLine[]
  readonly hidden: number
} {
  const more = (hidden: number): PresentedLine => continuation(`+${hidden} ${bound.more}`, failed ? tone : 'quiet')
  if (limit <= 0) return { lines: [], hidden: body.filter(body.some(isChange) ? isChange : () => true).length }
  if (body.some(isChange)) {
    const first = preview(body, limit)
    const { shown, hidden } = first.hidden === 1 ? preview(body, limit + 1) : first
    return { lines: [...shown, ...hidden === 0 ? [] : [more(hidden)]], hidden }
  }
  let from = 0
  let to = body.length
  while (from < to && body[from]!.text.trim() === '') from++
  while (to > from && body[to - 1]!.text.trim() === '') to--
  const lines = body.slice(from, to)
  if (lines.length <= limit + 1) return { lines, hidden: 0 }
  const tail = Math.floor(limit / 2)
  // A blank beside the count separates nothing from it, so it joins what the count stands for.
  let head = limit - tail
  let back = lines.length - tail
  while (head > 0 && lines[head - 1]!.text.trim() === '') head--
  while (back < lines.length && lines[back]!.text.trim() === '') back++
  const hidden = back - head
  return { lines: [...lines.slice(0, head), more(hidden), ...lines.slice(back)], hidden }
}

/**
 * Whether a result's headline only repeats the call's, as `Edit one.ts` does
 * under `edit one.ts`.
 */
function sameWords(headline: string, title: string, verb: Verb): boolean {
  return bare(headline, verb).trim() === title.trim()
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
  /** Locale-owned noun for one line, as in `1 line`; absent, {@link ResultBound.unit} serves. */
  readonly single?: string
  /** Locale-owned phrase for the lines a bound left out, as in `+67 more lines`. */
  readonly more: string
  /** Colours tool source, structured output, and fenced code; absent, text retains its semantic tone. */
  readonly code?: Highlight
  /** Locale-owned word for a step's failed calls, as in `1 failed`; absent, the head leaves the count to the calls' red. */
  readonly failures?: string
}

/**
 * Reasoning as scrollback keeps it: its first rows, and a count of the rest.
 *
 * Reasoning is watched while it streams and rarely read again, and printed
 * whole it buries the answer under the working-out. The bound is the one tool
 * results use, counted in rows at the width the transcript is drawn, since a
 * single paragraph of reasoning can wrap to a screenful. A count that would
 * hide one row costs the row it hides, so that row is drawn instead.
 *
 * @param lines - the block's lines.
 * @param result - the preview bound and its locale-owned count wording.
 * @param wrap - the rows a line wraps into.
 * @returns the lines to draw.
 */
function reasoningPreview(lines: readonly PresentedLine[], result: ResultBound, wrap: (line: PresentedLine) => readonly string[]): readonly PresentedLine[] {
  // The space a row broke after is kept by the wrap and draws nothing.
  const rows = lines.flatMap(line => {
    let offset = 0
    return wrap(line).map(row => {
      const text = row.trimEnd()
      const spans = line.spans === undefined ? {} : { spans: sliceSpans(line.spans, offset, offset + text.length) }
      offset += row.length
      // softBreaks replaces a space with a newline of the same UTF-16 length.
      if (line.literal !== true && line.text[offset] === ' ' && !row.endsWith(' ')) offset++
      return { ...line, ...spans, text }
    })
  })
  const shown = Math.max(0, result.lines)
  if (rows.length <= shown + 1) return lines
  // Nothing of the text: the count, slanted as the reasoning it stands for.
  if (shown === 0) {
    const { spans: _spans, ...first } = rows[0]!
    return [{ ...first, text: `${rows.length} ${result.unit}` }]
  }
  // A paragraph break at the cut would separate the count from the text it counts.
  const head = rows.slice(0, shown)
  while (head.length > 1 && head.at(-1)!.text === '') head.pop()
  return [...head, { ...continuation(`+${rows.length - head.length} ${result.more}`, 'quiet'), column: COLUMN.rail, prose: true }]
}

/**
 * Where a line's wrapping would open a row with a space, break at the space.
 *
 * Ink wraps `Text` keeping whitespace, so when a word ends exactly at the
 * width, the space after it cannot hang on that row and opens the next one:
 * the continuation sits a column right of its paragraph. Replacing that space
 * with the break draws the row where it belongs. The text keeps its length,
 * so styled runs measured against it still line up. Text the wrapper rewrites
 * in other ways, such as expanding a tab, is left as it is.
 *
 * @param text - one line of text.
 * @param width - columns it wraps at.
 * @returns the text, with each such space turned into a newline.
 */
export function softBreaks(text: string, width: number): string {
  let done = ''
  let rest = text
  for (;;) {
    const wrapped = wrapAnsi(rest, width, { hard: true, trim: false })
    let from = 0
    let cut: number | undefined
    for (let at = 0; at < wrapped.length && cut === undefined; at++) {
      if (wrapped[at] === rest[from]) from++
      else if (wrapped[at] !== '\n') return done + rest
      else if (rest[from] === ' ' && wrapped[at + 1] === ' ') cut = from
    }
    if (cut === undefined) return done + rest
    done += `${rest.slice(0, cut)}\n`
    rest = rest.slice(cut + 1)
  }
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
 * @param result - how much of a tool result or of reasoning this surface
 *   draws; ignored by every other row kind, whose length the harness does not
 *   choose.
 * @param wrap - the rows a line wraps into where it is drawn. Reasoning is
 *   previewed in rows, because one paragraph of it can fill a screen; without
 *   this, as in the live region, it is drawn whole.
 * @returns display lines in order, possibly empty.
 */
export function present(row: Row, result: ResultBound, wrap?: (line: PresentedLine) => readonly string[]): readonly PresentedLine[] {
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
      // As typed, from the rail: the slash is the first column, so a command
      // breaks the left edge the way it breaks the conversation, and its
      // outcome hangs from it on a branch as a step's calls hang from their
      // head. The name is bold, as the verb opening an action is, so it reads
      // without colour; the arguments are what the user wrote, at full weight.
      return opening([{
        marker: MARKER.none, verb: '', text: `/${row.name}${row.args}`, column: COLUMN.rail, flush: true,
        tone: 'plain', spans: [{ length: row.name.length + 1, tone: 'said' }],
      }])

    case 'assistant': {
      const lines = markdownLines(row.text, 'plain', result.code).map((line, index) => ({
        ...line,
        marker: index === 0 && row.continued !== true ? MARKER.reply : MARKER.none,
        verb: '', column: COLUMN.rail, tone: 'plain' as const,
      }))
      return row.continued === true ? lines : opening(lines)
    }

    case 'reasoning': {
      // A paragraph at the rail, as the answer is, without a verb: dim and
      // slanted, it reads as the working-out, and the answer's `<` says where
      // the reply begins.
      const lines = markdownLines(row.text, 'thought', result.code).map(line => ({
        ...line, marker: MARKER.none, verb: '', column: COLUMN.rail, tone: 'thought' as const, prose: true,
      }))
      const kept = wrap === undefined ? lines : reasoningPreview(lines, result, wrap)
      return row.continued === true ? kept : opening(kept)
    }

    case 'tool-call':
      return opening(action(row, result))

    case 'tool-group':
      return opening(group(row.calls, result))

    case 'tool-result': {
      const tone: Tone = row.ok ? 'plain' : 'failed'
      // A card leaves `text` empty and a tool without one leaves `detail`
      // absent, so exactly one of these carries the body.
      const body = [
        ...cardLines(outputLines(row.text), !row.ok, result.code),
        ...cardLines(row.detail, !row.ok, result.code),
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
      const { shown, hidden } = preview(body, result.lines)
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
      // A command's outcome continues its command, on the branch that closes
      // it: the reader reads the two as one exchange, and no verb repeats
      // what the command's name already says.
      if (row.placement === 'command') return linesOf(row.text).map((text, index) => ({
        marker: index === 0 ? TREE.corner : MARKER.none, markerTone: row.tone === 'error' ? 'failed' : 'quiet',
        verb: '', text, column: COLUMN.rail, wide: true, tone: row.tone === 'error' ? 'failed' : 'plain',
      }))
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
