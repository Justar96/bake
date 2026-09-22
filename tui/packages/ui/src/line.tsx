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
import { COLUMN, MARKER, windowOf, type Budget } from './layout.ts'
import { hintFor, present, styleOf, tailLines, type ComposerState, type Hint, type PresentedLine } from './present.ts'
import type { Row } from './rows.ts'

/**
 * One display line.
 *
 * Prose wraps at the measure however wide the terminal is; tool output takes
 * the full width, because wrapping a log to a narrow measure destroys the
 * alignment that makes it scannable. Neither is ever truncated: a result the
 * user cannot finish reading is worse than a ragged one.
 *
 * @param props.line - the placed line.
 * @param props.budget - budgets for the current terminal size.
 */
export function Line({ line, budget }: {
  readonly line: PresentedLine
  readonly budget: Budget
}): React.ReactElement {
  const style = styleOf(line.tone)
  const indented = line.column === COLUMN.output
  // `exactOptionalPropertyTypes` rejects an explicit undefined, so an absent
  // colour is an absent prop.
  const colored = style.color === undefined ? {} : { color: style.color }
  return (
    // `flexShrink={0}`: inside a region held at a fixed height, a shrinkable
    // line lets Yoga squash every line a little instead of pushing the oldest
    // ones off the top, which drops lines out of the middle of the stream.
    <Box flexDirection="row" flexShrink={0}>
      <Box width={COLUMN.rail} flexShrink={0}>
        <Text bold={style.bold} {...colored}>{line.marker}</Text>
      </Box>
      {indented
        ? (
          <Box width={COLUMN.verb} flexShrink={0}>
            <Text dimColor={line.verb !== ''} {...colored}>{line.verb}</Text>
          </Box>
          )
        : null}
      <Box width={indented ? budget.output : budget.measure}>
        <Text bold={style.bold} dimColor={style.dim} wrap="wrap" {...colored}>{line.text}</Text>
      </Box>
    </Box>
  )
}

/**
 * The in-flight turn: streaming answer and reasoning, above the chrome.
 *
 * Two rules from `tui/DESIGN-LAYOUT.md` meet here, and both are about height
 * rather than content.
 *
 * L1 caps the whole dynamic region, so the stream is tail-windowed: the last
 * `budget.live` lines are drawn and earlier ones are dropped. Nothing is lost —
 * the turn commits every line to the transcript, where the terminal's own
 * scrollback holds it.
 *
 * L2 holds the region still. Ink erases the previous dynamic block and rewrites
 * it each frame, so a region that grows line by line scrolls the terminal and
 * moves the status line and composer under the user's eyes on every chunk that
 * arrives. While a turn runs the region is held at `hold` rows whatever it
 * currently draws; at rest `hold` is absent, so a finished answer is not
 * followed by empty space.
 *
 * The height is held by the layout engine rather than by counting blank rows,
 * because a line that wraps occupies more rows than it is lines: prose wraps at
 * the measure, tool output at the terminal width, and both depend on display
 * width, which is exactly what Yoga already computes. Holding the box instead
 * of padding the list keeps the height exact when a line wraps, which is the
 * common case for streamed prose.
 *
 * Overflow clips from the top, because `justifyContent` pins the content to the
 * bottom: the newest line is always the one the user can see.
 *
 * @param props.rows - live rows in arrival order.
 * @param props.budget - budgets for the current terminal size.
 * @param props.limit - lines the region may draw, at most `budget.live`.
 * @param props.hold - height to hold, or undefined to let the region shrink.
 */
export function LiveRegion({ rows, budget, limit, hold }: {
  readonly rows: readonly Row[]
  readonly budget: Budget
  readonly limit: number
  readonly hold: number | undefined
}): React.ReactElement | null {
  // Windowed before measurement, not only for display: a long turn accumulates
  // more lines than any terminal can show, and handing all of them to Yoga
  // makes every frame cost the length of the turn.
  const lines = tailLines(rows.flatMap(present), limit)
  if (lines.length === 0 && hold === undefined) return null
  return (
    <Box
      flexDirection="column"
      flexShrink={0}
      justifyContent="flex-end"
      overflowY="hidden"
      {...hold === undefined ? {} : { height: hold }}
    >
      {lines.map((line, index) => <Line key={index} line={line} budget={budget} />)}
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
 * The status line: a left cluster and a right cluster pushed to the edges.
 *
 * Nothing in a transcript is right-aligned, so a row filled to both edges reads
 * as chrome before a word of it is read. That separation costs no rows and no
 * extra rule, which is why it survives NO_COLOR.
 *
 * The first left field is the session state and the only coloured word. The
 * model beside it, and everything on the right, stays dim: colouring them would
 * make the model and the path change colour for reasons that have nothing to
 * do with them.
 *
 * The line never wraps, because a wrapped status line silently spends a row of
 * the live region's budget. Width is yielded in priority order rather than
 * shared: the right cluster gives up room first and truncates from the start,
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
      {/* Grows into slack, never shrinks: the state and the model are what the
          row exists to say, and a path is not allowed to push them out. */}
      <Box flexGrow={1} flexShrink={0}>
        <Text wrap="truncate-end">
          <Text {...colored}>{head ?? ''}</Text>
          {secondary === '' ? null : <Text dimColor>{`${FIELD_GAP}${secondary}`}</Text>}
        </Text>
      </Box>
      {/* One box per field, because only one of them is unbounded. Shrinking
          the cluster as a whole cuts from wherever the join happens to fall,
          which costs a short bounded field to shorten a long one. Only the
          last field yields, from the start, so the caller puts the unbounded
          field there and keeps its tail — the workspace, not the mount point. */}
      {right.map((field, index) => (
        <Box key={index} flexShrink={index === last ? 1 : 0} marginLeft={index === 0 ? 1 : 0}>
          <Text dimColor wrap={index === last ? 'truncate-start' : 'truncate-end'}>
            {index === 0 ? field : `${FIELD_GAP}${field}`}
          </Text>
        </Box>
      ))}
    </Box>
  )
}

