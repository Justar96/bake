/**
 * Presentation of a transcript row as positioned lines.
 *
 * Pure. A row in, display lines out, with no React and no terminal. The
 * component layer turns these into Ink boxes, which keeps every placement rule
 * testable without rendering and without an Ink input channel.
 *
 * Placement follows `apps/tui/DESIGN-LAYOUT.md`. A marker column, a verb column
 * naming what the agent did, and output aligned under the verb's argument.
 *
 * @module @dsh-tui/ui/present
 */

import wrapAnsi from 'wrap-ansi'
import { markdownLines, sliceSpans } from './markdown.ts'
import { outputLines, outputSpans, toolText } from './tool-output.ts'
import { iconFor, ICON } from './icons.ts'
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
  /** Reasoning. Dim like metadata, and italic so it stays distinct from the answer. */
  | 'thought'
  /** What names a section. An action's verb and its tool. */
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
 * The provider is shown in `/model` when more than one is configured. The
 * status line repeats every frame, so it keeps only the model name.
 *
 * @param route - `provider/model`, or a bare model name.
 * @returns the model name alone.
 */
export const compactModel = (route: string): string => route.slice(route.lastIndexOf('/') + 1)

/**
 * Shorten a working directory against home.
 *
 * An absolute path spends most of its width on a prefix every path shares.
 *
 * @param cwd - absolute working directory.
 * @param home - home directory, when one is known.
 * @returns a `~`-relative path, or `cwd` when it is outside home.
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
 * Hint key for the composer's right slot.
 *
 * The hint is contextual, never a permanent row. A fixed hint costs one row
 * of the live region's budget on every frame. This returns a key, not text,
 * because copy is locale-owned.
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
  /** Whether the text is the model thinking, not speaking or acting. */
  readonly italic?: boolean
}

/**
 * Colour and weight for one tone.
 *
 * Colour is semantic, never decorative. Red is a failure or a removed line,
 * green is an added line or a finished action, and ocean blue is a question
 * waiting for an answer. `PALETTE` owns the tones; this function only maps a
 * tone to a meaning. A failure is never dimmed. Dim means supporting detail,
 * and a failure is what the user has to read. Bold marks structure. The verb
 * and tool that open an action are bold, so a scan down the left of the
 * transcript lands on each action, not on its arguments.
 *
 * @param tone - emphasis carried by the line.
 * @returns colour and weight for the component layer.
 */
