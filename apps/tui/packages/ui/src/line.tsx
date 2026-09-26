/**
 * Ink components for the layout's columns.
 *
 * Placement is decided in `present.ts` and budgets in `layout.ts`; this layer
 * only turns the result into boxes and chooses colour for each tone. Keeping
 * the arithmetic out of the components is what lets the geometry be tested
 * without a terminal.
 *
 * @module @dsh-tui/ui/line
 */

import React from 'react'
import { Box, Text } from 'ink'
import stringWidth from 'string-width'
import wrapAnsi from 'wrap-ansi'
import { wrapDraft } from './editor.ts'
import { chromeFor, COLUMN, COMPOSER_BUDGET, HINT_MIN_COLUMNS, MARKER, RULE, TREE, windowOf, type Budget, type ChromeLayout, type FrameStyle } from './layout.ts'
import { PALETTE, type PaletteColor } from './palette.ts'
import { fittedGroup, hintFor, isBlank, present, softBreaks, styleOf, tailLines, type ComposerState, type Hint, type LineStyle, type PresentedLine, type ResultBound } from './present.ts'
import type { Row } from './rows.ts'
import { FRAME_MS, formatElapsed, SPINNER_REST, spinnerFrame, type Clock, type Outcome, type TurnSummary } from './activity.ts'
import { useBeat } from './beat.tsx'

/**
 * One display line.
 *
 * Prose and tool output wrap in the columns left after their rail or verb.
 * Neither is truncated. A result the user cannot finish reading is worse than
 * a ragged wrap. The verb and body share a tone so an action label keeps the
 * same emphasis as its text.
 *
 * @param props.line - the placed line.
 * @param props.budget - budgets for the current terminal size.
 */
export function Line({ line, budget, clock }: {
  readonly line: PresentedLine
  readonly budget: Budget
  /** Clock for a blinking marker. Absent, the marker stays lit. */
  readonly clock?: Clock | undefined
}): React.ReactElement {
  const style = styleOf(line.tone)
  const marker = styleOf(line.markerTone ?? line.tone)
  // A verb with its own tone opens an action or reports its outcome, and is
  // bold regardless of colour.
  const verb = styleOf(line.verbTone ?? line.tone)
  const indented = line.column === COLUMN.output
  // Plain text in a result preview uses output grey. It is distinct from the
  // answer, but not dimmed to reasoning. Semantic and syntax colours keep
  // their own tones.
  const zone = line.zone === true
  const colored = colorOf(style, zone)
  const { rail, verb: verbWidth, width, content } = placement(line, budget)
  const text = content.literal === true ? content.text : softBreaks(content.text, width)
  return (
    // `flexShrink={0}`. Inside a region held at a fixed height, a shrinkable
    // line lets Yoga squash every line a little instead of pushing the oldest
    // ones off the top, which drops lines out of the middle of the stream.
    <Box flexDirection="row" flexShrink={0}>
      {rail === 0 ? null : <Box width={rail} flexShrink={0}>
        {line.pulse === true
          ? <Pulse glyph={line.marker} clock={clock} />
          : line.markerTone === undefined
            ? <Text bold={style.bold} {...colorOf(style)}>{line.marker}</Text>
            : <Text bold={marker.bold} dimColor={marker.dim} {...colorOf(marker)}>{line.marker}</Text>}
      </Box>}
      {indented && verbWidth > 0
        ? (
          <Box width={verbWidth} flexShrink={0}>
            {line.verb === '' && line.gutter !== undefined
              // Right-align the line number against the code, one space short of it.
              ? <Text dimColor={style.dim} {...colorOf(style)}>{`${line.gutter.padStart(COLUMN.verb - 1)} `}</Text>
              // A verb with its own tone is bold, except the quiet connector.
              : <Text bold={verb.bold || (line.verbTone !== undefined && line.verbTone !== 'quiet')} dimColor={verb.dim} {...colorOf(verb)}>{line.verb}</Text>}
          </Box>
          )
        : null}
      <Box width={width}>
        <Text bold={style.bold} dimColor={style.dim} italic={style.italic === true} wrap="wrap" {...colored}>
          {line.divider ? '-'.repeat(width) : content.spans === undefined ? text : spansOf({ ...content, text }, zone)}
        </Text>
      </Box>
    </Box>
  )
}

// `exactOptionalPropertyTypes` rejects an explicit `undefined`, so a missing
// colour is an omitted prop. In a result preview zone, text with no colour of
// its own, and not already dim, uses output grey.
const colorOf = (style: LineStyle, zone = false): { readonly color?: string } => style.color !== undefined
  ? { color: style.color }
  : zone && !style.dim ? { color: PALETTE.output } : {}

/**
 * A line's text as styled runs, nested in the `Text` that wraps them so the
 * line still wraps as one string.
 * @param line - a line carrying `spans`.
 * @param zone - whether the line is in a result's preview zone.
 * @returns one element per run, then the remainder in the line's own tone.
 */
function spansOf(line: PresentedLine, zone = false): React.ReactNode[] {
  const parts: React.ReactNode[] = []
  let offset = 0
  for (const [index, span] of (line.spans ?? []).entries()) {
    const style = styleOf(span.tone)
    const color = span.color === undefined ? colorOf(style, zone) : { color: span.color }
    parts.push(
      <Text key={index} bold={span.bold === true || style.bold} dimColor={style.dim} italic={span.italic === true || style.italic === true}
        underline={span.underline === true} strikethrough={span.strikethrough === true} inverse={span.inverse === true} {...color}>
        {line.text.slice(offset, offset + span.length)}
      </Text>,
    )
    offset += span.length
  }
  if (offset < line.text.length) parts.push(line.text.slice(offset))
  return parts
}

