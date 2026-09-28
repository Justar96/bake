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
 * Standing state for the goal: the glyph and colour of its phase, and its words.
 *
 * An armed goal reads `● Goal 3/256`: the glyph in the running orange, the
 * name and its round count in the terminal's own foreground, the count kept
 * whenever the name is and beside the glyph once the name no longer fits.
 * A held, paused, blocked, or finished goal keeps `○`, `✗`, or `✓` and the
 * words for its phase. Each phase pairs a colour with a glyph, so `NO_COLOR`
 * still distinguishes an armed goal from a held, blocked, or finished one.
 * The header keeps the turn's label first, then fits the goal at the right edge.
 *
 * The objective is left to the goal's sheet unless `objective` asks for it:
 * a sentence of it at the row's edge reads as clipped text, not as state.
 * A blocked goal's reason is its note either way, the one part cut to fit.
 *
 * @param goal - the current goal, or undefined when none is set.
 * @param copy - locale-owned labels.
 * @param options.objective - add the objective after the details.
 * @returns the goal's state, or undefined when there is no goal.
 */
export function goalState(goal: GoalEntry | undefined, copy: TuiCopy, options: { readonly objective?: boolean } = {}): StandingState | undefined {
  if (goal === undefined) return undefined
  const count = `${goal.rounds}/${goal.maxRounds}`
  const note = (...parts: (string | undefined)[]): { note?: string } => {
    const text = [...parts, options.objective === true ? goal.objective : undefined]
      .filter(part => part !== undefined && part !== '').join(' · ')
    return text === '' ? {} : { note: text }
  }
  switch (goal.phase) {
    case 'active': return goal.armed
      ? { glyph: MARKER.turn, label: copy.goalTitle, count, details: '', compact: count, ...note(), color: PALETTE.running }
      : { glyph: MARKER.waiting, label: copy.goalHeld, details: copy.goalResume, ...note(), color: PALETTE.waiting }
    case 'paused': return { glyph: MARKER.waiting, label: copy.goalPaused, details: copy.goalResume, ...note(), color: PALETTE.waiting }
    case 'blocked': return { glyph: '✗', label: copy.goalBlocked, details: '', ...note(goal.blocked), color: PALETTE.failed }
    case 'complete': return { glyph: '✓', label: copy.goalComplete, count, details: '', compact: count, ...note(), color: PALETTE.done }
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
  // The header's short `Goal` stands beside its count; the sheet names the phase.
  const label = goal.phase === 'active' && goal.armed ? copy.goalActive : state.label
  return [
    { text: label, glyph: state.glyph, glyphColor: state.color, color: state.color, bold: true },
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
