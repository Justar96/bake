/**
 * Rail icons for the few actions that change who is working or what is planned.
 *
 * An action's marker takes its state from colour and motion, as `MARKER.action`
 * does. Most calls, commands, reads, edits, and searches alike, keep that plain
 * dot: the head already names the tool, and a different shape per tool made
 * the rail busy without telling a reader anything the head did not. A shape
 * of its own is kept for a skill loaded into the conversation, a subagent
 * started, a message sent to one, and the task list, because those change
 * what the session knows, who is doing the work, or what is left to do.
 * Every icon is one cell wide by `string-width`, sits alone in the rail, and
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
  /** A task list update. */
  todo: '\u2610',
  /** Anything else. */
  other: MARKER.action,
} as const

/** An icon this vocabulary names. */
export type Icon = typeof ICON[keyof typeof ICON]

/** Icons for the tools Bake registers, by their model-visible names. */
const NAMED: Readonly<Record<string, Icon>> = {
  skill: ICON.skill,
  subagent: ICON.spawn,
  subagent_fork: ICON.spawn,
  send_message: ICON.send,
  todo_write: ICON.todo,
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