/** Milliseconds a blinking marker spends shown, and then as long hidden. Four beats. */
export const PULSE_MS = FRAME_MS * 4

/** Whether a blinking marker is shown at a time. Every marker is in phase, on the shared beat. */
const pulseLit = (now: number): boolean => Math.floor(now / PULSE_MS) % 2 === 0

/**
 * A marker that blinks while its action runs. It is shown in the running
 * orange, then hidden, in place. The blink is a clean on and off, not a dim
 * half-state, so it looks the same on every theme and under `NO_COLOR`. The
 * hidden phase draws a space of the glyph's width, so the row never moves
 * and a terminal rewrites one cell. Every running marker blinks together, on
 * the beat the header moves on, so several running actions cost no more
 * frames than one.
 *
 * @param props.glyph - the marker.
 * @param props.clock - time source; absent, the marker holds lit.
 */
function Pulse({ glyph, clock }: { readonly glyph: string, readonly clock: Clock | undefined }): React.ReactElement {
  // Unsubscribed on unmount, which is when the action finishes and leaves the
  // live region. No beat is spent on it after.
  const now = useBeat(clock !== undefined, time => String(pulseLit(time)))
  const lit = now === undefined || pulseLit(now)
  return lit
    ? <Text color={PALETTE.running} bold>{glyph}</Text>
    : <Text>{' '.repeat(stringWidth(glyph))}</Text>
}

/** Place both the rendered line and its height measurement against the same width. */
function placement(line: PresentedLine, budget: Budget): {
  readonly rail: number
  readonly verb: number
  readonly width: number
  readonly content: PresentedLine
} {
  const rail = line.flush === true ? 0 : Math.min(COLUMN.rail, Math.max(0, budget.columns - 1))
  const indented = line.column === COLUMN.output
  const verb = indented && budget.columns - rail > COLUMN.verb ? COLUMN.verb : 0
  const available = Math.max(1, budget.columns - rail - verb)
  const width = line.flush === true || line.wide === true || (indented && line.prose !== true) ? available : Math.min(available, budget.measure)
  if (!indented || verb > 0 || (line.verb === '' && line.gutter === undefined)) return { rail, verb, width, content: line }
  // A narrow window has no room for the verb column. Put its label in the
  // body instead of dropping it or letting a fixed-width gutter push text off screen.
  const prefix = `${line.verb || line.gutter} `
  const content: PresentedLine = { ...line, text: prefix + line.text,
    spans: [{ length: prefix.length, tone: line.verbTone ?? line.tone, bold: line.verb !== '' }, ...(line.spans ?? [])],
  }
  return { rail, verb, width, content }
}

/**
 * The rows a line's text wraps into, as `Line` draws them. Through
 * {@link softBreaks}, then the way Ink wraps `Text`.
 * @param line - the placed line.
 * @param budget - budgets for the current terminal size.
 * @returns at least one row.
 */
export function wrappedRows(line: PresentedLine, budget: Budget): readonly string[] {
  const { width, content } = placement(line, budget)
  if (line.divider === true || content.text === '') return [line.divider === true ? '-'.repeat(width) : content.text]
  return wrapAnsi(content.literal === true ? content.text : softBreaks(content.text, width), width, { hard: true, trim: false }).split('\n')
}

/**
 * Rows a line occupies once wrapped, measured the way Ink wraps `Text`.
 * @param line - the placed line.
 * @param budget - budgets for the current terminal size.
 * @returns at least one.
 */
export const lineHeight = (line: PresentedLine, budget: Budget): number => wrappedRows(line, budget).length

/**
 * Live output sized to what it holds, with a bounded window of the newest rows.
 *
 * The region grows as the turn streams, so the input below it follows the
 * newest line instead of waiting under a reserved block of empty rows.
 * @param props.rows - live rows in arrival order.
 * @param props.budget - terminal width and region budgets.
 * @param props.limit - maximum physical rows visible.
 * @param props.result - localized tool-result preview limit.
 * @returns the live region, or null when it has nothing to show.
 */
export function LiveRegion({ rows, budget, limit, result, clock }: {
  readonly rows: readonly Row[]
  readonly budget: Budget
  readonly limit: number
  readonly result: ResultBound
  /** Time source for the markers of actions still running. */
  readonly clock?: Clock | undefined
}): React.ReactElement | null {
  // Window before measuring, not only before drawing. A long turn accumulates
  // more lines than any terminal can show, and handing all of them to Yoga
  // makes every frame cost the length of the turn. Count wrapped rows, so the
  // window the lines are chosen for is the window they are drawn in, and the
  // clip below never cuts a section's verb line.
  const height = (line: PresentedLine): number => lineHeight(line, budget)
  // A running step folds its own detail to fit, so the window never cuts the
  // head that says what the step is doing.
  const lines = tailLines(rows.flatMap(row => row.kind === 'tool-group'
    ? fittedGroup(row.calls, result, limit, height)
    : present(row, result)), limit, height)
  if (lines.length === 0 || limit <= 0) return null
  // A section's opening blank is drawn outside the clip. The clipped rows are
  // the oldest text, never the gap that separates the section from history.
  const gap = limit > 1 && lines.length > 1 && isBlank(lines[0]!)
  const body = gap ? lines.slice(1) : lines
  return (
    <Box flexDirection="column" flexShrink={0}>
      {gap && <Text> </Text>}
      {/* Clipped from the top: the line that overflows is the oldest prose,
          and the rows kept are the ones still arriving. */}
      <Box flexDirection="column" flexShrink={0} maxHeight={limit - Number(gap)} justifyContent="flex-end" overflowY="hidden">
        {body.map((line, index) => <Line key={index} line={line} budget={budget} clock={clock} />)}
      </Box>
    </Box>
  )
}

