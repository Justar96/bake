/**
 * UI presentation of `ask_user_question` calls: a title naming what was asked,
 * and the answers one question to a line. The question card a user answers is
 * the interaction surface's own; these views are the call's record once it
 * has scrolled into the transcript. Both are pure over the logged arguments
 * and the model-facing result, so a replayed session draws the same card, and
 * neither changes the canonical `answers` JSON the model receives.
 * @module bake-tool-ask-user/src/presentation
 */

import type { GenericCallView, GenericResultView, ToolResult } from 'bake-tools'

/** The fields a title reads from one validated question. */
export interface AskedQuestion {
  readonly question: string
  readonly header?: string | undefined
}

/** One answer as the model-facing result carries it. */
interface Answer {
  readonly id: string
  readonly selected: readonly string[]
  readonly custom?: string
}

/** Characters a title may take: a card header or a log line, not the questions themselves. */
const TITLE_CHARS = 80

/** Characters a question may take in a title when it has no header to name it by. */
const QUESTION_CHARS = 48

/** Characters of a free-form answer shown on its line; the log keeps all of it. */
const CUSTOM_CHARS = 160

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
 * What names one question in a title: its header, or its words when the
 * header is missing or blank.
 * @param question - one validated question.
 * @returns a short name on one line.
 */
function nameOf(question: AskedQuestion): string {
  const header = oneLine(question.header ?? '')
  return header !== '' ? header : clip(oneLine(question.question), QUESTION_CHARS)
}

/**
 * The pending card: `Ask: Choose scope`, or `Ask 2 questions: Choose scope,
 * Runtime fixes`. The questions, options, and descriptions are left to the
 * interaction surface that asks them.
 * @param questions - the validated questions, in order.
 * @returns a generic card with a short title.
 */
export function presentAskCall(questions: readonly AskedQuestion[]): GenericCallView {
  const names = questions.map(nameOf)
  const [only] = names
  const title = only === undefined ? 'Ask'
    : names.length === 1 ? `Ask: ${only}`
      : `Ask ${names.length} questions: ${names.join(', ')}`
  return { card: 'generic', title: clip(title, TITLE_CHARS), kind: 'other' }
}

/**
 * Narrow the model-facing result text back to its answers.
 * @param content - the result's content blocks.
 * @returns the answers, or undefined when the text is not the canonical shape.
 */
function answersOf(content: ToolResult['content']): readonly Answer[] | undefined {
  const text = content.map(block => block.type === 'text' ? block.text : '').join('')
  let value: unknown
  try { value = JSON.parse(text) } catch { return undefined }
  if (typeof value !== 'object' || value === null || !('answers' in value) || !Array.isArray(value.answers)) return undefined
  const answers: unknown[] = value.answers
  const valid = answers.every((answer): answer is Answer => typeof answer === 'object' && answer !== null
    && 'id' in answer && typeof answer.id === 'string'
    && 'selected' in answer && Array.isArray(answer.selected) && answer.selected.every(label => typeof label === 'string')
    && (!('custom' in answer) || typeof answer.custom === 'string'))
  return valid && answers.length > 0 ? answers as Answer[] : undefined
}

/**
 * One answer on one line: `scope → Tooling swaps, Shared versions`, with a
 * free-form answer quoted after the chosen labels, and `—` for none.
 * @param answer - one canonical answer.
 * @returns the line.
 */
function answerLine(answer: Answer): string {
  const custom = oneLine(answer.custom ?? '')
  const parts = [...answer.selected.map(oneLine), ...custom === '' ? [] : [`"${clip(custom, CUSTOM_CHARS)}"`]]
  return `${oneLine(answer.id)} \u2192 ${parts.length === 0 ? '\u2014' : parts.join(', ')}`
}

/**
 * The completed card: each question's answer on its own line, in answer order.
 * A failure, or a result that is not the canonical answers JSON, keeps the
 * generic rendering of the raw result.
 * @param result - the final model-facing result.
 * @returns a generic card whose content is the answer lines, or undefined.
 */
export function presentAskResult(result: ToolResult): GenericResultView | undefined {
  if (result.isError) return undefined
  const answers = answersOf(result.content)
  if (answers === undefined) return undefined
  return { card: 'generic', content: [{ type: 'text', text: answers.map(answerLine).join('\n') }] }
}
