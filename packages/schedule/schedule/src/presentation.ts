/**
 * UI presentation of the Schedule tools' results. The model receives each
 * reminder as canonical JSON, prompt included; a reader of the transcript
 * needs the reminder's id and when it fires, since the call's own card
 * already shows the prompt or id it sent. Each presenter is pure over the
 * model-facing result, so a replayed session draws the same card, and none
 * changes the JSON the model receives. A failure, a stable error value, or
 * text that is not a canonical value keeps the generic rendering of the raw
 * result.
 * @module bake-schedule/src/presentation
 */

import type { GenericResultView, ToolResult } from 'bake-tools'

/** The reminder fields a summary reads from one canonical view. */
interface ReminderSummary {
  readonly id: string
  readonly prompt: string
  readonly scheduledAt: string
  readonly overdue: boolean
  /** The rule, worded: `after 30s`, `every 5m`, or empty for an absolute time. */
  readonly rule: string
}

/** Characters of a prompt a `schedule_list` line shows; the log keeps all of it. */
const PROMPT_CHARS = 60

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
 * A whole number of seconds in its largest exact unit: `30s`, `5m`, `2h`, `1d`.
 * @param seconds - a positive interval.
 * @returns the interval, worded.
 */
function duration(seconds: number): string {
  for (const [unit, size] of [['d', 86_400], ['h', 3600], ['m', 60]] as const) {
    if (seconds % size === 0) return `${seconds / size}${unit}`
  }
  return `${seconds}s`
}

/**
 * A canonical UTC instant as a reader scans it: `2026-08-05 12:00:30 UTC`,
 * keeping any fraction a target carries. Anything else is shown as sent.
 * @param instant - the view's `scheduledAt`.
 * @returns the instant, worded.
 */
function when(instant: string): string {
  const parts = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2}:\d{2})(\.\d+)?Z$/.exec(instant)
  if (parts === null) return instant
  const fraction = parts[3] === undefined || /^\.0+$/.test(parts[3]) ? '' : parts[3]
  return `${parts[1]} ${parts[2]}${fraction} UTC`
}

/**
 * Whether a value is a positive safe integer, as rule intervals are.
 * @param value - any parsed JSON value.
 * @returns true for an interval.
 */
const isInterval = (value: unknown): value is number => typeof value === 'number' && Number.isSafeInteger(value) && value > 0

/**
 * Narrow one parsed value to the reminder view it carries.
 * @param value - a parsed JSON value.
 * @returns the reminder, or undefined when the value is not a canonical view.
 */
function reminderOf(value: unknown): ReminderSummary | undefined {
  if (typeof value !== 'object' || value === null
    || !('id' in value) || typeof value.id !== 'string'
    || !('prompt' in value) || typeof value.prompt !== 'string'
    || !('scheduledAt' in value) || typeof value.scheduledAt !== 'string'
    || !('state' in value) || (value.state !== 'scheduled' && value.state !== 'overdue')
    || !('kind' in value)) return undefined
  let rule: string
  if (value.kind === 'after' && 'afterSeconds' in value && isInterval(value.afterSeconds)) rule = `after ${duration(value.afterSeconds)}`
  else if (value.kind === 'every' && 'everySeconds' in value && isInterval(value.everySeconds)) rule = `every ${duration(value.everySeconds)}`
  else if (value.kind === 'at') rule = ''
  else return undefined
  return { id: value.id, prompt: value.prompt, scheduledAt: value.scheduledAt, overdue: value.state === 'overdue', rule }
}

/**
 * One reminder on one line: `schedule-1 · every 5m · next 2026-08-05 12:05:00 UTC`,
 * `after` and `at` rules naming their target as `due` and `at`, and an
 * overdue reminder saying so.
 * @param reminder - one canonical reminder.
 * @returns the line, without the prompt.
 */
function reminderLine(reminder: ReminderSummary): string {
  const target = reminder.rule === '' ? `at ${when(reminder.scheduledAt)}`
    : `${reminder.rule} \u00b7 ${reminder.rule.startsWith('every') ? 'next' : 'due'} ${when(reminder.scheduledAt)}`
  return `${reminder.id} \u00b7 ${target}${reminder.overdue ? ' \u00b7 overdue' : ''}`
}

/**
 * Parse a successful result's text.
 * @param result - the final model-facing result.
 * @returns the parsed value, or undefined for a failure or text that is not JSON.
 */
function valueOf(result: ToolResult): { readonly value: unknown } | undefined {
  if (result.isError) return undefined
  const text = result.content.map(block => block.type === 'text' ? block.text : '').join('')
  try { return { value: JSON.parse(text) as unknown } } catch { return undefined }
}

/**
 * A generic result card whose content is the given lines.
 * @param text - the summary, one line per line.
 * @returns the card.
 */
const summary = (text: string): GenericResultView => ({ card: 'generic', content: [{ type: 'text', text }] })

/**
 * The completed card of `schedule_create`: the new reminder's id and when it
 * fires. The prompt is on the call's own card.
 * @param result - the final model-facing result.
 * @returns the summary card, or undefined to keep the raw result.
 */
export function presentCreateResult(result: ToolResult): GenericResultView | undefined {
  const parsed = valueOf(result)
  const reminder = parsed === undefined ? undefined : reminderOf(parsed.value)
  return reminder === undefined ? undefined : summary(reminderLine(reminder))
}

/**
 * The completed card of `schedule_list`: one line per active reminder, in
 * creation order, each with its prompt cut to {@link PROMPT_CHARS} characters,
 * or `No reminders`.
 * @param result - the final model-facing result.
 * @returns the summary card, or undefined to keep the raw result.
 */
export function presentListResult(result: ToolResult): GenericResultView | undefined {
  const parsed = valueOf(result)
  if (parsed === undefined || !Array.isArray(parsed.value)) return undefined
  const reminders = (parsed.value as unknown[]).map(reminderOf)
  if (reminders.some(reminder => reminder === undefined)) return undefined
  if (reminders.length === 0) return summary('No reminders')
  return summary((reminders as ReminderSummary[])
    .map(reminder => `${reminderLine(reminder)} \u00b7 ${clip(oneLine(reminder.prompt), PROMPT_CHARS)}`).join('\n'))
}

/**
 * The completed card of `schedule_delete`: `deleted`, or `not found` for an
 * unknown or finished id. The id is on the call's own card.
 * @param result - the final model-facing result.
 * @returns the summary card, or undefined to keep the raw result.
 */
export function presentDeleteResult(result: ToolResult): GenericResultView | undefined {
  const value = valueOf(result)?.value
  if (typeof value !== 'object' || value === null || !('id' in value) || typeof value.id !== 'string') return undefined
  if (!('deleted' in value)) return undefined
  if (value.deleted === true) return summary('deleted')
  return value.deleted === false && 'code' in value && value.code === 'schedule_not_found' ? summary('not found') : undefined
}