/** Blank rows between the thinking window and the header under it. */
export const THINKING_GAP = 1

/**
 * Newest rows of streaming reasoning, drawn above the header while a turn thinks.
 *
 * The window is a few rows held at a fixed height, never a growing block.
 * Reasoning arrives faster than it can be read, and drawing it in full scrolls
 * the surface. Placement matches transcript reasoning. It is a dim italic paragraph
 * at the rail, with no verb, so the window is the text the transcript will
 * keep. Each row is its own truncated `Text`. The rows are already wrapped to
 * fit. If Ink wrapped them again, the height would change.
 *
 * One blank row under the window keeps the reasoning off the header. Without
 * it, the turn's stats would look like the paragraph's last line.
 * The blank belongs to the window. It comes and goes with it, and it is the
 * first row given up when there is room for only one.
 *
 * @param props.rows - newest rows, from `thinkingRows`.
 * @param props.limit - maximum rows, the blank included; older rows are dropped.
 * @returns the window, or null when there is nothing to show or no room.
 */
export function Thinking({ rows, limit }: { readonly rows: readonly string[], readonly limit: number }): React.ReactElement | null {
  const gap = limit > 1 ? THINKING_GAP : 0
  const window = limit > 0 ? rows.slice(-(limit - gap)) : []
  if (window.length === 0) return null
  return (
    <Box flexDirection="column" flexShrink={0} marginBottom={gap}>
      {window.map((row, index) => <Box key={index} flexDirection="row" flexShrink={0}>
        <Box width={COLUMN.rail} flexShrink={0}><Text> </Text></Box>
        <Text dimColor italic wrap="truncate-end">{row}</Text>
      </Box>)}
    </Box>
  )
}

/** Glyph and colour for each way a turn ends. */
const OUTCOME: Readonly<Record<Outcome, { readonly glyph: string, readonly color: PaletteColor }>> = {
  done: { glyph: '\u2713', color: PALETTE.done },
  stopped: { glyph: '\u25a0', color: PALETTE.waiting },
  failed: { glyph: '\u2717', color: PALETTE.failed },
}

/**
 * What the activity header says about the turn. Work in progress, a turn
 * or a compaction, or how the last turn ended. Absent, it says nothing.
 */
export type ActivityState =
  | {
    readonly kind: 'running'
    /** The work's word, chosen once when it started. */
    readonly word: string
    /** What it is doing, absent while it waits on the model. */
    readonly phase: string | undefined
    /** Clock time it started. */
    readonly startedAt: number
    /** Colour for the glyph and word. */
    readonly color: PaletteColor
  }
  | { readonly kind: 'ended', readonly summary: TurnSummary }

/** A standing session state the header shows beside the turn's, such as the goal. */
export interface StandingState {
  /** One-cell shape that carries the state under `NO_COLOR`. */
  readonly glyph: string
  /** Bold word in the state's colour. */
  readonly label: string
  /** Dim text after the label; truncated first. */
  readonly details: string
  readonly color: PaletteColor
}

/**
 * Columns before the header's glyph. None: the glyph takes the rail's first
 * column, as an action's marker and the goal block's head do, so the turn
 * reads as the head of the work above it rather than as part of the draft.
 */
const HEADER_LEAD = 0

/** Cells between the turn's label and the standing state's. */
const HEADER_GAP = 2

/** Fewest cells of the standing state's details worth drawing before an ellipsis. */
const HEADER_DETAIL_MIN = 4

/**
 * Split the header row between the turn label and the standing state.
 *
 * The turn label is allocated first. It is what a returning reader checks.
 * The standing state is right-aligned in what remains. Its details are
 * truncated from the end while a few cells of them still fit, then omitted
 * so the glyph and label remain. Below that the whole state is dropped,
 * not drawn as a fragment.
 *
 * @param columns - row width.
 * @param left - full width of the turn label, in cells; 0 when there is none.
 * @param right - full width of the standing state, in cells; 0 when there is none.
 * @param rightMin - width of the standing state's glyph and label, the minimum worth drawing.
 * @returns the widths actually drawn on each side.
 */
export function headerRoom(columns: number, left: number, right: number, rightMin: number): { readonly left: number, readonly right: number } {
  const room = Math.max(0, columns - HEADER_LEAD)
  const shownLeft = Math.min(left, room)
  if (right <= 0) return { left: shownLeft, right: 0 }
  const gap = left > 0 ? HEADER_GAP : 0
  if (left + gap + right <= room) return { left, right }
  const cut = room - left - gap
  if (cut >= rightMin + HEADER_GAP + HEADER_DETAIL_MIN) return { left, right: cut }
  if (cut >= rightMin) return { left, right: Math.min(rightMin, right) }
  return { left: shownLeft, right: 0 }
}

/**
 * Header row above the input. Current work is on the left, and the standing state is on the right.
 *
 * One row answers both "is it still working?" and "what is it working toward?",
 * directly above the rule that frames the input.
 * `  ⠠⠞⠁ Kneading…  writing · 12s            ● Goal active  round 3/256 · Ship it`.
 * While a turn runs, the spinner, word, phase, and elapsed time lead the row.
 * When the turn ends, those cells hold the outcome until the next turn starts.
 * The standing state, currently the goal, sits at the right edge. With neither,
 * the row stays blank so the input does not move when either appears.
 *
 * Motion uses the surface's shared beat, and only when a clock is supplied.
 * Without a clock — a screen reader or a test — the glyph rests and elapsed
 * time is omitted, so the row does not change on its own. With `motion` off,
 * only the seconds advance. The row redraws only on beats where the glyph or
 * the seconds change.
 *
 * @param props.columns - terminal width; the row takes all of it.
 * @param props.state - what the turn is doing, or how it ended.
 * @param props.standing - the session's standing state, drawn at the right.
 * @param props.clock - time source; absent disables motion and the elapsed time.
 * @param props.motion - whether the glyph cycles; defaults to true.
 * @param props.compact - use a static ASCII chevron for screen readers.
 */
