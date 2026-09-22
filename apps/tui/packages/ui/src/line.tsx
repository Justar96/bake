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

import React, { useEffect, useState } from 'react'
import { Box, Text } from 'ink'
import stringWidth from 'string-width'
import wrapAnsi from 'wrap-ansi'
import { chromeFor, COLUMN, COMPOSER_BUDGET, HINT_MIN_COLUMNS, MARKER, windowOf, type Budget, type ChromeLayout, type FrameStyle } from './layout.ts'
import { hintFor, isBlank, present, styleOf, tailLines, type ComposerState, type Hint, type LineStyle, type PresentedLine, type ResultBound } from './present.ts'
import type { Row } from './rows.ts'
import { ACCENT, formatElapsed, glint, SPINNER_MS, SPINNER_REST, spinnerFrame, type Clock, type Outcome, type TurnSummary } from './activity.ts'

/**
 * One display line.
 *
 * Prose wraps at the measure however wide the terminal is; tool output takes
 * the full width, because wrapping a log to a narrow measure destroys the
 * alignment that makes it scannable. Neither is ever truncated: a result the
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
  return (
    // `flexShrink={0}`: inside a region held at a fixed height, a shrinkable
    // line lets Yoga squash every line a little instead of pushing the oldest
    // ones off the top, which drops lines out of the middle of the stream.
    <Box flexDirection="row" flexShrink={0}>
      <Box width={COLUMN.rail} flexShrink={0}>
        {line.pulse === true
          ? <Pulse glyph={line.marker} clock={clock} />
          : line.markerTone === undefined
            ? <Text bold={style.bold} {...colored}>{line.marker}</Text>
            : <Text bold={marker.bold} dimColor={marker.dim} {...colorOf(marker)}>{line.marker}</Text>}
      </Box>
      {indented
        ? (
          <Box width={COLUMN.verb} flexShrink={0}>
            <Text bold={verb.bold || line.verbTone !== undefined} dimColor={verb.dim} {...colorOf(verb)}>{line.verb}</Text>
          </Box>
          )
        : null}
      <Box width={textWidth(line, budget)}>
        <Text bold={style.bold} dimColor={style.dim} wrap="wrap" {...colored}>
          {line.divider ? '-'.repeat(budget.measure) : line.spans === undefined ? line.text : spansOf(line)}
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
    parts.push(<Text key={index} bold={style.bold} dimColor={style.dim} {...colorOf(style)}>{line.text.slice(offset, offset + span.length)}</Text>)
    offset += span.length
  }
  if (offset < line.text.length) parts.push(line.text.slice(offset))
  return parts
}

/** Milliseconds a pulsing marker spends lit, and then as long dimmed. */
export const PULSE_MS = 480

/**
 * A marker that breathes while its action runs: lit in the header's accent,
 * then dimmed, in place. Only its colour changes, so the row never moves and
 * a terminal rewrites one cell.
 *
 * @param props.glyph - the marker.
 * @param props.clock - time source; absent, the marker holds lit.
 */
function Pulse({ glyph, clock }: { readonly glyph: string, readonly clock: Clock | undefined }): React.ReactElement {
  const [lit, setLit] = useState(true)
  // Disposed on unmount, which is when the action finishes and leaves the
  // live region: no tick outlives it.
  useEffect(() => clock === undefined ? undefined : clock.every(PULSE_MS, () => { setLit(on => !on) }), [clock])
  return <Text color={ACCENT.base} bold={lit} dimColor={!lit}>{glyph}</Text>
}

/**
 * Columns a line's text wraps at: prose at the measure, tool output at the full width.
 * @param line - the placed line.
 * @param budget - budgets for the current terminal size.
 * @returns the width of the text box `Line` draws.
 */
const textWidth = (line: PresentedLine, budget: Budget): number =>
  line.column === COLUMN.output ? line.prose === true ? Math.min(budget.output, budget.measure) : budget.output : budget.measure