/**
 * The status line and the composer.
 *
 * Two rows. Every row here is charged to the budget that keeps Ink off its
 * screen-clearing path, on every frame, for the whole session. The status row
 * is the one filled to both edges. The composer is the next row and opens
 * with the prompt marker.
 *
 * What a key does appears in the composer's right slot at the moment it
 * applies. A standing hint would be a third row the user has already read.
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
 */
export function Chrome({ left, right, columns, color, state, before, after, placeholder, hints }: {
  readonly left: readonly string[]
  readonly right: readonly string[]
  readonly columns: number
  readonly color?: string
  readonly state: Omit<ComposerState, 'drafting'>
  readonly before: string
  readonly after: string
  readonly placeholder: string
  readonly hints: Readonly<Record<Exclude<Hint, undefined>, string>>
}): React.ReactElement {
  const hint = hintFor({ ...state, drafting: `${before}${after}` !== '' })
  const colored = color === undefined ? {} : { color }
  return (
    <Box flexDirection="column">
      <StatusBar left={left} right={right} columns={columns} {...colored} />
      <Composer
        marker={MARKER.prompt}
        before={before}
        after={after}
        placeholder={placeholder}
        {...hint === undefined ? {} : { hint: hints[hint] }}
      />
    </Box>
  )
}

/**
 * The input line.
 *
 * Its right slot holds contextual state and nothing else: a permanent hint
 * teaches nothing after the first day and becomes noise on the surface a user
 * looks at most. Nothing here animates, for the same reason. A draft taller
 * than `maxRows` keeps the caret row in view and marks the hidden lines above
 * with `^` in the rail.
 *
 * @param props.marker - prompt marker.
 * @param props.before - draft text before the caret.
 * @param props.after - draft text after the caret.
 * @param props.placeholder - locale-owned prompt text.
 * @param props.hint - contextual right-slot text; empty or absent leaves the
 *   slot unused, which a caller chooses when the surface already says it.
 * @param props.maxRows - rows kept on screen. A taller draft scrolls so the caret stays visible.
 */
export function Composer({ marker, before, after, placeholder, hint, maxRows = 5 }: {
  readonly marker: string
  readonly before: string
  readonly after: string
  readonly placeholder: string
  readonly hint?: string
  readonly maxRows?: number
}): React.ReactElement {
  const empty = before === '' && after === ''
  const lines = empty ? [''] : `${before}${after}`.split('\n')
  const caretRow = empty ? 0 : before.split('\n').length - 1
  const caretColumn = empty ? 0 : (before.split('\n').at(-1) ?? '').length
  // Window so the caret row is the last visible one. A draft taller than its
  // rows must never hide the line being typed, whichever line that is.
  const start = Math.max(0, Math.min(caretRow - maxRows + 1, lines.length - maxRows))
  const visible = lines.slice(Math.max(0, start), Math.max(0, start) + maxRows)
  return (
    <Box flexDirection="column">
      {visible.map((line, index) => {
        const absolute = Math.max(0, start) + index
        const caret = absolute === caretRow
        const promptLine = absolute === 0
        // Lines above the window are still in the draft. The rail says so,
        // because the prompt marker belongs to the first line and has scrolled off.
        const overflow = !promptLine && index === 0 && start > 0
        const railColor = promptLine ? { color: 'cyan' as const } : {}
        return (
          <Box key={absolute} flexDirection="row">
            <Box width={COLUMN.rail} flexShrink={0}>
              <Text bold={promptLine} dimColor={overflow} {...railColor}>
                {promptLine ? marker : overflow ? '^' : MARKER.none}
              </Text>
            </Box>
            <Box flexGrow={1}>
              {/* A drawn caret, not inverse video: ANSI attributes vanish under
                  NO_COLOR and in any non-TTY frame, which would leave the user
                  with no cursor at all. */}
              {caret
                ? (
                  <Text>
                    {line.slice(0, caretColumn)}{CARET}{line.slice(caretColumn)}
                    {/* The placeholder follows the caret rather than replacing
                        it: an empty composer still has to show where typing
                        will land. */}
                    {empty ? <Text dimColor>{placeholder}</Text> : null}
                  </Text>
                  )
                : <Text>{line}</Text>}
            </Box>
            {hint === undefined || hint === '' || index !== visible.length - 1
              ? null
              : (
                <Box flexShrink={0} marginLeft={2}>
                  <Text dimColor>{hint}</Text>
                </Box>
                )}
          </Box>
        )
      })}
    </Box>
  )
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
 * Candidate list under the composer.
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
              <Text dimColor={!active}>{item.description}</Text>
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