export function Header({ columns, state, standing, clock, motion = true, compact = false }: {
  readonly columns: number
  readonly state?: ActivityState | undefined
  readonly standing?: StandingState | undefined
  readonly clock?: Clock | undefined
  readonly motion?: boolean
  readonly compact?: boolean
}): React.ReactElement {
  const running = state?.kind === 'running' ? state : undefined
  const moving = running !== undefined && !compact && motion && clock !== undefined
  const title = running === undefined ? '' : `${running.word}\u2026`
  const detailsAt = (elapsed: number): string => running === undefined ? ''
    : [running.phase, clock === undefined ? undefined : formatElapsed(elapsed)]
      .filter((part): part is string => part !== undefined).join(' \u00b7 ')
  const glyphAt = (elapsed: number): string => compact ? '>' : moving ? spinnerFrame(elapsed) : SPINNER_REST
  // Subscription key. Elapsed seconds, plus the glyph while it is moving.
  // Dropped when the work ends, so no beat outlives it. The render that ends
  // the work still reads the key once before unsubscribe, which keeps a key
  // for a header that has nothing running.
  const now = useBeat(running !== undefined && clock !== undefined, time => {
    if (running === undefined) return ''
    const elapsed = time - running.startedAt
    return `${formatElapsed(elapsed)}${moving ? `|${spinnerFrame(elapsed)}` : ''}`
  }) ?? clock?.now() ?? running?.startedAt ?? 0
  const elapsed = running === undefined ? 0 : Math.max(0, now - running.startedAt)
  const details = detailsAt(elapsed)
  const ended = state?.kind === 'ended' ? state.summary : undefined
  const leftText = running !== undefined
    ? `${glyphAt(elapsed)} ${title}${details === '' ? '' : `  ${details}`}`
    : ended === undefined ? '' : `${OUTCOME[ended.outcome].glyph} ${ended.label}${ended.details === '' ? '' : `  ${ended.details}`}`
  const rightHead = standing === undefined ? '' : `${standing.glyph} ${standing.label}`
  const rightText = standing === undefined ? '' : `${rightHead}${standing.details === '' ? '' : `  ${standing.details}`}`
  const room = headerRoom(columns, stringWidth(leftText), stringWidth(rightText), stringWidth(rightHead))
  const left = running !== undefined
    ? <>
      <Text color={running.color}>{glyphAt(elapsed)}</Text>{' '}
      <Text color={running.color} bold>{title}</Text>
      {details === '' ? null : <Text dimColor>{`  ${details}`}</Text>}
    </>
    : ended === undefined ? null
      : <>
        <Text color={OUTCOME[ended.outcome].color} bold>{`${OUTCOME[ended.outcome].glyph} ${ended.label}`}</Text>
        {ended.details === '' ? null : <Text dimColor>{`  ${ended.details}`}</Text>}
      </>
  return (
    <Box width={columns} height={1} flexDirection="row" flexShrink={0} overflowX="hidden">
      {HEADER_LEAD > 0 && <Box width={HEADER_LEAD} flexShrink={0}><Text> </Text></Box>}
      {room.left > 0 && <Box width={room.left} flexShrink={0}><Text wrap="truncate-end">{left}</Text></Box>}
      <Box flexGrow={1} />
      {room.right > 0 && standing !== undefined && <Box width={room.right} flexShrink={0}>
        <Text wrap="truncate-end">
          <Text color={standing.color} bold>{rightHead}</Text>
          {standing.details === '' || room.right <= stringWidth(rightHead) ? null : <Text dimColor>{`  ${standing.details}`}</Text>}
        </Text>
      </Box>}
    </Box>
  )
}

/**
 * A rule framing the input. One dim row exactly the terminal's width, over
 * the draft and under it.
 *
 * Its glyphs are the style the terminal was resolved to draw, because a
 * full-width run of East Asian Ambiguous glyphs is the one place such a
 * character accumulates error across a row.
 *
 * @param props.columns - terminal width; the row takes all of it.
 * @param props.frame - line glyphs this terminal can draw.
 */
export function Rule({ columns, frame }: {
  readonly columns: number
  readonly frame: FrameStyle
}): React.ReactElement {
  return <Box width={columns} flexShrink={0} overflowX="hidden">
    <Text dimColor>{RULE[frame].line.repeat(Math.max(0, columns))}</Text>
  </Box>
}

/**
 * A titled list of harness-owned state, between the turn and the chrome.
 *
 * Queued input and staged attachments share one shape. A coloured title names
 * what is held, the held items, and one dim line naming the command that clears
 * them. Both lists are as long as the user made them, and each item is as long
 * as whatever it describes, so both use §5's bound. That bound keeps the
 * composer and status line on screen.
 *
 * Windowed and capped for the same two reasons as `Notice`. The window handles
 * more items than fit. The cap handles one item that wraps to more rows than
 * it has lines. The footer stays inside the window, because the command it
 * names is how the user dismisses the panel.
 *
 * @param props.title - locale-owned heading.
 * @param props.color - the title's semantic colour.
 * @param props.items - the held items, in the order the user added them.
 * @param props.footer - locale-owned line naming the command that clears them.
 * @param props.limit - rows the panel may draw, title and footer included.
 * @param props.more - locale-owned word for the omitted-item count.
 * @returns the panel, or null when it has nothing to hold or no room to hold it.
 */
