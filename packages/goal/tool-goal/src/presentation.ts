/**
 * UI presentation of the goal tools' results. The model receives the whole
 * goal as compact JSON after every call; a reader of the transcript needs only
 * what the call did, since the call's own card already shows the objective or
 * reason it sent. Each presenter is pure over the logged arguments and the
 * model-facing result, so a replayed session draws the same card, and none
 * changes the JSON the model receives. A failure, or text that is not the
 * canonical goal JSON, keeps the generic rendering of the raw result.
 * @module bake-tool-goal/src/presentation
 */

import type { GenericResultView, ToolResult } from 'bake-tools'

/** The goal fields a summary reads from one canonical result. */
interface GoalSummary {
  readonly objective: string
  readonly phase: keyof typeof PHASE_WORDS
  readonly roundsStarted: number
  readonly maxGoalRounds: number
  readonly blockedReason?: string
}

/** How a summary words each durable phase. */
const PHASE_WORDS = { active: 'active', paused: 'paused', blocked: 'blocked', complete: 'completed' } as const

/** Characters of the objective a `get_goal` summary shows; the log keeps all of it. */
const OBJECTIVE_CHARS = 60

/** Characters of a blocker's message a `get_goal` summary shows. */
const REASON_CHARS = 160

/**
 * Text on one line, whitespace runs folded to one space.
 * @param text - any text.
 * @returns the text on one line, trimmed.
 */
const oneLine = (text: string): string => text.replace(/\s+/g, ' ').trim()

/**
 * Cut text to a number of characters, marking the cut with an ellipsis.
 * Counted in code points, so a cut never splits a surrogate pair; a UI
 * measures display width itself.
 * @param text - one line of text.
 * @param limit - the most characters to keep, ellipsis included.
 * @returns the text, or its first characters and `…`.
 */
function clip(text: string, limit: number): string {
  const characters = Array.from(text)
  return characters.length <= limit ? text : `${characters.slice(0, limit - 1).join('').trimEnd()}\u2026`
}

/**
 * Whether a value is a non-negative safe integer, as round counts are.
 * @param value - any parsed JSON value.
 * @returns true for a count.
 */
const isCount = (value: unknown): value is number => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0

/**
 * Narrow a successful result back to the goal it reports.
 * @param result - the final model-facing result.
 * @returns the goal, `null` for the canonical no-goal result, or undefined for
 *   a failure or text that is not the canonical shape.
 */
function goalOf(result: ToolResult): GoalSummary | null | undefined {
  if (result.isError) return undefined
  const text = result.content.map(block => block.type === 'text' ? block.text : '').join('')
  let value: unknown
  try { value = JSON.parse(text) } catch { return undefined }
  if (typeof value !== 'object' || value === null || !('goal' in value)) return undefined
  const goal: unknown = value.goal
  if (goal === null) return null
  if (typeof goal !== 'object'
    || !('objective' in goal) || typeof goal.objective !== 'string'
    || !('phase' in goal) || typeof goal.phase !== 'string' || !Object.hasOwn(PHASE_WORDS, goal.phase)
    || !('roundsStarted' in goal) || !isCount(goal.roundsStarted)
    || !('maxGoalRounds' in goal) || !isCount(goal.maxGoalRounds)) return undefined
  const reason = 'blockedReason' in goal && typeof goal.blockedReason === 'object' && goal.blockedReason !== null
    && 'message' in goal.blockedReason && typeof goal.blockedReason.message === 'string'
    ? goal.blockedReason.message
    : undefined
  return {
    objective: goal.objective,
    phase: goal.phase as GoalSummary['phase'],
    roundsStarted: goal.roundsStarted,
    maxGoalRounds: goal.maxGoalRounds,
    ...reason === undefined ? {} : { blockedReason: reason },
  }
}

/**
 * A generic result card whose content is the given lines.
 * @param text - the summary, one line per line.
 * @returns the card.
 */
const summary = (text: string): GenericResultView => ({ card: 'generic', content: [{ type: 'text', text }] })

/**
 * The completed card of `create_goal`: `Goal created · 0/8 rounds`. The
 * objective is on the call's own card.
 * @param result - the final model-facing result.
 * @returns the summary card, or undefined to keep the raw result.
 */
export function presentCreateResult(result: ToolResult): GenericResultView | undefined {
  const goal = goalOf(result)
  if (goal === undefined || goal === null) return undefined
  return summary(`Goal created \u00b7 ${goal.roundsStarted}/${goal.maxGoalRounds} rounds`)
}

/**
 * The completed card of `get_goal`: `active · round 2/8 · Ship the parser`,
 * with the objective cut to {@link OBJECTIVE_CHARS} characters and a blocked
 * goal's reason on a second line, or `No goal`. The call sends no arguments,
 * so the objective appears only here.
 * @param result - the final model-facing result.
 * @returns the summary card, or undefined to keep the raw result.
 */
export function presentGetResult(result: ToolResult): GenericResultView | undefined {
  const goal = goalOf(result)
  if (goal === undefined) return undefined
  if (goal === null) return summary('No goal')
  const head = `${PHASE_WORDS[goal.phase]} \u00b7 round ${goal.roundsStarted}/${goal.maxGoalRounds} \u00b7 `
    + clip(oneLine(goal.objective), OBJECTIVE_CHARS)
  return summary(goal.blockedReason === undefined ? head : `${head}\n${clip(oneLine(goal.blockedReason), REASON_CHARS)}`)
}

/**
 * The completed card of `update_goal`: the state the action left, `paused`,
 * `resumed`, `completed`, or `blocked`, and for an edit its rounds,
 * `edited · 2/12 rounds`. A new objective or blocker is on the call's own card.
 * @param action - the update action the call requested.
 * @param result - the final model-facing result.
 * @returns the summary card, or undefined to keep the raw result.
 */
export function presentUpdateResult(action: string, result: ToolResult): GenericResultView | undefined {
  const goal = goalOf(result)
  if (goal === undefined || goal === null) return undefined
  if (action === 'edit') return summary(`edited \u00b7 ${goal.roundsStarted}/${goal.maxGoalRounds} rounds`)
  if (action === 'resume' && goal.phase === 'active') return summary('resumed')
  return summary(PHASE_WORDS[goal.phase])
}
