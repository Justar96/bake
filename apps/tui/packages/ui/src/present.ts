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
import { clipCells, outputLines, outputSpans, toolText } from './tool-output.ts'
import { callIcon, iconFor, ICON } from './icons.ts'
import { COLUMN, MARKER, PAST, TREE, VERB, type Verb } from './layout.ts'
import { PALETTE, type PaletteColor } from './palette.ts'
import { formatAttachment, type CardChanges, type CardFileChange, type CardLine, type JobDoneRow, type Row, type ToolCallRow, type ToolOutcome } from './rows.ts'

/** How a line is emphasized. Colour is chosen by the component layer. */
export type Tone =
  /** The user's own words. */
  | 'said'
  /** Anything else the user is waiting to read, at the terminal's own foreground. */
  | 'plain'
  /** The answer's prose: a step below `plain`, so its headings and bold words stand out. */
  | 'body'
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
  /** Work that was stopped before it finished, neither done nor failed. */
  | 'waiting'
  /** A line a change introduced. */
  | 'added'
  /** A line a change took away. */
  | 'removed'
  /** Where history was compacted. */
  | 'compacting'

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
    case 'body': return { color: PALETTE.body, dim: false, bold: false }
    case 'quiet': return { dim: true, bold: false }
    // Dim, like metadata, and italic, so reasoning stays distinct from tool
    // output in the same column.
    case 'thought': return { dim: true, bold: false, italic: true }
    case 'strong': return { dim: false, bold: true }
    case 'done': return { color: PALETTE.done, dim: false, bold: true }
    case 'failed': return { color: PALETTE.failed, dim: false, bold: false }
    case 'asking': return { color: PALETTE.asking, dim: false, bold: false }
    case 'waiting': return { color: PALETTE.waiting, dim: false, bold: false }
    // Both sides of a diff keep full weight. Dimming the removed side would
    // make a deletion look like supporting detail.
    case 'added': return { color: PALETTE.done, dim: false, bold: false }
    case 'removed': return { color: PALETTE.failed, dim: false, bold: false }
    // The blue the header wears while compaction runs, left where it happened.
    case 'compacting': return { color: PALETTE.compacting, dim: false, bold: false }
    default: return { dim: false, bold: false }
  }
}