export function Panel({ title, color, items, footer, limit, more }: {
  readonly title: string
  readonly color: PaletteColor
  readonly items: readonly string[]
  readonly footer: string
  readonly limit: number
  readonly more: string
}): React.ReactElement | null {
  // The title and the footer are the panel's own rows, so the items share
  // what is left. A limit too small for both leaves the title, which is the
  // row that says why the others are missing.
  const { shown, hidden } = windowOf(items, Math.max(0, limit - 2))
  if (items.length === 0 || limit <= 0) return null
  return (
    <Box flexDirection="column" flexShrink={0} maxHeight={limit} overflowY="hidden">
      <Text color={color}>{title}</Text>
      {shown.map((item, index) => <Text key={index} dimColor wrap="wrap">{item}</Text>)}
      {hidden === 0 ? null : <Text dimColor>{`+${hidden} ${more}`}</Text>}
      <Text dimColor>{footer}</Text>
    </Box>
  )
}

/**
 * Head of a block of held state, such as the task list or the subagents.
 *
 * A marker in the rail, the block's name in bold, and a dim count after it.
 * The same shape heads a step's calls in the transcript, so a block with a
 * head and hanging items has the same layout above the input as in it.
 *
 * @param props.color - the marker's semantic colour. Absent, the marker is dim.
 * @param props.title - locale-owned name of the block.
 * @param props.detail - dim text after the name, such as a count.
 */
export function SectionHead({ color, title, detail }: {
  readonly color?: PaletteColor | undefined
  readonly title: string
  readonly detail?: string | undefined
}): React.ReactElement {
  return (
    <Box flexDirection="row" flexShrink={0}>
      <Box width={COLUMN.rail} flexShrink={0}>
        <Text bold {...color === undefined ? { dimColor: true } : { color }}>{MARKER.action}</Text>
      </Box>
      <Text wrap="truncate-end">
        <Text bold>{title}</Text>
        {detail === undefined || detail === '' ? null : <Text dimColor>{`  ${detail}`}</Text>}
      </Text>
    </Box>
  )
}

/**
 * One item hanging from a {@link SectionHead}.
 *
 * A dim branch in the rail, the item's state glyph, and its text on one
 * truncated row. The panel is exactly as tall as its items, so its budget holds.
 *
 * @param props.last - whether this is the block's last item, which closes the tree.
 * @param props.glyph - the item's state, one cell. Absent, the text starts in its place.
 * @param props.color - the glyph's semantic colour.
 * @param props.children - the item's text.
 */
export function Branch({ last, glyph, color, children }: {
  readonly last: boolean
  readonly glyph?: string | undefined
  readonly color?: PaletteColor | undefined
  readonly children: React.ReactNode
}): React.ReactElement {
  return (
    <Box flexDirection="row" flexShrink={0}>
      <Box width={COLUMN.rail} flexShrink={0}><Text dimColor>{last ? TREE.corner : TREE.branch}</Text></Box>
      {glyph === undefined ? null : (
        <Box width={2} flexShrink={0}>
          <Text {...color === undefined ? { dimColor: true } : { color, bold: true }}>{glyph}</Text>
        </Box>
      )}
      <Text wrap="truncate-end">{children}</Text>
    </Box>
  )
}

/** Two spaces between status fields. A separator glyph would be another width to measure. */
const FIELD_GAP = '  '

/**
 * Transient feedback between the conversation and the chrome.
 *
 * Everything drawn here is charged to the budget that keeps Ink off its
 * screen-clearing path. Every row it takes also moves the status line and
 * composer while the user is reading them. A command whose full output
 * matters returns that output, so it commits to the transcript where the
 * terminal can scroll it. What remains here is short feedback. This bound
 * holds when a caller forgets that.
 *
 * The line count is windowed and the height is capped, because the two bounds
 * cover different cases. Windowing handles a notice with more lines than fit.
 * The cap handles one long line, which wraps to more rows than it has lines.
 * Text still wraps instead of being cut mid-sentence. A notice the user cannot
 * finish is worse than a ragged wrap, and the footer says when lines were left out.
 *
 * @param props.text - the notice, which may carry its own line breaks.
 * @param props.limit - rows the region may draw, footer included.
 * @param props.more - locale-owned word for the omitted-line count.
 */
export function Notice({ text, limit, more }: {
  readonly text: string
  readonly limit: number
  readonly more: string
}): React.ReactElement | null {
  const { shown, hidden } = windowOf(text.split('\n'), limit)
  if (shown.length === 0 && hidden === 0) return null
  return (
    <Box flexDirection="column" flexShrink={0} maxHeight={limit} overflowY="hidden">
      {shown.map((line, index) => <Text key={index} color={PALETTE.waiting} wrap="wrap">{line}</Text>)}
      {hidden === 0 ? null : <Text dimColor>{`+${hidden} ${more}`}</Text>}
    </Box>
  )
}

/**
 * A status value in its semantic tone beside a dim label. Never the unbounded field.
 */
export interface MeasuredField {
  readonly label: string
  readonly value: string
  readonly color: PaletteColor
}

/** Primary status text, with an optional complete narrow reading. */
export interface PrimaryField {
  readonly text: string
  readonly short?: string
}

/** One status-line field. Dim supporting text, primary text, or a measured reading. */
export type StatusField = string | PrimaryField | MeasuredField

/** A field's text as drawn, for measuring independently of emphasis. */
const textOf = (field: StatusField): string => typeof field === 'string' ? field
  : 'text' in field ? field.text : `${field.label} ${field.value}`

