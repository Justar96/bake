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
import { ACCENT, PALETTE, type PaletteColor } from './palette.ts'
import { hintFor, isBlank, present, softBreaks, styleOf, tailLines, type ComposerState, type Hint, type LineStyle, type PresentedLine, type ResultBound } from './present.ts'
import type { Row } from './rows.ts'
import { FRAME_MS, formatElapsed, lightStride, shimmer, shimmerStep, SPINNER_REST, spinnerFrame, type Clock, type Outcome, type TurnSummary } from './activity.ts'
import { useBeat } from './beat.tsx'

/**
 * One display line.
 *
 * Prose and tool output wrap in the columns the terminal leaves after their
 * rail or verb. Neither is ever truncated: a result the
 * user cannot finish reading is worse than a ragged one.
 * The verb and body share their tone so action labels keep the same emphasis.
 *
 * @param props.line - the placed line.
 * @param props.budget - budgets for the current terminal size.
 */
export function Line({ line, budget, clock }: {
  readonly line: PresentedLine
  readonly budget: Budget
  /** Time source for a pulsing marker; absent, it holds still. */
  readonly clock?: Clock | undefined
}): React.ReactElement {
  const style = styleOf(line.tone)
  const marker = styleOf(line.markerTone ?? line.tone)
  // A verb with its own tone opens an action or reports how it ended, and is
  // bold whatever its colour.
  const verb = styleOf(line.verbTone ?? line.tone)
  const indented = line.column === COLUMN.output
  const colored = colorOf(style)
  const { rail, verb: verbWidth, width, content } = placement(line, budget)
  const text = content.literal === true ? content.text : softBreaks(content.text, width)
  return (
    // `flexShrink={0}`: inside a region held at a fixed height, a shrinkable
    // line lets Yoga squash every line a little instead of pushing the oldest
    // ones off the top, which drops lines out of the middle of the stream.
    <Box flexDirection="row" flexShrink={0}>
      {rail === 0 ? null : <Box width={rail} flexShrink={0}>
        {line.pulse === true
          ? <Pulse glyph={line.marker} clock={clock} />
          : line.markerTone === undefined
            ? <Text bold={style.bold} {...colored}>{line.marker}</Text>
            : <Text bold={marker.bold} dimColor={marker.dim} {...colorOf(marker)}>{line.marker}</Text>}
      </Box>}
      {indented && verbWidth > 0
        ? (
          <Box width={verbWidth} flexShrink={0}>
            {line.verb === '' && line.gutter !== undefined
              // A number reads against the text it numbers, so it sits right
              // of the column, one space short of the code.
              ? <Text dimColor={style.dim} {...colored}>{`${line.gutter.padStart(COLUMN.verb - 1)} `}</Text>
              : <Text bold={verb.bold || line.verbTone !== undefined} dimColor={verb.dim} {...colorOf(verb)}>{line.verb}</Text>}
          </Box>
          )
        : null}
      <Box width={width}>
        <Text bold={style.bold} dimColor={style.dim} italic={style.italic === true} wrap="wrap" {...colored}>
          {line.divider ? '-'.repeat(width) : content.spans === undefined ? text : spansOf({ ...content, text })}
        </Text>
      </Box>
    </Box>
  )
}

// `exactOptionalPropertyTypes` rejects an explicit undefined, so an absent
// colour is an absent prop.
const colorOf = (style: LineStyle): { readonly color?: string } => style.color === undefined ? {} : { color: style.color }

/**
 * A line's text as styled runs, nested in the `Text` that wraps them so the
 * line still wraps as one string.
 * @param line - a line carrying `spans`.
 * @returns one element per run, then the remainder in the line's own tone.
 */