/**
 * Rows a line occupies once wrapped, measured the way Ink wraps `Text`.
 * @param line - the placed line.
 * @param budget - budgets for the current terminal size.
 * @returns at least one.
 */
export const lineHeight = (line: PresentedLine, budget: Budget): number =>
  line.divider === true || line.text === '' ? 1 : wrapAnsi(line.text, textWidth(line, budget), { hard: true, trim: false }).split('\n').length

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
 * The turn's header: a spinner, its word, and what it is doing, held for the
 * whole turn directly above the composer.
 *
 * Everything that streams changes somewhere below the last committed line;
 * this row is the one thing that stays put from the first chunk to the last,
 * so the eye has a fixed place to learn that the agent is still working. Only
 * its glyph, its phase, and its clock change, and they change in place.
 *
 * The component owns its animation rather than the application re-rendering
 * the whole surface on every frame, and it animates only with a clock: without
 * one — a screen reader or a test — it draws a resting glyph and leaves the
 * elapsed time out, so nothing on the row changes by itself. With a clock and
 * `motion` off — `NO_COLOR` — the glyph rests and only the seconds advance.
 *
 * @param props.word - the turn's word, chosen once when it started.
 * @param props.phase - what the turn is doing, absent while it waits on the model.
 * @param props.ticker - the newest reasoning line, absent unless reasoning.
 * @param props.startedAt - clock time the turn started.
 * @param props.clock - time source; absent disables motion and the elapsed time.
 * @param props.motion - whether the glyph cycles and the word glints; defaults to true.
 * @param props.color - colour for the glyph and word; the accent glints while it moves.
 * @param props.limit - rows it may draw: the header, then the ticker.
 * @returns the header, or null without room.
 */
export function Activity({ word, phase, ticker, startedAt, clock, motion = true, color, limit }: {
  readonly word: string
  readonly phase: string | undefined
  readonly ticker: string | undefined
  readonly startedAt: number
  readonly clock: Clock | undefined
  readonly motion?: boolean
  readonly color: string
  readonly limit: number
}): React.ReactElement | null {
  const [now, setNow] = useState(() => clock?.now() ?? startedAt)
  const moving = motion && clock !== undefined
  // Disposed on unmount, which is when the turn ends: no tick outlives it.
  useEffect(() => clock === undefined ? undefined : clock.every(moving ? SPINNER_MS : SECOND_MS, () => { setNow(clock.now()) }), [clock, moving])
  if (limit <= 0) return null
  const elapsed = now - startedAt
  const title = `${word}\u2026`
  // By grapheme, so a CJK word's characters are lit whole.
  const letters = Array.from(title)
  const lit = glint(letters.length, elapsed)
  const details = [phase, clock === undefined ? undefined : formatElapsed(elapsed)]
    .filter((part): part is string => part !== undefined).join(' \u00b7 ')
  return (
    <Box flexDirection="column" flexShrink={0}>
      {/* One row replaced in place, never a growing block: reasoning arrives
          faster than it can be read, and drawn row by row it scrolls the whole
          surface with it. Truncated, because a wrapped ticker is a row that
          appears and disappears with the length of a sentence. Above the
          header, with the output it summarizes, so the header is always the
          row resting on the input and nothing streams beneath it. */}
      {ticker !== undefined && limit >= 2 && <Box flexDirection="row" flexShrink={0}>
        <Box width={COLUMN.rail} flexShrink={0}><Text> </Text></Box>
        <Text dimColor wrap="truncate-end">{ticker}</Text>
      </Box>}
      <Box flexDirection="row" flexShrink={0}>
        <Box width={COLUMN.rail} flexShrink={0}>
          <Text color={color}>{moving ? spinnerFrame(elapsed) : SPINNER_REST}</Text>
        </Box>
        <Text wrap="truncate-end">
          {!moving || color !== ACCENT.base
            ? <Text color={color} bold>{title}</Text>
            : lit.map((on, index) => <Text key={index} color={on ? ACCENT.glint : ACCENT.base} bold>{letters[index]}</Text>)}
          {details === '' ? null : <Text dimColor>{`  ${details}`}</Text>}
        </Text>
      </Box>
    </Box>
  )
}

