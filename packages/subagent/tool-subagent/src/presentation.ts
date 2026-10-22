/**
 * UI presentation of the delegation tools' calls. A delegation is named by
 * the short label the model gave it, not by its prompt, which can run to many
 * paragraphs and is the child session's own first message. Pure over the
 * logged arguments, so a replayed session draws the same card.
 * @module bake-tool-subagent/src/presentation
 */

import type { GenericCallView } from 'bake-tools'

/** Characters a title may take: a card header or a log line. */
const TITLE_CHARS = 80

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
 * The first line of text that says anything, with a trailing `…` when more
 * follows it.
 * @param text - any text.
 * @returns one line, or the empty string for blank text.
 */
function firstLine(text: string): string {
  const [first = '', ...rest] = text.split('\n').map(line => line.replace(/\s+/g, ' ').trim()).filter(line => line !== '')
  return rest.length > 0 ? `${first} \u2026` : first
}

/**
 * The pending card of a delegation call: its short
 * description, falling back to the prompt's first line when the description
 * is blank. The prompt itself stays out of the card.
 * @param args - the validated delegation arguments.
 * @returns a generic card titled by the delegated task.
 */
export function presentDelegationCall(args: { readonly description: string; readonly prompt: string }): GenericCallView {
  const label = firstLine(args.description)
  return { card: 'generic', title: clip(label !== '' ? label : firstLine(args.prompt), TITLE_CHARS), kind: 'other' }
}

/**
 * The pending card of a `list_subagent_models` call, by what it looks up.
 * @param args - the validated discovery arguments.
 * @returns a generic read card.
 */
export function presentModelListCall(args: { readonly provider?: string; readonly model?: string }): GenericCallView {
  const provider = firstLine(args.provider ?? '')
  const model = firstLine(args.model ?? '')
  const title = model !== '' ? `Show ${provider === '' ? model : `${provider}/${model}`}`
    : provider !== '' ? `List ${provider} models`
      : 'List subagent providers'
  return { card: 'generic', title: clip(title, TITLE_CHARS), kind: 'read' }
}