/**
 * The status line. One left-packed list of fields below the composer.
 *
 * Fields sit two spaces apart in priority order and stop where they end.
 * The composer's rule already separates the chrome from the transcript, so
 * this row does not. Justifying it to both edges would open a gap the width
 * of the terminal between the model and the context meter, which would look
 * like two rows instead of one bar.
 *
 * Model and context fields use the normal foreground. Supporting fields stay
 * dim. The turn header, above the frame, is what says what the session is
 * doing, in colour and in words. A second coloured state word here would
 * repeat that on every frame. Colouring the model or the path would make them
 * change colour for reasons that have nothing to do with them. A
 * {@link MeasuredField} is the exception. Its value is a reading or an access
 * boundary, and its colour follows that value.
 *
 * The line never wraps. A wrapped status line silently spends a row of the
 * live region's budget. Once the fields reach the edge, width is yielded in
 * priority order, not shared. The last right field is the unbounded one, a
 * deep working directory. It yields first, truncating from the start so it
 * keeps the workspace name. The other right fields are bounded, and a bounded
 * field is dropped whole, not cut. `cache hi` is a different number,
 * and a clipped context meter is a smaller one. A first field with a short
 * reading may take cells from the model instead. It keeps the model label and
 * the access and thinking badges. Other fields drop from the end, so the
 * caller orders them by priority. The left cluster truncates from the end
 * within its remaining space. Widths are measured in terminal cells, so a
 * CJK field is not laid out by code-point length.
 *
 * @param props.left - fields that identify the session, highest priority first.
 * @param props.badge - labelled value whose width takes priority over every other field.
 * @param props.secondaryBadge - second labelled value, retained if it fits beside the first.
 * @param props.right - supporting fields, highest priority first, except the
 *   unbounded field, which goes last. It is the one that yields room, and it
 *   keeps its tail, not its head.
 * @param props.columns - terminal width.
 */
export function StatusBar({ left, right, badge, secondaryBadge, columns }: {
  readonly left: readonly (string | PrimaryField)[]
  readonly right: readonly StatusField[]
  readonly badge?: MeasuredField
  readonly secondaryBadge?: MeasuredField
  readonly columns: number
}): React.ReactElement {
  const head = left.map(textOf).join(FIELD_GAP)
  const badges: { field: MeasuredField; width: number }[] = []
  let badgesWidth = 0
  for (const field of [badge, secondaryBadge]) {
    if (field === undefined) continue
    const width = stringWidth(textOf(field))
    const gap = badges.length > 0 ? FIELD_GAP.length : 0
    if (badges.length > 0 && badgesWidth + gap + width > columns) break
    badges.push({ field, width: Math.min(columns, width) })
    badgesWidth += gap + Math.min(columns, width)
  }
  const availableHead = Math.max(0, columns - badgesWidth - (badges.length === 0 ? 0 : FIELD_GAP.length))
  let headWidth = Math.min(stringWidth(head), availableHead)
  const first = right[0]
  if (first !== undefined && typeof first !== 'string' && 'short' in first && first.short !== undefined) {
    const reserve = FIELD_GAP.length + stringWidth(first.short)
    const labelEnd = head.indexOf(':')
    const labelWidth = labelEnd < 0 ? 0 : stringWidth(head.slice(0, labelEnd + 2))
    if (availableHead >= labelWidth + reserve && headWidth + reserve > availableHead) {
      headWidth = Math.min(headWidth, availableHead - reserve)
    }
  }
  const used = headWidth + badgesWidth + (badges.length > 0 && headWidth > 0 ? FIELD_GAP.length : 0)
  const kept = used >= columns ? [] : fitting(right, columns - used, used > 0)
  const last = kept.length - 1
  return (
    // `overflowX`. The last guard against a wrapped status line. Below the
    // width the left cluster alone needs, there is no room left to yield, and
    // clipping costs a few characters where wrapping costs a row of the live
    // region on every frame.
    <Box width={columns} flexDirection="row" overflowX="hidden">
      {headWidth > 0 && <Box width={headWidth} flexShrink={0}>
        <Text wrap="truncate-end">{left.map((field, index) => <Text key={index} dimColor={typeof field === 'string'}>
          {`${index === 0 ? '' : FIELD_GAP}${textOf(field)}`}
        </Text>)}</Text>
      </Box>}
      {badges.map(({ field, width }, index) => <Box key={index} width={width} flexShrink={0}
        marginLeft={index > 0 || headWidth > 0 ? FIELD_GAP.length : 0}>
        <Text wrap="truncate-end"><Text dimColor>{`${field.label} `}</Text><Text color={field.color}>{field.value}</Text></Text>
      </Box>)}
      {/* One box per field, and only the last may shrink: shrinking the
          cluster as a whole cuts from wherever the join happens to fall. The
          gap is a margin, not text, so truncating a field's head cannot
          eat the separator that tells it apart from the one before. */}
      {kept.map((field, index) => (
        <Box
          key={index}
          flexShrink={index === last ? 1 : 0}
          marginLeft={index === 0 && used === 0 ? 0 : FIELD_GAP.length}
      >
          {typeof field === 'string'
            ? <Text dimColor wrap={index === last ? 'truncate-start' : 'truncate-end'}>{field}</Text>
            : 'text' in field
              ? <Text wrap={index === last ? 'truncate-start' : 'truncate-end'}>{field.text}</Text>
              : <Text wrap="truncate-end"><Text dimColor>{`${field.label} `}</Text><Text color={field.color}>{field.value}</Text></Text>}
        </Box>
      ))}
    </Box>
  )
}