/** Tick of a header whose glyph rests: only its seconds change. */
const SECOND_MS = 1000

/** Glyph and colour for each way a turn ends. */
const OUTCOME: Readonly<Record<Outcome, { readonly glyph: string, readonly color: string }>> = {
  done: { glyph: '\u2713', color: 'green' },
  stopped: { glyph: '\u25a0', color: 'yellow' },
  failed: { glyph: '\u2717', color: 'red' },
}

/**
 * What the turn header becomes when the turn ends: how it ended, how long it
 * took, and what it did, on the same row until the next turn replaces it.
 *
 * The header was the one fixed row while the turn ran; removing it at the end
 * moved the input up by a row and left the reader to find the outcome in the
 * scrollback. Kept, it reads the answer to the question the running header
 * posed, in the same place.
 *
 * @param props.summary - the finished turn's outcome and details.
 * @param props.limit - rows it may draw.
 * @returns the summary row, or null without room.
 */
export function Summary({ summary, limit }: { readonly summary: TurnSummary, readonly limit: number }): React.ReactElement | null {
  if (limit <= 0) return null
  const { glyph, color } = OUTCOME[summary.outcome]
  return (
    <Box flexDirection="row" flexShrink={0}>
      <Box width={COLUMN.rail} flexShrink={0}><Text color={color} bold>{glyph}</Text></Box>
      <Text wrap="truncate-end">
        <Text color={summary.outcome === 'done' ? ACCENT.base : color} bold>{summary.label}</Text>
        {summary.details === '' ? null : <Text dimColor>{`  ${summary.details}`}</Text>}
      </Text>
    </Box>
  )
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
  readonly color: 'yellow' | 'cyan'
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
      {shown.map((line, index) => <Text key={index} color="yellow" wrap="wrap">{line}</Text>)}
      {hidden === 0 ? null : <Text dimColor>{`+${hidden} ${more}`}</Text>}
    </Box>
  )
}

/**
 * The status line: one left-packed list of fields below the composer's frame.
 *
 * The fields sit two spaces apart in priority order and stop where they end.
 * The composer's border is what separates the chrome from the transcript, so
 * the row does not also have to: justifying it to both edges would open a gap
 * the width of the terminal between the model and the context meter, which
 * reads as two unrelated rows rather than one bar.
 *
 * The first left field is the session state and the only coloured word. The
 * model beside it, and everything on the right, stays dim: colouring them would
 * make the model and the path change colour for reasons that have nothing to
 * do with them.
 *
 * The line never wraps, because a wrapped status line silently spends a row of
 * the live region's budget. Width is yielded in priority order rather than
 * shared once the fields do reach the edge: the right cluster gives up room
 * first and truncates from the start,
 * so an unbounded field there — a deep working directory — cannot squeeze the
 * state and the model out of a row that exists to show them. The left cluster
 * yields only below the width it needs on its own, where it truncates from the
 * end and the row clips rather than wrapping. Within the right cluster only the
 * last field yields, so a bounded field is never shortened to make room for an
 * unbounded one. Yoga measures the fields, so a CJK cluster is not laid out by
 * code-point length.
 *
 * @param props.left - fields that identify the session, highest priority first.
 *   The first field is the state word.
 * @param props.right - supporting fields, each laid out on its own. Put the
 *   unbounded field last: it is the only one that yields room, and it keeps
 *   its tail rather than its head.
 * @param props.columns - terminal width.
 * @param props.color - colour for the state word.
 */
