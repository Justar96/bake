/** Compact header state and complete sheet for the session goal. */
import type { TuiCopy } from './copy.ts'
import { MARKER } from './layout.ts'
import { PALETTE } from './palette.ts'
import type { StandingState } from './line.tsx'
import { sheetBar, type SheetLine } from './sheet.tsx'

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
 * an armed goal from a held, blocked, or finished one. The header keeps the
 * turn's label first, then fits the goal at the right edge.
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

/** Cells of the goal sheet's round bar. */
const ROUND_BAR = 24

/**
 * The goal's sheet: its state in its colour, a bar of the rounds spent against
 * the limit, what continues a held goal, then the whole objective and a
 * blocked goal's reason under their own names, where the header has room for
 * a line of either at most.
 */
export function goalSheet(goal: GoalEntry, copy: TuiCopy): readonly SheetLine[] {
  const state = goalState(goal, copy)!
  const held = goal.phase === 'paused' || (goal.phase === 'active' && !goal.armed)
  return [
    { text: state.label, glyph: state.glyph, glyphColor: state.color, color: state.color, bold: true },
    { text: '', parts: [...sheetBar(goal.rounds, goal.maxRounds, ROUND_BAR, state.color),
      { text: `  ${copy.goalRound} ${goal.rounds}/${goal.maxRounds}`, dim: true }] },
    ...held ? [{ text: copy.goalResume, dim: true }] : [],
    { text: '' },
    { text: copy.goalObjective, dim: true, bold: true },
    { text: goal.objective },
    ...goal.blocked === undefined ? [] : [{ text: '' }, { text: copy.goalReason, color: PALETTE.failed, bold: true },
      { text: goal.blocked, color: PALETTE.failed }],
  ]
}
