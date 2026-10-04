/**
 * Rail icons for the few actions that change who is working or what is planned.
 *
 * An action's marker takes its state from colour and motion, as `MARKER.action`
 * does. Most calls, commands, reads, edits, and searches alike, keep that plain
 * dot: the head already names the tool, and a different shape per tool made
 * the rail busy without telling a reader anything the head did not. A shape
 * of its own is kept for a skill loaded into the conversation, a subagent
 * started, a message sent to one, a plugin's task list, and work left
 * running in the background, because those change what the session knows,
 * who is doing the work, what is left to do, or what is still running.
 * Every icon is one cell wide by `string-width`, sits alone in the rail, or
 * just past a step's tree branch when the call is one of a batch, and
 * has no emoji presentation, for the reason `MARKER` gives. The tool name is
 * still written in the head, so the icon never carries meaning on its own.
 *
 * @module @dsh-tui/ui/icons
 */

import { MARKER } from './layout.ts'

/** Icon for each kind of action. */
export const ICON = {
  /** A skill loaded into the conversation. */
  skill: '\u2726',
  /** A subagent started. Work handed down. */
  spawn: '\u21b3',
  /** A message sent to a running subagent. */
  send: '\u2192',
  /** A task list update from a plugin's tool. */
  todo: '\u2610',
  /**
   * Work a call started in the background, and that work's end. A dotted
   * ring: the call's own dot, with the work no longer held in it.
   */
  background: '\u25cc',
  /** Anything else. */
  other: MARKER.action,
} as const

/** An icon this vocabulary names. */
export type Icon = typeof ICON[keyof typeof ICON]

/** Icons for the tools Bake registers, by their model-visible names. */
const NAMED: Readonly<Record<string, Icon>> = {
  skill: ICON.skill,
  subagent: ICON.spawn,
  send_message: ICON.send,
}

/**
 * Whole words that place a name Bake does not register itself, such as a
 * plugin's tool or a renamed one, checked in order; the first match wins.
 * Each entry lists the words a name must all carry, so `send_email` is not a
 * message to an agent and `list_agents` does not start one.
 */
const GUESSED: readonly (readonly [readonly (readonly string[])[], Icon])[] = [
  [[['send', 'message'], ['message', 'agent']], ICON.send],
  [[['subagent'], ['spawn'], ['delegate']], ICON.spawn],
  [[['skill']], ICON.skill],
  [[['todo'], ['todos']], ICON.todo],
]

/**
 * Icon for one call: the background ring for a tool with no shape of its
 * own that started background work, else its tool's icon. A delegation in
 * the background keeps `↳`, since who does the work is what changed.
 * @param tool - tool name from the session log.
 * @param background - whether the call ran in the background.
 * @returns the rail icon for the call's head.
 */
export function callIcon(tool: string, background = false): Icon {
  const icon = iconFor(tool)
  return background && icon === ICON.other ? ICON.background : icon
}

/**
 * Icon for what a tool does, from its name.
 * @param tool - tool name from the session log.
 * @returns the rail icon for the call's head.
 */
export function iconFor(tool: string): Icon {
  const named = NAMED[tool.toLowerCase()]
  if (named !== undefined) return named
  // Whole words only, split at separators and camel case, so `skillet` is
  // not a skill.
  const words = new Set(tool.replace(/(\p{Ll})(\p{Lu})/gu, '$1 $2').toLowerCase().split(/[^\p{L}\p{N}]+/u))
  return GUESSED.find(([families]) => families.some(family => family.every(word => words.has(word))))?.[1] ?? ICON.other
}
