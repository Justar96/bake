/**
 * UI presentation of the subagent control tools' calls: short titles naming
 * the target, and for a message only its first line, since a message can be
 * as long as a delegation prompt and the target's session keeps all of it.
 * Pure over the logged arguments, so a replayed session draws the same card.
 * @module bake-tool-subagent-control/src/presentation
 */

import type { GenericCallView } from 'bake-tools'

/** Characters of a message's first line a title shows beside the target id. */
const MESSAGE_CHARS = 48

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
 * The first line of a message that says anything, cut to
 * {@link MESSAGE_CHARS}, ending in `…` whenever anything was left out.
 * @param text - the message.
 * @returns one line, or the empty string for a blank message.
 */
function firstLine(text: string): string {
  const lines = text.split('\n').map(line => line.replace(/\s+/g, ' ').trim()).filter(line => line !== '')
  const first = clip(lines[0] ?? '', MESSAGE_CHARS)
  return lines.length > 1 && !first.endsWith('\u2026') ? `${first} \u2026` : first
}

/**
 * The pending card of a `send_message` call: the target and the message's
 * first line, `agent-7: Also check the lockfile …`.
 * @param args - the validated message arguments.
 * @returns a generic card titled by target and message.
 */
export function presentSendMessageCall(args: { readonly agent_id: string; readonly message: string }): GenericCallView {
  const line = firstLine(args.message)
  return { card: 'generic', title: line === '' ? args.agent_id : `${args.agent_id}: ${line}`, kind: 'other' }
}

/**
 * The pending card of an `interrupt_agent` call.
 * @param args - the validated interrupt arguments.
 * @returns a generic execute card naming the target.
 */
export function presentInterruptCall(args: { readonly agent_id: string }): GenericCallView {
  return { card: 'generic', title: `Interrupt ${args.agent_id}`, kind: 'execute' }
}

/**
 * The pending card of a `list_agents` call, by the scope it lists.
 * @param args - the validated listing arguments.
 * @returns a generic read card.
 */
export function presentListAgentsCall(args: { readonly scope?: string }): GenericCallView {
  return { card: 'generic', title: args.scope === 'descendants' ? 'List all subagents below' : 'List subagents', kind: 'read' }
}