/**
 * The right fields that fit beside the left cluster.
 *
 * The last field is unbounded and truncates into whatever room is left, so it
 * is kept however little room there is; the bounded fields before it are kept
 * whole while they fit, highest priority first.
 *
 * @param right - the right fields, the unbounded one last.
 * @param room - cells left beside the left cluster.
 * @param after - whether a left cluster precedes them, and so a gap.
 * @returns the fields to draw, in order.
 */
function fitting(right: readonly StatusField[], room: number, after: boolean): readonly StatusField[] {
  const bounded = right.slice(0, -1)
  const unbounded = right.at(-1)
  const kept: StatusField[] = []
  let used = 0
  for (const field of bounded) {
    const gap = kept.length > 0 || after ? FIELD_GAP.length : 0
    let chosen = field
    if (used + gap + stringWidth(textOf(chosen)) > room) {
      if (typeof field === 'string' || !('short' in field) || field.short === undefined
        || used + gap + stringWidth(field.short) > room) break
      chosen = { text: field.short }
    }
    kept.push(chosen)
    used += gap + stringWidth(textOf(chosen))
  }
  return unbounded === undefined ? kept : [...kept, unbounded]
}

/**
 * The header, the framed composer under it, and the status line beneath them.
 *
 * Up to six rows. A blank, the header, the rule, the composer's first row,
 * the base rule, then the status line. The header ({@link Header}) is the one
 * row that says what the session is doing — the turn's spinner and word, or
 * how it ended, and the goal at the right edge — so the two rules can be bare
 * lines that only frame the input. The draft is one band between them. The
 * base rule also keeps the status line clear of the draft, so the
 * metadata sits directly under it as a footer, with no blank row opening a
 * gap between them.
 *
 * The blank above is required. Without it the header sits on the last line of
 * the answer and looks like part of that answer. Panels that belong to the
 * input instead of the conversation — a completion list, a notice, queued
 * input — are drawn between the blank and the header, so the blank opens the
 * whole stack instead of splitting it.
 *
 * Every row is one row at any width, so the chrome's height depends on the
 * terminal's rows alone, and the header holds its row while it has nothing to
 * say, so the input never moves when a turn starts or a goal is set. Both
 * rules are exactly the terminal's width; a narrowing reflows them, which the
 * application answers by repainting (see `useRepaint`). The header's glyph
 * sits in the rail's first column, as the markers of actions and held blocks
 * above it do; the draft and the status line start at the rail's column. On a short terminal, yield the gap, then the base rule,
 * then the upper rule, then the status line, and the header last, so the
 * input row stays visible.
 *
 * Hint text appears in the composer's right slot only while that key applies.
 * A permanent hint row would spend a row on text the user has already read.
 *
 * @param props.left - fields that identify the session, highest priority first.
 * @param props.badge - access indicator retained when other status fields yield.
 * @param props.secondaryBadge - thinking level retained when it fits beside access.
 * @param props.right - supporting fields, highest priority first and the
 *   unbounded one last; see {@link StatusBar}.
 * @param props.columns - terminal width.
 * @param props.state - what the surface is doing, apart from the draft, which
 *   is read from the draft text so the two cannot disagree.
 * @param props.before - draft text before the caret.
 * @param props.after - draft text after the caret.
 * @param props.placeholder - locale-owned prompt text.
 * @param props.hints - locale-owned text for each hint key.
 * @param props.maxRows - rows a draft may occupy, from `budget.composer`.
 * @param props.frame - line glyphs this terminal can draw.
 * @param props.activity - what the turn is doing, or how it ended; see {@link Header}.
 * @param props.standing - the session's standing state, the goal, at the header's right.
 * @param props.clock - time source for the header's motion.
 * @param props.motion - whether the header's glyph cycles.
 * @param props.compact - draw the header's glyph for a screen reader.
 * @param props.layout - structure that fits in the available terminal height.
 * @param props.children - panels drawn above the header, each within its own
 *   claimed rows; they are not counted in `layout.rows`.
 */
export function Chrome({ left, right, badge, secondaryBadge, columns, state, before, after, placeholder, hints, maxRows, frame, activity, standing, clock, motion, compact, layout = chromeFor(columns), children }: {
  readonly left: readonly (string | PrimaryField)[]
  readonly right: readonly StatusField[]
  readonly badge?: MeasuredField
  readonly secondaryBadge?: MeasuredField
  readonly columns: number
  readonly state: Omit<ComposerState, 'drafting'>
  readonly before: string
  readonly after: string
  readonly placeholder: string
  readonly hints: Readonly<Record<Exclude<Hint, undefined>, string>>
  readonly maxRows?: number
  readonly frame: FrameStyle
  readonly activity?: ActivityState | undefined
  readonly standing?: StandingState | undefined
  readonly clock?: Clock | undefined
  readonly motion?: boolean
  readonly compact?: boolean
  readonly layout?: ChromeLayout
  readonly children?: React.ReactNode
}): React.ReactElement {
  const hint = hintFor({ ...state, drafting: `${before}${after}` !== '' })
  // The draft's column, which the status line lines up with.
  const inset = columns > COLUMN.rail * 2 ? COLUMN.rail : 0
  return (
    <Box flexDirection="column" flexShrink={0}>
      {/* Not a margin: Ink gives a zero-height Box no row, and the gap has to
          be a row the layout counts. */}
      {layout.gap && <Text> </Text>}
      {children}
      {layout.header && <Header columns={columns} state={activity} standing={standing} clock={clock}
        {...motion === undefined ? {} : { motion }} {...compact === undefined ? {} : { compact }} />}
      {layout.rule && <Rule columns={columns} frame={frame} />}
      <Composer
        columns={columns}
        marker={MARKER.prompt}
        before={before}
        after={after}
        placeholder={placeholder}
        {...maxRows === undefined ? {} : { maxRows }}
        {...hint === undefined || columns < HINT_MIN_COLUMNS ? {} : { hint: hints[hint] }}
      />
      {layout.base && <Rule columns={columns} frame={frame} />}
      {layout.status && <Box paddingLeft={inset} flexShrink={0}>
        <StatusBar left={left} right={right} {...badge === undefined ? {} : { badge }}
          {...secondaryBadge === undefined ? {} : { secondaryBadge }} columns={Math.max(1, columns - inset)} />
      </Box>}
    </Box>
  )
}

