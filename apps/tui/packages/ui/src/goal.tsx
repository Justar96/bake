/** The session's goal, held above the input while one is set. */
import React from 'react'
import { Box, Text } from 'ink'
import wrapAnsi from 'wrap-ansi'
import type { TuiCopy } from './copy.ts'
import { COLUMN, MARKER, TREE } from './layout.ts'
import { PALETTE } from './palette.ts'
import type { StandingState } from './line.tsx'

/**
 * Display-only view of the session's current goal.
 *
 * `phase` is durable. `armed` is this process's permission to keep continuing
 * the goal, so an active goal that is not armed is waiting on a human.
 */
export interface GoalEntry {
  readonly objective: string
  readonly phase: 'active' | 'paused' | 'blocked' | 'complete'
  readonly armed: boolean
  readonly rounds: number
  readonly maxRounds: number
  /** Why a blocked goal stopped; present only while blocked. */
  readonly blocked?: string
}

/**
 * Standing state for the goal: the glyph, word, and colour of its phase.
 *
 * Each phase pairs a colour with a glyph, so `NO_COLOR` still distinguishes
 * an armed goal from a held, blocked, or finished one. The header draws it
 * when the goal block has no room; the block's head draws the same state.
 *
 * @param goal - the current goal, or undefined when none is set.
 * @param copy - locale-owned labels.
 * @returns the goal's state, or undefined when there is no goal.
 */
export function goalState(goal: GoalEntry | undefined, copy: TuiCopy): StandingState | undefined {
  if (goal === undefined) return undefined
  const rounds = `${copy.goalRound} ${goal.rounds}/${goal.maxRounds}`
  const details = (...parts: (string | undefined)[]): string => parts.filter(part => part !== undefined && part !== '').join(' · ')
  switch (goal.phase) {
    case 'active': return goal.armed
      ? { glyph: MARKER.turn, label: copy.goalActive, details: details(rounds, goal.objective), color: PALETTE.running }
      : { glyph: MARKER.waiting, label: copy.goalHeld, details: details(copy.goalResume, goal.objective), color: PALETTE.waiting }
    case 'paused': return { glyph: MARKER.waiting, label: copy.goalPaused, details: details(copy.goalResume, goal.objective), color: PALETTE.waiting }
    case 'blocked': return { glyph: '✗', label: copy.goalBlocked, details: details(goal.blocked, goal.objective), color: PALETTE.failed }
    case 'complete': return { glyph: '✓', label: copy.goalComplete, details: details(rounds, goal.objective), color: PALETTE.done }
  }
}

/** Rows the objective may wrap to. Enough for a sentence; a paragraph belongs to `/goal`. */
const OBJECTIVE_ROWS = 2

/** The `/goal` actions that apply in each phase, locale-owned. */
const KEYS = {
  armed: 'goalKeysActive',
  held: 'goalKeysHeld',
  blocked: 'goalKeysBlocked',
  complete: 'goalKeysComplete',
} as const satisfies Record<string, keyof TuiCopy>

/** The rows of the block, in the order they are dropped when rows run short: last first. */
interface GoalLayout {
  readonly objective: readonly string[]
  readonly reason: string | undefined
  readonly keys: string
}

function layoutOf(goal: GoalEntry, copy: TuiCopy, columns: number): GoalLayout {
  const width = Math.max(1, columns - COLUMN.rail)
  const wrapped = wrapAnsi(goal.objective.replace(/\s+/gu, ' ').trim(), width, { hard: true, trim: true }).split('\n')
  const objective = wrapped.length <= OBJECTIVE_ROWS ? wrapped
    : [...wrapped.slice(0, OBJECTIVE_ROWS - 1), `${wrapped[OBJECTIVE_ROWS - 1]!.slice(0, Math.max(0, width - 1))}…`]
  const phase = goal.phase === 'active' ? goal.armed ? 'armed' : 'held' : goal.phase === 'paused' ? 'held' : goal.phase
  return {
    objective,
    reason: goal.phase === 'blocked' && goal.blocked !== undefined && goal.blocked !== '' ? goal.blocked : undefined,
    keys: copy[KEYS[phase]],
  }
}

/**
 * Rows the goal block would draw with no limit, for the caller's height claim.
 * @param goal - the current goal, or undefined.
 * @param copy - locale-owned labels.
 * @param columns - terminal width.
 * @returns zero without a goal.
 */
export function goalRows(goal: GoalEntry | undefined, copy: TuiCopy, columns: number): number {
  if (goal === undefined) return 0
  const layout = layoutOf(goal, copy, columns)
  return 1 + layout.objective.length + Number(layout.reason !== undefined) + 1
}

/**
 * The goal as a held block above the input, drawn as the task list is.
 *
 * A head in the goal's colour names its phase and counts rounds. The
 * objective hangs from it, wrapped to at most two rows, and a blocked goal
 * says why above it. Under the tree, the `/goal` actions that apply now. When
 * rows run short the actions go first, then the objective's second row, then
 * the reason; the head and the objective's first row stay.
 *
 * @param props.goal - the current goal.
 * @param props.copy - locale-owned labels.
 * @param props.columns - terminal width.
 * @param props.limit - rows the block may draw.
 * @returns the block, or null without a goal or room.
 */
export function Goal({ goal, copy, columns, limit }: {
  readonly goal: GoalEntry | undefined
  readonly copy: TuiCopy
  readonly columns: number
  readonly limit: number
}): React.ReactElement | null {
  const state = goalState(goal, copy)
  if (goal === undefined || state === undefined || limit <= 0) return null
  const layout = layoutOf(goal, copy, columns)
  let room = limit - 1
  const firstRow = Math.min(1, room); room -= firstRow
  const reason = layout.reason !== undefined && room > 0 ? (room--, layout.reason) : undefined
  const extraRows = Math.min(layout.objective.length - 1, room); room -= extraRows
  const keys = room > 0
  const objective = layout.objective.slice(0, firstRow + extraRows)
  const cut = objective.length < layout.objective.length
  const rounds = goal.phase === 'active' || goal.phase === 'complete' ? `${copy.goalRound} ${goal.rounds}/${goal.maxRounds}` : ''
  return <Box flexDirection="column" flexShrink={0} maxHeight={limit} overflowY="hidden">
    <Box flexDirection="row" flexShrink={0}>
      <Box width={COLUMN.rail} flexShrink={0}><Text bold color={state.color}>{state.glyph}</Text></Box>
      <Text wrap="truncate-end">
        <Text bold color={state.color}>{state.label}</Text>
        {rounds === '' ? null : <Text dimColor>{`  ${rounds}`}</Text>}
      </Text>
    </Box>
    {reason !== undefined && <Box flexDirection="row" flexShrink={0}>
      <Box width={COLUMN.rail} flexShrink={0}><Text dimColor>{objective.length > 0 ? TREE.branch : TREE.corner}</Text></Box>
      <Text color={PALETTE.failed} wrap="truncate-end">{reason}</Text>
    </Box>}
    {objective.map((row, index) => <Box key={index} flexDirection="row" flexShrink={0}>
      <Box width={COLUMN.rail} flexShrink={0}><Text dimColor>{index === 0 ? TREE.corner : ' '}</Text></Box>
      <Text wrap="truncate-end">{cut && index === objective.length - 1 ? `${row}…` : row}</Text>
    </Box>)}
    {keys && <Box paddingLeft={COLUMN.rail} flexShrink={0}><Text dimColor wrap="truncate-end">{layout.keys}</Text></Box>}
  </Box>
}
