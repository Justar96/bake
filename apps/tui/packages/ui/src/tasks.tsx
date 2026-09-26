/** The agent's task list: one row above the header, and the complete checklist as a sheet. */
import React from 'react'
import { Box, Text } from 'ink'
import stringWidth from 'string-width'
import type { TuiCopy } from './copy.ts'
import { MARKER } from './layout.ts'
import { PALETTE } from './palette.ts'
import { sheetBar, type SheetLine } from './sheet.tsx'

export interface TaskEntry {
  readonly text: string
  readonly status: 'pending' | 'in_progress' | 'completed'
}

/** Cells of the progress bar. Enough to move on every task of a short plan. */
const TASK_BAR = 12

/** Fewest cells of the current task worth keeping before the key hint gives way. */
const TASK_TEXT_MIN = 12

/** The task list's shapes. Each state has its own, so `NO_COLOR` still reads. */
const TASK_GLYPH = {
  done: '✓',
  active: MARKER.selected,
  pending: '□',
  filled: '━',
  empty: '─',
} as const

const glyphOf = (status: TaskEntry['status']): string =>
  status === 'completed' ? TASK_GLYPH.done : status === 'in_progress' ? TASK_GLYPH.active : TASK_GLYPH.pending
const colorOf = (status: TaskEntry['status']) =>
  status === 'completed' ? PALETTE.done : status === 'in_progress' ? PALETTE.asking : undefined

/** Whether the row has anything left to say: a list with open work. */
export function tasksOpen(todos: readonly TaskEntry[] | undefined): boolean {
  return todos !== undefined && todos.some(item => item.status !== 'completed')
}

/**
 * The agent's task list as one row, as current state rather than history.
 *
 * Every write replaces the list, so the transcript would repeat the plan
 * with a different tick each time. The row shows the one version still
 * true: a progress bar of heavy and light rules, the count, and the task in
 * progress, or the next open one when none is. It costs one row while work
 * remains and none once it is done. The complete checklist is a sheet away.
 *
 * `hint` names the key that opens the sheet, at the row's right edge; the
 * row gives it up before it cuts the task below a few cells. Focused, the
 * head is marked and the hint says what Enter does.
 *
 * @param props.todos - the current list, in the agent's own order.
 * @param props.columns - row width.
 * @returns the row, or null when nothing is left to do.
 */
export function Tasks({ todos, copy, columns, focused = false, hint }: {
  readonly todos: readonly TaskEntry[]
  readonly copy: TuiCopy
  readonly columns: number
  readonly focused?: boolean
  readonly hint?: string | undefined
}): React.ReactElement | null {
  const done = todos.filter(item => item.status === 'completed').length
  const current = todos.find(item => item.status === 'in_progress') ?? todos.find(item => item.status !== 'completed')
  if (current === undefined || columns <= 0) return null
  const filled = Math.round(TASK_BAR * done / todos.length)
  const head = `${focused ? '> ' : ''}${copy.todoTitle}`
  const count = `  ${done}/${todos.length}`
  const task = ` · ${glyphOf(current.status)} ${current.text}`
  const tail = focused ? copy.todoOpen : hint
  const fixed = stringWidth(head) + 2 + TASK_BAR + stringWidth(count)
  const showTail = tail !== undefined && fixed + Math.min(stringWidth(task), TASK_TEXT_MIN) + 2 + stringWidth(tail) <= columns
  const color = colorOf(current.status)
  return <Box width={columns} height={1} flexDirection="row" flexShrink={0} overflowX="hidden">
    <Box width={showTail ? columns - 2 - stringWidth(tail) : columns} flexShrink={0}>
      <Text wrap="truncate-end">
        <Text bold inverse={focused}>{head}</Text>
        {'  '}
        <Text color={PALETTE.asking}>{TASK_GLYPH.filled.repeat(filled)}</Text>
        <Text dimColor>{TASK_GLYPH.empty.repeat(TASK_BAR - filled)}</Text>
        <Text dimColor>{count}</Text>
        <Text dimColor>{' · '}</Text>
        <Text bold={color !== undefined} {...color === undefined ? { dimColor: true } : { color }}>{glyphOf(current.status)}</Text>
        <Text bold={current.status === 'in_progress'} dimColor={current.status !== 'in_progress'}>{` ${current.text}`}</Text>
      </Text>
    </Box>
    {showTail && <Box flexShrink={0} marginLeft={2}><Text dimColor>{tail}</Text></Box>}
  </Box>
}

/** The list's tab: its name and how much of it is done. */
export function taskTab(todos: readonly TaskEntry[], copy: TuiCopy): string {
  return `${copy.todoTitle} ${todos.filter(item => item.status === 'completed').length}/${todos.length}`
}

/** Cells of the sheet's progress bar, wider than the row's since the sheet has the room. */
const SHEET_BAR = 24

/**
 * The complete checklist. A progress bar and the counts by state, then every
 * task in the agent's order, numbered and wrapped rather than truncated:
 * finished ones ticked and struck through, the current one bold, the rest
 * open boxes.
 */
export function taskSheet(todos: readonly TaskEntry[], copy: TuiCopy): readonly SheetLine[] {
  const done = todos.filter(item => item.status === 'completed').length
  const active = todos.filter(item => item.status === 'in_progress').length
  const left = todos.length - done - active
  const counts = [`${done}/${todos.length} ${copy.todoDone}`,
    ...active === 0 ? [] : [`${active} ${copy.todoActive}`], ...left === 0 ? [] : [`${left} ${copy.todoLeft}`]].join(' \u00b7 ')
  const digits = String(todos.length).length
  return [
    { text: '', parts: [...sheetBar(done, todos.length, SHEET_BAR, PALETTE.asking), { text: `  ${counts}`, dim: true }] },
    { text: '' },
    ...todos.map((item, index): SheetLine => {
      const color = colorOf(item.status)
      return {
        text: item.text, glyph: `${glyphOf(item.status)} ${String(index + 1).padStart(digits)}`,
        ...color === undefined ? {} : { glyphColor: color },
        bold: item.status === 'in_progress', dim: item.status !== 'in_progress', strikethrough: item.status === 'completed',
      }
    }),
  ]
}
