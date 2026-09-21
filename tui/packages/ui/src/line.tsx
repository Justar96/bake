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
import { COLUMN, type Budget } from './layout.ts'
import { styleOf, type PresentedLine } from './present.ts'

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
    <Box flexDirection="row">
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
 * The status line: a left cluster and a right cluster pushed to the edges.
 *
 * Nothing in a transcript is right-aligned, so a row filled to both edges reads
 * as chrome before a word of it is read. That separation costs no rows and no
 * colour, which is why it survives NO_COLOR where a dim rule would not.
 *
 * Fields drop from the right as width shrinks; the line never wraps, because a
 * wrapped status line silently spends a row of the live region's budget.
 *
 * @param props.left - fields that identify the session state, highest priority first.
 * @param props.right - supporting fields, dropped first.
 * @param props.columns - terminal width.
 * @param props.color - colour for the leading field.
 */
export function StatusBar({ left, right, columns, color }: {
  readonly left: readonly string[]
  readonly right: readonly string[]
  readonly columns: number
  readonly color?: string
}): React.ReactElement {
  const leftText = left.join('   ')
  const kept = [...right]
  while (kept.length > 0 && leftText.length + kept.join('   ').length + 3 > columns) kept.pop()
  const rightText = kept.join('   ')
  const gap = Math.max(1, columns - leftText.length - rightText.length)
  const colored = color === undefined ? {} : { color }
  return (
    <Text>
      <Text {...colored}>{leftText}</Text>
      <Text>{' '.repeat(gap)}</Text>
      <Text dimColor>{rightText}</Text>
    </Text>
  )
}

/**
 * The input line.
 *
 * Its right slot holds contextual state and nothing else: a permanent hint
 * teaches nothing after the first day and becomes noise on the surface a user
 * looks at most. Nothing here animates, for the same reason.
 *
 * @param props.marker - prompt marker.
 * @param props.text - current draft, or undefined to show the placeholder.
 * @param props.placeholder - locale-owned prompt text.
 * @param props.hint - contextual right-slot text, omitted when there is nothing to say.
 */
export function Composer({ marker, text, placeholder, hint }: {
  readonly marker: string
  readonly text: string | undefined
  readonly placeholder: string
  readonly hint?: string
}): React.ReactElement {
  return (
    <Box flexDirection="row">
      <Box width={COLUMN.rail} flexShrink={0}>
        <Text bold color="cyan">{marker}</Text>
      </Box>
      <Box flexGrow={1}>
        <Text dimColor={text === undefined}>{text ?? placeholder}</Text>
      </Box>
      {hint === undefined
        ? null
        : (
          <Box flexShrink={0}>
            <Text dimColor>{hint}</Text>
          </Box>
          )}
    </Box>
  )
}