export function styleOf(tone: Tone): LineStyle {
  switch (tone) {
    case 'said': return { dim: false, bold: true }
    case 'plain': return { dim: false, bold: false }
    case 'quiet': return { dim: true, bold: false }
    // Dim, like metadata, and italic, so reasoning stays distinct from tool
    // output in the same column.
    case 'thought': return { dim: true, bold: false, italic: true }
    case 'strong': return { dim: false, bold: true }
    case 'done': return { color: PALETTE.done, dim: false, bold: true }
    case 'failed': return { color: PALETTE.failed, dim: false, bold: false }
    case 'asking': return { color: PALETTE.asking, dim: false, bold: false }
    // Both sides of a diff keep full weight. Dimming the removed side would
    // make a deletion look like supporting detail.
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
  /** Marker column content. A prompt, a selection mark, or a space. */
  readonly marker: string
  /** Verb column content, empty for a line that continues one. */
  readonly verb: string
  /**
   * What the verb column shows when the line has no verb. A changed line's
   * number, drawn right-aligned against the text in the line's tone.
   */
  readonly gutter?: string
  /** Emphasis for the verb when it differs from the text's, as an outcome's does. */
  readonly verbTone?: Tone
  /** Emphasis for the marker when it differs from the text's. An action's state. */
  readonly markerTone?: Tone
  /** Whether the marker blinks, which it does while its action runs. */
  readonly pulse?: boolean
  /**
   * Whether the line is part of a tool result's preview. Its plain text is
   * drawn in the softer output grey.
   */
  readonly zone?: boolean
  /** Text for the remaining width. */
  readonly text: string
  /** Column the text starts at, which decides which budget bounds it. */
  readonly column: typeof COLUMN.rail | typeof COLUMN.output
  /**
   * Whether the text starts in the rail instead of after it, taking the
   * marker's columns. A command does this. Its slash is the first thing typed
   * and the first thing on its row. `marker` is then not drawn.
   */
  readonly flush?: boolean
  /**
   * Whether text at the rail wraps at the full width, as tool output does,
   * instead of at the prose measure. A command's outcome does this. It is
   * often a table, such as `/help`, and it is output, not prose.
   */
  readonly wide?: boolean
  /** Emphasis for the component layer to colour. */
  readonly tone: Tone
  /**
   * Emphasis for consecutive runs of `text`, from its start; text past the
   * last run takes `tone`. Kept beside the text instead of splitting it, so
   * wrapping, measuring, and the plain-text form all use one string.
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
  /** Drawn with foreground and background swapped. The words an edit changed. */
  readonly inverse?: boolean
}

/**
 * A run of highlighted code, as a {@link Highlight} reports it.
 *
 * Colour is the one thing on the surface that is not semantic. It comes from
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
 * Synchronous, because presentation is. A highlighter that does not know the
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
 * Verb for what a tool did.
 *
 * Use the tool's own name when it already names an action and fits the
 * column. Otherwise use the closest verb in the vocabulary. `bash`, `shell`,
 * and `zsh` are one kind of event, so they share `run` instead of three names.
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
  // Checked before `write`. Rewriting a plan is not an edit to the workspace.
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
 * A deliberate empty row, not a row that happens to be empty.
 *
 * It opens a zone. A user turn, and each action the agent took inside one.
 * Indentation alone separates an answer at the rail from output under a verb.
 * It cannot separate two zones that share a column. Reasoning directly under
 * an answer, or an action under the previous action's output, would look like
 * more of the same block. The blank is what separates them.
 *
 * A call's own result continues its zone and gets no blank. A command's
 * notice does not either, so a result stays attached to what produced it.
 */
const BLANK: PresentedLine =
  { marker: MARKER.none, verb: '', text: '', column: COLUMN.rail, tone: 'plain' }

/**
 * Placeholder for a call whose arguments are still streaming.
 *
 * Arguments arrive as a JSON string built from deltas. Every prefix is
 * invalid JSON, and most prefixes end mid-token. Showing that partial text
 * would put unparsed syntax on screen and redraw it on every chunk. The verb
 * already names the action. The complete arguments arrive with the committed
 * row a moment later.
 */
export const PENDING_ARGUMENTS = '...'

/**
 * Open a zone, unless the row has nothing to put in it.
 *
 * A blank belongs to the lines under it. On its own it is an empty row charged
 * to the live region's budget for content that never arrived, which is what an
 * empty text or reasoning block produces while a turn is still streaming.
 *
 * @param lines - the zone's lines, in order.
 * @returns the lines behind a blank row, or nothing when there are none.
 */
const opening = (lines: readonly PresentedLine[]): readonly PresentedLine[] =>
  lines.length === 0 ? [] : [BLANK, ...lines]

/** A continuation line. No marker, no verb, aligned under the argument. */
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
 * @param keep - which lines the highlighter sees; the rest keep plain tones.
 * @returns continuation lines in order, empty when there is no card.
 */
function cardLines(detail: readonly CardLine[] | undefined, failed = false, code?: Highlight, keep: (index: number) => boolean = () => true): readonly PresentedLine[] {
  const lines = detail ?? []
  const tokens = failed || code === undefined ? [] : highlighted(lines, code, keep)
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
 * @param keep - which lines to highlight; a run ends at a line left out.
 * @returns tokens by line index, absent where the line is not code, was left
 *   out, or the highlighter declined.
 */
function highlighted(lines: readonly CardLine[], code: Highlight, keep: (index: number) => boolean): readonly (readonly CodeToken[] | undefined)[] {
  const tokens: (readonly CodeToken[] | undefined)[] = []
  let from = 0
  while (from < lines.length) {
    const first = lines[from]!
    let to = from + 1
    if (first.source !== undefined && keep(from)) {
      while (to < lines.length && keep(to) && lines[to]!.source === first.source && lines[to]!.emphasis === first.emphasis
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

/**
 * A result's body, with syntax colour only on the lines a preview draws.
 *
 * A read can return a whole file and a command a megabyte of JSON, and the
 * preview draws a few lines of either. Highlighting is the costly step, so
 * the body is laid out plain, the preview picks its lines, and only those
 * reach the grammar. A tail run starts its grammar at its own first line.
 *
 * @param groups - card line groups, each highlighted on its own as before.
 * @param failed - whether the result stays red throughout.
 * @param code - the highlighter, absent for none.
 * @param drawn - the lines of a plain body the preview draws. It must return
 *   the body's own line objects, and may add lines of its own.
 * @returns the body in order, highlighted where drawn.
 */
function drawnBody(
  groups: readonly (readonly CardLine[] | undefined)[],
  failed: boolean,
  code: Highlight | undefined,
  drawn: (body: readonly PresentedLine[]) => readonly PresentedLine[],
): readonly PresentedLine[] {
  const plain = groups.map(group => cardLines(group, failed))
  if (failed || code === undefined) return plain.flat()
  const shown = new Set(drawn(plain.flat()))
  return groups.flatMap((group, at) => {
    const lines = plain[at]!
    return lines.some(line => shown.has(line)) ? cardLines(group, failed, code, index => shown.has(lines[index]!)) : lines
  })
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
 * The runs of one changed line. Its sign, then its code.
 *
 * The side's tone is the line's colour, and syntax colour is laid over it.
 * the sign, punctuation, and plain words keep the tone, so a highlighted line
 * still shows as added or removed. A token the theme colours takes that
 * colour. Code the edit changed is reversed in the tone.
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
 * Display form of a tool name. Each word is capitalized and concatenated.
 * `bash` becomes `Bash`; `read_file` becomes `ReadFile`.
 * @param tool - tool name from the session log.
 * @returns the display name, or `tool` itself when it contains no words.
 */
export function toolLabel(tool: string): string {
  const label = tool.split(/[^\p{L}\p{N}]+/u).filter(word => word !== '')
    .map(word => word[0]!.toUpperCase() + word.slice(1)).join('')
  return label === '' ? tool : label
}

/**
 * Connector drawn in the verb column of the first output line, hanging that
 * output from the action head.
 */
export const CONNECTOR = '\u23bf'

/**
 * Render one action as one block. The head, the arguments, and the outcome once it exists.
 *
 * The head is a call. The tool name and argument are in parentheses, for example
 * `Bash(cargo check --workspace)`. The marker carries state. It blinks while
 * the call runs. When the call finishes, the same line is printed once with a
 * green or red marker. Output hangs from the head on {@link CONNECTOR}, so
 * the reader sees one block per action, not a call and a result stacked
 * apart. The call id is not drawn. It only joins the two records.
 *
 * @param row - the call, including its outcome when one exists.
 * @param bound - how much of the outcome to preview.
 * @returns the block's lines, without the opening blank.
 */
function action(row: ToolCallRow, bound: ResultBound): readonly PresentedLine[] {
  const family = familyOf(row.tool)
  const verb = family ?? VERB.run
  const outcome = row.result
  const [first = '', ...rest] = linesOf(row.input)
  const title = bare(first, verb)
  // Arguments still streaming. Show `Name(...)` until the full argument arrives.
  const name = toolLabel(row.tool)
  const text = title === '' ? name : `${name}(${title})`
  const after = outcome === undefined ? undefined : outcomeLines(outcome, verb, bound, title)
  // A count with no body rides on the head, so a read is one row. A change's
  // size does the same, so an edit reports how large it was.
  const named: Styled = { text, spans: [{ length: name.length, tone: 'strong' },
    ...text.length === name.length ? [] : [{ length: text.length - name.length, tone: 'plain' as const }]] }
  const inline = after?.inline === undefined ? named : beside(named, after.inline)
  const head: PresentedLine = {
    marker: iconFor(row.tool),
    markerTone: outcome === undefined ? 'strong' : outcome.ok ? 'done' : 'failed',
    ...outcome === undefined ? { pulse: true } : {},
    verb: '', text: inline.text, column: COLUMN.rail, wide: true, tone: 'plain',
    ...inline.spans.length === 0 ? {} : { spans: inline.spans },
  }
  // Card lines describe the call, the way its description does. They stay
  // quiet under the head unless they carry diff colour. They and any extra
  // input lines share the output bound. A script the model wrote can be as
  // long as a file, and it is printed under the head on every call. At least
  // one line of each is always shown, so a description survives a bound that
  // collapses the output.
  const limit = Math.max(1, bound.lines)
  const described = cardLines(row.detail).map(line => line.tone === 'plain' ? { ...line, tone: 'quiet' as const } : line)
  const body = [
    ...excerpt(rest.map(text => continuation(text, 'plain')), limit, bound, 'plain', false).lines,
    ...excerpt(described, limit, bound, 'quiet', false).lines,
    ...after?.lines ?? []]
  return [head, ...connected(body)]
}

/**
 * Hang a block's body from its head. The first line takes the connector in
 * its verb column, unless that column already holds a changed line's number.
 * @param body - the lines under a head.
 * @returns the same lines, the first connected.
 */
function connected(body: readonly PresentedLine[]): readonly PresentedLine[] {
  const [first, ...rest] = body
  if (first === undefined || first.verb !== '' || first.gutter !== undefined) return body
  return [{ ...first, verb: CONNECTOR, verbTone: 'quiet' }, ...rest]
}

/**
 * Render one step's calls as one block. A head counts them, then each
 * call hangs from it, joined by the tree in the rail.
 *
 * The rail already shows an action's state in its marker, so each call's
 * branch takes that marker's place and colour. It pulses while the call runs
 * and turns green or red when the call ends. The verb, argument, and output
 * keep their columns. The head's marker is the step's state. It is running while
 * any call is running, red when one failed. Calls are not separated by a
 * blank row. They were one model decision, and the tree stem is what shows that.
 *
 * @param calls - the step's calls, in the order the model made them.
 * @param bound - how much of each outcome to preview.
 * @returns the block's lines, without the opening blank.
 */
function group(calls: readonly ToolCallRow[], bound: ResultBound): readonly PresentedLine[] {
  return [groupHead(calls, bound), ...hang(calls.map(call => action(call, bound)))]
}

/**
 * The head of a step's block. Its calls are counted by verb, in the step's tense.
 * @param calls - the step's calls, in the order the model made them.
 * @param bound - the locale's words for failures.
 * @returns the head line, its marker the step's state.
 */
function groupHead(calls: readonly ToolCallRow[], bound: ResultBound): PresentedLine {
  const running = calls.some(call => call.result === undefined)
  const failed = calls.filter(call => call.result?.ok === false).length
  // Count verbs in the order they first appear, in the step's tense.
  const counts = new Map<Verb, number>()
  for (const call of calls) counts.set(verbFor(call.tool), (counts.get(verbFor(call.tool)) ?? 0) + 1)
  const tally = [...counts].map(([verb, count]) => `${running ? verb : PAST[verb]} ${count}`).join(' \u00b7 ')
  const failures = failed === 0 || running || bound.failures === undefined ? undefined : ` \u00b7 ${failed} ${bound.failures}`
  // One kind of call throughout takes that kind's icon. A mix is just an action.
  const icons = new Set(calls.map(call => iconFor(call.tool)))
  return {
    marker: icons.size === 1 ? [...icons][0]! : ICON.other,
    markerTone: running ? 'strong' : failed > 0 ? 'failed' : 'done',
    ...running ? { pulse: true } : {},
    verb: '', text: `${tally}${failures ?? ''}`, column: COLUMN.rail, tone: 'strong',
    ...failures === undefined ? {} : { spans: [{ length: tally.length, tone: 'strong' as const }, { length: failures.length, tone: 'failed' as const }] },
  }
}

/**
 * Hang each call's lines from the block's head on the tree in the rail.
 * @param bodies - each call's lines, head first, in order.
 * @returns the lines, each call's head on a branch and the last on the corner.
 */
function hang(bodies: readonly (readonly PresentedLine[])[]): readonly PresentedLine[] {
  return bodies.flatMap((lines, index) => {
    const last = index === bodies.length - 1
    // Only the step's head blinks. A blinking branch would open a gap in the tree.
    // In a batch only the head carries an icon; each branch is the tree alone.
    return lines.map((line, row) => row === 0
      ? { ...line, marker: last ? TREE.corner : TREE.branch, pulse: false }
      : { ...line, marker: last ? MARKER.none : TREE.stem, markerTone: 'quiet' as const })
  })
}

/**
 * A step's block while it runs, fitted to the rows the live region has.
 *
 * Cut from the top like prose, a block taller than the window lost its head
 * first. That line says what the step is doing, and its marker says
 * that it is still running. Instead the block gives up detail oldest first,
 * and keeps its head.
 *
 * 1. Each finished call, oldest first, folds to its head line alone. The
 *    newest output stays in view longest, since it is the output just read.
 * 2. With every finished call folded, the oldest calls fold into one
 *    `+N earlier calls` branch, until the rest fit.
 *
 * Nothing is lost. Once the step ends, the block prints to history whole.
 *
 * @param calls - the step's calls, in the order the model made them.
 * @param bound - how much of each outcome to preview, and the locale's words.
 * @param rows - rows the live region may draw.
 * @param height - rows one line occupies once wrapped.
 * @returns the block's lines, at most `rows` tall unless even the head alone
 *   wraps past them; the head is always the first line or the one after the
 *   opening blank.
 */
export function fittedGroup(
  calls: readonly ToolCallRow[],
  bound: ResultBound,
  rows: number,
  height: (line: PresentedLine) => number = () => 1,
): readonly PresentedLine[] {
  const head = groupHead(calls, bound)
  const bodies = calls.map(call => action(call, bound))
  const whole = opening([head, ...hang(bodies)])
  const size = (lines: readonly PresentedLine[]): number => lines.reduce((sum, line) => sum + height(line), 0)
  if (rows <= 0 || size(whole) <= rows) return whole
  // Heights ignore the rail's marker, which never changes a line's width, so
  // each call is measured once however it ends up folded.
  const full = bodies.map(size)
  const folded = bodies.map((lines, index) => calls[index]!.result === undefined ? full[index]! : height(lines[0]!))
  const fixed = height(BLANK) + height(head)
  const fold = (lines: readonly PresentedLine[], index: number, count: number): readonly PresentedLine[] =>
    index < count && calls[index]!.result !== undefined ? lines.slice(0, 1) : lines
  for (let count = 1; count <= calls.length; count++) {
    const used = fixed + bodies.reduce((sum, _, index) => sum + (index < count ? folded[index]! : full[index]!), 0)
    if (used <= rows) return [BLANK, head, ...hang(bodies.map((lines, index) => fold(lines, index, count)))]
  }
  if (bound.earlier === undefined) return [BLANK, head, ...hang(bodies.map(lines => lines.slice(0, 1)))]
  // The summary is a branch of its own, so the tree is still one step.
  const summary = (hidden: number): PresentedLine => ({
    marker: MARKER.none, verb: '', text: `+${hidden} ${bound.earlier}`, column: COLUMN.rail, tone: 'quiet',
  })
  let hidden = 1
  while (hidden < calls.length - 1
    && fixed + height(summary(hidden)) + folded.slice(hidden).reduce((sum, rows) => sum + rows, 0) > rows) hidden++
  const kept = bodies.slice(hidden).map((lines, index) => fold(lines, index + hidden, calls.length))
  const least = [BLANK, head, ...hang([[summary(hidden)], ...kept])]
  if (size(least) <= rows) return least
  // A window too short for even that keeps what says the most per row. The
  // step's head, then the newest call's own head line, then the count of the
  // rest, and gives up the blank that opens the block before any of them.
  const newest = bodies.at(-1)!.slice(0, 1)
  const ladder = [
    [BLANK, head, ...hang([[summary(calls.length - 1)], newest])],
    [head, ...hang([[summary(calls.length - 1)], newest])],
    [head, ...hang([newest])],
  ]
  return ladder.find(lines => size(lines) <= rows) ?? [head]
}

/**
 * Drop a title's leading word when it repeats the verb.
 *
 * `Grep` under `find`, and `Read` under `read`, would print the action twice.
 * A command keeps its words unless the first word is `run` itself. Under
 * `run`, `bash deploy.sh` is the command, not a second name for it.
 *
 * @param title - the head's first line.
 * @param verb - the verb the title is drawn under.
 * @returns the title, with a redundant first word removed.
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
 * What a finished action reports. A headline and a size on the head, or a preview under it.
 *
 * A card summary — a search count, a read window, a command exit status —
 * stays on the head, where the preview bound cannot hide it. A failing run's
 * `exit 1` is the line that matters, and it is the last line of the output.
 *
 * @param outcome - how the call ended.
 * @param verb - the action's verb, which decides whether output is previewed.
 * @param bound - preview length and the words for a count.
 * @param title - the head's text. A headline that repeats it is omitted.
 * @returns an optional size or summary for the head, and the lines under the
 *   head at the output column.
 */
function outcomeLines(outcome: ToolOutcome, verb: Verb, bound: ResultBound, title: string): {
  readonly inline?: Styled
  readonly lines: readonly PresentedLine[]
} {
  const failed = !outcome.ok
  const tone: Tone = failed ? 'failed' : 'plain'
  const card = outcome.detail ?? []
  const summaries = card.filter(line => line.summary !== undefined)
  const stat = failed ? undefined : changeSize(outcome.detail)
  // A failure is always news, and a diff is what an edit did.
  const previewed = failed || PREVIEWED.has(verb) || stat !== undefined
  const body = drawnBody([outputLines(outcome.text), card.filter(line => line.summary === undefined)], failed, bound.code,
    plain => previewed ? excerpt(plain, bound.lines, bound, tone, failed).lines : [])
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
    lines: zoned([
      ...heading === undefined || heading.text === '' ? [] : [{ ...continuation(heading.text, tone), spans: heading.spans }],
      ...shown,
    ]),
  }
}

/**
 * Place lines in a result's preview zone.
 * @param lines - a result's preview, headline and count included.
 * @returns the same lines, each marked as zone.
 */
const zoned = (lines: readonly PresentedLine[]): readonly PresentedLine[] => lines.map(line => ({ ...line, zone: true }))

/**
 * A count in the bound's words. `1 line`, `70 lines`.
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
 * Output shows its first lines and its last, the count between them. What a
 * command ends with, a test summary or the error it stopped on, is as often
 * the news as what it opened with. The blank lines a command pads its output
 * with are dropped from either end. A change shows its first changed lines,
 * the way a patch is read from the top. A count that would stand for a single line
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
 * Required, not defaulted. How much output belongs in scrollback is a
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
  /**
   * Locale-owned phrase for a running step's calls folded out of view, as in
   * `+3 earlier calls`; absent, a step taller than its window folds each
   * finished call to its head but hides none.
   */
  readonly earlier?: string
}

/**
 * Reasoning as scrollback keeps its first rows, and a count of the rest.
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
  // Nothing of the text. The count, slanted as the reasoning it stands for.
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
 * width, the space after it cannot hang on that row and opens the next one.
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
 * A row may produce several lines. Multi-line text keeps its own breaks, and
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
      // Attachment metadata is about the prompt, not part of it. It stays dim
      // instead of carrying the weight of the user's own words.
      const staged = (row.attachments ?? []).map(formatAttachment).map(text => ({
        marker: MARKER.none, verb: '', text, column: COLUMN.rail, tone: 'quiet' as const,
      }))
      return opening([{ ...BLANK, divider: true, tone: 'quiet' }, ...said, ...staged])
    }

    case 'command':
      // As typed, from the rail. The slash is the first column, so a command
      // breaks the left edge the way it breaks the conversation, and its
      // outcome hangs from it on a branch as a step's calls hang from their
      // head. The name is bold, as the verb opening an action is, so it stays
      // readable without colour. The arguments are what the user wrote, at
      // full weight.
      return opening([{
        marker: MARKER.none, verb: '', text: `/${row.name}${row.args}`, column: COLUMN.rail, flush: true,
        tone: 'plain', spans: [{ length: row.name.length + 1, tone: 'said' }],
      }])

    case 'assistant': {
      const lines = markdownLines(row.text, 'plain', result.code).map(line => ({
        ...line,
        marker: MARKER.none,
        verb: '', column: COLUMN.rail, tone: 'plain' as const,
      }))
      return row.continued === true ? lines : opening(lines)
    }

    case 'rate':
      // Dim, at the rail, directly under the answer. It continues the answer's
      // section instead of opening one, and it is metadata about that answer.
      return [{ marker: MARKER.none, verb: '', text: row.text, column: COLUMN.rail, tone: 'quiet' }]

    case 'reasoning': {
      // A paragraph at the rail, as the answer is, without a verb. Dim and
      // italic, it is the working-out. The blank that opens the answer, at
      // full brightness, marks where the reply begins.
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
      const body = drawnBody([outputLines(row.text), row.detail], !row.ok, result.code, plain => preview(plain, result.lines).shown)
      // Calls may finish out of order, so the result names its own call.
      // The nearest preceding row may belong to a different call. An empty result still
      // acknowledges completion, with no size to report.
      const size = body.length === 0 ? undefined : `${body.length} ${result.unit}`
      // A success is its outcome and headline, with the id and size as detail.
      // A failure stays red throughout, because the whole line is the news.
      const outcome = joined([[`[${row.callId}]`, 'quiet'], [row.title, 'plain'], [size, 'quiet']])
      const head: PresentedLine = {
        marker: MARKER.none, verb: row.ok ? VERB.done : VERB.error, verbTone: row.ok ? 'done' : 'failed',
        text: outcome.text, column: COLUMN.output, tone, ...row.ok ? { spans: outcome.spans } : {},
      }
      // The head of the output prints once, under the outcome, and stays. Shown
      // for a moment and then removed, it was the rows the composer jumped by.
      const { shown, hidden } = preview(body, result.lines)
      return [head, ...zoned([...shown, ...hidden === 0 || shown.length === 0 ? [] : [continuation(`+${hidden} ${result.more}`, row.ok ? 'quiet' : tone)]])]
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
      // it. The two are one exchange, and no verb repeats
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
      // A build that does not know this row kind renders nothing instead of
      // guessing. The session log may carry events newer than this surface.
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
 * streamed line of the answer below it until only its footer was left, which
 * looked like the surface shredding the output. Dropped whole, it leaves once.
 * Nothing is lost either way. The transcript already holds each section's
 * committed row.
 *
 * A plain tail window also cuts a long block below its head, and what is
 * left is continuation lines at the output column with nothing saying
 * whether they are reasoning or a tool's output. Restoring the verb column's
 * mark — an action's {@link CONNECTOR} — onto the first surviving line costs
 * no row and keeps the section hanging from something for as long as any of
 * it is on screen.
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
    // The cut falls inside the section, below its opening blank, which stays.
    // without it the section's first surviving line runs into whatever the
    // transcript printed last.
    const blank = opens(start) && budget > 1
    const room = budget - Number(blank)
    const floor = start + Number(blank)
    let used = 0
    let from = lines.length
    while (from > floor && used + rows[from - 1]! <= room) used += rows[--from]!
    // Prose fills the window exactly, its oldest line clipped from the top the
    // way a terminal scrolls. Stopping at whole lines would leave the window a
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

/** A row that opens a section. Nothing in it, at the rail. */
export const isBlank = (line: PresentedLine): boolean =>
  line.text === '' && line.verb === '' && line.column === COLUMN.rail && line.marker === MARKER.none && line.divider !== true

/**
 * Cut `lines` at `from`, restoring the verb-column mark the cut removed.
 * @param lines - every line, in order.
 * @param from - index of the first line kept.
 * @returns the kept lines, the first of them named.
 */
function named(lines: readonly PresentedLine[], from: number): readonly PresentedLine[] {
  const shown = lines.slice(from)
  const first = shown[0]
  if (first === undefined || first.verb !== '' || first.column !== COLUMN.output) return shown
  for (let index = from - 1; index >= 0; index--) {
    const line = lines[index]!
    // A line outside the output column ends the section, so there is no verb
    // above this one to restore.
    if (line.column !== COLUMN.output) break
    if (line.verb !== '') return [{ ...first, verb: line.verb, ...line.verbTone === undefined ? {} : { verbTone: line.verbTone } }, ...shown.slice(1)]
  }
  return shown
}