function spansOf(line: PresentedLine): React.ReactNode[] {
  const parts: React.ReactNode[] = []
  let offset = 0
  for (const [index, span] of (line.spans ?? []).entries()) {
    const style = styleOf(span.tone)
    const color = span.color === undefined ? colorOf(style) : { color: span.color }
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

/** Milliseconds a pulsing marker spends lit, and then as long dimmed: four beats. */
export const PULSE_MS = FRAME_MS * 4

/** Whether a pulsing marker is lit at a time: every marker in phase, on the shared beat. */
const pulseLit = (now: number): boolean => Math.floor(now / PULSE_MS) % 2 === 0

/**
 * A marker that breathes while its action runs: lit in the header's accent,
 * then dimmed, in place. Only its colour changes, so the row never moves and
 * a terminal rewrites one cell. Every running marker breathes together, on the
 * beat the header moves on, so several running actions cost no more frames
 * than one.
 *
 * @param props.glyph - the marker.
 * @param props.clock - time source; absent, the marker holds lit.
 */
function Pulse({ glyph, clock }: { readonly glyph: string, readonly clock: Clock | undefined }): React.ReactElement {
  // Unsubscribed on unmount, which is when the action finishes and leaves the
  // live region: no beat is spent on it after.
  const now = useBeat(clock !== undefined, time => String(pulseLit(time)))
  const lit = now === undefined || pulseLit(now)
  return <Text color={ACCENT.base} bold={lit} dimColor={!lit}>{glyph}</Text>
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
  // A narrow window cannot afford the verb column. Put its label in the body
  // instead of losing it or letting a fixed-width gutter push text off screen.
  const prefix = `${line.verb || line.gutter} `
  const content: PresentedLine = { ...line, text: prefix + line.text,
    spans: [{ length: prefix.length, tone: line.verbTone ?? line.tone, bold: line.verb !== '' }, ...(line.spans ?? [])],
  }
  return { rail, verb, width, content }
}

/**
 * The rows a line's text wraps into, as `Line` draws them: through
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
 * Live output sized to what it holds, with a bounded window retaining the newest rows.
 * The region grows as the turn streams, so the input below it follows the newest
 * line rather than waiting under a reserved block of empty rows.
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
  // Windowed before measurement, not only for display: a long turn accumulates
  // more lines than any terminal can show, and handing all of them to Yoga
  // makes every frame cost the length of the turn.
  // Counted in wrapped rows, so the window the lines are chosen for is the one
  // they are drawn in, and the clip below never cuts a section's verb line.
  const lines = tailLines(rows.flatMap(row => present(row, result)), limit, line => lineHeight(line, budget))
  if (lines.length === 0 || limit <= 0) return null
  // A section's opening blank is drawn outside the clip, so the rows clipped
  // are the oldest text, never the gap that separates it from history.
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

/**
 * The newest rows of streaming reasoning, above the rule while a turn thinks.
 *
 * A few rows, held at their height, never a growing block: reasoning arrives
 * faster than it can be read, and drawn whole it scrolls the surface with it.
 * Placed and styled as reasoning is in the transcript, a dim italic paragraph
 * at the rail with no verb, so the window is the same text the transcript
 * will keep. Each row is its own truncated `Text`: the rows were wrapped to
 * fit, and a row Ink wrapped again would change the height.
 *
 * @param props.rows - the newest rows, from `thinkingRows`.
 * @param props.limit - rows it may draw; the newest are kept.
 * @returns the window, or null with nothing to show or no room.
 */
export function Thinking({ rows, limit }: { readonly rows: readonly string[], readonly limit: number }): React.ReactElement | null {
  const window = limit > 0 ? rows.slice(-limit) : []
  if (window.length === 0) return null
  return (
    <Box flexDirection="column" flexShrink={0}>
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
 * What the rule over the composer says: work in progress — a turn, or a
 * compaction — or how the last turn ended. Absent, the rule is a bare line.
 */
export type RuleState =
  | {
    readonly kind: 'running'
    /** The work's word, chosen once when it started. */
    readonly word: string
    /** What it is doing, absent while it waits on the model. */
    readonly phase: string | undefined
    /** Clock time it started. */
    readonly startedAt: number
    /** Colour for the glyph and word; only the running orange lights the rule. */
    readonly color: PaletteColor
  }
  | { readonly kind: 'ended', readonly summary: TurnSummary }

/** Columns before the label: one line glyph and a space, so the label starts at the draft's column. */
const RULE_LEAD = COLUMN.rail

/**
 * How a rule's row divides between its label and the line after it.
 *
 * The label is cut before the line is: a space and two cells of line always
 * follow it, so the row still reads as a rule. Too narrow for that, the rule
 * drops its label and is line from edge to edge.
 *
 * @param columns - the row's width.
 * @param label - the label's full width in cells; 0 for none.
 * @returns the label's drawn width and the line cells after it.
 */
export function ruleRoom(columns: number, label: number): { readonly label: number, readonly tail: number } {
  const room = columns - RULE_LEAD - 3
  if (label <= 0 || room < 1) return { label: 0, tail: Math.max(0, columns) }
  const shown = Math.min(label, room)
  return { label: shown, tail: columns - RULE_LEAD - shown - 1 }
}

/**
 * The rule over the composer, and the turn's light.
 *
 * One row, exactly the terminal's width, that separates the input from the
 * conversation and says what the session is doing: `─ ⠠⠞⠁ Kneading…  writing · 12s ───━━━───`.
 * While a turn runs its word sits in the rule and a band of light sweeps along
 * the line after it, then rests; the light is drawn in a heavier glyph, so it
 * still moves under `NO_COLOR`. When the turn ends the same row holds how it
 * ended until the next one starts, so the input never moves between the two.
 * Idle, it is a dim line.
 *
 * The row animates on the surface's shared beat and only with a clock:
 * without one — a screen reader or a test — the glyph rests, the light is
 * off, and the elapsed time is left out, so nothing on it changes by itself.
 * With `motion` off only the seconds advance. The row is redrawn only on the
 * beats where the glyph, the light, or the seconds change.
 *
 * Its glyphs are the style the terminal was resolved to draw, because a
 * full-width run of East Asian Ambiguous glyphs is the one place such a
 * character accumulates error across a row.
 *
 * @param props.columns - terminal width; the row takes all of it.
 * @param props.frame - line glyphs this terminal can draw.
 * @param props.state - what the rule says; absent, a bare line.
 * @param props.clock - time source; absent disables motion and the elapsed time.
 * @param props.motion - whether the glyph cycles and the light sweeps; defaults to true.
 * @param props.compact - use a static ASCII chevron for screen readers.
 */
export function Rule({ columns, frame, state, clock, motion = true, compact = false }: {
  readonly columns: number
  readonly frame: FrameStyle
  readonly state?: RuleState | undefined
  readonly clock?: Clock | undefined
  readonly motion?: boolean
  readonly compact?: boolean
}): React.ReactElement {
  const glyphs = RULE[frame]
  const running = state?.kind === 'running' ? state : undefined
  const moving = running !== undefined && !compact && motion && clock !== undefined
  const lit = moving && running.color === ACCENT.base
  const title = running === undefined ? '' : `${running.word}\u2026`
  const detailsAt = (elapsed: number): string => running === undefined ? ''
    : [running.phase, clock === undefined ? undefined : formatElapsed(elapsed)]
      .filter((part): part is string => part !== undefined).join(' \u00b7 ')
  const glyphAt = (elapsed: number): string => compact ? '>' : moving ? spinnerFrame(elapsed) : SPINNER_REST
  const labelAt = (elapsed: number): string => {
    if (running !== undefined) {
      const details = detailsAt(elapsed)
      return `${glyphAt(elapsed)} ${title}${details === '' ? '' : `  ${details}`}`
    }
    if (state?.kind === 'ended') {
      const { summary } = state
      return `${OUTCOME[summary.outcome].glyph} ${summary.label}${summary.details === '' ? '' : `  ${summary.details}`}`
    }
    return ''
  }
  const roomAt = (elapsed: number) => ruleRoom(columns, stringWidth(labelAt(elapsed)))
  // What would change on screen: the seconds always, and while moving the
  // glyph and where the light is. Unsubscribed when the work ends: no beat
  // outlives it. The render that ends it still reads the key once before the
  // subscription goes, so the key holds for a rule with nothing running.
  const now = useBeat(running !== undefined && clock !== undefined, time => {
    if (running === undefined) return ''
    const elapsed = time - running.startedAt
    const { tail } = roomAt(elapsed)
    return `${formatElapsed(elapsed)}${moving ? `|${spinnerFrame(elapsed)}` : ''}${lit ? `|${shimmerStep(tail, elapsed, lightStride(tail)) ?? ''}` : ''}`
  }) ?? clock?.now() ?? running?.startedAt ?? 0
  const elapsed = running === undefined ? 0 : Math.max(0, now - running.startedAt)
  const { label, tail } = roomAt(elapsed)
  const levels = lit ? shimmer(tail, elapsed, lightStride(tail)) : []
  const line = runsOf(Array.from({ length: tail }, (_, index) => levels[index] ?? 0)).map(([level, start, end]) => level === 0
    ? <Text key={start} dimColor>{glyphs.line.repeat(end - start)}</Text>
    : <Text key={start} bold color={ACCENT.ramp[level] ?? ACCENT.base}>{glyphs.light.repeat(end - start)}</Text>)
  if (label === 0) return <Box width={columns} flexShrink={0} overflowX="hidden"><Text>{line}</Text></Box>
  const details = running === undefined ? '' : detailsAt(elapsed)
  const content = running !== undefined
    ? <>
      <Text color={running.color}>{glyphAt(elapsed)}</Text>{' '}
      <Text color={running.color} bold>{title}</Text>
      {details === '' ? null : <Text dimColor>{`  ${details}`}</Text>}
    </>
    : state?.kind === 'ended'
      ? <>
        <Text color={OUTCOME[state.summary.outcome].color} bold>{`${OUTCOME[state.summary.outcome].glyph} ${state.summary.label}`}</Text>
        {state.summary.details === '' ? null : <Text dimColor>{`  ${state.summary.details}`}</Text>}
      </>
      : null
  return (
    <Box width={columns} flexDirection="row" flexShrink={0} overflowX="hidden">
      <Box width={RULE_LEAD} flexShrink={0}><Text dimColor>{`${glyphs.line} `}</Text></Box>
      <Box width={label} flexShrink={0}><Text wrap="truncate-end">{content}</Text></Box>
      <Box width={1 + tail} flexShrink={0}><Text>{' '}{line}</Text></Box>
    </Box>
  )
}

/**
 * Consecutive cells at one light level, as the runs a row is drawn in: at
 * most seven, and one while the light rests.
 * @param levels - a level per cell.
 * @returns `[level, start, end)` for each run, in order.
 */
function runsOf(levels: readonly number[]): readonly (readonly [number, number, number])[] {
  const runs: [number, number, number][] = []
  levels.forEach((level, index) => {
    const last = runs.at(-1)
    if (last !== undefined && last[0] === level) last[2] = index + 1
    else runs.push([level, index, index + 1])
  })
  return runs
}

/**
 * A titled list of harness-owned state, standing between the turn and the chrome.
 *
 * Queued input and staged attachments are the same thing shaped differently: a
 * coloured title naming what is held, the held items, and one dim line naming
 * the command that clears them. Both lists are as long as the user made them
 * and each item is as long as whatever it describes, so both need §5's bound —
 * and the bound is what keeps the composer and status line on screen while the
 * user reads them.
 *
 * Windowed and capped for the two different reasons `Notice` is: the window
 * answers more items than fit, and the cap answers one item that wraps to more
 * rows than it is lines. The footer stays inside the window, because the
 * command it names is how the user makes the panel go away.
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
  // The title and the footer are the panel's own rows, so the items share what
  // is left; a limit too small for both leaves the title, which is the row that
  // says why the others are missing.
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
 * The head of a block of held state, such as the task list or the subagents:
 * a marker in the rail, the block's name in bold, and a dim count after it.
 *
 * The same shape heads a step's calls in the transcript, so a block with a
 * head and items hanging from it reads the same above the input as in it.
 *
 * @param props.color - the marker's semantic colour; absent, the marker is dim.
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
 * One item hanging from a {@link SectionHead}: a dim branch in the rail, the
 * item's state glyph, and its text on one truncated row, so a panel is
 * exactly as tall as it has items and its budget holds.
 *
 * @param props.last - whether it is the block's last item, which closes the tree.
 * @param props.glyph - the item's state, one cell; absent, the text starts in its place.
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
 * Bounded, because everything drawn here is charged to the budget that keeps
 * Ink off its screen-clearing path, and because every row it takes moves the
 * status line and composer down the screen while the user is reading them. A
 * command whose full output matters returns that output instead, so it commits
 * to the transcript where the terminal can scroll it; what is left here is
 * short feedback, and this bound is what holds when something forgets that.
 *
 * The line count is windowed and the height is capped, because the two bound
 * different things: windowing answers a notice with more lines than fit, and
 * the cap answers one long line, which wraps to more rows than it is lines.
 * Text still wraps rather than being cut mid-sentence — a notice the user
 * cannot finish reading is worse than a ragged one, and the footer says when
 * lines were left out.
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

/** Primary status text, drawn in the terminal's normal foreground without dimming. */
export interface PrimaryField {
  readonly text: string
}

/** One status-line field: dim supporting text, primary text, or a measured reading. */
export type StatusField = string | PrimaryField | MeasuredField

/** A field's text as drawn, for measuring independently of emphasis. */
const textOf = (field: StatusField): string => typeof field === 'string' ? field
  : 'text' in field ? field.text : `${field.label} ${field.value}`

/**
 * The status line: one left-packed list of fields below the composer.
 *
 * The fields sit two spaces apart in priority order and stop where they end.
 * The composer's rule is what separates the chrome from the transcript, so
 * the row does not also have to: justifying it to both edges would open a gap
 * the width of the terminal between the model and the context meter, which
 * reads as two unrelated rows rather than one bar.
 *
 * Model and context fields use the normal foreground; supporting fields stay
 * dim. What the session is doing is
 * the turn header's to say, above the frame, in colour and in words; a second,
 * coloured state word here said it again on every frame, and colouring the
 * model or the path would make them change colour for reasons that have
 * nothing to do with them. A {@link MeasuredField} is the exception: its value
 * is a reading or access boundary, and its colour follows that value.
 *
 * The line never wraps, because a wrapped status line silently spends a row of
 * the live region's budget. Width is yielded in priority order rather than
 * shared once the fields do reach the edge. The last right field is the
 * unbounded one — a deep working directory — and it yields first, truncating
 * from the start so it keeps the workspace's name. The other right fields are
 * bounded, and a bounded field is dropped whole rather than cut: `cache hi`
 * reads as a different number, and a clipped context meter as a smaller one.
 * They drop from the end of the list, so the caller orders them by priority.
 * Optional badges reserve their width before the model and supporting fields.
 * The first (permission) takes priority when both do not fit.
 * The left cluster truncates from the end within its remaining space. Widths are
 * measured in terminal cells, so a CJK field is not laid out by code-point
 * length.
 *
 * @param props.left - fields that identify the session, highest priority first.
 * @param props.badge - labelled value whose width takes priority over every other field.
 * @param props.secondaryBadge - second labelled value, retained if it fits beside the first.
 * @param props.right - supporting fields, highest priority first, except the
 *   unbounded field, which goes last: it is the one that yields room, and it
 *   keeps its tail rather than its head.
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
  const headWidth = Math.max(0, Math.min(stringWidth(head), columns - badgesWidth - (badges.length === 0 ? 0 : FIELD_GAP.length)))
  const used = headWidth + badgesWidth + (badges.length > 0 && headWidth > 0 ? FIELD_GAP.length : 0)
  const kept = used >= columns ? [] : fitting(right, columns - used, used > 0)
  const last = kept.length - 1
  return (
    // `overflowX`: the last guard against a wrapped status line. Below the
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
          gap is a margin rather than text, so truncating a field's head cannot
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
    const width = (kept.length > 0 || after ? FIELD_GAP.length : 0) + stringWidth(textOf(field))
    if (used + width > room) break
    kept.push(field)
    used += width
  }
  return unbounded === undefined ? kept : [...kept, unbounded]
}

/**
 * The rule, the composer under it, and the status line beneath them.
 *
 * Up to five rows: a blank, the rule, the composer's first row, a blank
 * padding row, then the status line. The rule is the separator and the turn's
 * light at once ({@link Rule}): it says where typing lands without a box, and
 * the word and light the eye checks for "is it still working" sit directly
 * on the input rather than on a header of their own. The padding row keeps
 * the status line clear of the draft, so the metadata reads as a footer
 * rather than as a second line of input.
 *
 * The blank above buys the one thing the rule cannot: without it the rule
 * sits directly under the last line of the answer and reads as part of it.
 * Anything that belongs to the input rather than to the conversation — a
 * completion list, a notice, queued input — is drawn between the blank and
 * the rule, so the blank opens the whole stack instead of splitting it.
 *
 * Every row is one row at any width, so the chrome's height depends on the
 * terminal's rows alone. The rule is exactly the terminal's width; a narrowing
 * reflows it, which the application answers by repainting (see `useRepaint`).
 * The label, the draft, and the status line all start at the rail's column,
 * so the three rows read as one block. Short terminals yield the gap, the
 * padding, the status line, and then the rule to keep the input row visible.
 *
 * What a key does appears in the composer's right slot at the moment it
 * applies. A standing hint would be a further row the user has already read.
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
 * @param props.light - what the rule says; see {@link Rule}.
 * @param props.clock - time source for the rule's motion.
 * @param props.motion - whether the rule's glyph cycles and its light sweeps.
 * @param props.compact - draw the rule's glyph for a screen reader.
 * @param props.layout - structure that fits in the available terminal height.
 * @param props.children - panels drawn above the rule, each within its own
 *   claimed rows; they are not counted in `layout.rows`.
 */
export function Chrome({ left, right, badge, secondaryBadge, columns, state, before, after, placeholder, hints, maxRows, frame, light, clock, motion, compact, layout = chromeFor(columns), children }: {
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
  readonly light?: RuleState | undefined
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
      {layout.rule && <Rule columns={columns} frame={frame} state={light} clock={clock}
        {...motion === undefined ? {} : { motion }} {...compact === undefined ? {} : { compact }} />}
      <Composer
        columns={columns}
        marker={MARKER.prompt}
        before={before}
        after={after}
        placeholder={placeholder}
        {...maxRows === undefined ? {} : { maxRows }}
        {...hint === undefined || columns < HINT_MIN_COLUMNS ? {} : { hint: hints[hint] }}
      />
      {layout.pad && <Text> </Text>}
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
 * The caret is laid out around the draft rather than in it, so moving through
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
 * A Block Element, so East Asian Ambiguous: a CJK-configured terminal may draw
 * it two cells wide. That is accepted here and nowhere else, because the
 * alternative is an attribute that disappears exactly when colour does.
 */
const CARET = '\u258c'

/**
 * Candidate list above the composer.
 *
 * Selection is marked three ways, because no single one survives every terminal:
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
  // Sized to the content: a fixed column wraps a file path onto a second row and
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