/** One display line, already placed in its columns. */
export interface PresentedLine {
  /** Source offset of a Markdown table row, stable across responsive layouts. */
  readonly tableRow?: number
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
   * A call's own marker, drawn after the rail where a step's tree has taken
   * the rail from it. It keeps the state a lone call's marker shows:
   * blinking while the call runs, then its outcome's colour. Wrapped rows
   * hang past it, so the call's text keeps one edge.
   */
  readonly badge?: Badge
  /**
   * A quiet tree glyph drawn after the rail, before any badge, for a call
   * nested one level inside another, as a script's calls are. The rail is
   * left to the block the line belongs to. An indented line gives the
   * glyph's cells from its verb column, so its text keeps the output column.
   */
  readonly branch?: string
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

/** A call's marker moved out of the rail; see {@link PresentedLine.badge}. */
export interface Badge {
  readonly glyph: string
  readonly tone: Tone
  readonly pulse?: boolean
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

/** Verbs of the actions that have an icon of their own; see `ICON`. */
const FAMILY_OF_ICON: ReadonlyMap<string, Verb> = new Map([[ICON.spawn, VERB.spawn], [ICON.send, VERB.send], [ICON.skill, VERB.load]])

/** The verb a tool's name implies, or undefined when it names none of the families. */
function familyOf(tool: string): Verb | undefined {
  const name = tool.toLowerCase()
  // Checked before `write`. Rewriting a plan is not an edit to the workspace.
  if (name.includes('todo') || name.includes('plan')) return VERB.plan
  // A tool with an icon of its own names its verb by the same words, so a
  // step of delegations counts `spawn 3`, not `run 3`.
  const family = FAMILY_OF_ICON.get(iconFor(tool))
  if (family !== undefined) return family
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
 * output and edits, as do a delegation's, a message's, and a skill's
 * answers; plan updates retain their dedicated presentation.
 */
const PREVIEWED: ReadonlySet<Verb> = new Set([VERB.run, VERB.edit, VERB.read, VERB.find, VERB.fetch, VERB.spawn, VERB.send, VERB.load])

/**
 * Cells one line of a result without a card may take before it is cut.
 *
 * The preview bound counts lines, and a tool that answers in one line of
 * compact JSON draws a screenful under a preview of four. Three rows of an
 * 80-column terminal still read as a sentence or two of a prose answer.
 */
export const RAW_LINE_CELLS = 240

/**
 * Lines of a result's model-facing text, for a tool that declared no card.
 *
 * Each successful line is cut at {@link RAW_LINE_CELLS} with an ellipsis; the
 * whole text is in the session log. A failure keeps its whole lines, because
 * the failure is what the reader has to read.
 *
 * @param text - the result text, empty when a card replaced it.
 * @param failed - whether the call failed.
 * @returns the card lines to preview.
 */
function rawLines(text: string, failed: boolean): readonly CardLine[] {
  const lines = outputLines(text)
  return failed ? lines : lines.map(line => ({ ...line, text: clipCells(line.text, RAW_LINE_CELLS) }))
}

/**
 * Cells at the output column, given the cells at the rail.
 * @param measure - cells from the rail to the right edge, when known.
 * @returns cells from the output column, or undefined when unknown.
 */
const outputCells = (measure: number | undefined): number | undefined =>
  measure === undefined ? undefined : Math.max(1, measure - (COLUMN.output - COLUMN.rail))

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

/** A block's lines without the blank {@link opening} may have put before them. */
const withoutOpening = (lines: readonly PresentedLine[]): readonly PresentedLine[] =>
  lines[0] === BLANK ? lines.slice(1) : lines

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
 * `bash` becomes `Bash`; `read_file` becomes `ReadFile`. Code mode is named
 * by the terminal's `Script` label rather than its transport name.
 * @param tool - tool name from the session log.
 * @param script - locale-owned name for {@link SCRIPT_TOOL}; absent, it is named like any tool.
 * @returns the display name, or `tool` itself when it contains no words.
 */
export function toolLabel(tool: string, script?: string): string {
  if (tool === SCRIPT_TOOL && script !== undefined) return script
  const label = tool.split(/[^\p{L}\p{N}]+/u).filter(word => word !== '')
    .map(word => word[0]!.toUpperCase() + word.slice(1)).join('')
  return label === '' ? tool : label
}

/** Transport name of code mode, whose call carries a program and its nested calls. */
export const SCRIPT_TOOL = 'run_code'

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
 * While it waits for its result, the newest lines of its live output hang
 * under it, and a call that has finished but waits for an earlier one to
 * commit stops blinking and takes its outcome's colour. The logged result
 * replaces both.
 *
 * A script's head also counts the calls it made, and its failed ones once it
 * has finished, as a step's head does.
 *
 * @param row - the call, including its outcome when one exists.
 * @param bound - how much of the outcome to preview.
 * @param cells - cells a live output line may take, absent for {@link RAW_LINE_CELLS}.
 * @param dispatched - the script's calls as drawn; absent, every call {@link nestedLines} keeps.
 * @returns the block's lines, without the opening blank.
 */
function action(
  row: ToolCallRow, bound: ResultBound, cells?: number,
  dispatched: readonly PresentedLine[] = nestedLines(row.dispatches ?? [], bound, cells),
): readonly PresentedLine[] {
  const family = familyOf(row.tool)
  const verb = family ?? VERB.run
  const outcome = row.result
  const live = outcome === undefined ? row.live : undefined
  const [first = '', ...rest] = linesOf(row.input)
  const title = bare(first, verb)
  // Arguments still streaming. Show `Name(...)` until the full argument arrives.
  const name = toolLabel(row.tool, bound.script)
  const text = title === '' ? name : `${name}(${title})`
  const after = outcome === undefined ? undefined : outcomeLines(outcome, verb, bound, title)
  // A count with no body rides on the head, so a read is one row. A change's
  // size does the same, so an edit reports how large it was.
  const named: Styled = { text, spans: [{ length: name.length, tone: 'strong' },
    ...text.length === name.length ? [] : [{ length: text.length - name.length, tone: 'plain' as const }]] }
  // Work left running says so beside the call, ahead of what the start reported.
  const tagged = row.background !== true || bound.background === undefined ? named
    : beside(named, { text: bound.background, spans: [{ length: bound.background.length, tone: 'quiet' }] })
  const tallied = row.dispatches === undefined ? undefined : callTally(row, bound)
  const counted = tallied === undefined ? tagged : beside(tagged, tallied)
  const inline = after?.inline === undefined ? counted : beside(counted, after.inline)
  const head: PresentedLine = {
    marker: callIcon(row.tool, row.background === true),
    markerTone: stateTone(row),
    ...running(row) ? { pulse: true } : {},
    verb: '', text: inline.text, column: COLUMN.rail, wide: true, tone: 'plain',
    ...inline.spans.length === 0 ? {} : { spans: inline.spans },
  }
  // Supporting call detail stays quiet; source keeps literal whitespace and
  // full brightness, with syntax colour only on the lines the preview draws.
  // Detail and extra input lines share the output bound. A script the model wrote can be as
  // long as a file, and it is printed under the head on every call. At least
  // one line of each is always shown, so a description survives a bound that
  // collapses the output.
  const limit = Math.max(1, bound.lines)
  const described = drawnBody([row.detail], false, bound.code, plain => excerpt(plain, limit, bound, 'quiet', false).lines)
    .map(line => line.tone === 'plain' && line.literal !== true ? { ...line, tone: 'quiet' as const } : line)
  const body = [
    ...excerpt(rest.map(text => continuation(text, 'plain')), limit, bound, 'plain', false).lines,
    ...excerpt(described, limit, bound, 'quiet', false).lines,
    ...liveTail(live?.tail, bound, cells),
    ...dispatched,
    ...row.tool === SCRIPT_TOOL && bound.scriptOutput !== undefined && (after?.lines.length ?? 0) > 0
      ? [continuation(outcome?.ok === false ? bound.scriptError ?? bound.scriptOutput : bound.scriptOutput, 'quiet')] : [],
    ...after?.lines ?? []]
  return [head, ...connected(body)]
}

/**
 * How many calls a script made, and how many failed once it has finished, as
 * in `13 calls · 1 failed`. While the script runs, a call that failed is
 * already red on its own row, and the count follows its step's head in
 * waiting for the end.
 * @param row - a script call with its nested calls.
 * @param bound - the locale's nouns for a count of calls and of failures.
 * @returns the tally for the head, or nothing without the nouns or the calls.
 */
function callTally(row: ToolCallRow, bound: ResultBound): Styled | undefined {
  const calls = row.dispatches ?? []
  if (calls.length === 0 || bound.calls === undefined) return undefined
  const count = `${calls.length} ${calls.length === 1 ? bound.call ?? bound.calls : bound.calls}`
  const failed = running(row) || bound.failures === undefined ? 0 : calls.filter(call => stateTone(call) === 'failed').length
  const failures = failed === 0 ? '' : ` \u00b7 ${failed} ${bound.failures}`
  return { text: count + failures, spans: [{ length: count.length, tone: 'quiet' },
    ...failures === '' ? [] : [{ length: failures.length, tone: 'failed' as const }]] }
}

/**
 * The end of background work, headed as the call that started it so the two
 * read as one job: `◌ Bash(npm run dev)  bash-1 finished · exit code: 0`.
 * Its marker is the job's outcome, as a finished call's is: green, red when
 * the work failed, and the waiting yellow when it was stopped.
 * @param row - the job's completion.
 * @param bound - the locale's name for code mode.
 * @returns the one line it prints as.
 */
function jobHead(row: JobDoneRow, bound: ResultBound): PresentedLine {
  const name = toolLabel(row.tool, bound.script)
  const call = `${name}(${row.label})`
  const status = row.id === undefined ? row.status : `${row.id} ${row.status}`
  const tone: Tone = row.outcome === 'failed' ? 'failed' : 'quiet'
  return {
    marker: callIcon(row.tool, true),
    markerTone: row.outcome === 'done' ? 'done' : row.outcome === 'failed' ? 'failed' : 'waiting',
    verb: '', text: `${call}  ${status}`, column: COLUMN.rail, wide: true, tone: 'plain',
    spans: [{ length: name.length, tone: 'strong' }, { length: call.length - name.length, tone: 'plain' }, { length: status.length + 2, tone }],
  }
}

/** Calls a printed script keeps at each end, around the count its middle folds into. */
const NESTED_ENDS = 2

/** Failed calls a printed script keeps from its folded middle, where they are the news. */
const NESTED_FAILURES = 3

/**
 * A script's calls as its block prints them, hung from it one level in.
 *
 * A script can loop over every file in a workspace, and each call it makes
 * would otherwise print its own block. The first {@link NESTED_ENDS} and the
 * last are kept, with what the middle held as one `+N more calls` branch, so
 * the reader sees how the program started and how it ended. Calls that
 * failed or never finished are news, and up to {@link NESTED_FAILURES} of
 * them stay where they were. A count that would stand for one call costs the
 * row it saves, so that call is drawn instead. Each call is in the session log.
 *
 * @param calls - the script's calls, in the order it made them.
 * @param bound - how much of each outcome to preview, and the words for the count.
 * @param cells - cells a running call's live output line may take.
 * @returns the calls' lines, possibly empty.
 */
function nestedLines(calls: readonly ToolCallRow[], bound: ResultBound, cells?: number): readonly PresentedLine[] {
  const ends = calls.length - NESTED_ENDS
  let news = 0
  const kept = calls.map((call, index) => index < NESTED_ENDS || index >= ends || bound.moreCalls === undefined
    || (stateTone(call) !== 'done' && news++ < NESTED_FAILURES))
  // A lone folded call is drawn instead of a count of one.
  const shown = kept.map((keep, index) => keep || (kept[index - 1] !== false && kept[index + 1] !== false))
  const entries: (ToolCallRow | number)[] = []
  for (const [index, call] of calls.entries()) {
    if (shown[index]) entries.push(call)
    else if (typeof entries.at(-1) === 'number') entries[entries.length - 1] = (entries.at(-1) as number) + 1
    else entries.push(1)
  }
  return entries.flatMap((entry, index) => {
    const last = index === entries.length - 1
    return typeof entry === 'number' ? [foldedCalls(`+${entry} ${bound.moreCalls}`, last)] : nestedCall(entry, bound, last, cells)
  })
}

/**
 * One of a script's calls, on a branch of the tree one level in.
 *
 * It reads as a call in a step does: its own marker as a badge past the
 * branch, blinking while it runs, then green or red, and a failed call's
 * head red as well. A call that succeeded folds to its head, a size beside
 * it; the script's own result is what it worked toward. A failed call keeps
 * its error under it, and a running one its newest output.
 *
 * @param call - the nested call.
 * @param bound - how much of a failure or live output to preview.
 * @param last - whether it closes the tree, which takes the corner.
 * @param cells - cells a live output line may take.
 * @returns its head, then any lines hung from it.
 */
function nestedCall(call: ToolCallRow, bound: ResultBound, last: boolean, cells?: number): readonly PresentedLine[] {
  const folded = stateTone(call) === 'done'
  const [head, ...body] = action(call, folded ? { ...bound, lines: 0 } : bound, cells, [])
  return [{
    ...head!.markerTone === 'failed' ? failedHead(head!) : head!,
    marker: MARKER.none, markerTone: 'quiet', pulse: false, branch: last ? TREE.corner : TREE.branch,
    badge: { glyph: head!.marker, tone: head!.markerTone ?? 'strong', ...head!.pulse === true ? { pulse: true } : {} },
  }, ...folded ? [] : body.map(line => ({ ...line, branch: last ? MARKER.none : TREE.stem }))]
}

/**
 * A count of a script's calls left out, as a branch of its tree.
 * @param text - the count, as in `+9 more calls`.
 * @param last - whether it closes the tree.
 * @returns the line.
 */
const foldedCalls = (text: string, last: boolean): PresentedLine => ({
  marker: MARKER.none, branch: last ? TREE.corner : TREE.branch, verb: '', text, column: COLUMN.rail, tone: 'quiet',
})

/**
 * Keep the script head visible while source and older nested calls yield to a short live window.
 * @param row - a script call with its logged nested activity.
 * @param bound - source and result preview policy.
 * @param rows - physical rows available; a head taller than this is retained for the renderer to clip.
 * @param height - physical height of each line, including wrapping.
 * @param cells - width of live output, absent for the ordinary output cap.
 * @returns fitted lines, prioritizing the script head and newest nested call.
 */
export function fittedAction(
  row: ToolCallRow, bound: ResultBound, rows: number,
  height: (line: PresentedLine) => number = () => 1, cells?: number,
): readonly PresentedLine[] {
  if (rows <= 0) return []
  const calls = row.dispatches ?? []
  // Every dispatch needs at least a head. Window before formatting, so fitting
  // a long program cannot repeatedly walk its whole dispatch history.
  const skipped = Math.max(0, calls.length - Math.floor(rows))
  // The oldest `hidden` calls fold into one branch opening the tree, which never closes it.
  const earlier = (hidden: number): readonly PresentedLine[] =>
    hidden > 0 ? [foldedCalls(`+${hidden} ${bound.earlier ?? bound.more}`, false)] : []
  // The head still counts every call; only the tree under it is windowed.
  const format = (call: ToolCallRow, hidden: number, preview: ResultBound): readonly PresentedLine[] => action(call, preview, cells,
    [...earlier(hidden), ...calls.slice(hidden).flatMap((nested, index) => nestedCall(nested, preview, hidden + index === calls.length - 1, cells))])
  const size = (lines: readonly PresentedLine[]): number => lines.reduce((sum, line) => sum + height(line), 0)
  const blank = height(BLANK)
  const whole = opening(format(row, skipped, bound))
  if (size(whole) <= rows) return whole
  const { detail: _detail, ...withoutSource } = row
  const compact = { ...bound, lines: 0 }
  const least = format(withoutSource, skipped, compact)
  let used = size(least)
  if (used + blank <= rows) return opening(least)
  // Nested blocks are independent, and the tree's glyphs never change a
  // line's width, so hiding one more subtracts exactly its own rows. Measure each once, and
  // format only the layout that fits.
  const nested = calls.map((call, index) => index < skipped ? [] : nestedCall(call, compact, index === calls.length - 1, cells))
  used -= size(earlier(skipped))
  for (let hidden = skipped + 1; hidden < calls.length; hidden++) {
    used -= size(nested[hidden - 1]!)
    const total = used + size(earlier(hidden))
    if (total + blank <= rows) return opening(format(withoutSource, hidden, compact))
    if (total <= rows) return format(withoutSource, hidden, compact)
  }
  // Last, the head gives up its count of calls, which can wrap it past the window.
  const head = least[0]!
  const { detail: _source, dispatches: _calls, ...alone } = row
  const plain = action(alone, compact, cells, [])[0]!
  const newest = nested.at(-1)?.[0]
  const ladder = [...newest === undefined ? [] : [[head, newest], [plain, newest]], [head]]
  return ladder.find(lines => size(lines) <= rows) ?? [plain]
}

/**
 * Whether a call is still running: no result, and no word that it finished.
 * @param call - the call.
 * @returns whether its marker blinks.
 */
const running = (call: ToolCallRow): boolean => call.result === undefined && call.live?.finished === undefined

/**
 * Whether a call has an outcome to show, logged or reported live.
 * @param call - the call.
 * @returns whether it may fold to its head like a finished call.
 */
const settled = (call: ToolCallRow): boolean => !running(call)

/**
 * A call's marker tone: its outcome's colour once it has one, logged or live.
 * @param call - the call.
 * @returns `strong` while it runs, else `done` or `failed`.
 */
function stateTone(call: ToolCallRow): Tone {
  const ok = call.result?.ok ?? call.live?.finished?.ok
  return ok === undefined ? 'strong' : ok ? 'done' : 'failed'
}

/**
 * A running call's newest output lines, under its head in the output grey.
 *
 * The same bound as a result's preview, counted from the end, so the window
 * holds still while new lines replace old ones. Each line is cut to one row
 * at the output width rather than wrapped, so a long line cannot make the
 * block grow and shrink as the tail moves.
 *
 * @param tail - the call's newest lines, oldest first.
 * @param bound - the result preview's bound; zero shows none.
 * @param cells - cells a line may take, absent for {@link RAW_LINE_CELLS}.
 * @returns the lines, possibly empty.
 */
function liveTail(tail: readonly string[] | undefined, bound: ResultBound, cells: number | undefined): readonly PresentedLine[] {
  if (tail === undefined || bound.lines <= 0) return []
  const width = Math.max(1, Math.min(cells ?? RAW_LINE_CELLS, RAW_LINE_CELLS))
  return zoned(tail.slice(-bound.lines).map(text => continuation(clipCells(toolText(text), width), 'plain')))
}

/**
 * Hang a block's body from its head. The first line takes the connector in
 * its verb column, unless that column already holds a changed line's number.
 * @param body - the lines under a head.
 * @returns the same lines, the first connected.
 */
function connected(body: readonly PresentedLine[]): readonly PresentedLine[] {
  const [first, ...rest] = body
  // A script's tree hangs from its head by its own branches.
  if (first === undefined || first.verb !== '' || first.gutter !== undefined || first.branch !== undefined) return body
  return [{ ...first, verb: CONNECTOR, verbTone: 'quiet' }, ...rest]
}

/**
 * Render one step's calls as one block. A head counts them, then each
 * call hangs from it, joined by the tree in the rail.
 *
 * Each call's branch takes the rail, and the call's own marker moves just
 * past it as a badge, so every call of a batch still says whether it is
 * running, finished, or failed, and what kind of call it is. The tree is
 * structure, so every branch, stem, and connector is quiet and holds still;
 * the badge is what blinks, and a failed call's head turns red as well.
 * The head's marker is the step's state. It is running while any call is
 * running, red when one failed. Calls are not separated by a blank row.
 * They were one model decision, and the tree stem is what shows that.
 *
 * @param calls - the step's calls, in the order the model made them.
 * @param bound - how much of each outcome to preview.
 * @returns the block's lines, without the opening blank.
 */
function group(calls: readonly ToolCallRow[], bound: ResultBound, cells?: number): readonly PresentedLine[] {
  return [groupHead(calls, bound), ...hang(calls.map(call => action(call, bound, cells)))]
}

/**
 * The head of a step's block. Its calls are counted by verb, in the step's tense.
 * @param calls - the step's calls, in the order the model made them.
 * @param bound - the locale's words for failures.
 * @returns the head line, its marker the step's state.
 */
function groupHead(calls: readonly ToolCallRow[], bound: ResultBound): PresentedLine {
  const active = calls.some(running)
  const failed = calls.filter(call => stateTone(call) === 'failed').length
  // Count verbs in the order they first appear, in the step's tense.
  const counts = new Map<Verb, number>()
  for (const call of calls) counts.set(verbFor(call.tool), (counts.get(verbFor(call.tool)) ?? 0) + 1)
  const tally = [...counts].map(([verb, count]) => `${active ? verb : PAST[verb]} ${count}`).join(' \u00b7 ')
  const failures = failed === 0 || active || bound.failures === undefined ? undefined : ` \u00b7 ${failed} ${bound.failures}`
  // One kind of call throughout takes that kind's icon. A mix is just an action.
  const icons = new Set(calls.map(call => iconFor(call.tool)))
  return {
    marker: icons.size === 1 ? [...icons][0]! : ICON.other,
    markerTone: active ? 'strong' : failed > 0 ? 'failed' : 'done',
    ...active ? { pulse: true } : {},
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
    // A blinking branch would open a gap in the tree, so the tree holds still
    // and is quiet throughout. The call's marker, state and all, becomes its
    // badge. A line drawn without a marker, such as the `+N earlier` summary,
    // takes none.
    return lines.map((line, row) => row === 0
      ? { ...line.markerTone === 'failed' ? failedHead(line) : line, marker: last ? TREE.corner : TREE.branch, markerTone: 'quiet' as const, pulse: false,
        ...line.marker === MARKER.none ? {} : { badge: { glyph: line.marker, tone: line.markerTone ?? 'strong', ...line.pulse === true ? { pulse: true } : {} } } }
      : { ...line, marker: last ? MARKER.none : TREE.stem, markerTone: 'quiet' as const })
  })
}

/**
 * A failed call's head line in the failed tone, its tool's name still bold.
 *
 * On its own a call says it failed with its marker. In a step's block the
 * branch takes the marker's place and stays quiet, so the head carries the
 * failure instead; its output may be folded away, or empty.
 *
 * @param line - the call's head line.
 * @returns the same line, its plain and strong runs failed.
 */
function failedHead(line: PresentedLine): PresentedLine {
  return {
    ...line, tone: 'failed',
    ...line.spans === undefined ? {} : { spans: line.spans.map(span => span.tone === 'strong' ? { ...span, tone: 'failed' as const, bold: true }
      : span.tone === 'plain' ? { ...span, tone: 'failed' as const } : span) },
  }
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
 * @param cells - cells a running call's live output line may take.
 * @returns the block's lines, at most `rows` tall unless even the head alone
 *   wraps past them; the head is always the first line or the one after the
 *   opening blank.
 */
export function fittedGroup(
  calls: readonly ToolCallRow[],
  bound: ResultBound,
  rows: number,
  height: (line: PresentedLine) => number = () => 1,
  cells?: number,
): readonly PresentedLine[] {
  const head = groupHead(calls, bound)
  const size = (lines: readonly PresentedLine[]): number => lines.reduce((sum, line) => sum + height(line), 0)
  const plain = calls.map(call => rows > 0 && call.tool === SCRIPT_TOOL ? undefined : action(call, bound, cells))
  // A script windows its nested calls into the rows the step's other heads
  // leave it, as it does on its own, so its history is not formatted only to fold.
  const room = rows - height(BLANK) - height(head) - plain.reduce((sum, lines) => sum + (lines === undefined ? 0 : height(lines[0]!)), 0)
  const bodies = plain.map((lines, index) => lines ?? withoutOpening(fittedAction(calls[index]!, bound, Math.max(1, room), height, cells)))
  const whole = opening([head, ...hang(bodies)])
  if (rows <= 0 || size(whole) <= rows) return whole
  // Heights ignore the rail's marker, which never changes a line's width, so
  // each call is measured once however it ends up folded.
  const full = bodies.map(size)
  const folded = bodies.map((lines, index) => settled(calls[index]!) ? height(lines[0]!) : full[index]!)
  const fixed = height(BLANK) + height(head)
  const fold = (lines: readonly PresentedLine[], index: number, count: number): readonly PresentedLine[] =>
    index < count && settled(calls[index]!) ? lines.slice(0, 1) : lines
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
 * The files a command changed follow its output as a section of their own,
 * their size on the head before the exit status.
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
  const changes = outcome.changes
  const touched = changes === undefined ? undefined : changedSize(changes, bound)
  // A failure is always news, and a diff is what an edit did.
  const previewed = failed || PREVIEWED.has(verb) || stat !== undefined
  const body = drawnBody([rawLines(outcome.text, failed), card.filter(line => line.summary === undefined)], failed, bound.code,
    plain => previewed ? excerpt(plain, bound.lines, bound, tone, failed).lines : [])
  const { lines: shown, hidden } = previewed ? excerpt(body, bound.lines, bound, tone, failed) : { lines: [], hidden: body.length }
  // Previewed output counts what it left out below it; a count alone says how
  // much there was beside the headline. A change's size, or the card's own
  // count, says it either way, and so does what a command changed.
  const size = stat !== undefined || touched !== undefined || summaries.length > 0 || shown.length > 0 || body.length === 0 || hidden === 0
    ? undefined : countOf(body.length, bound)
  const summary = summaries.length === 0 ? undefined
    : joined(summaries.map(line => [line.text, line.summary === 'failure' || failed ? 'failed' : 'quiet'] as const))
  const headline = outcome.title === undefined || sameWords(outcome.title, title, verb) ? undefined : outcome.title
  const inline = [stat, touched, summary,
    headline === undefined && !failed && size !== undefined ? quietly(size) : undefined]
    .filter((part): part is Styled => part !== undefined)
    .reduce<Styled | undefined>((all, part) => all === undefined ? part : beside(all, part), undefined)
  const heading = headline === undefined && !failed ? undefined : joined([[headline, tone], [size, failed ? tone : 'quiet']])
  return {
    ...inline === undefined ? {} : { inline },
    lines: zoned([
      ...heading === undefined || heading.text === '' ? [] : [{ ...continuation(heading.text, tone), spans: heading.spans }],
      ...shown,
      ...changes === undefined ? [] : changeSection(changes, bound),
    ]),
  }
}

/** Text in the quiet tone, as a head's metadata is. */
const quietly = (text: string): Styled => ({ text, spans: [{ length: text.length, tone: 'quiet' }] })

/**
 * What a command changed, as its head reports it.
 *
 * The size counts every file the producer measured, including those whose
 * lines it did not send. Once the bound collapses the section, the number of
 * files joins it, since no path line is left to show there were several.
 *
 * @param changes - the files the command changed.
 * @param bound - the preview bound and the noun for a count of files.
 * @returns `+N −M`, then the file count when collapsed, or undefined when neither applies.
 */
function changedSize(changes: CardChanges, bound: ResultBound): Styled | undefined {
  const size = sizeOf(changes.files.reduce((sum, file) => sum + file.added, 0), changes.files.reduce((sum, file) => sum + file.removed, 0))
  const count = changes.files.length + (changes.omitted ?? 0)
  const files = bound.lines > 0 || count < 2 || bound.files === undefined ? undefined : quietly(`${count} ${bound.files}`)
  return size === undefined ? files : files === undefined ? size : beside(size, files)
}

/**
 * The files a command changed, as a section under its output.
 *
 * It is bounded apart from the output, by the same number of lines. A
 * file's changed lines count against it, as an edit's do, and a file with
 * none to draw counts its path line. Files are drawn while some of the
 * bound is left; a file that is drawn always keeps its path line, under
 * `edited` in the verb column. The files left out, by the bound or by the
 * producer, are counted, and the section's caveats close it.
 *
 * The section keeps its own tones whatever the command's exit status. A
 * failed command still changed what it changed.
 *
 * @param changes - the files the command changed.
 * @param bound - how many changed lines to draw, and the words for the rest.
 * @returns the section's lines, none at a bound of zero.
 */
function changeSection(changes: CardChanges, bound: ResultBound): readonly PresentedLine[] {
  if (bound.lines <= 0) return []
  const lines: PresentedLine[] = []
  let budget = bound.lines
  let drawn = 0
  for (const file of changes.files) {
    if (budget <= 0) break
    const body = drawnBody([file.lines], false, bound.code, plain => excerpt(plain, budget, bound, 'plain', false).lines)
    const { lines: shown } = excerpt(body, budget, bound, 'plain', false)
    const counted = shown.filter(isChange).length
    lines.push(fileHead(file, counted === 0), ...shown)
    budget -= Math.max(1, counted)
    drawn++
  }
  const omitted = changes.omitted ?? 0
  // A count standing for one file costs the row that file's own path line takes.
  const last = drawn === changes.files.length - 1 && omitted === 0 ? changes.files.at(-1) : undefined
  if (last !== undefined) lines.push(fileHead(last, true))
  const hidden = changes.files.length - drawn - (last === undefined ? 0 : 1) + omitted
  const more = hidden === 1 ? bound.moreFile ?? bound.moreFiles : bound.moreFiles
  return [
    ...lines,
    ...hidden <= 0 ? [] : [continuation(more === undefined ? `+${hidden}` : `+${hidden} ${more}`, 'quiet')],
    ...(changes.notes ?? []).map(note => continuation(note, 'quiet')),
  ]
}

/**
 * A changed file's path line: `edited` in the verb column, then the path,
 * how it changed when that was not a plain edit, and, when none of its
 * lines are drawn, how many it added and removed.
 *
 * @param file - the file.
 * @param sized - whether the line carries the file's size.
 * @returns the line, in the path's own tone whatever the command's exit.
 */
function fileHead(file: CardFileChange, sized: boolean): PresentedLine {
  const path: Styled = { text: file.path, spans: [{ length: file.path.length, tone: 'plain', color: PALETTE.reference }] }
  const head = [file.status === undefined ? undefined : quietly(file.status), sized ? sizeOf(file.added, file.removed) : undefined]
    .reduce<Styled>((all, part) => part === undefined ? all : beside(all, part), path)
  return { marker: MARKER.none, verb: PAST[VERB.edit], verbTone: 'strong', text: head.text, column: COLUMN.output, tone: 'plain', spans: head.spans }
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
  return sizeOf((detail ?? []).filter(line => line.emphasis === 'added').length, (detail ?? []).filter(line => line.emphasis === 'removed').length)
}

/**
 * `+2 −1` in each side's tone.
 * @param added - lines added.
 * @param removed - lines removed.
 * @returns the size, leaving out a side that is zero, or undefined when both are.
 */
function sizeOf(added: number, removed: number): Styled | undefined {
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
  /** Locale-owned name for a code-mode call, as in `Script`; absent, it is labelled like any tool. */
  readonly script?: string
  /** Locale-owned label above a script's own result, as in `Script output`; absent, the result hangs unlabelled. */
  readonly scriptOutput?: string
  /** Locale-owned label above a failed script's error, as in `Script error`; absent, {@link ResultBound.scriptOutput} serves. */
  readonly scriptError?: string
  /** Locale-owned noun for a script's calls, as in `13 calls`; absent, a script's head does not count them. */
  readonly calls?: string
  /** Locale-owned noun for one call, as in `1 call`; absent, {@link ResultBound.calls} serves. */
  readonly call?: string
  /**
   * Locale-owned phrase for a printed script's calls folded between its first
   * and last, as in `+9 more calls`; absent, every call is printed.
   */
  readonly moreCalls?: string
  /** Locale-owned noun for a count of the files a command changed, as in `3 files`; absent, a collapsed result leaves the count out. */
  readonly files?: string
  /** Locale-owned phrase for changed files a bound left out, as in `+2 more files`; absent, the count is drawn alone. */
  readonly moreFiles?: string
  /** Locale-owned phrase for one changed file left out, as in `+1 more file`; absent, {@link ResultBound.moreFiles} serves. */
  readonly moreFile?: string
  /** Locale-owned tag on a call that started background work, as in `background`; absent, only its icon says so. */
  readonly background?: string
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
 * @param width - available prose cells for responsive Markdown tables.
 * @returns display lines in order, possibly empty.
 */
export function present(row: Row, result: ResultBound, wrap?: (line: PresentedLine) => readonly string[], width?: number): readonly PresentedLine[] {
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
      const lines = markdownLines(row.text, 'body', result.code, width).map(line => ({
        ...line,
        marker: MARKER.none,
        verb: '', column: COLUMN.rail, tone: 'body' as const,
      }))
      return row.continued === true ? lines : opening(lines)
    }

    case 'rate':
      // Nothing. A row of its own under the answer split one turn's numbers
      // across two rows; the ended turn's summary on the header reports it.
      return []

    case 'reasoning': {
      // A paragraph at the rail, as the answer is, without a verb. Dim and
      // italic, it is the working-out. The blank that opens the answer, at
      // full brightness, marks where the reply begins.
      const lines = markdownLines(row.text, 'thought', result.code, width).map(line => ({
        ...line, marker: MARKER.none, verb: '', column: COLUMN.rail, tone: 'thought' as const, prose: true,
      }))
      const kept = wrap === undefined ? lines : reasoningPreview(lines, result, wrap)
      return row.continued === true ? kept : opening(kept)
    }

    case 'tool-call':
      return opening(action(row, result, outputCells(width)))

    case 'tool-group':
      return opening(group(row.calls, result, outputCells(width)))

    case 'job-done':
      return opening([jobHead(row, result)])

    case 'tool-result': {
      const tone: Tone = row.ok ? 'plain' : 'failed'
      // A card that reformats the result leaves `text` empty, and a result
      // shown raw leaves `detail` absent, so exactly one carries the body.
      const body = drawnBody([rawLines(row.text, !row.ok), row.detail], !row.ok, result.code, plain => preview(plain, result.lines).shown)
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
      return [head, ...zoned([...shown, ...hidden === 0 || shown.length === 0 ? [] : [continuation(`+${hidden} ${result.more}`, row.ok ? 'quiet' : tone)],
        ...row.changes === undefined ? [] : changeSection(row.changes, result)])]
    }

    case 'notice': {
      const tone: Tone = row.tone === 'error' ? 'failed' : row.compaction === true ? 'compacting' : 'quiet'
      // A completed turn says so on the summary row above the input instead,
      // with its time and what it did; a line here as well repeated it.
      if (row.placement === 'turn-end' && row.tone === 'info') return []
      if (row.placement === 'turn-end') return opening(linesOf(row.text).map((text, index) => ({
        marker: index === 0 ? '-' : MARKER.none, verb: '', text, column: COLUMN.rail, tone,
      })))
      // A command's outcome continues its command, on the branch that closes
      // it. The two are one exchange, and no verb repeats
      // what the command's name already says. The branch is structure and
      // stays quiet; a failed command's text is what turns red.
      if (row.placement === 'command') return linesOf(row.text).map((text, index) => ({
        marker: index === 0 ? TREE.corner : MARKER.none, markerTone: 'quiet' as const,
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

/**
 * A row that opens a section. Nothing in it, at the rail. A thought's own
 * paragraph breaks stay inside its section, so a long one streaming in the
 * live region scrolls by rows instead of dropping whole paragraphs.
 */
export const isBlank = (line: PresentedLine): boolean =>
  line.text === '' && line.verb === '' && line.column === COLUMN.rail && line.marker === MARKER.none && line.divider !== true
    && line.tone !== 'thought'

/**
 * A streaming thought's lines, parsed from only as many of its newest
 * paragraphs as fill `limit` rows.
 *
 * Reasoning never prints while it streams, so the live row holds the whole
 * thought, and parsing and wrapping all of it on every delta costs the length
 * of the thought. The live region draws its newest rows only. Parse the newest
 * paragraphs, and more of them only while they draw no more rows than the
 * window holds, so the window is cut as the whole thought would be.
 *
 * @param row - the newest live row, still streaming.
 * @param result - the surface's result bound, for code highlighting.
 * @param limit - rows the live region may draw.
 * @param height - rows one line occupies once wrapped.
 * @param width - available prose cells for responsive Markdown tables.
 * @returns the lines of the thought's tail, opening blank included.
 */
export function streamingThought(row: Row & { readonly kind: 'reasoning' }, result: ResultBound, limit: number,
  height: (line: PresentedLine) => number, width?: number): readonly PresentedLine[] {
  for (let paragraphs = 1; ; paragraphs *= 2) {
    const start = paragraphStart(row.text, paragraphs)
    const lines = present(start === 0 ? row : { ...row, text: row.text.slice(start) }, result, undefined, width)
    if (start === 0 || lines.reduce((sum, line) => sum + height(line), 0) > limit) return lines
  }
}

/** A line that opens or closes a fenced code block. */
const FENCE = /^ {0,3}(?:`{3,}|~{3,})/gmu

/**
 * Where the `count`th paragraph from the end starts, so the text after it
 * parses as it does within the whole. A paragraph starts after a blank line;
 * a start inside a fenced block moves back to the fence that opened it.
 * @returns 0 when the text has no more paragraphs than that.
 */
function paragraphStart(text: string, count: number): number {
  let start = text.length
  for (let found = 0; found < count; found++) {
    // A blank line at the very start has no paragraph before it.
    const blank = start < 1 ? -1 : text.lastIndexOf('\n\n', start - 1)
    if (blank <= 0) return 0
    start = blank
  }
  start += 2
  const fences = [...text.slice(0, start).matchAll(FENCE)]
  return fences.length % 2 === 0 ? start : fences.at(-1)!.index
}

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