/**
 * Input window measured in terminal rows, following the cursor within wrapped text.
 *
 * The hint shares the caret's row. Wrapping precedes windowing, so Home, End,
 * Unicode input, and edits inside a long paragraph keep the caret visible.
 * The caret is laid out around the draft, not inside it, so moving through
 * the text leaves every row in place.
 *
 * @param props.columns - available width.
 * @param props.marker - prompt marker.
 * @param props.before - draft text before the caret.
 * @param props.after - draft text after the caret.
 * @param props.placeholder - locale-owned prompt text.
 * @param props.hint - contextual key hint.
 * @param props.maxRows - maximum physical rows of the draft to display.
 */
export function Composer({ columns, marker, before, after, placeholder, hint, maxRows = COMPOSER_BUDGET }: {
  readonly columns: number
  readonly marker: string
  readonly before: string
  readonly after: string
  readonly placeholder: string
  readonly hint?: string
  readonly maxRows?: number
}): React.ReactElement {
  const empty = before === '' && after === ''
  const rail = Math.min(COLUMN.rail, Math.max(0, columns - 1))
  const slot = hint === undefined || hint === '' ? 0 : stringWidth(hint) + 2
  const showHint = slot > 0 && columns - rail - slot >= 1
  // `wrapDraft` keeps its last column for the caret. Beside a hint, that is
  // the first of the two columns separating the draft from it.
  const width = Math.max(1, columns - rail - (showHint ? slot - 1 : 0))
  const { rows: lines, caret: caretRow } = wrapDraft(`${before}${after}`, before.length, width, CARET)
  const height = Math.max(1, maxRows)
  const start = Math.max(0, Math.min(caretRow - height + 1, lines.length - height))
  const visible = lines.slice(start, start + height)
  return <Box flexDirection="column" width={columns} flexShrink={0}>
    {visible.map((line, index) => {
      const absolute = start + index
      const promptLine = absolute === 0
      const overflow = index === 0 && start > 0
      return <Box key={absolute} flexDirection="row" flexShrink={0}>
        {rail > 0 && <Box width={rail} flexShrink={0}>
          <Text bold={promptLine} dimColor={overflow} {...promptLine ? { color: PALETTE.asking } : {}}>
            {promptLine ? marker : overflow ? '^' : MARKER.none}
          </Text>
        </Box>}
        <Box width={width} flexShrink={0}>
          <Text wrap="truncate-end">
            {line}{empty && <Text dimColor>{placeholder}</Text>}
          </Text>
        </Box>
        {showHint && absolute === caretRow && <Box marginLeft={1} flexShrink={0}>
          <Text dimColor>{hint}</Text>
        </Box>}
      </Box>
    })}
  </Box>
}

/**
 * Caret drawn in the composer.
 *
 * A Block Element, so East Asian Ambiguous. A CJK-configured terminal may draw
 * it two cells wide. That is accepted here and nowhere else, because the
 * alternative is an attribute that disappears exactly when colour does.
 */
const CARET = '\u258c'

/**
 * Candidate list above the composer.
 *
 * Selection is marked three ways, because no single one survives every terminal.
 * a marker for a screen reader and for NO_COLOR, weight for a glance, and an
 * undimmed description so the selected row's meaning is the one in focus. The
 * marker is `*`, never the composer's `>`, which sits directly above it.
 *
 * @param props.items - candidates in display order, already windowed.
 * @param props.selected - index of the selected candidate.
 * @param props.hidden - candidates omitted by the window.
 * @param props.more - locale-owned text for the omitted-count row.
 * @param props.nameWidth - width of the name column; sized to the content when absent.
 */
export function Completion({ items, selected, hidden, more, nameWidth }: {
  readonly items: readonly { readonly name: string, readonly description: string }[]
  readonly selected: number
  readonly hidden: number
  readonly more: string
  readonly nameWidth?: number
}): React.ReactElement | null {
  if (items.length === 0 && hidden === 0) return null
  // Sized to the content. A fixed column wraps a file path onto a second row and
  // leaves command names in a field far wider than they need.
  const width = nameWidth ?? Math.min(40, Math.max(8, ...items.map(item => item.name.length + 2)))
  return (
    <Box flexDirection="column">
      {items.map((item, index) => {
        const active = index === selected
        return (
          <Box key={item.name} flexDirection="row">
            <Box width={COLUMN.rail} flexShrink={0}>
              <Text bold color={PALETTE.asking}>{active ? MARKER.selected : MARKER.none}</Text>
            </Box>
            <Box width={width} flexShrink={0}>
              <Text bold={active} wrap="truncate-start" {...active ? { color: PALETTE.asking } : {}}>{item.name}</Text>
            </Box>
            <Box flexGrow={1}>
              <Text dimColor={!active} wrap="truncate-end">{item.description}</Text>
            </Box>
          </Box>
        )
      })}
      {hidden === 0
        ? null
        : (
          <Box flexDirection="row">
            <Box width={COLUMN.rail} flexShrink={0}><Text> </Text></Box>
            <Text dimColor>{more}</Text>
          </Box>
          )}
    </Box>
  )
}
