/**
 * Rail icons that say what kind of action a call is.
 *
 * An action's marker takes its state from colour and motion, as `MARKER.action`
 * does. Its shape says what the call does, so a scan down the rail tells
 * a command from a read, and a message sent to a subagent from a job's output
 * read back.
 * Every icon is one cell wide by `string-width`, sits alone in the rail, and
 * has no emoji presentation, for the reason `MARKER` gives. The tool name is
 * still written in the head, so the icon never carries meaning on its own.
 *
 * @module @dsh-tui/ui/icons
 */

import { MARKER } from './layout.ts'

/** Icon for each kind of action. */
export const ICON = {
  /** A shell command. The prompt it would be typed at. */
  run: '$',
  /** A file or resource read. Lines of text. */
  read: '\u2261',
  /** A change to the workspace. */
  edit: '\u270e',
  /** A search of files or the web. */
  find: '\u2315',
  /** A download from the network. */
  fetch: '\u21e3',
  /** A listing of jobs, agents, or resources. */
  list: '\u22ee',
  /** A plan or task list update. */
  plan: '\u2610',
  /** A question put to the user. */
  ask: '?',
  /** A subagent started. Work handed down. */
  spawn: '\u21b3',
  /** A message sent to a running subagent, or a child's result sent back up. */
  send: '\u2192',
  /** Output read back from background work. */
  receive: '\u2190',
  /** A stop or interrupt of running work. */
  stop: '\u25a0',
  /** A multi-agent workflow. Work fanned out. */
  workflow: '\u21f6',
  /** A repeating worker loop. */
  loop: '\u21bb',
  /** A skill loaded into the conversation. */
  skill: '\u2726',
  /** Code run against the tools as an SDK. */
  code: '\u03bb',
  /** A goal read or update. */
  goal: '\u25ce',
  /** A deliverable presented to the user. */
  present: '\u25c8',
  /** A scheduled task. */
  schedule: '\u25f7',
  /** A look inside the running host or its plugins. */
  inspect: '\u25c7',
  /** A tool an MCP server provides. */
  mcp: '\u25c6',
  /** Anything else. */
  other: MARKER.action,
} as const

/** An icon this vocabulary names. */
export type Icon = typeof ICON[keyof typeof ICON]

/** Icons for the tools Bake registers, by their model-visible names. */
const NAMED: Readonly<Record<string, Icon>> = {
  read: ICON.read,
  read_image: ICON.read,
  read_mcp_resource: ICON.read,
  write: ICON.edit,
  edit: ICON.edit,
  str_replace_editor: ICON.edit,
  grep: ICON.find,
  glob: ICON.find,
  web_search: ICON.find,
  web_fetch: ICON.fetch,
  bash: ICON.run,
  pwsh: ICON.run,
  job_output: ICON.receive,
  job_list: ICON.list,
  job_kill: ICON.stop,
  todo_write: ICON.plan,
  exit_plan_mode: ICON.plan,
  ask_user_question: ICON.ask,
  skill: ICON.skill,
  get_goal: ICON.goal,
  create_goal: ICON.goal,
  update_goal: ICON.goal,
  subagent: ICON.spawn,
  subagent_fork: ICON.spawn,
  send_message: ICON.send,
  structured_output: ICON.send,
  interrupt_agent: ICON.stop,
  list_agents: ICON.list,
  list_subagent_models: ICON.list,
  list_mcp_resources: ICON.list,
  list_mcp_resource_templates: ICON.list,
  workflow: ICON.workflow,
  ralph: ICON.loop,
  run_code: ICON.code,
  present: ICON.present,
  schedule_create: ICON.schedule,
  schedule_list: ICON.schedule,
  schedule_delete: ICON.schedule,
  cordis_inspect_list: ICON.inspect,
  cordis_inspect_query: ICON.inspect,
  plugin_manager: ICON.inspect,
}

/**
 * Words that place a name Bake does not register itself, such as a plugin's
 * tool or a renamed one, checked in order. The first match wins, so more
 * specific words come before the families they share a name with.
 */
const GUESSED: readonly (readonly [readonly string[], Icon])[] = [
  [['question', 'ask'], ICON.ask],
  [['send', 'message', 'reply'], ICON.send],
  [['subagent', 'agent', 'spawn', 'fork', 'delegate'], ICON.spawn],
  [['kill', 'cancel', 'interrupt', 'stop'], ICON.stop],
  [['workflow'], ICON.workflow],
  [['schedule', 'cron'], ICON.schedule],
  [['goal'], ICON.goal],
  [['skill'], ICON.skill],
  [['todo', 'plan'], ICON.plan],
  [['bash', 'shell', 'exec', 'pwsh', 'run'], ICON.run],
  [['write', 'edit', 'patch', 'replace', 'create', 'update', 'delete'], ICON.edit],
  [['search', 'grep', 'glob', 'find', 'query'], ICON.find],
  [['list'], ICON.list],
  [['read', 'cat', 'view', 'get'], ICON.read],
  [['fetch', 'http', 'web', 'url', 'download'], ICON.fetch],
]

/**
 * Icon for what a tool does, from its name.
 * @param tool - tool name from the session log.
 * @returns the rail icon for the call's head.
 */
export function iconFor(tool: string): Icon {
  const named = NAMED[tool.toLowerCase()]
  if (named !== undefined) return named
  if (tool.startsWith('mcp__')) return ICON.mcp
  // Whole words only, split at separators and camel case, so `frobnicate`
  // is not a `cat`.
  const words = new Set(tool.replace(/(\p{Ll})(\p{Lu})/gu, '$1 $2').toLowerCase().split(/[^\p{L}\p{N}]+/u))
  return GUESSED.find(([family]) => family.some(word => words.has(word)))?.[1] ?? ICON.other
}