export function StatusBar({ left, right, columns, color }: {
  readonly left: readonly string[]
  readonly right: readonly string[]
  readonly columns: number
  readonly color?: string
}): React.ReactElement {
  const [head, ...rest] = left
  const secondary = rest.join(FIELD_GAP)
  const colored = color === undefined ? {} : { color }
  const last = right.length - 1
  return (
    // `overflowX`: the last guard against a wrapped status line. Below the
    // width the left cluster alone needs, there is no room left to yield, and
    // clipping costs a few characters where wrapping costs a row of the live
    // region on every frame.
    <Box width={columns} flexDirection="row" overflowX="hidden">
      {/* Never shrinks: the state and the model are what the row exists to
          say, and a path is not allowed to push them out. */}
      <Box flexShrink={0}>
        <Text wrap="truncate-end">
          <Text {...colored}>{head ?? ''}</Text>
          {secondary === '' ? null : <Text dimColor>{`${FIELD_GAP}${secondary}`}</Text>}
        </Text>
      </Box>
      {/* One box per field, because only one of them is unbounded. Shrinking
          the cluster as a whole cuts from wherever the join happens to fall,
          which costs a short bounded field to shorten a long one. Only the
          last field yields, from the start, so the caller puts the unbounded
          field there and keeps its tail — the workspace, not the mount point.
          The gap is a margin rather than text, so truncating a field's head
          cannot eat the separator that tells it apart from the one before. */}
      {right.map((field, index) => (
        <Box
          key={index}
          flexShrink={index === last ? 1 : 0}
          marginLeft={index === 0 && head === undefined ? 0 : FIELD_GAP.length}
        >
          <Text dimColor wrap={index === last ? 'truncate-start' : 'truncate-end'}>{field}</Text>
        </Box>
      ))}
    </Box>
  )
}

/**
 * The framed composer and the status line beneath it.
 *
 * Up to five rows: a blank, a box holding the composer's first row, then the
 * status line. The blank is the only row on this surface spent on nothing at
 * all, and it buys the one thing the box cannot: without it the box's top edge
 * sits directly under the last line of the answer, and the input reads as
 * attached to the output rather than as the place the next turn starts.
 * Anything that belongs to the input rather than to the conversation — a
 * completion list, a notice, queued input — is drawn between the blank and the
 * box, so the blank opens the whole stack instead of splitting it.
 *
 * Every row here is charged to the budget that keeps Ink off its
 * screen-clearing path, on every frame, for the whole session, so the box is
 * the one place in the dynamic region that spends rows on nothing but
 * structure. It earns them: the frame says where typing lands without needing
 * colour, alignment, or a word of copy, and it is what lets the status line
 * below stay a plain left-packed list instead of a justified bar. The status
 * line is indented to the prompt inside the frame, so it reads as the box's
 * footer rather than as one more row of the transcript. A draft taller than
 * one line grows inside the frame. Short terminals yield the gap, status, and
 * frame in that order to keep the input row visible.
 *
 * The border is whichever style the terminal can draw, resolved once at the
 * application boundary. Box-drawing characters are East Asian Ambiguous, and a
 * terminal that draws them two cells wide would double the width of a
 * full-width frame on every row — the accumulating error `layout.ts` excludes
 * from everything but a marker in a fixed-width rail. A terminal that is not
 * encoding UTF-8 writes them through as mojibake. Both fall back to ASCII.
 *
 * What a key does appears in the composer's right slot at the moment it
 * applies. A standing hint would be a further row the user has already read.
 *
 * @param props.left - session-state fields, highest priority first.
 * @param props.right - supporting fields, dropped first as width shrinks.
 * @param props.columns - terminal width.
 * @param props.color - colour for the state word.
 * @param props.state - what the surface is doing, apart from the draft, which
 *   is read from the draft text so the two cannot disagree.
 * @param props.before - draft text before the caret.
 * @param props.after - draft text after the caret.
 * @param props.placeholder - locale-owned prompt text.
 * @param props.hints - locale-owned text for each hint key.
 * @param props.maxRows - rows a draft may occupy, from `budget.composer`.
 * @param props.frame - border style this terminal can draw.
 * @param props.layout - structure that fits in the available terminal height.
 * @param props.children - panels drawn above the box, each within its own
 *   claimed rows; they are not counted in `layout.rows`.
 */
export function Chrome({ left, right, columns, color, state, before, after, placeholder, hints, maxRows, frame, layout = chromeFor(columns), children }: {
  readonly left: readonly string[]
  readonly right: readonly string[]
  readonly columns: number
  readonly color?: string
  readonly state: Omit<ComposerState, 'drafting'>
  readonly before: string
  readonly after: string
  readonly placeholder: string
  readonly hints: Readonly<Record<Exclude<Hint, undefined>, string>>
  readonly maxRows?: number
  readonly frame: FrameStyle
  readonly layout?: ChromeLayout
  readonly children?: React.ReactNode
}): React.ReactElement {
  const hint = hintFor({ ...state, drafting: `${before}${after}` !== '' })
  const colored = color === undefined ? {} : { color }
  // Border and padding: the prompt's column inside the frame, which the status
  // line lines up with.
  const inset = layout.frame ? 2 : 0
  return (
    <Box flexDirection="column" flexShrink={0}>
      {/* Not a margin: Ink gives a zero-height Box no row, and the gap has to
          be a row the layout counts. */}
      {layout.gap && <Text> </Text>}
      {children}
      {/* Dim, never coloured by state: the frame says where the input is, and
          a border that changed colour with the session would animate the
          largest shape on the surface for a reason that is not about it.
          Dropped outright on a terminal too narrow to spend four columns on
          structure, rather than shrunk — see `FRAME_MIN_COLUMNS`. */}
      <Box
        width={columns}
        flexDirection="column"
        flexShrink={0}
        {...layout.frame ? { borderStyle: frame, borderDimColor: true, paddingX: 1 } : {}}
      >
        <Composer
          columns={Math.max(1, columns - inset * 2)}
          marker={MARKER.prompt}
          before={before}
          after={after}
          placeholder={placeholder}
          {...maxRows === undefined ? {} : { maxRows }}
          {...hint === undefined || columns < HINT_MIN_COLUMNS ? {} : { hint: hints[hint] }}
        />
      </Box>
      {layout.status && <Box paddingLeft={inset} flexShrink={0}>
        <StatusBar left={left} right={right} columns={Math.max(1, columns - inset)} {...colored} />
      </Box>}
    </Box>
  )
}

/**
 * Input window measured in terminal rows, following the cursor within wrapped text.
 *
 * The hint shares the caret's row. Wrapping precedes windowing, so Home, End,
 * Unicode input, and edits inside a long paragraph keep the caret visible.
 *
 * @param props.columns - available width inside the composer's frame.
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
  const hintWidth = hint === undefined || hint === '' ? 0 : stringWidth(hint) + 2
  const showHint = hintWidth > 0 && columns - rail - hintWidth >= 1
  const width = Math.max(1, columns - rail - (showHint ? hintWidth : 0))
  const lines = wrapAnsi(`${before}${CARET}${after}`, width, { hard: true, trim: false }).split('\n')
  // User text may contain the caret glyph too; its occurrence before the
  // insertion identifies the actual cursor without reserving a text character.
  let preceding = before.split(CARET).length - 1
  const caretRow = lines.findIndex(line => {
    const count = line.split(CARET).length - 1
    if (count > preceding) return true
    preceding -= count
    return false
  })
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
          <Text bold={promptLine} dimColor={overflow} {...promptLine ? { color: 'cyan' } : {}}>
            {promptLine ? marker : overflow ? '^' : MARKER.none}
          </Text>
        </Box>}
        <Box width={width} flexShrink={0}>
          <Text wrap="truncate-end">{line}{empty && <Text dimColor>{placeholder}</Text>}</Text>
        </Box>
        {showHint && absolute === caretRow && <Box marginLeft={2} flexShrink={0}>
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
              <Text bold color="cyan">{active ? MARKER.selected : MARKER.none}</Text>
            </Box>
            <Box width={width} flexShrink={0}>
              <Text bold={active} wrap="truncate-start" {...active ? { color: 'cyan' } : {}}>{item.name}</Text>
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
